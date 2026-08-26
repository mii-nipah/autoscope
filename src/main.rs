mod control;
mod handlers;
mod input;
mod launch;
mod render;
mod state;

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
use launch::{HostSession, LaunchSpec, Sandbox};
use serde_json::json;
use smithay::reexports::{calloop::EventLoop, wayland_server::Display};
use state::Autowayland;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run one application inside a private automation session.
    Run(RunArgs),
    /// Send one automation command to an instance.
    Ctl(CtlArgs),
    /// Copy the realtime AWF1 frame stream to stdout.
    Stream { socket: PathBuf },
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
    /// Give a bwrap-launched application a private network namespace.
    #[arg(long)]
    no_network: bool,
    /// Launch a Flatpak app ID. Arguments after -- are passed to the app.
    #[arg(long)]
    flatpak: Option<String>,
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
    },
    MouseDown {
        #[arg(default_value = "left")]
        button: String,
    },
    MouseUp {
        #[arg(default_value = "left")]
        button: String,
    },
    Scroll {
        dx: f64,
        dy: f64,
    },
    #[command(name = "type")]
    Type {
        text: String,
    },
    Key {
        combo: String,
    },
    Screenshot {
        path: PathBuf,
    },
    RecordStart {
        path: PathBuf,
    },
    RecordStop,
    Quit,
}

fn main() -> Result<()> {
    init_logging();
    match Cli::parse().command {
        Command::Run(args) => run(args),
        Command::Ctl(args) => ctl(args),
        Command::Stream { socket } => control::stream_to_stdout(&socket),
    }
}

fn run(args: RunArgs) -> Result<()> {
    validate_run_args(&args)?;
    let host = HostSession::capture()?;
    let runtime = tempfile::Builder::new()
        .prefix(&format!("autowayland-{}-", safe_name(&args.name)))
        .tempdir_in(&host.runtime_dir)
        .context("create private instance runtime directory")?;

    let mut event_loop: EventLoop<Autowayland> = EventLoop::try_new()?;
    let display: Display<Autowayland> = Display::new()?;
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

    let mut state = Autowayland::new(
        &mut event_loop,
        display,
        control_rx,
        stream_listener,
        (args.width, args.height),
        args.fps,
    );
    render::install(&mut event_loop, &mut state, backend, args.fps)?;

    let spec = match args.flatpak {
        Some(app_id) => LaunchSpec::Flatpak {
            app_id,
            args: args.app,
        },
        None => LaunchSpec::Command {
            argv: args.app,
            sandbox: args.sandbox.into(),
            network: !args.no_network,
        },
    };
    let child = launch::spawn(spec, runtime.path(), &state.socket_name, &host)?;
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

    println!(
        "{}",
        json!({
            "status": "ready",
            "pid": std::process::id(),
            "app_pid": child_pid,
            "control": control_path,
            "stream": stream_path,
            "size": [args.width, args.height],
            "fps": args.fps,
        })
    );
    io::stdout().flush()?;

    let result = event_loop.run(None, &mut state, |_| {});
    state.shutdown();
    let cleanup = cleanup_runtime(runtime.path());
    result?;
    cleanup?;
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
        CtlCommand::Move { x, y } => Request::Move { x, y },
        CtlCommand::Click { button, x, y } => Request::Click { button, x, y },
        CtlCommand::MouseDown { button } => Request::Button {
            button,
            pressed: true,
        },
        CtlCommand::MouseUp { button } => Request::Button {
            button,
            pressed: false,
        },
        CtlCommand::Scroll { dx, dy } => Request::Scroll { dx, dy },
        CtlCommand::Type { text } => Request::Type { text },
        CtlCommand::Key { combo } => Request::Key { combo },
        CtlCommand::Screenshot { path } => Request::Screenshot { path },
        CtlCommand::RecordStart { path } => Request::RecordStart { path },
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
    if !(320..=3840).contains(&args.width) || !(200..=2160).contains(&args.height) {
        bail!("size must be between 320x200 and 3840x2160");
    }
    if !(1..=60).contains(&args.fps) {
        bail!("fps must be between 1 and 60");
    }
    if args.flatpak.is_none() && args.app.is_empty() {
        bail!("provide --flatpak APP_ID or a command after --");
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
        .unwrap_or_else(|_| "autowayland=info,warn".into());
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

#[cfg(test)]
mod tests {
    use super::safe_name;

    #[test]
    fn instance_names_cannot_escape_the_runtime_directory() {
        assert_eq!(safe_name("../../qa chrome"), "qachrome");
        assert_eq!(safe_name(""), "instance");
    }
}
