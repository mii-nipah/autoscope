# autoscope

`autoscope` runs one Linux application in an isolated UI automation session for autonomous agents. Native Wayland and X11 applications use the same control and capture interface; a normal agent does not configure displays, choose a renderer, manage application profiles, or understand compositor protocols.

Run `autoscope readme` for the complete standalone command guide.

## Agent contract

An agent only needs this lifecycle:

1. Start `autoscope run` and read its first JSON line.
2. Use the returned `control` socket for mouse, keyboard, screenshots, and recording.
3. Optionally consume the returned `stream` socket or request `--window` for live visualization.
4. Send `quit` when finished.

The ready JSON deliberately contains only operational session facts: process IDs, `control`, `stream`, size, and frame rate. Backend display names, runtime directories, renderer state, and application-profile plumbing are private implementation details.

## Build

```bash
cargo build --release
```

## Run an application

Host command, isolated with bwrap by default when `bwrap` is installed:

```bash
autoscope run --name my-app -- ./target/debug/my-app
```

Each session supplies both a private Wayland display and a private XWayland display. Toolkits can choose either without changing the agent contract.

Flatpak Chrome needs only the application ID and normal Chrome arguments; autoscope supplies its compatibility, permission-reset sandbox, and profile isolation internally:

```bash
autoscope run --name chrome --flatpak com.google.Chrome -- https://www.google.com/
```

`--window` opens a small read-only viewer of the same frames used for capture. Viewer input never reaches the application.

For host commands, `--sandbox auto` uses bwrap when present, `--sandbox bwrap` requires it, and `--sandbox off` disables it. Network is shared unless `--no-network` is set; the same flag removes Flatpak network access.

## File choosers

Toolkit-native Wayland and X11 choosers render inside the session. Flatpak Chrome also falls back to its in-process GTK chooser because host desktop portals and bus sockets are unavailable. Choosers see only the application's sandbox: a private home plus the explicitly exposed read-only `/work` tree for bwrap commands. They cannot open a dialog on the host desktop.

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

Recording FPS is independent from the session FPS: a session and its viewer may remain at 60 FPS while evidence is sampled at 1 FPS. Omitting recording `--fps` preserves the old behavior by using the session FPS; a recording cannot request more frames than the session produces. `--mode images` creates chronological contact-sheet PNGs instead of a video, ordered left-to-right and then top-to-bottom. Each sampled frame is immediately reduced to at most 320 pixels wide, and `--frames-per-image` accepts 1 through 10 thumbnails per sheet, keeping both images and memory compact.

## MCP coordinator

`autoscope mcp` is a stdio MCP server for agents that should not manage application-session processes or socket paths themselves. Configure an MCP client to launch:

```json
{"command":"autoscope","args":["mcp"]}
```

The coordinator exposes tools to spawn host commands or Flatpak applications, list and inspect sessions, move/click/scroll, hold mouse buttons for drags, type text, press keys, capture screenshots, record MP4 videos or contact sheets, and close sessions. A host command is passed as an argument array without shell interpretation. Spawn allows up to five seconds for its first window to become visually quiet, then returns a short coordinator ID such as `app-1`, its screenshot, and a `view` number. Every input tool requires the `view` from the most recently returned image, waits for visual stability by default, and returns the next screenshot and view. If the screen changed—or another preplanned action already consumed that observation—the stale action is rejected before input injection. This makes the agent observe each state transition instead of blindly replaying coordinates against a newer layout. `start_recording` independently selects `fps` and `mode`; `stop_recording` returns an MP4 path for video mode or chronological contact sheets as MCP image content for models without native video understanding.

One MCP process may own multiple independent sessions. Closing the MCP input gracefully closes and reaps every session and application process it still owns; Linux parent-death signaling also prevents a hard-killed coordinator from leaving live session children. Absolute pointer pixels remain the default, and MCP input tools accept `normalize: true` for `0.0..=1.0` coordinates.

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

Autoscope is implemented as a one-application nested Wayland compositor using Smithay's headless Pixman renderer and one rootless XWayland server. Smithay is pinned to commit `92ba0e92c2f8e775cc61a17bdf8a68e122c15610`. X11 windows become ordinary Smithay `Window` elements, so both protocols share one output, one seat, one SVG-derived arrow cursor, and one canonical RGBA frame. Screenshots, recording, streaming, and the optional viewer all consume that frame. Host mouse and keyboard events are never admitted; only control-socket commands enter Smithay's seat.

The bwrap profile exposes `/usr`, `/etc`, and `/sys` read-only, an isolated home and `/tmp`, the current working tree read-only at `/work`, the session runtime directory, and exactly one private X11 socket. It omits host displays, session bus, audio sockets, and input devices. Flatpak launch resets manifest permissions with `--sandbox`, then admits only optional network access, DRI rendering, the private displays, and the private session directory. Host session/system bus and document-portal sockets are absent. Chrome runs its real binary directly with its nested process sandbox disabled because the complete browser process tree remains inside that outer Flatpak sandbox; this also avoids its host-portal-dependent wrapper.

The current compatibility boundary is Linux applications using native Wayland or X11 through XWayland and shared-memory presentation. There is intentionally no desktop shell, host portal bridge, manual-input bridge, clipboard bridge, audio bridge, or remote network protocol.

On minimal Linux systems that ship `libxkbcommon.so.0` or `libpixman-1.so.0` without development symlinks, `build.rs` supplies only the missing local linker aliases.
