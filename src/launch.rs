use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

use anyhow::{Context, Result, bail};

pub struct HostSession {
    pub runtime_dir: PathBuf,
    pub wayland_display: Option<OsString>,
}

impl HostSession {
    pub fn capture() -> Result<Self> {
        Ok(Self {
            runtime_dir: runtime_directory(env::var_os("XDG_RUNTIME_DIR"))?,
            wayland_display: env::var_os("WAYLAND_DISPLAY"),
        })
    }
}

fn runtime_directory(configured: Option<OsString>) -> Result<PathBuf> {
    let uid = unsafe { libc::geteuid() };
    let path = configured
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}")));
    if !path.is_absolute() {
        bail!("XDG_RUNTIME_DIR must be an absolute directory path");
    }
    let metadata = fs::metadata(&path).with_context(|| {
        format!(
            "cannot access user runtime directory {}; set XDG_RUNTIME_DIR to an existing private runtime directory",
            path.display()
        )
    })?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o7777 != 0o700 {
        bail!(
            "runtime directory {} must be a directory owned by UID {uid} with mode 0700",
            path.display()
        );
    }
    Ok(path)
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
    pub xdisplay: Option<u32>,
    pub log: &'a Path,
    pub shared: &'a Path,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Sandbox {
    Auto,
    Bwrap,
    Off,
}

pub fn spawn(spec: LaunchSpec, session: &LaunchSession<'_>, host: &HostSession) -> Result<Child> {
    let home = session.runtime.join("home");
    fs::create_dir(&home)?;
    std::os::unix::fs::symlink(session.shared, home.join("Shared"))?;
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
    let display = session.xdisplay.map(|number| format!(":{number}"));
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
        "--nosocket=x11",
    ]);
    if display.is_some() {
        command.arg("--socket=x11");
    }
    if network {
        command.arg("--share=network");
    }
    command
        .arg(format!("--filesystem={}:rw", session.shared.display()))
        .arg(format!("--env=AUTOSCOPE_SHARED_DIR={}", session.shared.display()))
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
        .arg(display.as_deref().unwrap_or_default())
        .arg(app_command)
        .args(app_args)
        .env("XDG_RUNTIME_DIR", &host.runtime_dir)
        .env_remove("WAYLAND_DISPLAY")
        .envs(host.wayland_display.as_ref().map(|value| ("WAYLAND_DISPLAY", value)))
        .env_remove("DISPLAY")
        .env_remove("XAUTHORITY");
    if let Some(display) = display {
        command.env("DISPLAY", display);
    }
    spawn_child(command, session.log, "launch Flatpak application")
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
            .env("AUTOSCOPE_SHARED_DIR", session.shared)
            .env("XDG_RUNTIME_DIR", session.runtime)
            .env("WAYLAND_DISPLAY", session.wayland_display)
            .env_remove("DISPLAY")
            .env_remove("XAUTHORITY")
            .env_remove("DBUS_SESSION_BUS_ADDRESS");
        if let Some(display) = session.xdisplay {
            command.env("DISPLAY", format!(":{display}"));
        }
        return spawn_child(command, session.log, "launch application");
    }

    let cwd = env::current_dir()?;
    let home = session.runtime.join("home");
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
        .arg("--bind")
        .arg(session.shared)
        .arg(session.shared)
        .args(["--chdir", "/work"])
        .args(["--setenv", "HOME", "/home/agent"])
        .arg("--setenv")
        .arg("AUTOSCOPE_SHARED_DIR")
        .arg(session.shared)
        .args(["--setenv", "PATH", "/usr/local/bin:/usr/bin:/bin"])
        .arg("--setenv")
        .arg("XDG_RUNTIME_DIR")
        .arg(session.runtime)
        .arg("--setenv")
        .arg("WAYLAND_DISPLAY")
        .arg(session.wayland_display)
        .args(["--unsetenv", "DISPLAY"])
        .args([
            "--unsetenv",
            "XAUTHORITY",
            "--unsetenv",
            "DBUS_SESSION_BUS_ADDRESS",
        ]);
    if let Some(display) = session.xdisplay {
        let socket = PathBuf::from(format!("/tmp/.X11-unix/X{display}"));
        if !fs::metadata(&socket)?.file_type().is_socket() {
            bail!("private X11 display socket is not a socket");
        }
        command.args(["--ro-bind"]).arg(&socket).arg(&socket);
        command.args(["--setenv", "DISPLAY", &format!(":{display}")]);
    }
    if network {
        command.arg("--share-net");
    }
    command.arg("--").arg(program).args(&argv[1..]);
    spawn_child(command, session.log, "launch bwrap application")
}

fn spawn_child(mut command: Command, log: &Path, context: &'static str) -> Result<Child> {
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .context("open application log")?;
    command
        .stdout(log.try_clone()?)
        .stderr(log)
        .stdin(Stdio::null());
    command.spawn().context(context)
}

pub(crate) fn which(program: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?).find_map(|dir| {
        let candidate = dir.join(program);
        candidate.is_file().then_some(candidate)
    })
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, fs, os::unix::fs::PermissionsExt};

    use super::{flatpak_compat_args, runtime_directory};

    #[test]
    fn runtime_directory_honors_private_override_and_rejects_invalid_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_path_buf();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(runtime_directory(Some(path.clone().into())).unwrap(), path);
        assert!(runtime_directory(Some("relative/path".into())).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(runtime_directory(Some(path.clone().into())).is_err());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(runtime_directory(Some(path.join("missing").into())).is_err());
    }

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
