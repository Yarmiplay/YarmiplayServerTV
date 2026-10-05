#!/usr/bin/env python3
"""
Helpers for the Linux store packages (packaging/, snap/). Standard library only.

  fetch-jellyfin --arch amd64 --dest DIR
      Downloads the pinned Jellyfin server and jellyfin-ffmpeg from src-tauri/jellyfin-manifest.json, checks
      their size and SHA-256, and unpacks them as the app expects a built-in copy: the server in DIR, ffmpeg in
      DIR/ffmpeg, plus a NOTICE.txt. The Snap uses it.
  flatpak-sources [--app-source dir|git] [--tag TAG --commit SHA]
      Writes packaging/flatpak/jellyfin-sources.json (the same downloads as Flatpak sources) and
      app-source.json: this checkout ("dir", for local builds) or the release tag and commit ("git", for
      Flathub). Clones flathub/shared-modules at the pinned commit next to the manifest.
  check-metainfo VERSION
      Fails unless the AppStream metainfo has a <release version="VERSION"> entry.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
import xml.etree.ElementTree as ET

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MANIFEST = os.path.join(ROOT, "src-tauri", "jellyfin-manifest.json")
METAINFO = os.path.join(ROOT, "packaging", "linux", "com.yarmiplay.servertv.metainfo.xml")
FLATPAK_DIR = os.path.join(ROOT, "packaging", "flatpak")
REPO_URL = "https://github.com/Yarmiplay/YarmiplayServerTV.git"
SHARED_MODULES = ("https://github.com/flathub/shared-modules.git", "cb9ec602a1ece1c76d5a4f8aa1d87c4a6bf99c3e")

# Debian/Snap architecture names, Flatpak's and the manifest's.
ARCHES = {"amd64": ("x86_64", "linux-x86_64"), "arm64": ("aarch64", "linux-aarch64")}
ARCH_ALIASES = {"x86_64": "amd64", "aarch64": "arm64"}


def manifest():
    with open(MANIFEST, encoding="utf-8") as f:
        return json.load(f)


def artifacts(arch):
    m = manifest()
    entry = m["platforms"][ARCHES[arch][1]]
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


def flatpak_sources(app_source, tag, commit):
    sources = []
    for arch, (flatpak_arch, _) in ARCHES.items():
        _, server, ffmpeg = artifacts(arch)
        for artifact, name in ((server, "jellyfin.tar.xz"), (ffmpeg, "jellyfin-ffmpeg.tar.xz")):
            sources.append({"type": "file", "url": artifact["url"], "sha256": artifact["sha256"],
                            "dest-filename": name, "only-arches": [flatpak_arch]})
    sources.append({"type": "inline", "dest-filename": "NOTICE.txt", "contents": notice(manifest())})
    write_json(os.path.join(FLATPAK_DIR, "jellyfin-sources.json"), sources)

    if app_source == "git":
        if not (tag and commit):
            sys.exit("--app-source git needs --tag and --commit")
        app = [{"type": "git", "url": REPO_URL, "tag": tag, "commit": commit}]
    else:
        app = [{"type": "dir", "path": "../..",
                "skip": [".git", "node_modules", "dist", "build", "_site", "src-tauri/target", "src-tauri/gen",
                         "packaging/flatpak/.flatpak-builder", "packaging/flatpak/build-dir",
                         "packaging/flatpak/repo"]}]
    write_json(os.path.join(FLATPAK_DIR, "app-source.json"), app)

    shared = os.path.join(FLATPAK_DIR, "shared-modules")
    url, rev = SHARED_MODULES
    if not os.path.isdir(os.path.join(shared, ".git")):
        shutil.rmtree(shared, ignore_errors=True)
        subprocess.run(["git", "clone", "--quiet", url, shared], check=True)
    subprocess.run(["git", "-C", shared, "fetch", "--quiet", "origin", rev], check=True)
    subprocess.run(["git", "-C", shared, "checkout", "--quiet", rev], check=True)


def write_json(path, data):
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        json.dump(data, f, indent=2)
        f.write("\n")
    print(f"wrote {os.path.relpath(path, ROOT)}")


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
    s = sub.add_parser("flatpak-sources")
    s.add_argument("--app-source", choices=["dir", "git"], default="dir")
    s.add_argument("--tag")
    s.add_argument("--commit")
    c = sub.add_parser("check-metainfo")
    c.add_argument("version")
    a = ap.parse_args()
    if a.cmd == "fetch-jellyfin":
        arch = ARCH_ALIASES.get(a.arch, a.arch)
        if arch not in ARCHES:
            sys.exit(f"unsupported architecture {a.arch}")
        fetch_jellyfin(arch, a.dest)
    elif a.cmd == "flatpak-sources":
        flatpak_sources(a.app_source, a.tag, a.commit)
    else:
        check_metainfo(a.version)


if __name__ == "__main__":
    main()
