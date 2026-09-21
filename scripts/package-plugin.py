#!/usr/bin/env python3
"""Package the same prebuilt runtime as a Codex plugin and a standalone AppImage."""
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile


def archive_bytes(root, paths):
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w") as archive:
        for path in sorted(paths):
            if path.is_symlink():
                raise ValueError(f"Refusing to package symlink: {path}")
            if not path.is_file():
                continue
            info = archive.gettarinfo(str(path), arcname=str(path.relative_to(root)))
            info.uid = info.gid = info.mtime = 0
            info.uname = info.gname = ""
            info.mode = 0o755 if path.stat().st_mode & 0o111 else 0o644
            with path.open("rb") as source:
                archive.addfile(info, source)
    return gzip.compress(raw.getvalue(), mtime=0)


def main():
    repo = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=repo / "target" / "plugin")
    parser.add_argument("--appdir", type=Path, default=repo / "target" / "bundle" / "Autoscope.AppDir")
    args = parser.parse_args()
    output = args.output.expanduser().resolve()
    output.mkdir(parents=True, exist_ok=True)
    appdir = args.appdir.expanduser().resolve()
    if not (appdir / "usr/bin/autoscope").is_file():
        parser.error("Build the portable runtime first with scripts/build-bundle.sh")
    architecture = (appdir / "architecture").read_text().strip()
    with tempfile.TemporaryDirectory(prefix="autoscope-package-") as temporary:
        stage = Path(temporary) / "autoscope"
        shutil.copytree(repo / "plugin" / "autoscope", stage)
        shutil.copy2(repo / "LICENSE", stage / "LICENSE")
        shutil.copytree(appdir, stage / "runtime")
        version = json.loads((stage / ".codex-plugin" / "plugin.json").read_text())["version"]
        package = output / f"autoscope-{version}-linux-{architecture}.tar.gz"
        package.write_bytes(archive_bytes(stage.parent, stage.rglob("*")))
        checksum = hashlib.sha256(package.read_bytes()).hexdigest()
        package.with_name(package.name + ".sha256").write_text(f"{checksum}  {package.name}\n")
        print(package)
        appimage = output / f"autoscope-{version}-{architecture}.AppImage"
        with (output / "appimage.log").open("w") as log:
            subprocess.run(["appimagetool", "--no-appstream", str(appdir), str(appimage)],
                           stdout=log, stderr=subprocess.STDOUT, check=True)
        checksum = hashlib.sha256(appimage.read_bytes()).hexdigest()
        appimage.with_name(appimage.name + ".sha256").write_text(f"{checksum}  {appimage.name}\n")
        print(appimage)


if __name__ == "__main__":
    main()
