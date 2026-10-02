#!/usr/bin/env python3
"""Regenerate src-tauri/jellyfin-manifest.json.

repo.jellyfin.org only publishes MD5 hashes, so this script downloads every
pinned archive once, computes SHA-256 itself and records the result. The app
refuses any download whose SHA-256 does not match the manifest.

    python scripts/update_jellyfin_manifest.py --jellyfin 12.1 --ffmpeg 8.1.3-1
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import tempfile
import urllib.request
import zipfile
from pathlib import Path

REPO = "https://repo.jellyfin.org/files"
ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "src-tauri" / "jellyfin-manifest.json"

# platform key -> (server os dir, server arch dir, server ext, ffmpeg os dir, ffmpeg arch dir, ffmpeg file suffix)
PLATFORMS = {
    "windows-x86_64": ("windows", "amd64", "zip", "windows", "win64", "portable_win64-clang-gpl.zip"),
    "windows-aarch64": ("windows", "arm64", "zip", "windows", "winarm64", "portable_winarm64-clang-gpl.zip"),
    "linux-x86_64": ("linux", "amd64", "tar.xz", "linux", "amd64", "portable_linux64-gpl.tar.xz"),
    "linux-aarch64": ("linux", "arm64", "tar.xz", "linux", "arm64", "portable_linuxarm64-gpl.tar.xz"),
    "macos-x86_64": ("macos", "amd64", "tar.xz", "macos", "x86_64", "portable_mac64-gpl.tar.xz"),
    "macos-aarch64": ("macos", "arm64", "tar.xz", "macos", "arm64", "portable_macarm64-gpl.tar.xz"),
}


def listing(url: str) -> list[str]:
    with urllib.request.urlopen(url, timeout=30) as res:
        html = res.read().decode("utf-8", "replace")
    return re.findall(r"""href=["']([^"'?]+)["']""", html)


def hash_url(url: str, keep: bool) -> tuple[str, int, Path | None]:
    sha = hashlib.sha256()
    size = 0
    tmp = None
    out = None
    if keep:
        fd, name = tempfile.mkstemp(suffix=Path(url).suffix)
        tmp = Path(name)
        out = os.fdopen(fd, "wb")
    with urllib.request.urlopen(url, timeout=60) as res:
        while chunk := res.read(1 << 20):
            sha.update(chunk)
            size += len(chunk)
            if out:
                out.write(chunk)
    if out:
        out.close()
    return sha.hexdigest(), size, tmp


def resolve(base: str, pattern: str) -> str:
    names = [n for n in listing(base) if re.fullmatch(pattern, n)]
    if len(names) != 1:
        sys.exit(f"expected exactly one match for {pattern} in {base}, got {names}")
    return base + names[0]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--jellyfin", required=True, help="server version, e.g. 12.1")
    ap.add_argument("--ffmpeg", required=True, help="jellyfin-ffmpeg version, e.g. 8.1.3-1")
    ap.add_argument("--only", help="comma separated platform keys")
    args = ap.parse_args()

    major = args.ffmpeg.split(".")[0]
    platforms = PLATFORMS
    if args.only:
        platforms = {k: v for k, v in PLATFORMS.items() if k in args.only.split(",")}

    manifest = {"jellyfin": args.jellyfin, "ffmpeg": args.ffmpeg, "platforms": {}}
    if OUT.exists() and args.only:
        manifest = json.loads(OUT.read_text())
        manifest["platforms"] = {k: v for k, v in manifest["platforms"].items() if k not in platforms}

    for key, (sos, sarch, sext, fos, farch, fsuffix) in platforms.items():
        server_base = f"{REPO}/server/{sos}/stable/v{args.jellyfin}/{sarch}/"
        server_url = resolve(server_base, rf"jellyfin_{re.escape(args.jellyfin)}-{sarch}\.{re.escape(sext)}")
        print(f"[{key}] {server_url}", flush=True)
        sha, size, tmp = hash_url(server_url, keep=sext == "zip")
        entry = {"server": {"url": server_url, "sha256": sha, "size": size, "format": sext}}

        bundled = None
        if tmp:
            with zipfile.ZipFile(tmp) as z:
                for name in z.namelist():
                    if re.search(r"(^|/)ffmpeg\.exe$", name, re.I):
                        bundled = name.split("/", 1)[1] if name.startswith("jellyfin/") else name
                        break
            tmp.unlink()
        if bundled:
            entry["bundled_ffmpeg"] = bundled
            print(f"[{key}] ffmpeg bundled at {bundled}", flush=True)
        else:
            ffmpeg_base = f"{REPO}/ffmpeg/{fos}/{major}.x/{args.ffmpeg}/{farch}/"
            ffmpeg_url = resolve(ffmpeg_base, rf"jellyfin-ffmpeg_{re.escape(args.ffmpeg)}_{re.escape(fsuffix)}")
            print(f"[{key}] {ffmpeg_url}", flush=True)
            fsha, fsize, _ = hash_url(ffmpeg_url, keep=False)
            fext = "zip" if ffmpeg_url.endswith(".zip") else "tar.xz"
            entry["ffmpeg"] = {"url": ffmpeg_url, "sha256": fsha, "size": fsize, "format": fext}
        manifest["platforms"][key] = entry
        OUT.write_text(json.dumps(manifest, indent=2) + "\n")

    manifest["platforms"] = dict(sorted(manifest["platforms"].items()))
    OUT.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
