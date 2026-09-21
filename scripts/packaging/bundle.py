#!/usr/bin/env python3
"""Stage an AppDir from the Ubuntu build environment without changing host library lookup."""
import json
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys

source, output = map(Path, sys.argv[1:])
appdir = output / "Autoscope.AppDir"
if appdir.exists():
    shutil.rmtree(appdir)
bin_dir = appdir / "usr/bin"
lib_dir = appdir / "usr/lib"
bin_dir.mkdir(parents=True)
lib_dir.mkdir()
system_libraries = {"libc.so.6", "libm.so.6", "libdl.so.2", "librt.so.1", "libpthread.so.0",
                    "libresolv.so.2", "libanl.so.1", "libutil.so.1"}
origins = set()
executables = [output / "cargo/release/autoscope", *(Path(shutil.which(name)) for name in ("bwrap", "ffmpeg", "ffplay"))]
for executable in executables:
    destination = bin_dir / executable.name
    shutil.copy2(executable, destination)
    subprocess.run(["strip", str(destination)], check=True)
    subprocess.run(["patchelf", "--set-rpath", "$ORIGIN/../lib", str(destination)], check=True)
    origins.add(executable)
    dependencies = subprocess.check_output(["ldd", str(executable)], text=True)
    if "not found" in dependencies:
        raise RuntimeError(dependencies)
    for name, path in re.findall(r"\s+(\S+) => (/\S+)", dependencies):
        if name in system_libraries or (lib_dir / name).exists():
            continue
        shutil.copy2(path, lib_dir / name)
        subprocess.run(["patchelf", "--set-rpath", "$ORIGIN", str(lib_dir / name)], check=True)
        origins.add(Path(path))

shutil.copytree("/usr/share/X11/xkb", appdir / "usr/share/X11/xkb")
shutil.copy2(source / "scripts/packaging/AppRun", appdir / "AppRun")
(appdir / "AppRun").chmod(0o755)
(appdir / "architecture").write_text(platform.machine() + "\n")
shutil.copy2(source / "scripts/packaging/autoscope.desktop", appdir / "autoscope.desktop")
shutil.copy2(source / "plugin/autoscope/assets/icon.svg", appdir / "autoscope.svg")
shutil.copy2(appdir / "autoscope.svg", appdir / ".DirIcon")
shutil.copy2(source / "LICENSE", appdir / "LICENSE")

packages = {"xkb-data"}
for path in origins:
    if path.name == "autoscope":
        continue
    for candidate in dict.fromkeys((str(path), str(path.resolve()), "/usr" + str(path))):
        owner = subprocess.run(["dpkg-query", "-S", candidate], capture_output=True, text=True)
        if owner.returncode == 0:
            packages.add(owner.stdout.split(": ")[0])
            break
    else:
        raise RuntimeError(f"No package provenance for {path}")
licenses = appdir / "usr/share/licenses"
licenses.mkdir()
package_sources = []
for package in sorted(packages):
    name = package.split(":")[0]
    copyright_file = Path("/usr/share/doc") / name / "copyright"
    if copyright_file.exists():
        shutil.copy2(copyright_file, licenses / f"{name}.txt")
    package_sources.append(subprocess.check_output([
        "dpkg-query", "-W", "-f=${source:Package}=${source:Version}", package], text=True))

metadata = subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1",
                                    "--manifest-path", str(source / "Cargo.toml")], text=True)
rust_packages = json.loads(metadata)["packages"]
for package in rust_packages:
    root = Path(package["manifest_path"]).parent
    for path in root.iterdir():
        if path.is_file() and path.name.upper().startswith(("LICENSE", "COPYING", "NOTICE")):
            dest = licenses / f"{package['name']}-{package['version']}"
            dest.mkdir(exist_ok=True)
            shutil.copy2(path, dest / path.name)

info = {"architecture": platform.machine(), "build_base": "Ubuntu 22.04", "glibc_minimum": "2.35",
        "system_sources": sorted(set(package_sources)),
        "rust_packages": [{key: p.get(key) for key in ("name", "version", "license", "source")} for p in rust_packages]}
(appdir / "build-info.json").write_text(json.dumps(info, indent=2) + "\n")
(output / "system-sources.txt").write_text("\n".join(info["system_sources"]) + "\n")
print(appdir)
