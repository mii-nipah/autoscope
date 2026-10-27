#!/usr/bin/env python3
"""Build a relocatable Xwayland with xwayland-shell support on the old glibc base."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
from urllib.request import urlopen

prefix = Path("/opt/autoscope-x11")
sources = Path("/opt/autoscope-sources")
sources.mkdir()
manifest = Path(__file__).with_name("xwayland-sources.json")
shutil.copy2(manifest, sources)
for source in json.loads(manifest.read_text()):
    archive = sources / (source["name"] + ".tar.xz")
    with urlopen(source["url"], timeout=60) as response:
        data = response.read()
    assert hashlib.sha256(data).hexdigest() == source["sha256"], source["name"]
    archive.write_bytes(data)
    with tarfile.open(archive) as tar:
        tar.extractall("/tmp")
    root = Path("/tmp") / source["name"]
    subprocess.run(["meson", "setup", str(root / "build"), str(root), "--buildtype=release",
                    "--wrap-mode=nofallback", f"--prefix={prefix}", "--libdir=lib", *source["options"]], check=True)
    if source["name"].startswith("xwayland-"):
        # Empty means PATH lookup in xkb/ddxLoad.c, not the build host's /usr/bin.
        header = root / "build/include/xkb-config.h"
        lines = header.read_text().splitlines()
        assert sum(line.startswith("#define XKB_BIN_DIRECTORY ") for line in lines) == 1
        header.write_text("\n".join('#define XKB_BIN_DIRECTORY ""' if line.startswith(
            "#define XKB_BIN_DIRECTORY ") else line for line in lines) + "\n")
    subprocess.run(["ninja", "-C", str(root / "build"), "-j4", "install"], check=True)
    licenses = prefix / "share/autoscope-licenses" / source["name"]
    licenses.mkdir(parents=True)
    for path in root.iterdir():
        if path.is_file() and path.name.startswith(("COPYING", "LICENSE")):
            shutil.copy2(path, licenses)
    if not any(licenses.iterdir()):
        # libdrm keeps its copyright notices in the individual source files.
        shutil.copy2(archive, licenses)
    shutil.rmtree(root)
