---
name: autoscope
description: Run, inspect, and automate Linux GUI applications in private Autoscope sessions. Use for Wayland, X11, or Flatpak app testing, visual workflows, drawing, screenshots, recordings, and Autoscope setup. Controls applications it launches, not the user's existing desktop windows.
---

# Autoscope

Use this plugin's MCP tools to launch and operate one Linux application per session. The launcher manages displays and application profiles; use the returned session ID and images.

## Setup

The plugin contains its runtime and helpers; there is no build or setup step. If tools are unavailable or the user asks to check Autoscope, run `runtime/AppRun doctor` from the plugin root (two directories above this skill) and read [the plugin README](../../README.md). A new Codex task is required after installation. A missing runtime indicates an incomplete package; reinstall it rather than installing an unrelated package named Autoscope. Xwayland is optional and needed only for X11 apps; Flatpak is needed only for Flatpak apps.

## Launch and observe

- Discover the installed application executable or Flatpak ID. `spawn_command` takes `command: ["program", "arg", ...]`, without shell interpretation. For project apps, set `options.working_dir` to the absolute project directory and use a relative executable such as `./target/debug/my-app`. That directory becomes `/work` inside bwrap. The plugin server's own directory is not the user's project.
- The plugin bundles bwrap, so native applications use filesystem isolation by default. Use `sandbox: "bwrap"` to require it explicitly. Resolve a sandbox failure before weakening isolation.
- For offline X11 workflows, use `options.no_network: true`. A tested host has a display-authorization failure for X11 clients with shared networking; see the README's troubleshooting note. Do not solve this by granting access to the user's host display.
- `spawn_flatpak` takes `application_id` and optional `arguments`. Autoscope creates the private displays and application profile. Avoid the user's normal browser/profile and host display sockets.
- `options` supports `name`, `width`, `height`, `fps`, `shared_dir`, `window`, and `no_network`. Portrait is simply a taller display, e.g. `width: 390, height: 844`. `window: true` opens a read-only viewer when the user wants to watch.
- Inspect the returned image before choosing an action. Every input requires the latest returned `view`. Perform one action, inspect its image, and use its new view for the next. Do not send parallel or preplanned inputs for the same session.
- A stale-view error means no input was injected. Take a fresh `screenshot`, inspect it, and choose again. A visual wait timeout does not mean the action failed; inspect before retrying. Stable pixels do not prove that an application operation succeeded.

## Input and files

Coordinates are absolute screenshot pixels unless a tool exposes `normalize: true` for 0..1 coordinates. `type_text` accepts printable US ASCII; use `press_key` for combinations such as `CTRL+L`, `ENTER`, and `TAB`. There is no clipboard or Unicode typing bridge.

Use `drag` for a whole stroke: `points: [[x,y], ...]`, latest `view`, and optionally `origin: [x,y]` and `scale` to map canvas pixels to the screen. Reserve held buttons and `move_pointer` with `wait: false` for gestures needing intermediate observations; release held buttons if interrupted.

Place input files and save outputs in the returned `shared_dir`, which has the same absolute host and application path. Sandboxed apps also see it as `$HOME/Shared`. The working tree is read-only at `/work` under bwrap. Files outside the shared folder may be inaccessible; host desktop portal dialogs are not available. Verify an export at its actual host path, beyond the save dialog.

## Evidence and completion

Use `screenshot` for still evidence. `start_recording` with `mode: "images", fps: 1` and then `stop_recording` returns compact contact sheets directly as MCP images; frames read left-to-right, then top-to-bottom. Use `mode: "video"` when a video is useful; FFmpeg is bundled. Stop recording before closing the session and retain the returned paths.

Check the requested application outcome, then `close_session`. `list_sessions` lists only sessions owned by this MCP server. Server exit closes its sessions; shared files, logs, and recordings survive. For an explicitly persistent session, use the CLI's detached lifecycle and `autoscope readme` instead.

Report what worked, the evidence, and any unverified boundary. Compatibility covers native Wayland and XWayland applications using shared-memory presentation; one successful app is not proof of all Linux/GPU/toolkit combinations.
