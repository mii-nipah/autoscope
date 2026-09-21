# autoscope command guide

Autoscope runs one Wayland or X11 application in a private automation session. Start a session, read its ready line, use the returned control socket, then quit it.

Xwayland is detected automatically and is only needed for X11 applications. Native Wayland applications also work without a host Wayland display. If `XDG_RUNTIME_DIR` is missing or empty, Autoscope uses the existing `/run/user/<uid>` directory; an explicit runtime directory is honored. The selected directory must belong to the current user and have mode `0700`. The binary plugin and AppImage include libraries, keyboard data, bubblewrap, and FFmpeg helpers. Use `runtime/AppRun` or the AppImage path in place of `autoscope` in the commands below. The AppImage also accepts `--appimage-extract-and-run` when FUSE is unavailable.

Print this guide with `autoscope readme`.

## Start a session

```text
autoscope run [OPTIONS] -- COMMAND [ARGUMENTS...]
autoscope run [OPTIONS] --flatpak APP_ID -- [ARGUMENTS...]
```

Examples:

```sh
autoscope run --name game --width 1280 --height 800 --fps 30 -- ./game
autoscope run --name chrome --flatpak com.google.Chrome -- https://example.com
```

`run` stays in the foreground. Its ready line is JSON:

```json
{"status":"ready","pid":1234,"app_pid":1235,"control":"/run/user/1000/autoscope-game-XXXX/control.sock","stream":"/run/user/1000/autoscope-game-XXXX/stream.sock","size":[1280,800],"fps":30}
```

Keep the process alive and retain `control` and, if needed, `stream`.

Every ready response also includes `session` (a durable session directory), `shared_dir` (the same writable path on host and in the application), and `log` (application output; detached sessions also include compositor diagnostics).

Run options:

```text
--name NAME                 Session label; default: instance
--width PIXELS              320..3840; default: 1280
--height PIXELS             200..2160; default: 800
--fps FPS                   1..60; default: 15
--window                    Open a read-only live viewer
--sandbox auto|bwrap|off    Host-command sandbox; default: auto
--no-network                Remove application network access
--shared-dir PATH           Share this folder read/write; defaults to a private durable folder
--detach                    Return after startup and keep the session running independently
--flatpak APP_ID            Launch a Flatpak instead of a host command
```

`auto` uses bwrap when available. `bwrap` requires it. `off` runs the host command without bwrap. Flatpak applications always use their isolated Flatpak launch path. `--window` requires ffplay.

For work that must outlive the launching command or terminal, use `--detach`:

```sh
autoscope run --detach --name drawing --shared-dir /home/me/art --flatpak com.orama_interactive.Pixelorama
autoscope status SESSION
autoscope attach SESSION
```

`SESSION` is the returned `session` directory. `status` reports live display information while reachable, and preserves the exit reason afterward, including a shutdown signal and its sender PID when available. `attach` opens a read-only viewer; closing it leaves the application running. Stop the session with `autoscope ctl SOCKET quit`. Detached sessions disconnect from the launching terminal; an explicit signal, machine shutdown, or external process supervisor can still stop them. A missing control connection is reported as `unreachable`, not guessed to be a particular crash.

## Control a session

`SOCKET` below is the ready line's `control` value. Every command prints one JSON response and exits nonzero on failure.

```text
autoscope ctl SOCKET info
autoscope ctl SOCKET move [--normalize] [--no-wait] X Y
autoscope ctl SOCKET click [--button BUTTON] [--x X --y Y] [--normalize] [--no-wait]
autoscope ctl SOCKET drag [--button BUTTON] [--normalize | --origin X,Y --scale N] [--no-wait] X,Y X,Y ...
autoscope ctl SOCKET mouse-down [BUTTON]
autoscope ctl SOCKET mouse-up [BUTTON]
autoscope ctl SOCKET scroll [--no-wait] DX DY
autoscope ctl SOCKET type [--no-wait] TEXT
autoscope ctl SOCKET key [--no-wait] COMBO
autoscope ctl SOCKET wait [--timeout-ms MS] [--quiet-ms MS]
autoscope ctl SOCKET screenshot PATH
autoscope ctl SOCKET record-start [--fps FPS] [--mode video|images] [--frames-per-image 1..10] PATH
autoscope ctl SOCKET record-stop
autoscope ctl SOCKET quit
```

`BUTTON` is `left`, `right`, or `middle`. `click` uses the current pointer position unless both `--x` and `--y` are supplied.

Coordinates are frame pixels by default. `--normalize` makes both axes fractions from `0.0` to `1.0`, inclusive.

Input commands wait for the screen to remain visually unchanged for 600 ms, bounded by five seconds. A successful response reports `wait.status` as `stable`, `timed-out`, or `unavailable`; `timed-out` means the input was sent but the screen did not settle. Use `wait` for an explicit barrier. Use `--no-wait` only when composing raw steps such as a drag.

Examples:

```sh
autoscope ctl "$CONTROL" move --normalize 0.5 0.4
autoscope ctl "$CONTROL" click
autoscope ctl "$CONTROL" click --button right --x 640 --y 320
autoscope ctl "$CONTROL" type "search text"
autoscope ctl "$CONTROL" key CTRL+L
autoscope ctl "$CONTROL" screenshot /tmp/screen.png

# Drag from (300, 200) to (700, 500).
autoscope ctl "$CONTROL" drag 300,200 700,500

# Draw a closed path in a canvas whose pixel (0,0) is at screen (235,149), at 400% zoom.
autoscope ctl "$CONTROL" drag --origin 235,149 --scale 4 10,10 20,10 10,20 10,10
```

`drag` validates all 2–4096 vertices before moving or pressing, holds the button across the entire path, releases even if an intermediate move fails, and settles once at the end. Each vertex is delivered on its own rendered frame. `--origin` and `--scale` provide a reusable local coordinate system; they cannot be combined with `--normalize`. Inspect the result before planning another gesture or assuming a dialog is open. A visually stable screen does not establish which application control is active.

Screenshot and recording paths are host paths, not paths inside the application sandbox.

`type` accepts the US-ASCII keyboard map. `key` accepts a character or a named key: `ESCAPE`, `BACKSPACE`, `TAB`, `ENTER`, `SPACE`, `HOME`, `END`, `PAGEUP`, `PAGEDOWN`, arrow keys, `DELETE`, and `F1` through `F12`. Prefix combinations with `CTRL`, `SHIFT`, `ALT`, `META`, or `SUPER`, joined by `+`.

## Record actions

Video mode requires ffmpeg and writes H.264; use an `.mp4` path for an MP4 file:

```sh
autoscope ctl "$CONTROL" record-start --fps 10 /tmp/actions.mp4
# perform actions
autoscope ctl "$CONTROL" record-stop
```

Image recording produces chronological PNG contact sheets for image-only consumers:

```sh
autoscope ctl "$CONTROL" record-start --mode images --fps 1 --frames-per-image 10 /tmp/actions.png
# perform actions
autoscope ctl "$CONTROL" record-stop
```

The recording FPS defaults to the session FPS and cannot exceed it. Image mode writes `/tmp/actions-001.png`, `/tmp/actions-002.png`, and so on; frames are at most 320 pixels wide and ordered left-to-right, then top-to-bottom.

## Read the realtime frame stream

```sh
autoscope stream "$STREAM" > frames.asf
```

The stream begins with a 20-byte little-endian header: `ASF1`, then width, height, FPS, and RGBA byte length as four `u32` values. Each frame is an increasing `u64` sequence followed by that many top-to-bottom RGBA bytes. A slow reader is disconnected instead of delaying the session.

## Application files and shutdown

Copy input files into `shared_dir` and open that same absolute path in the application's chooser. Save exports there to make them immediately available on the host. For example, the folder passed as `--shared-dir /home/me/art` is `/home/me/art` inside a Flatpak too. Sandboxed applications also have a `Shared` shortcut in their private home, and `AUTOSCOPE_SHARED_DIR` contains the absolute path. The chosen folder is deliberately writable by the application; unrelated host folders stay outside the sandbox.

By default, session metadata and shared files are retained under `$XDG_STATE_HOME/autoscope/sessions` (or `~/.local/state/autoscope/sessions`). They are not deleted on exit. Application stdout/stderr go to `log`; foreground compositor diagnostics go to stderr, and detached compositor diagnostics also go to `log`.

With bwrap, the launch directory remains available read-only as `/work`; the application gets a private writable home and `/tmp`. Host displays, input devices, audio, session bus, and desktop portals are not exposed. `--no-network` also removes network access.

Finish with:

```sh
autoscope ctl "$CONTROL" quit
```

The `run` process then closes the application, viewer, sockets, and disposable display runtime. It retains the shared folder, status, logs, and recordings written outside that runtime. SIGINT, SIGTERM, and SIGHUP request the same cleanup; SIGKILL and power loss cannot be handled. This preserves saved files, not unsaved application edits.
