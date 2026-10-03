#!/usr/bin/env python3
"""
Writes latest.json, the manifest the app's updater (tauri-plugin-updater) reads from the latest GitHub release.

  python scripts/updater_manifest.py --dir release --version 1.1.0 --tag v1.1.0 --repo Yarmiplay/YarmiplayServerTV

Every installer in --dir needs its .sig next to it (made with `tauri signer sign`). Platform keys are
`{os}-{arch}-{installer}` plus a plain `{os}-{arch}` fallback; the app looks for its own installer type first,
so an .msi install updates from the .msi and a per-user setup.exe install from the setup.exe. The Release
workflow runs this. Standard library only.
"""
from __future__ import annotations

import argparse
import datetime
import json
import os
import sys
import urllib.parse

# File name suffix -> platform keys it serves.
TARGETS = [
    ("-setup.exe", ["windows-x86_64-nsis", "windows-x86_64"]),
    (".msi", ["windows-x86_64-msi"]),
    (".app.tar.gz", ["darwin-aarch64-app", "darwin-x86_64-app", "darwin-aarch64", "darwin-x86_64"]),
    (".AppImage", ["linux-x86_64-appimage", "linux-x86_64"]),
    (".deb", ["linux-x86_64-deb"]),
]


def build_manifest(folder, version, tag, repo, now=None):
    platforms = {}
    for name in sorted(os.listdir(folder)):
        keys = next((k for suffix, k in TARGETS if name.endswith(suffix)), None)
        if keys is None:
            continue
        sig_path = os.path.join(folder, name + ".sig")
        if not os.path.isfile(sig_path):
            raise SystemExit(f"{name} has no signature ({name}.sig)")
        with open(sig_path, encoding="utf-8") as f:
            signature = f.read().strip()
        url = f"https://github.com/{repo}/releases/download/{tag}/{urllib.parse.quote(name)}"
        for key in keys:
            if key in platforms:
                raise SystemExit(f"two files for {key}: {name} and {platforms[key]['url']}")
            platforms[key] = {"signature": signature, "url": url}
    if not platforms:
        raise SystemExit(f"no installers in {folder}")
    now = now or datetime.datetime.now(datetime.timezone.utc)
    return {
        "version": version,
        "notes": f"See https://github.com/{repo}/releases/tag/{tag}",
        "pub_date": now.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "platforms": platforms,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True, help="folder with the signed installers and their .sig files")
    ap.add_argument("--version", required=True)
    ap.add_argument("--tag", required=True)
    ap.add_argument("--repo", required=True, help="owner/name")
    ap.add_argument("--out", default=None, help="defaults to DIR/latest.json")
    a = ap.parse_args()
    manifest = build_manifest(a.dir, a.version, a.tag, a.repo)
    out = a.out or os.path.join(a.dir, "latest.json")
    with open(out, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2)
    print(f"wrote {out}: {', '.join(sorted(manifest['platforms']))}", file=sys.stderr)


if __name__ == "__main__":
    main()
