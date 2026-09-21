# Autoscope for Codex

**A private Linux app. Eyes and hands for your agent.**

Autoscope lets Codex open a Linux application, see its screen, click, type, draw, and capture the result. Each application gets its own display and automation session. Your mouse and keyboard stay yours.

## Get started

1. Install **Autoscope** in Codex from its plugin card or shared plugin entry.
2. Start a new task and ask: **“Open my app in Autoscope, test its main workflow, and show me the result.”**

The plugin includes a prebuilt runtime, its libraries, keyboard data, filesystem sandbox, and recording/viewer helpers. Installation requires no Rust, compilation, setup command, account, API key, or manual MCP configuration. Nothing is compiled or downloaded during MCP startup. The application you want to use must already be installed. Screenshots and tool results enter your Codex conversation; the local runtime is not a promise that model processing stays on your machine.

## Linux prerequisites

| Dependency | Used for |
| --- | --- |
| Linux x86_64 with glibc 2.35 or newer | Running this binary package (Ubuntu 22.04 baseline) |
| Xwayland, optional | X11 applications; detected automatically when present |
| Flatpak, optional | Launching installed Flatpak applications |
| Working user namespaces | The bundled bubblewrap filesystem sandbox |

Native Wayland applications work without Xwayland and without a host Wayland display. When an MCP client omits `XDG_RUNTIME_DIR`, Autoscope uses the existing `/run/user/<uid>` directory automatically. An explicit runtime directory is honored; the selected directory must belong to the current user and have mode `0700`. Native app launches use the bundled sandbox by default. Host policies can still restrict user namespaces.

Run `runtime/AppRun doctor` from the plugin folder to check runtime availability and optional host helpers. This checks availability; a real app launch also tests sandbox permissions and graphics compatibility. The plugin starts directly from its extracted runtime, so it does not need FUSE.

## What you can ask for

- **Test a Linux app:** exercise its actual GUI and capture the outcome.
- **Use a Flatpak app:** launch by its installed application ID with a fresh profile.
- **Check a narrow layout:** use a 390 × 844 display, or any supported dimensions.
- **Draw or edit:** use complete drag paths and share input/output files with the app.
- **Show the work:** screenshots, MP4 recordings, or small contact sheets.

Screenshots accompany actions, and each input must use the latest `view`. If the screen changes, stale actions are rejected before injection. Files go through the session's shared folder and remain available after the app closes. No desktop shell, host clipboard, audio, or host portal bridge is included. Text input currently supports printable US ASCII.

For a project application, `spawn_command` accepts `options.working_dir` as an absolute project directory and a relative command such as `./target/debug/my-app`. That directory is mounted read-only at `/work` under bwrap.

## Troubleshooting and manual use

- **Tools missing after installation:** start a new Codex task. If `runtime/AppRun` is missing, reinstall the complete binary plugin package.
- **X11 app cannot start:** check `runtime/AppRun doctor`; install Xwayland on the host if that application requires X11. Native Wayland apps do not require it.
- **Sandbox launch denied:** inspect the reported bwrap/user-namespace error. Fix host policy or deliberately choose a different sandbox policy for that task.
- **X11 reports “Authorization required” or “Can't open display”:** the tested X11 app hit this failure when networking was shared. For an offline app, set `options.no_network: true`; that path passed the real X11 test. Networked X11 compatibility remains unresolved on this host. Native Wayland Flatpak Chrome passed with networking enabled.
- **File not visible:** copy it into the returned `shared_dir`; save exports there too.
- **Stale view:** request another screenshot before sending input.

`runtime/AppRun readme` prints the complete CLI guide. Advanced users can use `run --detach` for persistent sessions; MCP sessions close when their coordinator exits. Shared files, recordings, and logs are retained separately from the plugin installation. The standalone AppImage provides the same CLI; if FUSE is unavailable, run it with `--appimage-extract-and-run` before the Autoscope command.

This package targets local Linux x86_64 hosts. ARM, macOS, Windows, remote ChatGPT execution, and arbitrary GPU presentation paths are not supported by this build. Component notices and build provenance are included in `runtime/usr/share/licenses/` and `runtime/build-info.json`. See [OpenAI's plugin packaging guide](https://developers.openai.com/plugins/build/plugins) for Codex installation and distribution surfaces.
