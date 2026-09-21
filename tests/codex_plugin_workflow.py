#!/usr/bin/env python3
"""Launch and operate the installed plugin through Codex itself, without a model turn."""
import argparse
import json
from pathlib import Path
import tempfile
import tomllib

from plugin_workflow import Mcp, save_image


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("plugin", type=Path)
    parser.add_argument("--codex", required=True)
    parser.add_argument("--artifacts", type=Path, required=True)
    args = parser.parse_args()
    artifacts = args.artifacts.resolve()
    artifacts.mkdir(parents=True, exist_ok=True)
    command = [args.codex, "app-server"]
    config = tomllib.loads((Path.home() / ".codex/config.toml").read_text())
    for group in ("plugins", "mcp_servers"):
        for name in config.get(group, {}):
            if group == "plugins" and name == "autoscope@personal":
                continue
            # These are per-process overrides; leave the user's config untouched.
            assert "." not in name, f"Unsupported config identifier: {name}"
            command.extend(["-c", f"{group}.{name}.enabled=false"])
    with tempfile.TemporaryDirectory(prefix="autoscope-codex-probe-") as temporary:
        root = Path(temporary)
        shared = root / "shared"
        shared.mkdir()
        (root / "launch.sh").write_text('''#!/bin/sh
set -eu
[ "$PWD" = /work ]
[ ! -w . ]
export GDK_BACKEND=wayland GSK_RENDERER=cairo
value=$(/usr/bin/zenity --entry --title="Codex MCP launch" --text="Enter verification text:")
printf '%s' "$value" > "$AUTOSCOPE_SHARED_DIR/result.txt"
exec /usr/bin/zenity --info --title="Codex MCP verified" --text="Saved: $value"
''')
        client = Mcp(args.plugin.resolve(), artifacts, command=command)
        try:
            client.request("initialize", {"clientInfo": {"name": "autoscope-codex-check", "version": "1"},
                                          "capabilities": {"experimentalApi": True}})
            # Ephemeral context supplies the MCP tool-call scope without saving a task or running a model.
            context = client.request("thread/start", {"cwd": str(root), "ephemeral": True})
            thread = context["thread"]["id"]
            servers = client.request("mcpServerStatus/list", {"threadId": thread, "limit": 100})["data"]
            server = next(item for item in servers if "autoscope" in item["name"])
            assert len(server["tools"]) == 15, server["name"]

            def tool(name, **arguments):
                result = client.request("mcpServer/tool/call", {
                    "threadId": thread, "server": server["name"], "tool": name, "arguments": arguments})
                assert not result.get("isError"), result
                return result

            result = tool("spawn_command", command=["/bin/sh", "./launch.sh"], sandbox="bwrap",
                          options={"working_dir": str(root), "shared_dir": str(shared),
                                   "no_network": True, "width": 800, "height": 600})
            state = save_image(result, artifacts / "01-open.png")
            session = state["session"]
            state = save_image(tool("type_text", session=session, view=state["view"],
                                    text="Codex MCP works"), artifacts / "02-typed.png")
            save_image(tool("press_key", session=session, view=state["view"], combo="ENTER"),
                       artifacts / "03-saved.png")
            assert (shared / "result.txt").read_bytes() == b"Codex MCP works"
            tool("close_session", session=session)
            assert tool("list_sessions")["structuredContent"]["sessions"] == []
            report = {"client": "Codex app-server", "tools": 15, "inherited_xdg_runtime_dir": False,
                      "sandbox": "bwrap", "network": False, "saved_bytes": "Codex MCP works"}
            (artifacts / "result.json").write_text(json.dumps(report, indent=2) + "\n")
            print(json.dumps(report, indent=2))
        finally:
            client.close()


if __name__ == "__main__":
    main()
