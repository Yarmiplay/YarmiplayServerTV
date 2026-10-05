#!/usr/bin/env python3
"""
Helpers for the Snap (snap/) and the release checks. Standard library only.

  fetch-jellyfin --arch amd64 --dest DIR
      Downloads the pinned Jellyfin server and jellyfin-ffmpeg from src-tauri/jellyfin-manifest.json, checks
      their size and SHA-256, and unpacks them as the app expects a built-in copy: the server in DIR, ffmpeg in
      DIR/ffmpeg, plus a NOTICE.txt.
  check-metainfo VERSION
      Fails unless the AppStream metainfo has a <release version="VERSION"> entry.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import tarfile
import tempfile
import urllib.request
import xml.etree.ElementTree as ET

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MANIFEST = os.path.join(ROOT, "src-tauri", "jellyfin-manifest.json")
METAINFO = os.path.join(ROOT, "packaging", "linux", "com.yarmiplay.servertv.metainfo.xml")

# Debian/Snap architecture names and the manifest's.
ARCHES = {"amd64": "linux-x86_64", "arm64": "linux-aarch64"}
ARCH_ALIASES = {"x86_64": "amd64", "aarch64": "arm64"}


def manifest():
    with open(MANIFEST, encoding="utf-8") as f:
        return json.load(f)


def artifacts(arch):
    m = manifest()
    entry = m["platforms"][ARCHES[arch]]
    return m, entry["server"], entry["ffmpeg"]


def notice(m):
    return (f"Jellyfin {m['jellyfin']} and jellyfin-ffmpeg {m['ffmpeg']} for Linux, unmodified, as published at\n"
            "https://repo.jellyfin.org/\n\n"
            "Jellyfin is free software under the GNU General Public License, version 2; jellyfin-ffmpeg is under "
            "the GNU General Public License, version 3. Their source code:\n"
            f"https://github.com/jellyfin/jellyfin/tree/v{m['jellyfin']}\n"
            f"https://github.com/jellyfin/jellyfin-web/tree/v{m['jellyfin']}\n"
            f"https://github.com/jellyfin/jellyfin-ffmpeg/tree/v{m['ffmpeg']}\n")


def download(artifact, dest):
    h = hashlib.sha256()
    size = 0
    req = urllib.request.Request(artifact["url"], headers={"User-Agent": "YarmiplayServerTV-packaging"})
    with urllib.request.urlopen(req, timeout=60) as resp, open(dest, "wb") as out:
        while chunk := resp.read(1 << 20):
            h.update(chunk)
            size += len(chunk)
            out.write(chunk)
    if size != artifact["size"] or h.hexdigest() != artifact["sha256"]:
        os.remove(dest)
        sys.exit(f"checksum mismatch for {artifact['url']}")


def fetch_jellyfin(arch, dest):
    m, server, ffmpeg = artifacts(arch)
    os.makedirs(os.path.join(dest, "ffmpeg"), exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        for artifact, target in ((server, dest), (ffmpeg, os.path.join(dest, "ffmpeg"))):
            archive = os.path.join(tmp, os.path.basename(artifact["url"]))
            print(f"downloading {artifact['url']}")
            download(artifact, archive)
            with tarfile.open(archive, "r:xz") as tar:
                tar.extractall(target, filter="tar")
    with open(os.path.join(dest, "NOTICE.txt"), "w", encoding="utf-8") as f:
        f.write(notice(m))
    print(f"Jellyfin {m['jellyfin']} with jellyfin-ffmpeg {m['ffmpeg']} in {dest}")


def metainfo_versions(path=METAINFO):
    return [r.get("version") for r in ET.parse(path).getroot().iter("release")]


def check_metainfo(version):
    if version not in metainfo_versions():
        sys.exit(f'{os.path.relpath(METAINFO, ROOT)} has no <release version="{version}">; '
                 f"run python scripts/bump-version.py {version}")
    print(f"metainfo has release {version}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("fetch-jellyfin")
    f.add_argument("--arch", required=True, help="amd64 or arm64 (x86_64 and aarch64 work too)")
    f.add_argument("--dest", required=True)
    c = sub.add_parser("check-metainfo")
    c.add_argument("version")
    a = ap.parse_args()
    if a.cmd == "fetch-jellyfin":
        arch = ARCH_ALIASES.get(a.arch, a.arch)
        if arch not in ARCHES:
            sys.exit(f"unsupported architecture {a.arch}")
        fetch_jellyfin(arch, a.dest)
    else:
        check_metainfo(a.version)


if __name__ == "__main__":
    main()
