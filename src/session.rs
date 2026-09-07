use std::{
    env, fs,
    io::{self, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

/// Artifacts live outside the disposable display runtime, including after exit.
pub(crate) fn artifact_directory(name: &str) -> Result<PathBuf> {
    let state = match env::var_os("XDG_STATE_HOME").filter(|p| Path::new(p).is_absolute()) {
        Some(path) => PathBuf::from(path),
        None => {
            PathBuf::from(env::var_os("HOME").context("HOME is required")?).join(".local/state")
        }
    };
    let parent = state.join("autoscope/sessions");
    fs::create_dir_all(&parent).context("create autoscope artifact directory")?;
    Ok(tempfile::Builder::new()
        .prefix(&format!("{}-", crate::safe_name(name)))
        .tempdir_in(parent)?
        .keep())
}

pub(crate) struct Files {
    pub root: PathBuf,
    pub shared: PathBuf,
}

impl Files {
    pub fn create(name: &str, shared: Option<&Path>) -> Result<Self> {
        let root = artifact_directory(name)?;
        let shared = shared
            .map(Path::to_owned)
            .unwrap_or_else(|| root.join("files"));
        fs::create_dir_all(&shared)
            .with_context(|| format!("create shared directory {}", shared.display()))?;
        let files = Self {
            root,
            // Preserve the user's absolute spelling (including /home -> /var/home aliases),
            // so application arguments can use the path supplied to --shared-dir.
            shared: std::path::absolute(&shared)?,
        };
        files.log_file()?;
        files.update(json!({
            "status": "starting", "session": files.root, "shared_dir": files.shared,
            "log": files.root.join("session.log"),
        }))?;
        Ok(files)
    }

    pub fn reopen(root: PathBuf) -> Result<Self> {
        let record = read(&root)?;
        Ok(Self {
            root,
            shared: PathBuf::from(
                record["shared_dir"]
                    .as_str()
                    .context("session omitted shared_dir")?,
            ),
        })
    }

    pub fn log_file(&self) -> Result<fs::File> {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("session.log"))
            .context("open session log")
    }

    pub fn update(&self, fields: Value) -> Result<Value> {
        let path = self.root.join("session.json");
        let mut record = if path.exists() {
            read(&self.root)?
        } else {
            json!({})
        };
        record
            .as_object_mut()
            .context("session record is not an object")?
            .extend(
                fields
                    .as_object()
                    .context("session update is not an object")?
                    .clone(),
            );
        let mut file = tempfile::NamedTempFile::new_in(&self.root)?;
        serde_json::to_writer_pretty(&mut file, &record)?;
        file.flush()?;
        file.persist(path).context("save session status")?;
        Ok(record)
    }
}

pub(crate) fn read(directory: &Path) -> Result<Value> {
    let record: Value = serde_json::from_slice(
        &fs::read(directory.join("session.json"))
            .context("read session status; use the session directory from the ready response")?,
    )?;
    if !record.is_object() {
        bail!("invalid session status");
    }
    Ok(record)
}

/// Detach only the explicit CLI mode. MCP children retain their owned lifetime.
pub(crate) fn detach(files: &Files) -> Result<()> {
    let mut command = Command::new(env::current_exe()?);
    command
        .args(worker_arguments(env::args_os().skip(1), &files.root))
        .stdin(Stdio::null())
        .stdout(files.log_file()?)
        .stderr(files.log_file()?);
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().context("start detached session")?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let record = read(&files.root)?;
        if record["status"] == "ready" {
            println!("{record}");
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            bail!(
                "session exited during startup ({status}); see {}",
                files.root.join("session.log").display()
            );
        }
        if Instant::now() >= deadline {
            // Request graceful cleanup first; do not leave a half-started session behind.
            unsafe {
                libc::kill(child.id() as i32, libc::SIGTERM);
            }
            for _ in 0..40 {
                if child.try_wait()?.is_some() {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            if child.try_wait()?.is_none() {
                child.kill()?;
            }
            child.wait()?;
            bail!(
                "session startup timed out; see {}",
                files.root.join("session.log").display()
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn worker_arguments(
    args: impl Iterator<Item = std::ffi::OsString>,
    root: &Path,
) -> Vec<std::ffi::OsString> {
    let mut args = args.peekable();
    let mut worker = Vec::new();
    while let Some(arg) = args.next_if(|arg| arg != "--") {
        if arg != "--detach" {
            worker.push(arg);
        }
    }
    worker.extend(["--session-dir".into(), root.as_os_str().to_owned()]);
    worker.extend(args);
    worker
}

pub(crate) fn status(directory: &Path) -> Result<Value> {
    let mut record = read(directory)?;
    if record["status"] == "ready" {
        let control = Path::new(
            record["control"]
                .as_str()
                .context("session omitted control socket")?,
        );
        // A dead process may leave a ready record after SIGKILL or a machine restart.
        match crate::control::send_request_with_timeout(
            control,
            &crate::control::Request::Info,
            Some(Duration::from_secs(2)),
        ) {
            Ok(response) if response.ok => {
                record["live"] = response.result.unwrap_or(Value::Null);
            }
            result => {
                record["status"] = "unreachable".into();
                record["error"] = match result {
                    Err(error) => error.to_string(),
                    Ok(response) => response
                        .error
                        .unwrap_or_else(|| "session did not answer".into()),
                }
                .into();
            }
        }
    }
    Ok(record)
}

#[derive(Debug, Serialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub(crate) enum ExitReason {
    Requested,
    Application {
        code: Option<i32>,
        signal: Option<i32>,
    },
    Signal {
        signal: i32,
        sender_pid: u32,
    },
    Error {
        message: String,
    },
}

static STOP_SIGNAL: AtomicU64 = AtomicU64::new(0);

extern "C" fn receive_signal(signal: i32, info: *mut libc::siginfo_t, _: *mut libc::c_void) {
    // Only a lock-free atomic store in the signal handler; cleanup runs in the event loop.
    let sender = if info.is_null() {
        0
    } else {
        (unsafe { (*info).si_pid() }) as u32
    };
    let _ = STOP_SIGNAL.compare_exchange(
        0,
        (u64::from(sender) << 32) | signal as u64,
        Ordering::Relaxed,
        Ordering::Relaxed,
    );
}

pub(crate) fn install_signals() -> Result<()> {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = receive_signal as *const () as usize;
        action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
        if unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } == -1 {
            return Err(io::Error::last_os_error()).context("install session shutdown handler");
        }
    }
    Ok(())
}

pub(crate) fn received_signal() -> Option<ExitReason> {
    let value = STOP_SIGNAL.load(Ordering::Relaxed);
    (value != 0).then_some(ExitReason::Signal {
        signal: value as u32 as i32,
        sender_pid: (value >> 32) as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detaching_preserves_application_arguments_including_its_own_detach_flag() {
        let args = [
            "run", "--detach", "--name", "art app", "--", "app", "--detach", "a b",
        ];
        let worker = worker_arguments(
            args.into_iter().map(Into::into),
            Path::new("/session files"),
        );
        assert_eq!(
            worker,
            [
                "run",
                "--name",
                "art app",
                "--session-dir",
                "/session files",
                "--",
                "app",
                "--detach",
                "a b"
            ]
            .map(std::ffi::OsString::from)
        );
    }
}
