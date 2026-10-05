#!/usr/bin/env python3
"""
Sets the app version everywhere it is written down:

  python scripts/bump-version.py 1.3.0 [--note "What changed, one sentence."] [--date 2026-10-05]

tauri.conf.json, Cargo.toml, Cargo.lock, package.json, package-lock.json and a new <release> at the top of the
AppStream metainfo (the stores show its note; edit it afterwards for more than one paragraph). The release
workflow refuses a tag whose version is missing from any of them. packaging/aur/PKGBUILD is left alone: the
release workflow's aur job sets its version and checksum when it publishes.
"""
from __future__ import annotations

import argparse
import datetime
import os
import re
import sys
from xml.sax.saxutils import escape

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
METAINFO = os.path.join("packaging", "linux", "com.yarmiplay.servertv.metainfo.xml")


def edit(rel, pattern, repl, count=1):
    path = os.path.join(ROOT, rel)
    with open(path, encoding="utf-8", newline="") as f:
        text = f.read()
    new, n = re.subn(pattern, repl, text, count=count, flags=re.MULTILINE)
    if n != count:
        sys.exit(f"{rel}: expected {count} version entr{'y' if count == 1 else 'ies'}, found {n}")
    with open(path, "w", encoding="utf-8", newline="") as f:
        f.write(new)
    print(f"updated {rel}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("version")
    ap.add_argument("--note", default="Bug fixes and improvements.")
    ap.add_argument("--date", default=datetime.date.today().isoformat())
    a = ap.parse_args()
    if not re.fullmatch(r"\d+\.\d+\.\d+", a.version):
        sys.exit("the version must look like 1.3.0")
    v = a.version

    edit(os.path.join("src-tauri", "tauri.conf.json"), r'^(  "version": )"[^"]+"', rf'\g<1>"{v}"')
    edit("package.json", r'^(  "version": )"[^"]+"', rf'\g<1>"{v}"')
    # The root package appears twice: at the top and as packages[""].
    edit("package-lock.json", r'^(  "version": |      "version": )"[^"]+"(,\r?\n(?:      "license|  "lockfileVersion))',
         rf'\g<1>"{v}"\g<2>', count=2)
    edit(os.path.join("src-tauri", "Cargo.toml"), r'^(version = )"[^"]+"', rf'\g<1>"{v}"')
    edit(os.path.join("src-tauri", "Cargo.lock"), r'^(name = "yarmiplayservertv"\r?\nversion = )"[^"]+"',
         rf'\g<1>"{v}"')

    with open(os.path.join(ROOT, METAINFO), encoding="utf-8") as f:
        if f'<release version="{v}"' in f.read():
            print(f"{METAINFO} already has release {v}")
            return
    entry = (f'<releases>\n    <release version="{v}" date="{a.date}">\n      <description>\n'
             f"        <p>{escape(a.note)}</p>\n      </description>\n    </release>")
    edit(METAINFO, r"<releases>", entry.replace("\\", r"\\"))


if __name__ == "__main__":
    main()
