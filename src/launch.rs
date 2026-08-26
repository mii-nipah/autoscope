use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::{Child, Command},
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
    },
    Command {
        argv: Vec<OsString>,
        sandbox: Sandbox,
        network: bool,
    },
}

#[derive(Clone, Copy, PartialEq)]
pub enum Sandbox {
    Auto,
    Bwrap,
    Off,
}

pub fn spawn(
    spec: LaunchSpec,
    runtime: &Path,
    wayland_display: &OsStr,
    host: &HostSession,
) -> Result<Child> {
    match spec {
        LaunchSpec::Flatpak { app_id, args } => {
            spawn_flatpak(&app_id, &args, runtime, wayland_display, host)
        }
        LaunchSpec::Command {
            argv,
            sandbox,
            network,
        } => spawn_command(&argv, sandbox, network, runtime, wayland_display),
    }
}

fn spawn_flatpak(
    app_id: &str,
    args: &[OsString],
    runtime: &Path,
    wayland_display: &OsStr,
    host: &HostSession,
) -> Result<Child> {
    let runtime_name = runtime
        .file_name()
        .context("instance runtime has no name")?;
    let home = runtime.join("home");
    fs::create_dir(&home)?;
    if app_id == "com.google.Chrome" {
        fs::create_dir_all(home.join("config/google-chrome"))?;
        fs::create_dir_all(runtime.join("app/com.google.Chrome"))?;
    }
    let app_command = flatpak_app_command(app_id)?;
    let app_args = flatpak_compat_args(app_id, args);
    let mut command = Command::new("flatpak");
    command
        .arg("run")
        .args(["--command=sh", "--nosocket=wayland", "--nosocket=x11"])
        .arg("--nosocket=fallback-x11")
        .arg(format!(
            "--filesystem=xdg-run/{}",
            runtime_name.to_string_lossy()
        ))
        .arg(format!("--env=XDG_RUNTIME_DIR={}", runtime.display()))
        .arg(format!("--env=HOME={}", home.display()))
        .arg("--env=DISPLAY=")
        .arg(app_id)
        .args([
            "-c",
            "export WAYLAND_DISPLAY=\"$1\"; shift; export XDG_CONFIG_HOME=\"$HOME/config\" XDG_CACHE_HOME=\"$HOME/cache\" XDG_DATA_HOME=\"$HOME/data\" XDG_STATE_HOME=\"$HOME/state\"; mkdir -p \"$XDG_CONFIG_HOME\" \"$XDG_CACHE_HOME\" \"$XDG_DATA_HOME\" \"$XDG_STATE_HOME\"; exec \"$@\"",
            "autowayland-flatpak",
        ])
        .arg(wayland_display)
        .arg(app_command)
        .args(app_args)
        .env("XDG_RUNTIME_DIR", &host.runtime_dir)
        .env("WAYLAND_DISPLAY", &host.wayland_display)
        .env_remove("DISPLAY");
    command.spawn().context("launch Flatpak application")
}

fn flatpak_compat_args(app_id: &str, args: &[OsString]) -> Vec<OsString> {
    let mut effective = Vec::new();
    if app_id == "com.google.Chrome" {
        for (prefix, value) in [
            ("--ozone-platform=", "--ozone-platform=wayland"),
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
    runtime: &Path,
    wayland_display: &OsStr,
) -> Result<Child> {
    let Some(program) = argv.first() else {
        bail!("empty application command");
    };
    if sandbox == Sandbox::Off || (sandbox == Sandbox::Auto && which("bwrap").is_none()) {
        return Command::new(program)
            .args(&argv[1..])
            .env("XDG_RUNTIME_DIR", runtime)
            .env("WAYLAND_DISPLAY", wayland_display)
            .env_remove("DISPLAY")
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .spawn()
            .context("launch application");
    }

    let cwd = env::current_dir()?;
    let home = runtime.join("home");
    fs::create_dir(&home)?;
    let uid_dir = runtime.parent().context("runtime has no user directory")?;
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
        .arg(runtime)
        .arg(runtime)
        .args(["--bind", home.to_string_lossy().as_ref(), "/home/agent"])
        .arg("--ro-bind")
        .arg(&cwd)
        .arg("/work")
        .args(["--tmpfs", "/tmp", "--chdir", "/work"])
        .args(["--setenv", "HOME", "/home/agent"])
        .args(["--setenv", "PATH", "/usr/local/bin:/usr/bin:/bin"])
        .arg("--setenv")
        .arg("XDG_RUNTIME_DIR")
        .arg(runtime)
        .arg("--setenv")
        .arg("WAYLAND_DISPLAY")
        .arg(wayland_display)
        .args([
            "--unsetenv",
            "DISPLAY",
            "--unsetenv",
            "DBUS_SESSION_BUS_ADDRESS",
        ]);
    if network {
        command.arg("--share-net");
    }
    command.arg("--").arg(program).args(&argv[1..]);
    command.spawn().context("launch bwrap application")
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
