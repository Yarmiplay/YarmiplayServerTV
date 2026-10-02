#!/usr/bin/env python3
"""
Builds the YarmiplayServerTV download page: one card per desktop OS, each with its downloads and install steps.

  python scripts/download_site.py --dist dist --out _site [--version 0.1.0]

Files in --dist are sorted onto platforms by extension (.msi/.exe: Windows, .dmg: macOS, .deb/.AppImage: Linux)
and copied under stable names such as YarmiplayServerTV.msi, so links keep working across builds. Platforms
without a file show how to build from source. The Pages workflow publishes the result. Standard library only.
"""
from __future__ import annotations

import argparse
import datetime
import hashlib
import html
import json
import os
import shutil
import sys
from dataclasses import dataclass

NAME = "YarmiplayServerTV"
REPO_URL = "https://github.com/Yarmiplay/YarmiplayServerTV"
CLIENT_URL = "https://yarmiplay.github.io/YarmiplayTV/"
CLIENT_REPO = "https://github.com/Yarmiplay/YarmiplayTV"
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Lower-case extension -> platform and button label.
EXTENSIONS = {
    ".msi": ("windows", "Installer (.msi)"),
    ".exe": ("windows", "Setup (.exe)"),
    ".dmg": ("macos", "Disk image (.dmg)"),
    ".deb": ("linux", "Debian / Ubuntu (.deb)"),
    ".appimage": ("linux", "AppImage"),
}


@dataclass
class Download:
    href: str
    label: str
    size: int
    sha256: str | None = None


@dataclass
class Platform:
    key: str
    title: str
    blurb: str
    steps: list[str]
    uninstall: str


BUILD_FROM_SOURCE = "git clone {repo}\ncd YarmiplayServerTV\nnpm install\nnpx tauri build".format(repo=REPO_URL)

WINDOWS_UNSIGNED = ("Run the installer. This build isn't code-signed, so Windows SmartScreen may warn you: "
                    "choose <b>More info</b>, then <b>Run anyway</b>.")
WINDOWS_SIGNED = "Run the installer (code-signed by SignPath Foundation)."

PLATFORMS = [
    Platform("windows", "Windows", "Windows 10 or 11, 64-bit.", [
        WINDOWS_UNSIGNED,
        "YarmiplayServerTV starts in the system tray. Click the tray icon to open the control panel.",
    ], "Quit the app from the tray, then remove it in <b>Settings &gt; Apps</b>."),
    Platform("macos", "macOS", "macOS 11 or newer, Apple Silicon and Intel.", [
        "Open the disk image and drag YarmiplayServerTV to Applications.",
        "The app isn't notarized yet: the first time, right-click it in Applications and choose <b>Open</b>, "
        "then <b>Open</b> again. If macOS says the app is damaged, run "
        "<code>xattr -cr /Applications/YarmiplayServerTV.app</code> in Terminal.",
    ], "Quit the app from the menu bar, then move it from Applications to the Trash."),
    Platform("linux", "Linux", "64-bit, with a desktop that shows tray icons.", [
        "Install the .deb with <code>sudo apt install ./YarmiplayServerTV.deb</code>, or mark the AppImage "
        "executable (<code>chmod +x YarmiplayServerTV.AppImage</code>) and run it.",
        "On GNOME, the AppIndicator extension is needed to see the tray icon.",
    ], "Quit the app from the tray, then <code>sudo apt remove yarmiplay-server-tv</code>, or delete the AppImage."),
]


def sha256_of(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(1 << 20):
            h.update(chunk)
    return h.hexdigest()


def fmt_size(n):
    return f"{n / 1e6:.1f} MB"


def render_page(downloads, version, built, windows_signed=False):
    """downloads: {platform key: [Download]}. Returns the page as a str."""
    cards = []
    for p in PLATFORMS:
        files = downloads.get(p.key, [])
        if files:
            buttons = "".join(
                f'<a class="btn" href="{html.escape(d.href)}" download>{html.escape(d.label)}'
                f'<small>{fmt_size(d.size)}</small></a>' for d in files)
            steps = "".join(f"<li>{WINDOWS_SIGNED if windows_signed and s == WINDOWS_UNSIGNED else s}</li>"
                            for s in p.steps)
            sums = "".join(f"<div>{html.escape(d.href)}<br><code>{d.sha256}</code></div>" for d in files if d.sha256)
            body = (f'<div class="dl">{buttons}</div><ol>{steps}</ol>'
                    f'<p class="uninstall"><b>Uninstall:</b> {p.uninstall} Settings and Jellyfin data stay in your '
                    f'<a href="{REPO_URL}#uninstalling">app-data folder</a>.</p>'
                    + (f"<details><summary>SHA-256</summary>{sums}</details>" if sums else ""))
        else:
            body = (f'<p class="none">No package for this platform yet. Build it from source '
                    f'(Node.js 22 and Rust):</p><pre>{html.escape(BUILD_FROM_SOURCE)}</pre>')
        cards.append(f'<section class="card" data-platform="{p.key}"><span class="badge">For this computer</span>'
                     f'<h2>{p.title}</h2><p class="blurb">{p.blurb}</p>{body}</section>')

    meta = f"Version {html.escape(version)} &middot; built {html.escape(built)}"
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{NAME} downloads</title>
<meta name="description" content="Host a Syncplay server and a Jellyfin server for YarmiplayTV from your Windows, macOS or Linux computer.">
<link rel="icon" href="logo.svg" type="image/svg+xml">
<style>
 :root {{ --bg:#0E1116; --card:#171B22; --card-high:#212733; --line:#262C36; --text:#E8ECF2; --muted:#9AA4B2; --accent:#3DA5F4; --on-accent:#04121F; }}
 * {{ box-sizing:border-box; }}
 body {{ margin:0; background:var(--bg); color:var(--text); font:16px/1.5 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif; }}
 main {{ max-width:1100px; margin:0 auto; padding:6vh 4vw 4vh; }}
 header {{ text-align:center; margin-bottom:2.5em; }}
 header img {{ width:96px; height:96px; }}
 h1 {{ font-size:clamp(2.2em,6vw,3.4em); margin:.1em 0 0; letter-spacing:-.02em; }}
 h1 span {{ color:var(--accent); }}
 header p {{ color:var(--muted); font-size:1.15em; margin:.4em auto; max-width:46em; }}
 .meta {{ font-size:.95em; }}
 a {{ color:var(--accent); }}
 .features {{ display:flex; flex-wrap:wrap; justify-content:center; gap:.5em; margin:1.2em 0 0; padding:0; list-style:none; }}
 .features li {{ background:var(--card-high); color:var(--muted); border-radius:99px; padding:.2em .9em; font-size:.92em; }}
 .grid {{ display:grid; grid-template-columns:repeat(auto-fit,minmax(300px,1fr)); gap:1.2em; }}
 .card {{ background:var(--card); border:1px solid var(--line); border-radius:16px; padding:1.4em 1.5em; position:relative; }}
 .card.here {{ border-color:var(--accent); box-shadow:0 0 0 1px var(--accent); }}
 .badge {{ display:none; position:absolute; top:-.75em; left:1.2em; background:var(--accent); color:var(--on-accent);
          font-size:.8em; font-weight:700; padding:.1em .7em; border-radius:99px; }}
 .card.here .badge {{ display:block; }}
 h2 {{ margin:0 0 .2em; font-size:1.35em; }}
 .blurb, ol, details, .none, .uninstall {{ color:var(--muted); }}
 .uninstall {{ font-size:.85em; margin:.6em 0 0; }}
 .blurb {{ margin:0 0 1em; }}
 .dl {{ display:flex; flex-wrap:wrap; gap:.6em; margin-bottom:.8em; }}
 .btn {{ display:inline-flex; flex-direction:column; padding:.6em 1.2em; border-radius:10px; background:var(--accent);
        color:var(--on-accent); text-decoration:none; font-weight:700; font-size:1.05em; }}
 .btn small {{ font-weight:500; opacity:.8; font-size:.8em; }}
 .btn:hover {{ filter:brightness(1.1); }}
 a:focus-visible, summary:focus-visible {{ outline:3px solid var(--text); outline-offset:3px; }}
 ol {{ padding-left:1.3em; margin:.4em 0; }}
 code, pre {{ font-family:ui-monospace,Consolas,monospace; font-size:.9em; }}
 code {{ background:var(--bg); padding:.1em .35em; border-radius:4px; color:var(--text); overflow-wrap:anywhere; }}
 pre {{ background:var(--bg); padding:.7em 1em; border-radius:8px; overflow-x:auto; color:var(--text); margin:.3em 0 0; }}
 .none {{ margin:0 0 .3em; }}
 details {{ margin-top:.8em; font-size:.85em; }} details div {{ margin-top:.5em; }}
 summary {{ cursor:pointer; }}
 .client {{ margin-top:1.6em; text-align:center; background:var(--card); border:1px solid var(--line); border-radius:16px; padding:1.2em 1.5em; }}
 .client p {{ color:var(--muted); margin:.3em 0 0; }}
 footer {{ text-align:center; color:var(--muted); margin-top:3em; font-size:.95em; }}
 @media (min-width:1600px) {{ body {{ font-size:20px; }} main {{ max-width:1500px; }} }}
</style></head>
<body><main>
<header>
 <img src="logo.svg" alt="">
 <h1>Yarmiplay<span>ServerTV</span></h1>
 <p>Host your own Syncplay server and Jellyfin server from the system tray, for YarmiplayTV and any Syncplay client.</p>
 <ul class="features"><li>Syncplay 1.7 server</li><li>Jellyfin, installed for you</li><li>Optional UPnP port forwarding</li><li>Free HTTPS with DuckDNS + Let's Encrypt</li></ul>
 <p class="meta">{meta}</p>
</header>
<div class="grid">
{chr(10).join(cards)}
</div>
<section class="client">
 <strong>Need the player?</strong>
 <p><a href="{CLIENT_URL}">Get YarmiplayTV</a> for Google TV, Android and desktop, then connect it to this server.</p>
</section>
<footer><a href="{REPO_URL}">Source</a> &middot; <a href="{REPO_URL}#readme">Setup guide</a> &middot; <a href="{CLIENT_REPO}">YarmiplayTV</a>
 &middot; <a href="{REPO_URL}#code-signing-policy">Code signing policy</a></footer>
</main>
<script>
(function () {{
  var ua = navigator.userAgent, key = null;
  if (/Android|iPhone|iPad|iPod/i.test(ua)) return;
  if (/Windows/.test(ua)) key = "windows";
  else if (/Macintosh|Mac OS X/.test(ua)) key = "macos";
  else if (/Linux|X11|CrOS/.test(ua)) key = "linux";
  var card = key && document.querySelector('[data-platform="' + key + '"]');
  if (!card) return;
  card.classList.add("here");
  card.parentNode.insertBefore(card, card.parentNode.firstChild);
}})();
</script>
</body></html>
"""


def build(dist, out, version, windows_signed=False):
    found = {}
    for name in sorted(os.listdir(dist)) if os.path.isdir(dist) else []:
        path = os.path.join(dist, name)
        ext = os.path.splitext(name)[1].lower()
        if not os.path.isfile(path) or ext not in EXTENSIONS:
            continue
        if ext in found:
            sys.exit(f"two {ext} files in {dist}: {found[ext]} and {name}")
        found[ext] = name

    if os.path.isdir(out):
        shutil.rmtree(out)
    os.makedirs(out)
    downloads = {}
    for ext in EXTENSIONS:
        if ext not in found:
            continue
        name = found[ext]
        src = os.path.join(dist, name)
        target = NAME + os.path.splitext(name)[1]
        shutil.copyfile(src, os.path.join(out, target))
        platform, label = EXTENSIONS[ext]
        downloads.setdefault(platform, []).append(Download(target, label, os.path.getsize(src), sha256_of(src)))
        print(f"  {name} -> {target}")

    shutil.copyfile(os.path.join(ROOT, "assets", "logo.svg"), os.path.join(out, "logo.svg"))
    built = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    with open(os.path.join(out, "index.html"), "w", encoding="utf-8") as f:
        f.write(render_page(downloads, version, built, windows_signed))
    open(os.path.join(out, ".nojekyll"), "w").close()
    print(f"wrote {out} ({sum(len(v) for v in downloads.values())} downloads)")


def app_version():
    with open(os.path.join(ROOT, "src-tauri", "tauri.conf.json"), encoding="utf-8") as f:
        return json.load(f).get("version", "dev")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dist", required=True, help="folder with the built installers")
    ap.add_argument("--out", required=True, help="site folder to (re)create")
    ap.add_argument("--version", default=None, help="defaults to the version in tauri.conf.json")
    ap.add_argument("--windows-signed", action="store_true", help="the Windows installers are code-signed")
    a = ap.parse_args()
    build(a.dist, a.out, a.version or app_version(), a.windows_signed)


if __name__ == "__main__":
    main()
