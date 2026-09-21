#!/usr/bin/env python3
"""Exercise a packaged plugin with Flatpak Chrome, Xmessage and bwrap; no model or remote site."""
import argparse
import base64
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
from queue import Queue
import selectors
import shlex
import shutil
import subprocess
import tempfile
from threading import Thread


class Mcp:
    def __init__(self, plugin, artifacts, env=None, command=None):
        # Exercise Codex's missing desktop-session environment at application launch.
        if env is None:
            env = {key: value for key, value in os.environ.items()
                   if key not in ("XDG_RUNTIME_DIR", "WAYLAND_DISPLAY", "DISPLAY")}
        config = json.loads((plugin / ".mcp.json").read_text())["mcpServers"]["autoscope"]
        cwd = plugin / config["cwd"] if config.get("cwd") else artifacts
        self.log = (artifacts / "mcp.log").open("w")
        self.process = subprocess.Popen(
            command or [config["command"], *config["args"]], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=self.log, text=True, cwd=cwd,
            env={**env, "XDG_STATE_HOME": str(artifacts / "state")},
        )
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.process.stdout, selectors.EVENT_READ)
        self.sequence = 0

    def request(self, method, params):
        self.sequence += 1
        self.send({"id": self.sequence, "method": method, "params": params})
        while True:
            assert self.selector.select(40), f"MCP timeout: {method}; see mcp.log"
            line = self.process.stdout.readline()
            assert line, f"MCP exited: {self.process.poll()}; see mcp.log"
            reply = json.loads(line)
            if reply.get("id") == self.sequence:
                assert "error" not in reply, reply
                return reply["result"]

    def send(self, message):
        self.process.stdin.write(json.dumps({"jsonrpc": "2.0", **message}) + "\n")
        self.process.stdin.flush()

    def tool(self, name, **arguments):
        result = self.request("tools/call", {"name": name, "arguments": arguments})
        assert not result.get("isError"), result
        return result

    def close(self):
        self.process.stdin.close()
        try:
            assert self.process.wait(timeout=15) == 0
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait()
            self.selector.close()
            self.log.close()


def save_image(result, destination):
    image = next(item for item in result["content"] if item["type"] == "image")
    destination.write_bytes(base64.b64decode(image["data"]))
    return result["structuredContent"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("plugin", type=Path)
    parser.add_argument("--artifacts", type=Path, required=True)
    args = parser.parse_args()
    artifacts = args.artifacts.resolve()
    artifacts.mkdir(parents=True, exist_ok=True)
    submitted = Queue()

    class Page(BaseHTTPRequestHandler):
        def do_GET(self):
            body = b'''<!doctype html><meta charset="utf-8"><title>Autoscope plugin check</title>
<style>body{background:#0b2428;color:#d9fff7;font:24px sans-serif;margin:40px}
input,button{font:24px sans-serif;padding:16px} input{width:420px}</style>
<h1>Autoscope plugin check</h1><p>Real Linux app. Packaged MCP connection.</p>
<form><input autofocus aria-label="Message"><p><button>Verify workflow</button></p></form>
<div id="result">Waiting for agent input.</div>
<script>document.querySelector('form').onsubmit=async e=>{e.preventDefault();
let text=document.querySelector('input').value;
await fetch('/submit',{method:'POST',body:text});
document.querySelector('#result').textContent='Verified: '+text;};</script>'''
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self):
            submitted.put(self.rfile.read(int(self.headers["Content-Length"])).decode())
            self.send_response(204)
            self.end_headers()

        def log_message(self, *unused):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Page)
    Thread(target=server.serve_forever, daemon=True).start()
    mcp = Mcp(args.plugin.resolve(), artifacts)
    try:
        mcp.request("initialize", {
            "protocolVersion": "2024-11-05", "capabilities": {},
            "clientInfo": {"name": "autoscope-plugin-check", "version": "1"},
        })
        mcp.send({"method": "notifications/initialized"})
        tools = mcp.request("tools/list", {})["tools"]
        invalid = mcp.request("tools/call", {"name": "spawn_command", "arguments": {
            "command": ["/bin/true"], "options": {"working_dir": "."},
        }})
        assert invalid.get("isError") and "working_dir" in str(invalid), invalid
        with tempfile.TemporaryDirectory(prefix="autoscope-project-") as project:
            project = Path(project)
            shared = project / "shared"
            shared.mkdir()
            (project / "proof.txt").write_text("project directory reached through read-only /work\n")
            xmessage = shutil.which("xmessage")
            assert xmessage, "Install xmessage for the native X11 working-directory check"
            fixture = project / "launch.sh"
            fixture.write_text('#!/bin/sh\nset -eu\n[ "$PWD" = /work ]\n[ ! -w . ]\n'
                               'cat proof.txt > "$AUTOSCOPE_SHARED_DIR/proof.txt"\n'
                               f'exec {shlex.quote(xmessage)} -buttons OK:0 "Autoscope project workspace verified"\n')
            fixture.chmod(0o755)
            native = mcp.tool("spawn_command", command=["./launch.sh"], sandbox="bwrap",
                              options={"working_dir": str(project), "shared_dir": str(shared), "no_network": True})
            native_state = save_image(native, artifacts / "native-workspace.png")
            assert (shared / "proof.txt").read_bytes() == (project / "proof.txt").read_bytes()
            mcp.tool("close_session", session=native_state["session"])
        with tempfile.TemporaryDirectory(prefix="autoscope-plugin-shared-") as shared:
            spawn = mcp.tool("spawn_flatpak", application_id="com.google.Chrome", arguments=[
                f"--app=http://127.0.0.1:{server.server_port}/", "--window-size=800,600",
            ], options={"width": 800, "height": 600, "shared_dir": shared})
            state = save_image(spawn, artifacts / "01-open.png")
            session = state["session"]
            initial_view = state["view"]
            mcp.tool("start_recording", session=session, mode="images", fps=1)
            for index, (tool, values) in enumerate([
                ("click", {"x": 240, "y": 235}),
                ("type_text", {"text": "Autoscope works"}),
                ("press_key", {"combo": "TAB"}),
                ("press_key", {"combo": "ENTER"}),
            ], start=2):
                result = mcp.tool(tool, session=session, view=state["view"], **values)
                state = save_image(result, artifacts / f"{index:02}-{tool}.png")
            assert submitted.get(timeout=5) == "Autoscope works", "Browser did not submit the typed value"
            stale = mcp.request("tools/call", {"name": "press_key", "arguments": {
                "session": session, "view": initial_view, "combo": "ENTER",
            }})
            assert stale.get("isError"), "Consumed observation accepted another action"
            recording = mcp.tool("stop_recording", session=session)
            images = [item for item in recording["content"] if item["type"] == "image"]
            assert images, "Contact-sheet recording returned no images"
            for index, image in enumerate(images):
                (artifacts / f"recording-{index + 1}.png").write_bytes(base64.b64decode(image["data"]))
            mcp.tool("close_session", session=session)
            sessions = mcp.tool("list_sessions")["structuredContent"]["sessions"]
            assert sessions == [], sessions
            assert submitted.empty(), "Stale input unexpectedly submitted the form again"
            report = {"tools": len(tools), "project_working_directory": "verified read-only /work",
                      "native_x11_network": False,
                      "form_submission": "Autoscope works", "stale_input_rejected": True,
                      "recording_sheets": len(images), "sessions_after_close": sessions}
            (artifacts / "result.json").write_text(json.dumps(report, indent=2) + "\n")
            print(json.dumps(report, indent=2))
    finally:
        mcp.close()
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    main()
