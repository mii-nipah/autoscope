# autoscope

`autoscope` runs one Linux application in an isolated UI automation session for autonomous agents. Native Wayland and X11 applications use the same control and capture interface; a normal agent does not configure displays, choose a renderer, manage application profiles, or understand compositor protocols.

Run `autoscope readme` for the complete standalone command guide.

## Codex plugin

The [Autoscope plugin](plugin/autoscope/README.md) bundles a prebuilt Linux runtime, libraries, keyboard data, bubblewrap, FFmpeg, the MCP connection, and a skill for visual app workflows. Install it in Codex, start a new task, and ask **“Open my app in Autoscope, test its main workflow, and show me the result.”** No compilation, setup command, or manual MCP configuration is needed.

Maintainers build with `scripts/build-bundle.sh` (Podman), then `python3 scripts/package-plugin.py` (Python and appimagetool). The plugin archive, standalone AppImage, and SHA-256 checksums appear in `target/plugin/`. Both use the same runtime built on Ubuntu 22.04; this package supports Linux x86_64 with glibc 2.35 or newer. Xwayland is an optional host dependency for X11 apps, and Flatpak is needed only for Flatpak apps. Recipients do not need the build tools.

The maintained plugin files live in `plugin/autoscope/`; packaging adds `runtime/` from the generated AppDir. Register the extracted `autoscope` folder with a Codex marketplace or share its plugin card. The plugin runs from the extracted AppDir without FUSE. The standalone AppImage accepts the same CLI commands; use `--appimage-extract-and-run` if FUSE is unavailable. Building an archive does not publish it to the public plugin directory.

`scripts/package-sources.sh` prepares a separate `target/plugin/autoscope-sources.tar.gz` for distribution alongside the binaries. It includes this project's source and build scripts, vendored Rust dependencies, and exact Ubuntu source packages with their patches and SHA-256 verification. Component notices and package versions are included in each runtime. These companion sources are for rebuilding or redistribution; users do not need to download them to run the plugin.

To verify an extracted or installed package, run `python3 tests/plugin_workflow.py /path/to/autoscope --artifacts target/plugin-evidence` with Flatpak Chrome, Xmessage, and Xwayland installed. It checks a real project working directory under a read-only `/work` mount, offline X11 launch, browser typing and form submission, stale-input rejection, contact-sheet capture, and session cleanup. `python3 tests/portable_workflow.py /path/to/autoscope --artifacts target/native-evidence` uses Zenity to verify native Wayland input, exact saved bytes, and bundled video encoding/decoding with Xwayland absent from PATH and no host display environment. Both omit `XDG_RUNTIME_DIR`, as Codex can do. `python3 tests/codex_plugin_workflow.py /path/to/autoscope --codex /path/to/codex --artifacts target/codex-evidence` exercises the installed `autoscope@personal` plugin through Codex's real tool-call API using an ephemeral context without a model turn. [Plugin troubleshooting](plugin/autoscope/README.md#troubleshooting-and-manual-use) records the current networked X11 limitation.

## Agent contract

An agent only needs this lifecycle:

1. Start `autoscope run` and read its first JSON line.
2. Use the returned `control` socket for mouse, keyboard, screenshots, and recording.
3. Optionally consume the returned `stream` socket or request `--window` for live visualization.
4. Send `quit` when finished.

The ready JSON contains operational session facts: process IDs, `control`, `stream`, size, frame rate, a durable `session` directory, `shared_dir`, and `log`. Backend display names and renderer plumbing remain private implementation details.

## Build

```bash
cargo build --release
```

## Run an application

Host command, isolated with bwrap by default when `bwrap` is installed:

```bash
autoscope run --name my-app -- ./target/debug/my-app
```

Each session supplies a private Wayland display and, when Xwayland is available, a private X11 display. Native Wayland apps work without Xwayland or a host Wayland display. Toolkits choose an available protocol without changing the agent contract.

Flatpak Chrome needs only the application ID and normal Chrome arguments; autoscope supplies its compatibility, permission-reset sandbox, and profile isolation internally:

```bash
autoscope run --name chrome --flatpak com.google.Chrome -- https://www.google.com/
```

`--window` opens a small read-only viewer of the same frames used for capture. Viewer input never reaches the application.

For host commands, `--sandbox auto` uses bwrap when present, `--sandbox bwrap` requires it, and `--sandbox off` disables it. Network is shared unless `--no-network` is set; the same flag removes Flatpak network access.

## File choosers

Toolkit-native Wayland and X11 choosers render inside the session. Flatpak Chrome also falls back to its in-process GTK chooser because host desktop portals and bus sockets are unavailable. Each session now shares one writable folder with its application at the same absolute host path. Use `--shared-dir /path/to/art` or the private default reported as `shared_dir`; copy inputs into it and save outputs there. Sandboxed apps also have `$HOME/Shared` and `AUTOSCOPE_SHARED_DIR`. The bwrap `/work` mount remains read-only. Choosers cannot open a dialog on the host desktop.

Shared files, logs, and metadata survive session exit under `$XDG_STATE_HOME/autoscope/sessions` by default. `autoscope run --detach` starts a session independently of the launching terminal; `autoscope status SESSION` inspects it and `autoscope attach SESSION` opens a read-only live viewer. Use the returned `control` socket to stop it with `quit`. Signals received by Autoscope request graceful shutdown and record their number and sender; an unrecorded exit is reported as unreachable rather than assigned an invented cause.

## Automate

Assuming `CONTROL` is the `control` path from the ready JSON:

```bash
autoscope ctl "$CONTROL" move 620 370
autoscope ctl "$CONTROL" move --normalize 0.5 0.5
autoscope ctl "$CONTROL" click
autoscope ctl "$CONTROL" type "smithay compositor"
autoscope ctl "$CONTROL" key ENTER
autoscope ctl "$CONTROL" screenshot /tmp/search.png

autoscope ctl "$CONTROL" record-start --fps 1 /tmp/click.mp4
autoscope ctl "$CONTROL" click --x 620 --y 250
autoscope ctl "$CONTROL" click --normalize --x 0.5 --y 0.25
autoscope ctl "$CONTROL" record-stop

autoscope ctl "$CONTROL" record-start --mode images --fps 1 --frames-per-image 10 /tmp/actions.png
# perform actions, then stop to produce actions-001.png, actions-002.png, ...
autoscope ctl "$CONTROL" record-stop

autoscope ctl "$CONTROL" scroll 0 640
autoscope ctl "$CONTROL" key CTRL+L
autoscope ctl "$CONTROL" wait
autoscope ctl "$CONTROL" info
autoscope ctl "$CONTROL" quit
```

`type` accepts printable US ASCII and validates the complete string before injecting any key. Named keys include navigation keys, F1–F12, Enter, Tab, Escape, Backspace, Delete, and modifier combinations such as `CTRL+L`.

Input is safe-paced by default. Only one input step is injected per rendered frame, `type` sends one validated character at a time, and `click` gives the application real move, dwell, press, and release phases. Move, click, scroll, type, and key commands then wait up to five seconds for significant pixels to remain unchanged for 600 ms. Their JSON result reports `wait.status` as `stable`, `timed-out`, or `unavailable`, together with the resulting `frame` and `view`; a timeout means “observe this frame and decide,” not that the input failed. `autoscope ctl wait` provides the same bounded barrier explicitly. `--no-wait` skips only the visual-stability wait, and is intended for raw gesture steps such as movement during a held-button drag.

`move` and coordinate-bearing `click` use absolute frame pixels by default. Add `--normalize` to interpret both axes as `0.0..=1.0`; the endpoints map exactly to the first and last frame pixels. Direct socket clients use the same wire option, for example `{"cmd":"move","x":0.5,"y":0.5,"normalize":true}`. Values outside the normalized range are rejected.

For a whole drawing gesture, use `autoscope ctl "$CONTROL" drag 300,200 400,250 700,500`. It validates the entire path, sends one vertex per frame with the button held, releases, and settles once. `drag --origin 235,149 --scale 4 10,10 20,20` maps local canvas pixels to screen coordinates. This avoids one process and one stability wait per vertex. Intermediate failure still releases the held button.

Recording FPS is independent from the session FPS: a session and its viewer may remain at 60 FPS while evidence is sampled at 1 FPS. Omitting recording `--fps` preserves the old behavior by using the session FPS; a recording cannot request more frames than the session produces. `--mode images` creates chronological contact-sheet PNGs instead of a video, ordered left-to-right and then top-to-bottom. Each sampled frame is immediately reduced to at most 320 pixels wide, and `--frames-per-image` accepts 1 through 10 thumbnails per sheet, keeping both images and memory compact.

## MCP coordinator

`autoscope mcp` is a stdio MCP server for agents that should not manage application-session processes or socket paths themselves. Configure an MCP client to launch:

```json
{"command":"autoscope","args":["mcp"]}
```

The coordinator exposes tools to spawn host commands or Flatpak applications, list and inspect sessions, move/click/scroll, hold mouse buttons for drags, type text, press keys, capture screenshots, record MP4 videos or contact sheets, and close sessions. A host command is passed as an argument array without shell interpretation. Spawn allows up to five seconds for its first window to become visually quiet, then returns a short coordinator ID such as `app-1`, its screenshot, and a `view` number. Every input tool requires the `view` from the most recently returned image, waits for visual stability by default, and returns the next screenshot and view. If the screen changed—or another preplanned action already consumed that observation—the stale action is rejected before input injection. This makes the agent observe each state transition instead of blindly replaying coordinates against a newer layout. `start_recording` independently selects `fps` and `mode`; `stop_recording` returns an MP4 path for video mode or chronological contact sheets as MCP image content for models without native video understanding.

One MCP process may own multiple independent sessions. Closing the MCP input gracefully closes and reaps every session and application process it still owns; Linux parent-death signaling also prevents a hard-killed coordinator from leaving live session children. Absolute pointer pixels remain the default, and MCP input tools accept `normalize: true` for `0.0..=1.0` coordinates.

Spawn options accept `shared_dir`, and each observation reports that folder. For project applications, `working_dir` selects an absolute working directory and the read-only `/work` mount under bwrap; this works even when an MCP client starts the coordinator in its plugin directory. The `drag` tool takes `points: [[x,y], ...]`, optional `origin` and `scale`, and the latest `view`; it returns the existing post-action screenshot and stale-view safeguards. The coordinator's recordings are retained after server exit. Detached CLI sessions are explicit; MCP-owned sessions still close with their coordinator.

## Realtime frame feed

```bash
autoscope stream "$STREAM" > frames.asf
```

The stream starts with a fixed 20-byte little-endian header:

| Offset | Value |
|---:|---|
| 0 | ASCII `ASF1` |
| 4 | width, `u32` |
| 8 | height, `u32` |
| 12 | frames per second, `u32` |
| 16 | RGBA frame byte length, `u32` |

Each frame is an increasing `u64` sequence followed by exactly `frame byte length` top-to-bottom RGBA bytes. Slow readers are disconnected instead of delaying the session.

## Internal architecture

Autoscope is implemented as a one-application Wayland compositor using Smithay's headless Pixman renderer and an optional rootless XWayland server. Smithay is pinned to commit `92ba0e92c2f8e775cc61a17bdf8a68e122c15610`. X11 windows become ordinary Smithay `Window` elements, so both protocols share one output, one seat, one SVG-derived arrow cursor, and one canonical RGBA frame. Screenshots, recording, streaming, and the optional viewer all consume that frame. Host mouse and keyboard events are never admitted; only control-socket commands enter Smithay's seat.

The bwrap profile exposes `/usr`, `/etc`, and `/sys` read-only, an isolated home and `/tmp`, the current working tree read-only at `/work`, the session runtime directory, exactly one private X11 socket, and the shared file directory read/write. It omits host displays, session bus, audio sockets, and input devices. Flatpak launch resets manifest permissions with `--sandbox`, then admits only optional network access, DRI rendering, the private displays, the private session directory, and the chosen shared folder. Host session/system bus and document-portal sockets are absent. Chrome runs its real binary directly with its nested process sandbox disabled because the complete browser process tree remains inside that outer Flatpak sandbox; this also avoids its host-portal-dependent wrapper.

The current compatibility boundary is Linux applications using native Wayland or X11 through XWayland and shared-memory presentation. There is intentionally no desktop shell, host portal bridge, manual-input bridge, clipboard bridge, audio bridge, or remote network protocol.

On minimal Linux systems that ship `libxkbcommon.so.0` or `libpixman-1.so.0` without development symlinks, `build.rs` supplies only the missing local linker aliases.
