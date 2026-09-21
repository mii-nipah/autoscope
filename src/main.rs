mod control;
mod cursor;
mod handlers;
mod input;
mod launch;
mod mcp;
mod render;
mod session;
mod state;
mod x11;

use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    path::PathBuf,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use control::Request;
use launch::{HostSession, LaunchSession, LaunchSpec, Sandbox};
use serde_json::json;
use smithay::reexports::{calloop::EventLoop, wayland_server::Display};
use state::Autoscope;

const COMMAND_GUIDE: &str = include_str!("../USAGE.md");

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the complete non-MCP command guide.
    Readme,
    /// Run one application inside a private automation session.
    Run(RunArgs),
    /// Inspect a running or exited session using its durable session directory.
    Status { session: PathBuf },
    /// Open a read-only viewer of an existing session.
    Attach { session: PathBuf },
    /// Send one automation command to an instance.
    Ctl(CtlArgs),
    /// Copy the realtime ASF1 frame stream to stdout.
    Stream { socket: PathBuf },
    /// Run a stdio MCP server that owns and controls application sessions.
    Mcp,
}

#[derive(Args)]
struct RunArgs {
    #[arg(long, default_value = "instance")]
    name: String,
    #[arg(long, default_value_t = 1280)]
    width: i32,
    #[arg(long, default_value_t = 800)]
    height: i32,
    #[arg(long, default_value_t = 15)]
    fps: u32,
    /// Open a read-only live viewer; viewer input is never forwarded.
    #[arg(long)]
    window: bool,
    #[arg(long, value_enum, default_value_t = SandboxArg::Auto)]
    sandbox: SandboxArg,
    /// Remove network access from the application sandbox.
    #[arg(long)]
    no_network: bool,
    /// A host folder shared read/write at the same absolute path inside the app.
    /// Defaults to a durable, private folder reported in the ready response.
    #[arg(long)]
    shared_dir: Option<PathBuf>,
    /// Keep the session running after this command exits; return its ready JSON.
    #[arg(long, conflicts_with_all = ["coordinator", "session_dir"])]
    detach: bool,
    #[arg(long, hide = true)]
    session_dir: Option<PathBuf>,
    /// Launch a Flatpak app ID. Arguments after -- are passed to the app.
    #[arg(long)]
    flatpak: Option<String>,
    /// Reserve stdout for the coordinator readiness handshake.
    #[arg(long, hide = true)]
    coordinator: bool,
    /// Command and arguments. Use -- before the command.
    #[arg(last = true)]
    app: Vec<OsString>,
}

#[derive(Clone, Copy, ValueEnum)]
enum SandboxArg {
    Auto,
    Bwrap,
    Off,
}

impl From<SandboxArg> for Sandbox {
    fn from(value: SandboxArg) -> Self {
        match value {
            SandboxArg::Auto => Sandbox::Auto,
            SandboxArg::Bwrap => Sandbox::Bwrap,
            SandboxArg::Off => Sandbox::Off,
        }
    }
}

#[derive(Args)]
struct CtlArgs {
    socket: PathBuf,
    #[command(subcommand)]
    command: CtlCommand,
}

#[derive(Subcommand)]
enum CtlCommand {
    Info,
    Move {
        /// Interpret X and Y as fractions of the session size (0.0 to 1.0).
        #[arg(long)]
        normalize: bool,
        /// Return after input injection instead of waiting for visual stability.
        #[arg(long)]
        no_wait: bool,
        x: f64,
        y: f64,
    },
    Click {
        #[arg(long, default_value = "left")]
        button: String,
        #[arg(long, requires = "y")]
        x: Option<f64>,
        #[arg(long, requires = "x")]
        y: Option<f64>,
        /// Interpret X and Y as fractions of the session size (0.0 to 1.0).
        #[arg(long, requires = "x")]
        normalize: bool,
        /// Return after input injection instead of waiting for visual stability.
        #[arg(long)]
        no_wait: bool,
    },
    MouseDown {
        #[arg(default_value = "left")]
        button: String,
    },
    /// Draw or drag through a complete path; settle once after releasing the button.
    Drag {
        #[command(flatten)]
        path: control::DragPath,
        #[arg(long)]
        no_wait: bool,
    },
    MouseUp {
        #[arg(default_value = "left")]
        button: String,
    },
    Scroll {
        /// Return after input injection instead of waiting for visual stability.
        #[arg(long)]
        no_wait: bool,
        dx: f64,
        dy: f64,
    },
    #[command(name = "type")]
    Type {
        /// Return after paced typing instead of waiting for visual stability.
        #[arg(long)]
        no_wait: bool,
        text: String,
    },
    Key {
        /// Return after input injection instead of waiting for visual stability.
        #[arg(long)]
        no_wait: bool,
        combo: String,
    },
    /// Wait until the significant screen contents stop changing.
    Wait {
        #[arg(long, default_value_t = 5_000)]
        timeout_ms: u64,
        #[arg(long, default_value_t = 600)]
        quiet_ms: u64,
    },
    Screenshot {
        path: PathBuf,
    },
    RecordStart {
        path: PathBuf,
        /// Recording FPS; defaults to the session FPS.
        #[arg(long)]
        fps: Option<u32>,
        #[arg(long, value_enum, default_value_t = RecordingModeArg::Video)]
        mode: RecordingModeArg,
        /// Frames tiled into each image sheet (images mode, maximum 10).
        #[arg(long, default_value_t = 10)]
        frames_per_image: u32,
    },
    RecordStop,
    Quit,
}

#[derive(Clone, Copy, ValueEnum)]
enum RecordingModeArg {
    Video,
    Images,
}

impl From<RecordingModeArg> for control::RecordingMode {
    fn from(value: RecordingModeArg) -> Self {
        match value {
            RecordingModeArg::Video => Self::Video,
            RecordingModeArg::Images => Self::Images,
        }
    }
}

fn main() -> Result<()> {
    init_logging();
    match Cli::parse().command {
        Command::Readme => io::stdout()
            .write_all(COMMAND_GUIDE.as_bytes())
            .context("write command guide"),
        Command::Run(args) => run(args),
        Command::Status { session } => {
            println!("{}", session::status(&session)?);
            Ok(())
        }
        Command::Attach { session } => control::Viewer::attach(&session),
        Command::Ctl(args) => ctl(args),
        Command::Stream { socket } => control::stream_to_stdout(&socket),
        Command::Mcp => mcp::run(),
    }
}

fn run(args: RunArgs) -> Result<()> {
    validate_run_args(&args)?;
    let files = match &args.session_dir {
        Some(path) => session::Files::reopen(path.clone())?,
        None => session::Files::create(&args.name, args.shared_dir.as_deref())?,
    };
    let result = if args.detach {
        session::detach(&files)
    } else {
        session::install_signals().and_then(|_| run_session(args, &files))
    };
    if let Err(error) = &result {
        let _ = files.update(json!({"status": "failed", "error": format!("{error:#}")}));
    }
    result
}

fn run_session(args: RunArgs, files: &session::Files) -> Result<()> {
    let host = HostSession::capture()?;
    let runtime = tempfile::Builder::new()
        .prefix(&format!("autoscope-{}-", safe_name(&args.name)))
        .tempdir_in(&host.runtime_dir)
        .context("create private instance runtime directory")?;

    let mut event_loop: EventLoop<'static, Autoscope> = EventLoop::try_new()?;
    let display: Display<Autoscope> = Display::new()?;
    let backend = render::create_headless_backend(args.width, args.height)?;

    // Children inherit only the instance runtime directory and cannot discover
    // the host Wayland socket captured above for Flatpak/viewer launch plumbing.
    unsafe { std::env::set_var("XDG_RUNTIME_DIR", runtime.path()) };
    unsafe { std::env::remove_var("DISPLAY") };

    let control_path = runtime.path().join("control.sock");
    let stream_path = runtime.path().join("stream.sock");
    let control_rx = control::start_server(&control_path)?;
    let stream_listener = std::os::unix::net::UnixListener::bind(&stream_path)?;
    stream_listener.set_nonblocking(true)?;

    let mut state = Autoscope::new(
        &mut event_loop,
        display,
        control_rx,
        stream_listener,
        (args.width, args.height),
        args.fps,
    );
    render::install(&mut event_loop, &mut state, backend, args.fps)?;
    let xdisplay = if launch::which("Xwayland").is_some() {
        Some(x11::start(&mut event_loop, &mut state)?)
    } else {
        tracing::info!("Xwayland is not installed; running with native Wayland support");
        None
    };

    let spec = match args.flatpak {
        Some(app_id) => LaunchSpec::Flatpak {
            app_id,
            args: args.app,
            network: !args.no_network,
        },
        None => LaunchSpec::Command {
            argv: args.app,
            sandbox: args.sandbox.into(),
            network: !args.no_network,
        },
    };
    let child = launch::spawn(
        spec,
        &LaunchSession {
            runtime: runtime.path(),
            wayland_display: &state.socket_name,
            xdisplay,
            log: &files.root.join("session.log"),
            shared: &files.shared,
        },
        &host,
    )?;
    let child_pid = child.id();
    state.app = Some(child);
    if args.window {
        state.viewer = Some(control::Viewer::start(
            args.width,
            args.height,
            args.fps,
            &args.name,
            &host,
        )?);
    }

    let ready = files.update(json!({
        "status": "ready",
        "pid": std::process::id(),
        "app_pid": child_pid,
        "control": control_path,
        "stream": stream_path,
        "size": [args.width, args.height],
        "fps": args.fps,
    }))?;
    println!("{ready}");
    io::stdout().flush()?;

    let result = event_loop.run(None, &mut state, |_| {});
    state.shutdown();
    x11::stop(&mut state);
    let cleanup = cleanup_runtime(runtime.path());
    result?;
    cleanup?;
    files.update(json!({"status": "exited", "exit": state.exit_reason}))?;
    Ok(())
}

fn cleanup_runtime(path: &std::path::Path) -> Result<()> {
    for attempt in 0..20 {
        match fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) if attempt == 19 => {
                return Err(error).context("remove private instance runtime directory");
            }
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
    unreachable!()
}

fn ctl(args: CtlArgs) -> Result<()> {
    let request = match args.command {
        CtlCommand::Info => Request::Info,
        CtlCommand::Drag { path, no_wait } => Request::Drag {
            path,
            wait: !no_wait,
            view: None,
        },
        CtlCommand::Move {
            x,
            y,
            normalize,
            no_wait,
        } => Request::Move {
            x,
            y,
            normalize,
            wait: !no_wait,
            view: None,
        },
        CtlCommand::Click {
            button,
            x,
            y,
            normalize,
            no_wait,
        } => Request::Click {
            button,
            x,
            y,
            normalize,
            wait: !no_wait,
            view: None,
        },
        CtlCommand::MouseDown { button } => Request::Button {
            button,
            pressed: true,
            view: None,
        },
        CtlCommand::MouseUp { button } => Request::Button {
            button,
            pressed: false,
            view: None,
        },
        CtlCommand::Scroll { dx, dy, no_wait } => Request::Scroll {
            dx,
            dy,
            wait: !no_wait,
            view: None,
        },
        CtlCommand::Type { text, no_wait } => Request::Type {
            text,
            wait: !no_wait,
            view: None,
        },
        CtlCommand::Key { combo, no_wait } => Request::Key {
            combo,
            wait: !no_wait,
            view: None,
        },
        CtlCommand::Wait {
            timeout_ms,
            quiet_ms,
        } => Request::Wait {
            timeout_ms,
            quiet_ms,
        },
        CtlCommand::Screenshot { path } => Request::Screenshot { path },
        CtlCommand::RecordStart {
            path,
            fps,
            mode,
            frames_per_image,
        } => Request::RecordStart {
            path,
            fps,
            mode: mode.into(),
            frames_per_image,
        },
        CtlCommand::RecordStop => Request::RecordStop,
        CtlCommand::Quit => Request::Quit,
    };
    let response = control::send_request(&args.socket, &request)?;
    println!("{}", serde_json::to_string(&response)?);
    if response.ok {
        Ok(())
    } else {
        bail!(response.error.unwrap_or_else(|| "command failed".into()))
    }
}

fn validate_run_args(args: &RunArgs) -> Result<()> {
    validate_display(args.width, args.height, args.fps)?;
    if args.flatpak.is_none() && args.app.is_empty() {
        bail!("provide --flatpak APP_ID or a command after --");
    }
    Ok(())
}

pub(crate) fn validate_display(width: i32, height: i32, fps: u32) -> Result<()> {
    if !(320..=3840).contains(&width) || !(200..=2160).contains(&height) {
        bail!("size must be between 320x200 and 3840x2160");
    }
    if !(1..=60).contains(&fps) {
        bail!("fps must be between 1 and 60");
    }
    Ok(())
}

fn safe_name(name: &str) -> String {
    let filtered: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(32)
        .collect();
    if filtered.is_empty() {
        "instance".into()
    } else {
        filtered
    }
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "autoscope=info,warn".into());
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(filter)
        .init();
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::{COMMAND_GUIDE, Cli, safe_name};

    #[test]
    fn instance_names_cannot_escape_the_runtime_directory() {
        assert_eq!(safe_name("../../qa chrome"), "qachrome");
        assert_eq!(safe_name(""), "instance");
    }

    #[test]
    fn command_guide_covers_the_public_non_mcp_cli() {
        let cli = Cli::command();
        for command in cli
            .get_subcommands()
            .filter(|command| command.get_name() != "mcp")
        {
            let invocation = format!("autoscope {}", command.get_name());
            assert!(
                COMMAND_GUIDE.contains(&invocation),
                "command guide omits {invocation}"
            );

            for option in command
                .get_arguments()
                .filter_map(|argument| argument.get_long())
                .filter(|option| !matches!(*option, "coordinator" | "session-dir"))
            {
                assert!(
                    COMMAND_GUIDE.contains(&format!("--{option}")),
                    "command guide omits --{option}"
                );
            }

            if command.get_name() == "ctl" {
                for control in command.get_subcommands() {
                    let invocation = format!("autoscope ctl SOCKET {}", control.get_name());
                    assert!(
                        COMMAND_GUIDE.contains(&invocation),
                        "command guide omits {invocation}"
                    );
                    for option in control
                        .get_arguments()
                        .filter_map(|argument| argument.get_long())
                    {
                        assert!(
                            COMMAND_GUIDE.contains(&format!("--{option}")),
                            "command guide omits --{option}"
                        );
                    }
                }
            }
        }
        assert!(!COMMAND_GUIDE.to_ascii_lowercase().contains("mcp"));
    }
}
