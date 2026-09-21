#!/usr/bin/env python3
"""Verify native GUI input, export and bundled video without Xwayland or a host display."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

from plugin_workflow import Mcp, save_image


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("plugin", type=Path)
    parser.add_argument("--artifacts", type=Path, required=True)
    args = parser.parse_args()
    plugin = args.plugin.resolve()
    artifacts = args.artifacts.resolve()
    artifacts.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="autoscope-native-") as temporary:
        root = Path(temporary)
        path = root / "bin"
        path.mkdir()
        for name in ("dirname", "uname", "cat"):
            (path / name).symlink_to(shutil.which(name))
        assert shutil.which("Xwayland", path=str(path)) is None
        env = {key: value for key, value in os.environ.items()
               if key not in ("XDG_RUNTIME_DIR", "WAYLAND_DISPLAY", "DISPLAY", "LD_LIBRARY_PATH")}
        env.update(PATH=str(path), GDK_BACKEND="wayland", GSK_RENDERER="cairo")
        shared = root / "shared"
        shared.mkdir()
        fixture = root / "launch.sh"
        fixture.write_text('''#!/bin/sh
set -eu
[ -z "${DISPLAY:-}" ]
[ "$PWD" = /work ]
[ ! -w . ]
value=$(/usr/bin/zenity --entry --title="Autoscope native Wayland" --text="Enter verification text:")
printf '%s' "$value" > "$AUTOSCOPE_SHARED_DIR/result.txt"
exec /usr/bin/zenity --info --title="Autoscope verified" --text="Saved: $value"
''')
        fixture.chmod(0o755)
        mcp = Mcp(plugin, artifacts, env)
        try:
            mcp.request("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                                      "clientInfo": {"name": "portable-check", "version": "1"}})
            mcp.send({"method": "notifications/initialized"})
            result = mcp.tool("spawn_command", command=["./launch.sh"], sandbox="bwrap",
                              options={"working_dir": str(root), "shared_dir": str(shared),
                                       "width": 800, "height": 600, "no_network": True})
            state = save_image(result, artifacts / "01-native.png")
            session = state["session"]
            mcp.tool("start_recording", session=session, mode="video", fps=5)
            state = save_image(mcp.tool("type_text", session=session, view=state["view"],
                                        text="Native Wayland works"), artifacts / "02-typed.png")
            save_image(mcp.tool("press_key", session=session, view=state["view"], combo="ENTER"),
                       artifacts / "03-saved.png")
            assert (shared / "result.txt").read_bytes() == b"Native Wayland works"
            recording = mcp.tool("stop_recording", session=session)
            (artifacts / "recording.json").write_text(json.dumps(recording, indent=2) + "\n")
            videos = list((artifacts / "state").rglob("*.mp4"))
            assert len(videos) == 1, videos
            video = artifacts / "native-workflow.mp4"
            shutil.copy2(videos[0], video)
            decode = subprocess.run([str(plugin / "runtime/usr/bin/ffmpeg"), "-v", "error",
                                     "-xerror", "-i", str(video), "-f", "null", "-"],
                                    capture_output=True, text=True, env=env)
            assert decode.returncode == 0, decode.stderr
            mcp.tool("close_session", session=session)
            assert mcp.tool("list_sessions")["structuredContent"]["sessions"] == []
            report = {"xwayland_on_server_path": False, "host_display_environment": False,
                      "inherited_xdg_runtime_dir": False,
                      "sandbox": "bundled bwrap", "project_directory": "read-only /work",
                      "native_gui_saved_bytes": "Native Wayland works", "video_decoded": True}
            (artifacts / "result.json").write_text(json.dumps(report, indent=2) + "\n")
            print(json.dumps(report, indent=2))
        finally:
            mcp.close()


if __name__ == "__main__":
    main()
