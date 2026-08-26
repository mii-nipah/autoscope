use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    os::unix::fs::FileTypeExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

use anyhow::{Context, Result, bail};

pub struct HostSession {
    pub runtime_dir: PathBuf,
    pub wayland_display: OsString,
}

impl HostSession {
    pub fn capture() -> Result<Self> {
        Ok(Self {
            runtime_dir: env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .context("XDG_RUNTIME_DIR is required for the nested backend")?,
            wayland_display: env::var_os("WAYLAND_DISPLAY")
                .context("WAYLAND_DISPLAY is required for the nested backend")?,
        })
    }
}

pub enum LaunchSpec {
    Flatpak {
        app_id: String,
        args: Vec<OsString>,
        network: bool,
    },
    Command {
        argv: Vec<OsString>,
        sandbox: Sandbox,
        network: bool,
    },
}

pub struct LaunchSession<'a> {
    pub runtime: &'a Path,
    pub wayland_display: &'a OsStr,
    pub xdisplay: u32,
    pub discard_stdout: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Sandbox {
    Auto,
    Bwrap,
    Off,
}

pub fn spawn(spec: LaunchSpec, session: &LaunchSession<'_>, host: &HostSession) -> Result<Child> {
    match spec {
        LaunchSpec::Flatpak {
            app_id,
            args,
            network,
        } => spawn_flatpak(&app_id, &args, network, session, host),
        LaunchSpec::Command {
            argv,
            sandbox,
            network,
        } => spawn_command(&argv, sandbox, network, session),
    }
}

fn spawn_flatpak(
    app_id: &str,
    args: &[OsString],
    network: bool,
    session: &LaunchSession<'_>,
    host: &HostSession,
) -> Result<Child> {
    let runtime_name = session
        .runtime
        .file_name()
        .context("instance runtime has no name")?;
    let home = session.runtime.join("home");
    fs::create_dir(&home)?;
    if app_id == "com.google.Chrome" {
        fs::create_dir_all(home.join("config/google-chrome"))?;
        fs::create_dir_all(session.runtime.join("app/com.google.Chrome"))?;
    }
    let app_command = if app_id == "com.google.Chrome" {
        "/app/extra/chrome".into()
    } else {
        flatpak_app_command(app_id)?
    };
    let app_args = flatpak_compat_args(app_id, args);
    let display = format!(":{}", session.xdisplay);
    let mut command = Command::new("flatpak");
    command.arg("run").args([
        "--sandbox",
        "--die-with-parent",
        "--no-session-bus",
        "--no-a11y-bus",
        "--no-documents-portal",
        "--device=dri",
        "--command=sh",
        "--nosocket=wayland",
        "--socket=x11",
    ]);
    if network {
        command.arg("--share=network");
    }
    command
        .arg(format!(
            "--filesystem=xdg-run/{}",
            runtime_name.to_string_lossy()
        ))
        .arg(format!(
            "--env=XDG_RUNTIME_DIR={}",
            session.runtime.display()
        ))
        .arg(format!("--env=HOME={}", home.display()))
        .arg("--env=DISPLAY=")
        .arg(app_id)
        .args([
            "-c",
            "export WAYLAND_DISPLAY=\"$1\" DISPLAY=\"$2\"; shift 2; export XDG_CONFIG_HOME=\"$HOME/config\" XDG_CACHE_HOME=\"$HOME/cache\" XDG_DATA_HOME=\"$HOME/data\" XDG_STATE_HOME=\"$HOME/state\"; mkdir -p \"$XDG_CONFIG_HOME\" \"$XDG_CACHE_HOME\" \"$XDG_DATA_HOME\" \"$XDG_STATE_HOME\"; exec \"$@\"",
            "autoscope-flatpak",
        ])
        .arg(session.wayland_display)
        .arg(&display)
        .arg(app_command)
        .args(app_args)
        .env("XDG_RUNTIME_DIR", &host.runtime_dir)
        .env("WAYLAND_DISPLAY", &host.wayland_display)
        .env("DISPLAY", &display)
        .env_remove("XAUTHORITY");
    spawn_child(
        command,
        session.discard_stdout,
        "launch Flatpak application",
    )
}

fn flatpak_compat_args(app_id: &str, args: &[OsString]) -> Vec<OsString> {
    let mut effective = Vec::new();
    if app_id == "com.google.Chrome" {
        for (prefix, value) in [
            ("--ozone-platform=", "--ozone-platform=wayland"),
            ("--no-sandbox", "--no-sandbox"),
            ("--test-type", "--test-type"),
            ("--no-first-run", "--no-first-run"),
            ("--no-default-browser-check", "--no-default-browser-check"),
        ] {
            if !args
                .iter()
                .any(|arg| arg.to_string_lossy().starts_with(prefix))
            {
                effective.push(value.into());
            }
        }
    }
    effective.extend_from_slice(args);
    effective
}

fn flatpak_app_command(app_id: &str) -> Result<String> {
    let output = Command::new("flatpak")
        .args(["info", "--show-metadata", app_id])
        .output()
        .context("read Flatpak application metadata")?;
    if !output.status.success() {
        bail!("flatpak info failed for {app_id}");
    }
    String::from_utf8(output.stdout)?
        .lines()
        .find_map(|line| line.strip_prefix("command=").map(str::to_owned))
        .context("Flatpak metadata has no application command")
}

fn spawn_command(
    argv: &[OsString],
    sandbox: Sandbox,
    network: bool,
    session: &LaunchSession<'_>,
) -> Result<Child> {
    let Some(program) = argv.first() else {
        bail!("empty application command");
    };
    if sandbox == Sandbox::Off || (sandbox == Sandbox::Auto && which("bwrap").is_none()) {
        let mut command = Command::new(program);
        command
            .args(&argv[1..])
            .env("XDG_RUNTIME_DIR", session.runtime)
            .env("WAYLAND_DISPLAY", session.wayland_display)
            .env("DISPLAY", format!(":{}", session.xdisplay))
            .env_remove("XAUTHORITY")
            .env_remove("DBUS_SESSION_BUS_ADDRESS");
        return spawn_child(command, session.discard_stdout, "launch application");
    }

    let cwd = env::current_dir()?;
    let home = session.runtime.join("home");
    fs::create_dir(&home)?;
    let x_socket = PathBuf::from(format!("/tmp/.X11-unix/X{}", session.xdisplay));
    if !fs::metadata(&x_socket)?.file_type().is_socket() {
        bail!("private X11 display socket is not a socket");
    }
    let uid_dir = session
        .runtime
        .parent()
        .context("runtime has no user directory")?;
    let mut command = Command::new("bwrap");
    command
        .args(["--die-with-parent", "--new-session", "--unshare-all"])
        .args(["--proc", "/proc", "--dev", "/dev"])
        .args(["--ro-bind", "/usr", "/usr", "--ro-bind", "/etc", "/etc"])
        .args(["--ro-bind", "/sys", "/sys"])
        .args([
            "--symlink",
            "usr/bin",
            "/bin",
            "--symlink",
            "usr/sbin",
            "/sbin",
        ])
        .args([
            "--symlink",
            "usr/lib",
            "/lib",
            "--symlink",
            "usr/lib64",
            "/lib64",
        ])
        .args(["--dir", "/run", "--dir", "/run/user"])
        .args(["--dir", uid_dir.to_string_lossy().as_ref()])
        .arg("--bind")
        .arg(session.runtime)
        .arg(session.runtime)
        .args(["--bind", home.to_string_lossy().as_ref(), "/home/agent"])
        .arg("--ro-bind")
        .arg(&cwd)
        .arg("/work")
        .args(["--tmpfs", "/tmp", "--dir", "/tmp/.X11-unix"])
        .arg("--ro-bind")
        .arg(&x_socket)
        .arg(&x_socket)
        .args(["--chdir", "/work"])
        .args(["--setenv", "HOME", "/home/agent"])
        .args(["--setenv", "PATH", "/usr/local/bin:/usr/bin:/bin"])
        .arg("--setenv")
        .arg("XDG_RUNTIME_DIR")
        .arg(session.runtime)
        .arg("--setenv")
        .arg("WAYLAND_DISPLAY")
        .arg(session.wayland_display)
        .arg("--setenv")
        .arg("DISPLAY")
        .arg(format!(":{}", session.xdisplay))
        .args([
            "--unsetenv",
            "XAUTHORITY",
            "--unsetenv",
            "DBUS_SESSION_BUS_ADDRESS",
        ]);
    if network {
        command.arg("--share-net");
    }
    command.arg("--").arg(program).args(&argv[1..]);
    spawn_child(command, session.discard_stdout, "launch bwrap application")
}

fn spawn_child(mut command: Command, discard_stdout: bool, context: &'static str) -> Result<Child> {
    if discard_stdout {
        command.stdout(Stdio::null());
    }
    command.spawn().context(context)
}

fn which(program: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?).find_map(|dir| {
        let candidate = dir.join(program);
        candidate.is_file().then_some(candidate)
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::flatpak_compat_args;

    #[test]
    fn chrome_compatibility_preserves_gpu_and_user_overrides_win() {
        let args = [
            OsString::from("--ozone-platform=headless"),
            OsString::from("https://example.com"),
        ];
        let effective = flatpak_compat_args("com.google.Chrome", &args);
        assert_eq!(
            effective
                .iter()
                .filter(|arg| arg.to_string_lossy().starts_with("--ozone-platform="))
                .count(),
            1
        );
        assert!(!effective.iter().any(|arg| arg == "--disable-gpu"));
        assert!(effective.iter().any(|arg| arg == "--no-sandbox"));
        assert!(effective.iter().any(|arg| arg == "--test-type"));
        assert_eq!(
            effective.last(),
            Some(&OsString::from("https://example.com"))
        );
    }

    #[test]
    fn unrelated_flatpaks_are_not_modified() {
        let args = [OsString::from("document.pdf")];
        assert_eq!(flatpak_compat_args("org.example.App", &args), args);
    }
}
