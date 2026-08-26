# autoscope

`autoscope` runs one application in an isolated UI automation session for autonomous agents. A normal agent does not configure a display, choose a renderer, manage application profiles, or understand compositor protocols.

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

Flatpak Chrome needs only the application ID and normal Chrome arguments; autoscope supplies its compatibility and profile isolation internally:

```bash
autoscope run --name chrome --flatpak com.google.Chrome -- https://www.google.com/
```

`--window` opens a small read-only viewer of the same frames used for capture. Viewer input never reaches the application.

For host commands, `--sandbox auto` uses bwrap when present, `--sandbox bwrap` requires it, and `--sandbox off` disables it. Network is shared unless `--no-network` is set.

## Automate

Assuming `CONTROL` is the `control` path from the ready JSON:

```bash
autoscope ctl "$CONTROL" move 620 370
autoscope ctl "$CONTROL" click
autoscope ctl "$CONTROL" type "smithay compositor"
autoscope ctl "$CONTROL" key ENTER
autoscope ctl "$CONTROL" screenshot /tmp/search.png

autoscope ctl "$CONTROL" record-start /tmp/click.mp4
autoscope ctl "$CONTROL" click --x 620 --y 250
autoscope ctl "$CONTROL" record-stop

autoscope ctl "$CONTROL" scroll 0 640
autoscope ctl "$CONTROL" key CTRL+L
autoscope ctl "$CONTROL" info
autoscope ctl "$CONTROL" quit
```

`type` accepts printable US ASCII and validates the complete string before injecting any key. Named keys include navigation keys, F1–F12, Enter, Tab, Escape, Backspace, Delete, and modifier combinations such as `CTRL+L`.

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

Autoscope is implemented as a one-application nested Wayland compositor using Smithay's headless Pixman renderer. Smithay is pinned to commit `92ba0e92c2f8e775cc61a17bdf8a68e122c15610`. One process owns one private Wayland display, one application, one output, one cursor, and one canonical RGBA frame. Screenshots, recording, streaming, and the optional viewer all consume that frame. Host mouse and keyboard events are never admitted; only control-socket commands enter Smithay's seat.

The bwrap profile exposes `/usr`, `/etc`, and `/sys` read-only, an isolated home and `/tmp`, the current working tree read-only at `/work`, and only the session runtime directory. It omits the host display, session bus, audio sockets, and input devices. Flatpak applications retain Flatpak's own bubblewrap sandbox; host X11 and Wayland sockets are removed, while independent home, config, cache, data, and state directories prevent singleton/profile coupling across parallel sessions.

The current compatibility boundary is Linux applications using native Wayland and shared-memory buffers. There is intentionally no XWayland, desktop shell, manual-input bridge, clipboard bridge, audio bridge, or remote network protocol.

On minimal Linux systems that ship `libxkbcommon.so.0` or `libpixman-1.so.0` without development symlinks, `build.rs` supplies only the missing local linker aliases.
