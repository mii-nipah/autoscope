#!/usr/bin/env python3
"""Verify default LK arrows, optional trails/off and history in real recordings."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import shutil
import subprocess
from threading import Thread
import time

from plugin_workflow import Mcp, save_image


def colors(plugin, path, crop="crop=iw:ih/2:0:ih/4"):
    # The scene is grayscale; crop browser decorations before counting trail colors.
    decoded = subprocess.run([str(plugin / "runtime/usr/bin/ffmpeg"), "-v", "error",
                              "-xerror", "-i", str(path), "-vf", crop,
                              "-f", "rawvideo", "-pix_fmt", "rgb24", "-"],
                             capture_output=True, check=True).stdout
    assert decoded, path
    blue = amber = 0
    for r, g, b in zip(decoded[::3], decoded[1::3], decoded[2::3]):
        blue += b > r + 20 and b > g + 5
        amber += r > b + 20 and g > b + 5
    return {"blue_pixels": blue, "amber_pixels": amber}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("plugin", type=Path)
    parser.add_argument("--artifacts", type=Path, required=True)
    args = parser.parse_args()
    plugin, artifacts = args.plugin.resolve(), args.artifacts.resolve()
    artifacts.mkdir(parents=True, exist_ok=True)
    fixture = {"hold": False}

    class Page(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path == "/hold":
                self.send_response(200)
                self.end_headers()
                self.wfile.write(json.dumps(fixture).encode())
                return
            body = b'''<!doctype html><style>body{margin:0;background:#121212}
canvas{display:block}</style><canvas width="800" height="600"></canvas><script>
const c=document.querySelector('canvas').getContext('2d');
let holdStart=null;
setInterval(async()=>{if((await (await fetch('/hold')).json()).hold && holdStart===null)
holdStart=performance.now()},100);
function draw(t){c.fillStyle='#121212';c.fillRect(0,0,800,600);
c.fillStyle='#666';c.fillRect(32,48,140,10);c.fillRect(32,76,100,10);
c.fillStyle='#ddd';c.beginPath();
const x=holdStart===null?200+(t*.2)%550:200+450*Math.min(1,(t-holdStart)/1000);
c.arc(x,300,22,0,Math.PI*2);c.fill();
c.fillStyle='#444';c.fillRect(x-12,288,8,8);c.fillRect(x+4,302,10,6);
requestAnimationFrame(draw)}requestAnimationFrame(draw);</script>'''
            self.send_response(200)
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Page)
    Thread(target=server.serve_forever, daemon=True).start()
    mcp = Mcp(plugin, artifacts)
    try:
        mcp.request("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                                  "clientInfo": {"name": "motion-check", "version": "1"}})
        mcp.send({"method": "notifications/initialized"})
        schema = next(t for t in mcp.request("tools/list", {})["tools"]
                      if t["name"] == "start_recording")["inputSchema"]
        assert schema["properties"]["motion"]["default"] == "arrows", schema
        state = save_image(mcp.tool("spawn_flatpak", application_id="com.google.Chrome",
                                   arguments=[f"--app=http://127.0.0.1:{server.server_port}/"],
                                   options={"width": 800, "height": 600, "fps": 30}),
                           artifacts / "unmodified.png")
        session = state["session"]
        report = {}
        for mode in ("images", "video"):
            for motion in ("arrows", "trails", "off"):
                name = f"{mode}-{motion}"
                options = {} if motion == "arrows" else {"motion": motion}
                started = mcp.tool("start_recording", session=session,
                         mode=None if mode == "video" and motion == "arrows" else mode,
                         frames_per_image=None,
                         fps=1 if mode == "images" else 10, **options)
                assert started["structuredContent"]["result"]["motion"] == motion, started
                time.sleep(2.3)
                result = mcp.tool("stop_recording", session=session)["structuredContent"]
                assert result["motion"] == motion and result["frames"] >= 2, result
                files = []
                for index, source in enumerate(result["files"]):
                    target = artifacts / f"{name}-{index}{Path(source).suffix}"
                    shutil.copy2(source, target)
                    files.append(colors(plugin, target))
                assert all((c["blue_pixels"] > 10 and c["amber_pixels"] > 10)
                           if motion != "off" else c == {"blue_pixels": 0, "amber_pixels": 0}
                           for c in files), (name, files)
                report[name] = {"frames": result["frames"], "colors": files}

        mcp.tool("start_recording", session=session, mode="images", fps=1,
                 frames_per_image=5)
        fixture["hold"] = True
        time.sleep(8.3)
        result = mcp.tool("stop_recording", session=session)["structuredContent"]
        assert len(result["files"]) == 2 and result["frames"] >= 8, result
        first = second = None
        for index, source in enumerate(result["files"]):
            target = artifacts / f"history-sheet-{index + 1}.png"
            shutil.copy2(source, target)
            if index == 0:
                first = colors(plugin, target, "crop=320:120:1300:64")
            else:
                second = colors(plugin, target)
        assert first["blue_pixels"] > 10, first
        assert second == {"blue_pixels": 0, "amber_pixels": 0}, second
        report["sheet-history"] = {"frames": result["frames"],
                                   "retained_after_stopping": first,
                                   "next_sheet_reset": second}
        save_image(mcp.tool("screenshot", session=session), artifacts / "still-after-motion.png")
        assert colors(plugin, artifacts / "still-after-motion.png") == {
            "blue_pixels": 0, "amber_pixels": 0}
        mcp.tool("close_session", session=session)
        assert not mcp.tool("list_sessions")["structuredContent"]["sessions"]
        (artifacts / "result.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(report, indent=2))
    finally:
        mcp.close()
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    main()
