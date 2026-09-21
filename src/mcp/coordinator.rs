use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, BufRead, BufReader},
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::control::{self, RecordingMode, RecordingResult, Request};

const START_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub(super) struct SessionOptions {
    /// Human-readable session name used by logs and the optional viewer.
    name: Option<String>,
    /// Virtual display width in pixels (default 1280).
    width: Option<i32>,
    /// Virtual display height in pixels (default 800).
    height: Option<i32>,
    /// Capture and visualization frame rate (default 15).
    fps: Option<u32>,
    /// Open a read-only host visualization window.
    window: Option<bool>,
    /// Remove application network access.
    no_network: Option<bool>,
    /// Host folder shared read/write with the app; defaults to a durable private folder.
    shared_dir: Option<PathBuf>,
    /// Absolute application working directory; also mounted read-only at /work under bwrap.
    working_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(super) enum SandboxMode {
    Auto,
    Bwrap,
    Off,
}

impl SandboxMode {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Bwrap => "bwrap",
            Self::Off => "off",
        }
    }
}

pub(super) enum Application {
    Command {
        argv: Vec<String>,
        sandbox: SandboxMode,
    },
    Flatpak {
        id: String,
        args: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub(super) struct SessionInfo {
    pub(super) session: String,
    name: String,
    width: i32,
    height: i32,
    fps: u32,
    pub(super) shared_dir: PathBuf,
    session_dir: PathBuf,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(super) struct ActionResult {
    pub(super) session: String,
    pub(super) result: Value,
}

pub(super) struct Capture {
    pub(super) info: SessionInfo,
    pub(super) frame: u64,
    pub(super) view: u64,
    pub(super) bytes: Vec<u8>,
}

#[derive(Deserialize)]
struct Ready {
    status: String,
    control: PathBuf,
    size: [i32; 2],
    fps: u32,
    shared_dir: PathBuf,
    session: PathBuf,
}

struct Session {
    info: SessionInfo,
    control: PathBuf,
    child: Child,
    recording: bool,
    observed_view: Option<u64>,
}

impl Session {
    fn wait_for_surface(&mut self) -> Result<()> {
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait()? {
                bail!("application session exited before showing a window: {status}");
            }
            if let Ok(response) = control::send_request(&self.control, &Request::Info)
                && response.ok
                && response
                    .result
                    .as_ref()
                    .and_then(|result| result["surfaces"].as_u64())
                    .is_some_and(|surfaces| surfaces > 0)
            {
                let response = control::send_request(
                    &self.control,
                    &Request::Wait {
                        timeout_ms: 5_000,
                        quiet_ms: 750,
                    },
                )?;
                if !response.ok {
                    bail!(
                        response
                            .error
                            .unwrap_or_else(|| "visual wait failed".into())
                    );
                }
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!("application did not show a window within 10 seconds");
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn stop(mut self) -> Result<()> {
        let _ = control::send_request(&self.control, &Request::Quit);
        let deadline = Instant::now() + STOP_TIMEOUT;
        while Instant::now() < deadline {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
        }
        self.child
            .kill()
            .context("kill unresponsive autoscope session")?;
        self.child.wait().context("reap autoscope session")?;
        Ok(())
    }
}

pub(super) struct Coordinator {
    sessions: BTreeMap<String, Session>,
    next_session: u64,
    next_artifact: u64,
    artifacts: PathBuf,
}

impl Coordinator {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            sessions: BTreeMap::new(),
            next_session: 1,
            next_artifact: 1,
            artifacts: crate::session::artifact_directory("mcp")?,
        })
    }

    pub(super) fn spawn(
        &mut self,
        application: Application,
        options: SessionOptions,
    ) -> Result<SessionInfo> {
        if matches!(&application, Application::Command { argv, .. } if argv.is_empty()) {
            bail!("command must contain a program");
        }
        let width = options.width.unwrap_or(1280);
        let height = options.height.unwrap_or(800);
        let fps = options.fps.unwrap_or(15);
        crate::validate_display(width, height, fps)?;
        let session_id = format!("app-{}", self.next_session);
        self.next_session += 1;
        let name = options.name.unwrap_or_else(|| session_id.clone());
        let mut command = Command::new(env::current_exe().context("locate autoscope executable")?);
        if let Some(path) = options.working_dir {
            if !path.is_absolute() {
                bail!("working_dir must be an absolute directory path");
            }
            command.current_dir(path.canonicalize().context("resolve working_dir")?);
        }
        command
            .arg("run")
            .arg("--coordinator")
            .arg("--name")
            .arg(&name)
            .arg("--width")
            .arg(width.to_string())
            .arg("--height")
            .arg(height.to_string())
            .arg("--fps")
            .arg(fps.to_string());
        if options.window.unwrap_or(false) {
            command.arg("--window");
        }
        if options.no_network.unwrap_or(false) {
            command.arg("--no-network");
        }
        if let Some(path) = options.shared_dir {
            command.arg("--shared-dir").arg(path);
        }
        match application {
            Application::Command { argv, sandbox } => {
                command
                    .arg("--sandbox")
                    .arg(sandbox.as_str())
                    .arg("--")
                    .args(argv);
            }
            Application::Flatpak { id, args } => {
                command.arg("--flatpak").arg(id).arg("--").args(args);
            }
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        set_parent_death_signal(&mut command);
        let mut child = command.spawn().context("spawn autoscope session")?;
        let ready = match wait_until_ready(&mut child) {
            Ok(ready) => ready,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        if ready.status != "ready" {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "autoscope child returned unexpected status {:?}",
                ready.status
            );
        }
        let info = SessionInfo {
            session: session_id.clone(),
            name,
            width: ready.size[0],
            height: ready.size[1],
            fps: ready.fps,
            shared_dir: ready.shared_dir,
            session_dir: ready.session,
        };
        let mut session = Session {
            info: info.clone(),
            control: ready.control,
            child,
            recording: false,
            observed_view: None,
        };
        if let Err(error) = session.wait_for_surface() {
            let _ = session.stop();
            return Err(error);
        }
        self.sessions.insert(session_id, session);
        Ok(info)
    }

    fn reap(&mut self) -> Result<()> {
        let mut exited = Vec::new();
        for (id, session) in &mut self.sessions {
            if session.child.try_wait()?.is_some() {
                exited.push(id.clone());
            }
        }
        for id in exited {
            self.sessions.remove(&id);
        }
        Ok(())
    }

    pub(super) fn list(&mut self) -> Result<Vec<SessionInfo>> {
        self.reap()?;
        Ok(self.sessions.values().map(|s| s.info.clone()).collect())
    }

    fn request(&mut self, session: &str, request: Request) -> Result<Value> {
        self.reap()?;
        let session = self
            .sessions
            .get(session)
            .with_context(|| format!("unknown or exited session {session:?}"))?;
        let response = control::send_request(&session.control, &request)?;
        if !response.ok {
            bail!(response.error.unwrap_or_else(|| "command failed".into()));
        }
        Ok(response.result.unwrap_or(Value::Null))
    }

    pub(super) fn action(&mut self, session: &str, request: Request) -> Result<ActionResult> {
        let result = self.request(session, request)?;
        Ok(ActionResult {
            session: session.into(),
            result,
        })
    }

    pub(super) fn observed_action(
        &mut self,
        session: &str,
        view: u64,
        request: Request,
    ) -> Result<(ActionResult, Capture)> {
        self.guard_view(session, view)?;
        let action = self.action(session, request)?;
        let capture = self.screenshot(session)?;
        Ok((action, capture))
    }

    fn guard_view(&mut self, session: &str, expected: u64) -> Result<()> {
        let observed = self
            .sessions
            .get(session)
            .with_context(|| format!("unknown or exited session {session:?}"))?
            .observed_view
            .context("take a screenshot before sending input")?;
        if observed != expected {
            bail!(
                "view {expected} is not the latest screenshot delivered for {session}; use view {observed}"
            );
        }
        Ok(())
    }

    fn artifact_path(&mut self, session: &str, extension: &str) -> PathBuf {
        let sequence = self.next_artifact;
        self.next_artifact += 1;
        self.artifacts
            .join(format!("{session}-{sequence}.{extension}"))
    }

    pub(super) fn screenshot(&mut self, session: &str) -> Result<Capture> {
        self.reap()?;
        let info = self
            .sessions
            .get(session)
            .with_context(|| format!("unknown or exited session {session:?}"))?
            .info
            .clone();
        let path = self.artifact_path(session, "png");
        let result = self.request(session, Request::Screenshot { path: path.clone() })?;
        let frame = result["frame"]
            .as_u64()
            .context("screenshot omitted frame")?;
        let view = result["view"].as_u64().context("screenshot omitted view")?;
        let bytes = fs::read(&path).context("read captured screenshot")?;
        let _ = fs::remove_file(path);
        self.sessions.get_mut(session).unwrap().observed_view = Some(view);
        Ok(Capture {
            info,
            frame,
            view,
            bytes,
        })
    }

    pub(super) fn start_recording(
        &mut self,
        session: &str,
        fps: Option<u32>,
        mode: RecordingMode,
        frames_per_image: u32,
    ) -> Result<ActionResult> {
        self.reap()?;
        if self
            .sessions
            .get(session)
            .with_context(|| format!("unknown or exited session {session:?}"))?
            .recording
        {
            bail!("session is already recording");
        }
        let extension = match mode {
            RecordingMode::Video => "mp4",
            RecordingMode::Images => "png",
        };
        let path = self.artifact_path(session, extension);
        let result = self.request(
            session,
            Request::RecordStart {
                path,
                fps,
                mode,
                frames_per_image,
            },
        )?;
        self.sessions.get_mut(session).unwrap().recording = true;
        Ok(ActionResult {
            session: session.into(),
            result,
        })
    }

    pub(super) fn stop_recording(&mut self, session: &str) -> Result<RecordingResult> {
        if !self
            .sessions
            .get(session)
            .with_context(|| format!("unknown or exited session {session:?}"))?
            .recording
        {
            bail!("session was not recording");
        }
        let result = self.request(session, Request::RecordStop)?;
        self.sessions.get_mut(session).unwrap().recording = false;
        serde_json::from_value(result).context("decode recording result")
    }

    pub(super) fn close(&mut self, session: &str) -> Result<SessionInfo> {
        self.reap()?;
        let session = self
            .sessions
            .remove(session)
            .with_context(|| format!("unknown or exited session {session:?}"))?;
        let info = session.info.clone();
        session.stop()?;
        Ok(info)
    }

    pub(super) fn shutdown_all(&mut self) {
        for (_, session) in std::mem::take(&mut self.sessions) {
            let _ = session.stop();
        }
    }
}

impl Drop for Coordinator {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}

fn wait_until_ready(child: &mut Child) -> Result<Ready> {
    let stdout = child
        .stdout
        .take()
        .context("capture autoscope ready output")?;
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout)
            .read_line(&mut line)
            .and_then(|read| {
                if read == 0 {
                    Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "autoscope exited before becoming ready",
                    ))
                } else {
                    Ok(line)
                }
            });
        let _ = sender.send(result);
    });
    let line = match receiver.recv_timeout(START_TIMEOUT) {
        Ok(result) => result?,
        Err(_) => bail!("autoscope session did not become ready within 10 seconds"),
    };
    serde_json::from_str(&line).context("decode autoscope ready output")
}

fn set_parent_death_signal(command: &mut Command) {
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() == 1 {
                return Err(io::Error::from_raw_os_error(libc::ECHILD));
            }
            Ok(())
        });
    }
}
