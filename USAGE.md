# autoscope command guide

Autoscope runs one Wayland or X11 application in a private automation session. Start a session, read its ready line, use the returned control socket, then quit it.

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

Run options:

```text
--name NAME                 Session label; default: instance
--width PIXELS              320..3840; default: 1280
--height PIXELS             200..2160; default: 800
--fps FPS                   1..60; default: 15
--window                    Open a read-only live viewer
--sandbox auto|bwrap|off    Host-command sandbox; default: auto
--no-network                Remove application network access
--flatpak APP_ID            Launch a Flatpak instead of a host command
```

`auto` uses bwrap when available. `bwrap` requires it. `off` runs the host command without bwrap. Flatpak applications always use their isolated Flatpak launch path. `--window` requires ffplay.

## Control a session

`SOCKET` below is the ready line's `control` value. Every command prints one JSON response and exits nonzero on failure.

```text
autoscope ctl SOCKET info
autoscope ctl SOCKET move [--normalize] [--no-wait] X Y
autoscope ctl SOCKET click [--button BUTTON] [--x X --y Y] [--normalize] [--no-wait]
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
autoscope ctl "$CONTROL" move 300 200
autoscope ctl "$CONTROL" mouse-down left
autoscope ctl "$CONTROL" move --no-wait 700 500
autoscope ctl "$CONTROL" mouse-up left
autoscope ctl "$CONTROL" wait
```

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

With bwrap, the launch directory is available read-only as `/work`; the application gets a private writable home and `/tmp`. Host displays, input devices, audio, session bus, and desktop portals are not exposed. `--no-network` also removes network access.

Finish with:

```sh
autoscope ctl "$CONTROL" quit
```

The `run` process then closes the application, viewer, sockets, and private session files.
