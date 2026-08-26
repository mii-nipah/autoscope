# autoscope

`autoscope` runs one Linux application in an isolated UI automation session for autonomous agents. Native Wayland and X11 applications use the same control and capture interface; a normal agent does not configure displays, choose a renderer, manage application profiles, or understand compositor protocols.

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

autoscope ctl "$CONTROL" record-start /tmp/click.mp4
autoscope ctl "$CONTROL" click --x 620 --y 250
autoscope ctl "$CONTROL" click --normalize --x 0.5 --y 0.25
autoscope ctl "$CONTROL" record-stop

autoscope ctl "$CONTROL" scroll 0 640
autoscope ctl "$CONTROL" key CTRL+L
autoscope ctl "$CONTROL" info
autoscope ctl "$CONTROL" quit
```

`type` accepts printable US ASCII and validates the complete string before injecting any key. Named keys include navigation keys, F1–F12, Enter, Tab, Escape, Backspace, Delete, and modifier combinations such as `CTRL+L`.

`move` and coordinate-bearing `click` use absolute frame pixels by default. Add `--normalize` to interpret both axes as `0.0..=1.0`; the endpoints map exactly to the first and last frame pixels. Direct socket clients use the same wire option, for example `{"cmd":"move","x":0.5,"y":0.5,"normalize":true}`. Values outside the normalized range are rejected.

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

Autoscope is implemented as a one-application nested Wayland compositor using Smithay's headless Pixman renderer and one rootless XWayland server. Smithay is pinned to commit `92ba0e92c2f8e775cc61a17bdf8a68e122c15610`. X11 windows become ordinary Smithay `Window` elements, so both protocols share one output, one seat, one cursor, and one canonical RGBA frame. Screenshots, recording, streaming, and the optional viewer all consume that frame. Host mouse and keyboard events are never admitted; only control-socket commands enter Smithay's seat.

The bwrap profile exposes `/usr`, `/etc`, and `/sys` read-only, an isolated home and `/tmp`, the current working tree read-only at `/work`, the session runtime directory, and exactly one private X11 socket. It omits host displays, session bus, audio sockets, and input devices. Flatpak launch resets manifest permissions with `--sandbox`, then admits only optional network access, DRI rendering, the private displays, and the private session directory. Host session/system bus and document-portal sockets are absent. Chrome runs its real binary directly with its nested process sandbox disabled because the complete browser process tree remains inside that outer Flatpak sandbox; this also avoids its host-portal-dependent wrapper.

The current compatibility boundary is Linux applications using native Wayland or X11 through XWayland and shared-memory presentation. There is intentionally no desktop shell, host portal bridge, manual-input bridge, clipboard bridge, audio bridge, or remote network protocol.

On minimal Linux systems that ship `libxkbcommon.so.0` or `libpixman-1.so.0` without development symlinks, `build.rs` supplies only the missing local linker aliases.
