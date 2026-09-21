#!/usr/bin/env python3
"""Fetch the exact Ubuntu source releases, including versions retired from apt indexes."""
from concurrent.futures import ThreadPoolExecutor
import hashlib
from pathlib import Path
import re
import subprocess
import sys
from urllib.parse import quote, urljoin
from urllib.error import URLError
from urllib.request import urlopen

manifest, output = map(Path, sys.argv[1:])


def download(url, path, digest=None):
    if path.exists() and (digest is None or hashlib.sha256(path.read_bytes()).hexdigest() == digest):
        return
    for attempt in range(3):
        try:
            with urlopen(url.replace("http://", "https://", 1), timeout=30) as response:
                data = response.read()
            break
        except (OSError, URLError) as error:
            if attempt == 2:
                raise RuntimeError(f"Source download failed: {url}") from error
    if digest and hashlib.sha256(data).hexdigest() != digest:
        raise RuntimeError(f"Source checksum mismatch: {url}")
    path.write_bytes(data)


def package(spec):
    name, version = spec.split("=", 1)
    dsc = f"{name}_{version.split(':')[-1]}.dsc"
    indexed = subprocess.run(["apt-get", "source", "--print-uris", spec], capture_output=True, text=True)
    uri = re.search(r"'([^']+\.dsc)'", indexed.stdout) if indexed.returncode == 0 else None
    base = uri.group(1) if uri else (
        f"https://launchpad.net/ubuntu/+archive/primary/+sourcefiles/{quote(name)}/{quote(version)}/{quote(dsc)}")
    destination = output / dsc
    download(base, destination)
    contents = destination.read_text()
    assert re.search(rf"^Version: {re.escape(version)}$", contents, re.M), dsc
    block = re.search(r"^Checksums-Sha256:\n((?: .+\n)+)", contents, re.M)
    if block is None:
        raise RuntimeError(f"No source checksums: {dsc}")
    for line in block.group(1).splitlines():
        digest, size, filename = line.split()
        assert Path(filename).name == filename
        path = output / filename
        download(urljoin(base, quote(filename)), path, digest)
        assert path.stat().st_size == int(size), filename
    return spec


with ThreadPoolExecutor(max_workers=6) as pool:
    for spec in pool.map(package, manifest.read_text().splitlines()):
        print(f"Verified sources: {spec}", flush=True)
