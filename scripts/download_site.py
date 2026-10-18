#!/usr/bin/env python3
"""
Builds the YarmiplayServerTV download page: what the app does, then one card per desktop OS, each with its
downloads and install steps.

  python scripts/download_site.py --dist dist --out _site [--version 0.1.0] [--site-url URL]

Files in --dist are sorted onto platforms by extension (.msi/.exe: Windows, .dmg: macOS, .deb/.AppImage: Linux)
and copied under stable names such as YarmiplayServerTV.msi, so links keep working across builds. Platforms
without a file or store listing show how to build from source. The Linux card's main button is the Snap Store.
<site>/privacy/ is docs/privacy.md (the Microsoft Store privacy policy) and
<site>/screenshots/ is docs/store/screenshots (the AppStream metainfo links there). With --site-url the pages carry canonical URLs and link-preview tags, and the site a
sitemap.xml and a robots.txt pointing to it for search engines. The Pages workflow publishes the result.
Standard library only.
"""
from __future__ import annotations

import argparse
import datetime
import hashlib
import html
import json
import os
import re
import shutil
import sys
from dataclasses import dataclass

NAME = "YarmiplayServerTV"
REPO_URL = "https://github.com/Yarmiplay/YarmiplayServerTV"
CLIENT_URL = "https://tv.yarmiplay.com/"
CLIENT_REPO = "https://github.com/Yarmiplay/YarmiplayTV"
HOME_URL = "https://yarmiplay.com/"
TITLE = f"{NAME}: host a Syncplay server and a Jellyfin server from your tray (Windows, macOS, Linux)"
DESCRIPTION = ("Host a Syncplay server and a Jellyfin server for YarmiplayTV from your Windows, macOS or Linux "
               "computer. Watch videos in sync with friends. Free and open source.")
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# Store listings: the platform's main button, with its files as smaller links below.
STORE_LINKS = {
    "windows": ("Microsoft Store", "https://apps.microsoft.com/detail/9P6B9C1KXFFQ"),
}


@dataclass
class LinuxStore:
    name: str
    url: str
    command: str
    remove: str


# The Linux card's install steps name each; the first is its main button.
LINUX_STORES = [
    LinuxStore("Snap Store", "https://snapcraft.io/yarmiplayservertv", "sudo snap install yarmiplayservertv",
               "sudo snap remove yarmiplayservertv"),
]

# Lower-case extension -> platform and button label. Without a store listing the first file is the platform's
# main button.
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

WINDOWS_UNSIGNED = ("Get it from the Microsoft Store (Jellyfin built in, updated by the Store), or run the "
                    "installer. The installer isn't code-signed, so Windows SmartScreen may warn you: choose "
                    "<b>More info</b>, then <b>Run anyway</b>.")
WINDOWS_SIGNED = ("Get it from the Microsoft Store (Jellyfin built in, updated by the Store), or run the "
                  "installer (code-signed by SignPath Foundation).")

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


def linux_store_step(stores):
    ways = "; ".join(f'<a href="{html.escape(s.url)}">{html.escape(s.name)}</a>: '
                     f'<code>{html.escape(s.command)}</code>' for s in stores)
    return f"From a store, which keeps it up to date: {ways}."


def fmt_size(n):
    return f"{n / 1e6:.1f} MB"


def page_url(site_url, path=""):
    return site_url.rstrip("/") + "/" + path


def search_head(title, description, site_url=None, path=""):
    """Description, plus canonical URL and link-preview tags when the site's public URL is known."""
    tags = [f'<meta name="description" content="{html.escape(description)}">']
    if site_url:
        url = html.escape(page_url(site_url, path))
        tags += [f'<link rel="canonical" href="{url}">',
                 '<meta property="og:type" content="website">',
                 f'<meta property="og:site_name" content="{NAME}">',
                 f'<meta property="og:title" content="{html.escape(title)}">',
                 f'<meta property="og:description" content="{html.escape(description)}">',
                 f'<meta property="og:url" content="{url}">',
                 '<meta name="twitter:card" content="summary">']
    return "\n".join(tags)


def app_json_ld(version, site_url):
    app = {"@context": "https://schema.org", "@type": "SoftwareApplication", "name": NAME,
           "description": DESCRIPTION, "operatingSystem": "Windows, macOS, Linux",
           "applicationCategory": "MultimediaApplication", "softwareVersion": version,
           "offers": {"@type": "Offer", "price": "0", "priceCurrency": "USD"}, "sameAs": [REPO_URL]}
    if site_url:
        app["url"] = page_url(site_url)
    return f'<script type="application/ld+json">{json.dumps(app)}</script>'


def sitemap(site_url, paths, day):
    urls = "".join(f"<url><loc>{html.escape(page_url(site_url, p))}</loc><lastmod>{day}</lastmod></url>\n"
                   for p in paths)
    return ('<?xml version="1.0" encoding="UTF-8"?>\n'
            f'<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n{urls}</urlset>\n')


# Material Icons (Apache-2.0): the paths of the few this page shows.
ICONS = {
    "desktop_windows": '<path d="M20 3H4c-1.1 0-2 .9-2 2v10c0 1.1.9 2 2 2h6v2H8v2h8v-2h-2v-2h6c1.1 0 2-.9 2-2V5c0-1.1-.9-2-2-2"/>',
    "laptop_mac": '<path d="M20 18c1.1 0 1.99-.9 1.99-2L22 5c0-1.1-.9-2-2-2H4c-1.1 0-2 .9-2 2v11c0 1.1.9 2 2 2H0c0 1.1.9 2 2 2h20c1.1 0 2-.9 2-2h-4zM4 5h16v11H4V5zm8 14c-.55 0-1-.45-1-1s.45-1 1-1 1 .45 1 1-.45 1-1 1z"/>',
    "terminal": '<path d="M20 4H4a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h16c1.1 0 2-.9 2-2V6a2 2 0 0 0-2-2zm0 14H4V8h16v10zm-2-1h-6v-2h6v2zM7.5 17l-1.41-1.41L8.67 13l-2.59-2.59L7.5 9l4 4-4 4z"/>',
    "groups": '<path d="M12 12.75c1.63 0 3.07.39 4.24.9 1.08.48 1.76 1.56 1.76 2.73V18H6v-1.61c0-1.18.68-2.26 1.76-2.73 1.17-.52 2.61-.91 4.24-.91zM4 13c1.1 0 2-.9 2-2s-.9-2-2-2-2 .9-2 2 .9 2 2 2zm1.13 1.1c-.37-.06-.74-.1-1.13-.1-.99 0-1.93.21-2.78.58A2.01 2.01 0 0 0 0 16.43V18h4.5v-1.61c0-.83.23-1.61.63-2.29zM20 13c1.1 0 2-.9 2-2s-.9-2-2-2-2 .9-2 2 .9 2 2 2zm4 3.43c0-.81-.48-1.53-1.22-1.85A6.95 6.95 0 0 0 20 14c-.39 0-.76.04-1.13.1.4.68.63 1.46.63 2.29V18H24v-1.57zM12 6c1.66 0 3 1.34 3 3s-1.34 3-3 3-3-1.34-3-3 1.34-3 3-3z"/>',
    "video_library": '<path d="M4 6H2v14c0 1.1.9 2 2 2h14v-2H4V6zm16-4H8c-1.1 0-2 .9-2 2v12c0 1.1.9 2 2 2h12c1.1 0 2-.9 2-2V4c0-1.1-.9-2-2-2zm-8 12.5v-9l6 4.5-6 4.5z"/>',
    "live_tv": '<path d="M21 6h-7.59l3.29-3.29L16 2l-4 4-4-4-.71.71L10.59 6H3a2 2 0 0 0-2 2v12c0 1.1.9 2 2 2h18c1.1 0 2-.9 2-2V8a2 2 0 0 0-2-2zm0 14H3V8h18v12zM9 10v8l7-4z"/>',
    "swap_horiz": '<path d="M6.99 11 3 15l3.99 4v-3H14v-2H6.99v-3zM21 9l-3.99-4v3H10v2h7.01v3L21 9z"/>',
    "lock": '<path d="M18 8h-1V6c0-2.76-2.24-5-5-5S7 3.24 7 6v2H6c-1.1 0-2 .9-2 2v10c0 1.1.9 2 2 2h12c1.1 0 2-.9 2-2V10c0-1.1-.9-2-2-2zm-6 9c-1.1 0-2-.9-2-2s.9-2 2-2 2 .9 2 2-.9 2-2 2zm3.1-9H8.9V6c0-1.71 1.39-3.1 3.1-3.1 1.71 0 3.1 1.39 3.1 3.1v2z"/>',
    "verified_user": '<path d="M12 1 3 5v6c0 5.55 3.84 10.74 9 12 5.16-1.26 9-6.45 9-12V5l-9-4zm-2 16-4-4 1.41-1.41L10 14.17l6.59-6.59L18 9l-8 8z"/>',
    "download": '<path d="M5 20h14v-2H5v2zM19 9h-4V3H9v6H5l7 7 7-7z"/>',
    "tv": '<path d="M21 3H3c-1.1 0-2 .9-2 2v12c0 1.1.9 2 2 2h5v2h8v-2h5c1.1 0 1.99-.9 1.99-2L23 5c0-1.1-.9-2-2-2zm0 14H3V5h18v12z"/>',
}


def icon(name):
    return f'<svg viewBox="0 0 24 24" aria-hidden="true">{ICONS[name]}</svg>'


PLATFORM_ICONS = {"windows": "desktop_windows", "macos": "laptop_mac", "linux": "terminal"}


def features():
    """What the server does, under the heading: (icon, title, text)."""
    return (
        ("groups", "A Syncplay server",
         f"Rooms, passwords, chat, readiness and shared playlists, for YarmiplayTV and any Syncplay "
         f"{html.escape(syncplay_version())} client."),
        ("video_library", "Jellyfin, set up for you",
         "Downloaded and configured from the control panel, no setup wizard, and shared with the people in your "
         "rooms in one tap."),
        ("live_tv", "Your Plex too",
         "Manage your own Plex Media Server's libraries and share it with the people on your Syncplay server."),
        ("swap_horiz", "File relay",
         "When one person has the file, everyone in the room can play it, streamed through your server."),
        ("lock", "Reachable from anywhere",
         "Optional port forwarding, and free HTTPS with a DuckDNS name and a Let's Encrypt certificate that "
         "renews itself."),
        ("verified_user", "Your rules",
         "Admit only the devices you approve, and the control panel only opens on your own computer."),
    )


# The heading's picture: the control panel with both servers running, drawn rather than a screenshot so it stays
# sharp at any size.
ILLUSTRATION = (
    '<svg class="art" viewBox="0 0 560 400" role="img" '
    'aria-label="The control panel, with the Syncplay and Jellyfin servers running">'
    '<defs>'
    '<radialGradient id="art-glow"><stop offset="0" stop-color="#3DA5F4" stop-opacity=".22"/>'
    '<stop offset="1" stop-color="#3DA5F4" stop-opacity="0"/></radialGradient>'
    '<clipPath id="art-win"><rect x="10" y="30" width="470" height="320" rx="14"/></clipPath>'
    '<linearGradient id="art-p1" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#2D5F93"/>'
    '<stop offset="1" stop-color="#0F1C33"/></linearGradient>'
    '<linearGradient id="art-p2" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#F0A868"/>'
    '<stop offset="1" stop-color="#7A3E2A"/></linearGradient>'
    '<linearGradient id="art-p3" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#4CC38A"/>'
    '<stop offset="1" stop-color="#1B4A3A"/></linearGradient>'
    '</defs>'
    '<ellipse cx="270" cy="200" rx="280" ry="200" fill="url(#art-glow)"/>'
    # The window.
    '<rect x="10" y="30" width="470" height="320" rx="14" fill="#12161D"/>'
    '<g clip-path="url(#art-win)">'
    '<rect x="10" y="30" width="470" height="28" fill="#0A0D12"/>'
    '<text x="44" y="48" font-size="10" fill="#9AA4B2">YarmiplayServerTV</text>'
    '<path d="M24 38V50L33 44Z" fill="#3DA5F4"/>'
    '<rect x="10" y="58" width="112" height="292" fill="#151A21"/>'
    '<rect x="20" y="70" width="92" height="26" rx="7" fill="#212733"/>'
    '<text x="30" y="87" font-size="10.5" font-weight="600" fill="#E8ECF2">Dashboard</text>'
    '<text x="30" y="121" font-size="10.5" fill="#9AA4B2">Syncplay</text><circle cx="104" cy="117" r="3.5" fill="#4CC38A"/>'
    '<text x="30" y="149" font-size="10.5" fill="#9AA4B2">Jellyfin</text><circle cx="104" cy="145" r="3.5" fill="#4CC38A"/>'
    '<text x="30" y="177" font-size="10.5" fill="#9AA4B2">Network</text>'
    '<text x="30" y="205" font-size="10.5" fill="#9AA4B2">Logs</text>'
    '<text x="140" y="90" font-size="18" font-weight="700" fill="#E8ECF2">Dashboard</text>'
    '<text x="140" y="108" font-size="9" fill="#9AA4B2">Your servers, running from the tray</text>'
    # Syncplay.
    '<rect x="140" y="122" width="160" height="210" rx="12" fill="#171B22" stroke="#262C36"/>'
    '<text x="154" y="146" font-size="12" font-weight="700" fill="#E8ECF2">Syncplay</text>'
    '<rect x="234" y="134" width="56" height="18" rx="9" fill="#173327"/><circle cx="244" cy="143" r="3" fill="#4CC38A"/>'
    '<text x="251" y="146.5" font-size="8.5" font-weight="600" fill="#4CC38A">Running</text>'
    '<text x="154" y="174" font-size="10" font-weight="600" fill="#E8ECF2">Syncplay server</text>'
    '<text x="154" y="187" font-size="8.5" fill="#9AA4B2">Port 8999</text>'
    '<rect x="258" y="170" width="30" height="16" rx="8" fill="#3DA5F4"/><circle cx="280" cy="178" r="6" fill="#0A0D12"/>'
    '<text x="154" y="210" font-size="8.5" fill="#9AA4B2">Over the internet</text>'
    '<rect x="154" y="217" width="134" height="22" rx="5" fill="#0E1116"/>'
    '<text x="162" y="231.5" font-size="8.5" font-family="ui-monospace,Consolas,monospace" fill="#E8ECF2">'
    'myhome.duckdns.org</text>'
    '<text x="154" y="262" font-size="8.5" fill="#9AA4B2">Rooms</text>'
    '<circle cx="164" cy="285" r="9" fill="#3DA5F4" stroke="#171B22" stroke-width="2"/>'
    '<circle cx="177" cy="285" r="9" fill="#F0A868" stroke="#171B22" stroke-width="2"/>'
    '<circle cx="190" cy="285" r="9" fill="#4CC38A" stroke="#171B22" stroke-width="2"/>'
    '<text x="206" y="283" font-size="9.5" font-weight="600" fill="#E8ECF2">Movie night</text>'
    '<text x="206" y="295" font-size="8" fill="#9AA4B2">3 watching</text>'
    # Jellyfin.
    '<rect x="310" y="122" width="160" height="210" rx="12" fill="#171B22" stroke="#262C36"/>'
    '<text x="324" y="146" font-size="12" font-weight="700" fill="#E8ECF2">Jellyfin</text>'
    '<rect x="404" y="134" width="56" height="18" rx="9" fill="#173327"/><circle cx="414" cy="143" r="3" fill="#4CC38A"/>'
    '<text x="421" y="146.5" font-size="8.5" font-weight="600" fill="#4CC38A">Running</text>'
    '<text x="324" y="174" font-size="10" font-weight="600" fill="#E8ECF2">Jellyfin server</text>'
    '<text x="324" y="187" font-size="8.5" fill="#9AA4B2">Shared with your rooms</text>'
    '<rect x="428" y="170" width="30" height="16" rx="8" fill="#3DA5F4"/><circle cx="450" cy="178" r="6" fill="#0A0D12"/>'
    '<text x="324" y="210" font-size="8.5" fill="#9AA4B2">Libraries</text>'
    '<rect x="324" y="217" width="40" height="58" rx="5" fill="url(#art-p1)"/>'
    '<rect x="370" y="217" width="40" height="58" rx="5" fill="url(#art-p2)"/>'
    '<rect x="416" y="217" width="40" height="58" rx="5" fill="url(#art-p3)"/>'
    '<text x="324" y="290" font-size="8.5" fill="#E8ECF2">Movies · Shows · Anime</text>'
    '</g>'
    '<rect x="10" y="30" width="470" height="320" rx="14" fill="none" stroke="#2A313C" stroke-width="2"/>'
    # Friends in sync, and HTTPS.
    '<g transform="translate(372 4)"><rect width="176" height="46" rx="14" fill="#151A21" stroke="#2A313C" stroke-width="1.5"/>'
    '<circle cx="24" cy="23" r="14" fill="#3DA5F4"/>'
    f'<g transform="translate(15 14) scale(.75)" fill="#06121F">{ICONS["groups"]}</g>'
    '<text x="46" y="21" font-size="10.5" font-weight="700" fill="#E8ECF2">3 friends in sync</text>'
    '<text x="46" y="34" font-size="8.5" fill="#9AA4B2">Movie night · 24:18</text></g>'
    '<g transform="translate(318 330)"><rect width="200" height="54" rx="14" fill="#151A21" stroke="#2A313C" stroke-width="1.5"/>'
    '<circle cx="27" cy="27" r="15" fill="#173327"/>'
    f'<g transform="translate(17.5 17.5) scale(.8)" fill="#4CC38A">{ICONS["lock"]}</g>'
    '<text x="50" y="24" font-size="10.5" font-weight="700" fill="#E8ECF2">HTTPS ready</text>'
    '<text x="50" y="38" font-size="8.5" fill="#9AA4B2">Let\'s Encrypt · renews itself</text></g>'
    '</svg>'
)

# The look the main page and the privacy page share: the control panel's colours, and the bar along the top.
BASE_CSS = """ :root { --bg:#0E1116; --surface:#151A21; --surface2:#1E242E; --line:#262C36; --text:#E8ECF2; --soft:#C4C9D1;
         --muted:#9AA4B2; --accent:#3DA5F4; --on-accent:#04121F; --tint:rgba(61,165,244,.13); }
 * { box-sizing:border-box; }
 html { scroll-behavior:smooth; }
 body { margin:0; color:var(--text); font:16px/1.55 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;
        background:radial-gradient(1100px 620px at 78% -8%,rgba(61,165,244,.14),transparent 70%) no-repeat,var(--bg); }
 .wrap { max-width:1120px; margin:0 auto; padding:0 24px; }
 a { color:var(--accent); }
 .top { display:flex; align-items:center; justify-content:space-between; gap:1em; padding:1.1em 0; }
 .brand { display:inline-flex; align-items:center; gap:.5em; color:var(--text); text-decoration:none; font-weight:700;
          font-size:1.15em; letter-spacing:-.01em; }
 .brand img { width:2.1em; height:2.1em; flex:none; }
 .brand span span { color:var(--accent); }
 .top nav { display:flex; flex-wrap:wrap; gap:.2em; }
 .top nav a { color:var(--soft); text-decoration:none; padding:.4em .75em; border-radius:8px; font-weight:500; }
 .top nav a:hover { color:var(--text); background:var(--surface); }
 .top nav a.active { color:var(--accent); background:var(--tint); font-weight:600; }
 code, pre { font-family:ui-monospace,Consolas,monospace; font-size:.9em; }
 code { background:var(--bg); padding:.1em .35em; border-radius:4px; color:var(--text); overflow-wrap:anywhere; }
 a:focus-visible, summary:focus-visible { outline:3px solid var(--text); outline-offset:3px; border-radius:6px; }
 @media (max-width:620px) { .wrap { padding:0 16px; } .top nav a:not(:first-child) { display:none; } }
 @media (min-width:1600px) { body { font-size:20px; } .wrap { max-width:1500px; } }"""

BRAND = f'<img src="{{root}}logo.svg" alt=""><span>Yarmiplay<span>ServerTV</span></span>'


def brand(root=""):
    return BRAND.replace("{root}", root)


def render_page(downloads, version, built, windows_signed=False, privacy=False, site_url=None,
                linux_stores=LINUX_STORES):
    """downloads: {platform key: [Download]}. Returns the page as a str."""
    cards = []
    for p in PLATFORMS:
        files = downloads.get(p.key, [])
        store = STORE_LINKS.get(p.key)
        steps, uninstall = list(p.steps), p.uninstall
        if p.key == "linux" and linux_stores:
            store = (linux_stores[0].name, linux_stores[0].url)
            steps.insert(0, linux_store_step(linux_stores))
            uninstall += " Store copies: " + ", ".join(f"<code>{html.escape(s.remove)}</code>"
                                                       for s in linux_stores) + "."
        if files or store:
            links = [(html.escape(store[1]), "", html.escape(store[0]), "Updates automatically")] if store else []
            links += [(html.escape(d.href), " download", html.escape(d.label), fmt_size(d.size)) for d in files]
            (href, attr, label, note), rest = links[0], links[1:]
            buttons = f'<a class="btn" href="{href}"{attr}>{label}<small>{note}</small></a>'
            if rest:
                buttons += '<p class="more">' + "".join(
                    f'<a href="{href}"{attr}>{label} <small>{note}</small></a>' for href, attr, label, note in rest
                ) + "</p>"
            steps = "".join(f"<li>{WINDOWS_SIGNED if windows_signed and s == WINDOWS_UNSIGNED else s}</li>"
                            for s in steps)
            sums = "".join(f"<div>{html.escape(d.href)}<br><code>{d.sha256}</code></div>" for d in files if d.sha256)
            body = (f'<div class="dl">{buttons}</div>'
                    f'<details><summary>Installation guide</summary><ol>{steps}</ol></details>'
                    f'<details><summary>Uninstall guide</summary><p class="uninstall">{uninstall} Settings and '
                    f'Jellyfin data stay in your <a href="setup/#where-things-are-stored">app-data folder</a>'
                    + (f' (the {html.escape(store[0])} copy removes them with the app)'
                       if p.key in STORE_LINKS else '') + '.</p></details>'
                    + (f'<details class="sums"><summary>SHA-256</summary>{sums}</details>' if sums else ""))
        else:
            body = (f'<p class="none">No package for this platform yet. Build it from source '
                    f'(Node.js 22 and Rust):</p><pre>{html.escape(BUILD_FROM_SOURCE)}</pre>')
        cards.append(f'<section class="card" data-platform="{p.key}"><span class="badge">For this computer</span>'
                     f'<div class="head"><span class="pi">{icon(PLATFORM_ICONS[p.key])}</span><h3>{p.title}</h3></div>'
                     f'<p class="blurb">{p.blurb}</p>{body}</section>')

    feature_cards = "".join(f'<div class="feature"><span class="fi">{icon(name)}</span><h3>{title}</h3><p>{text}</p>'
                            '</div>' for name, title, text in features())
    columns = (
        ("Help", [("setup/", "Setup guide"), ("internet-access/", "Internet access"),
                  (f"{REPO_URL}/issues", "Report a problem")]),
        ("Project", [(HOME_URL, "yarmiplay.com"), (CLIENT_URL, "YarmiplayTV"), (REPO_URL, "Source code")]),
        ("Legal", ([("privacy/", "Privacy")] if privacy else [])
         + ([(f"{REPO_URL}#code-signing-policy", "Code signing policy")] if windows_signed else [])),
    )
    footer_columns = "".join(
        f'<div><h4>{title}</h4><ul>'
        + "".join(f'<li><a href="{html.escape(href)}">{html.escape(label)}</a></li>' for href, label in links)
        + "</ul></div>" for title, links in columns if links)
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="theme-color" content="#0E1116">
<title>{html.escape(TITLE)}</title>
{search_head(TITLE, DESCRIPTION, site_url)}
{app_json_ld(version, site_url)}
<link rel="icon" href="logo.svg" type="image/svg+xml">
<style>
{BASE_CSS}
 .hero {{ display:grid; grid-template-columns:1fr 1fr; align-items:center; gap:3em; padding:2.5em 0 1em; }}
 .eyebrow {{ display:inline-block; color:var(--accent); background:var(--tint); font-weight:600; font-size:.88em;
            padding:.3em .9em; border-radius:99px; margin:0 0 1.2em; }}
 h1 {{ font-size:clamp(2.4em,5.2vw,3.8em); line-height:1.06; letter-spacing:-.03em; margin:0 0 .45em; }}
 h1 span {{ color:var(--accent); }}
 .lede {{ color:var(--soft); font-size:1.18em; margin:0 0 1.7em; max-width:32em; }}
 .cta {{ display:flex; flex-wrap:wrap; gap:.75em; }}
 .hero-meta {{ color:var(--muted); font-size:.9em; margin:1.3em 0 0; }}
 .art {{ width:100%; height:auto; display:block; }}
 .section {{ padding:3.5em 0 0; }}
 h2 {{ font-size:clamp(1.6em,3vw,2.1em); letter-spacing:-.02em; margin:0 0 .25em; }}
 .sub {{ color:var(--muted); margin:0 0 1.6em; font-size:1.05em; }}
 .features {{ display:grid; grid-template-columns:repeat(3,1fr); gap:1em; }}
 .feature {{ background:var(--surface); border:1px solid var(--line); border-radius:18px; padding:1.4em; }}
 .fi {{ width:2.75em; height:2.75em; border-radius:.75em; background:var(--tint); color:var(--accent); display:flex;
       align-items:center; justify-content:center; margin-bottom:.9em; flex:none; }}
 .fi svg {{ width:1.5em; height:1.5em; fill:currentColor; }}
 .feature h3 {{ margin:0 0 .3em; font-size:1.1em; }}
 .feature p {{ margin:0; color:var(--muted); font-size:.95em; }}
 .grid {{ display:grid; grid-template-columns:repeat(3,1fr); gap:1em; }}
 .card {{ background:var(--surface); border:1px solid var(--line); border-radius:18px; padding:1.4em; position:relative;
         display:flex; flex-direction:column; }}
 .card.here {{ border-color:var(--accent); box-shadow:0 0 0 1px var(--accent),0 16px 48px -16px rgba(61,165,244,.45); }}
 .badge {{ display:none; position:absolute; top:-.8em; left:1.2em; background:var(--accent); color:var(--on-accent);
          font-size:.78em; font-weight:700; padding:.15em .75em; border-radius:99px; }}
 .card.here .badge {{ display:block; }}
 .head {{ display:flex; align-items:center; gap:.75em; margin-bottom:.6em; }}
 .pi {{ width:2.5em; height:2.5em; border-radius:.7em; background:var(--surface2); color:var(--text); display:flex;
       align-items:center; justify-content:center; flex:none; }}
 .here .pi {{ background:var(--tint); color:var(--accent); }}
 .pi svg {{ width:1.4em; height:1.4em; fill:currentColor; }}
 h3 {{ margin:0; font-size:1.2em; }}
 .blurb {{ color:var(--muted); margin:0 0 1.2em; font-size:.95em; }}
 .dl {{ display:flex; flex-direction:column; gap:.6em; margin:0 0 1em; }}
 .btn {{ display:flex; flex-direction:column; padding:.65em 1.1em; border-radius:12px; background:var(--tint);
        color:var(--accent); text-decoration:none; font-weight:700; line-height:1.3; border:1px solid transparent; }}
 .btn small {{ font-weight:500; opacity:.8; font-size:.8em; }}
 .btn:hover {{ background:rgba(61,165,244,.22); }}
 .here .btn, .btn.primary {{ background:var(--accent); color:var(--on-accent); }}
 .here .btn:hover, .btn.primary:hover {{ background:#5DB5F6; }}
 .btn.big {{ display:inline-flex; flex-direction:row; align-items:center; gap:.75em; padding:.8em 1.5em .8em 1.1em;
            font-size:1.05em; }}
 .btn.big svg {{ width:1.5em; height:1.5em; fill:currentColor; flex:none; }}
 .btn.big span {{ display:flex; flex-direction:column; }}
 .btn.ghost {{ background:none; color:var(--text); border-color:var(--line); }}
 .btn.ghost:hover {{ background:var(--surface); border-color:var(--muted); }}
 .more {{ display:flex; flex-wrap:wrap; gap:.5em; margin:0; }}
 .more a {{ padding:.35em .75em; border:1px solid var(--line); border-radius:9px; text-decoration:none; color:var(--text);
           font-size:.88em; font-weight:600; white-space:nowrap; }}
 .more a:hover {{ border-color:var(--accent); }}
 .more small {{ color:var(--muted); font-weight:400; }}
 details {{ color:var(--muted); font-size:.92em; }}
 details + details {{ margin-top:.45em; }}
 summary {{ cursor:pointer; color:var(--soft); }}
 summary:hover {{ color:var(--text); }}
 details ol {{ padding-left:1.25em; margin:.5em 0 .2em; }}
 details li + li {{ margin-top:.45em; }}
 .uninstall {{ margin:.5em 0 .2em; }}
 .sums {{ font-size:.82em; }}
 .sums div {{ margin-top:.5em; }}
 pre {{ background:var(--bg); padding:.7em 1em; border-radius:8px; overflow-x:auto; color:var(--text); margin:.3em 0 0; }}
 .none {{ color:var(--muted); margin:0 0 .4em; }}
 .band {{ display:flex; align-items:center; flex-wrap:wrap; gap:1.2em 1.6em; margin-top:3.5em; padding:1.8em 2em;
         border:1px solid var(--line); border-radius:20px; background:linear-gradient(135deg,#16212F,var(--surface) 60%); }}
 .band .fi {{ margin:0; width:3.25em; height:3.25em; }}
 .band div {{ flex:1 1 20em; }}
 .band h2 {{ font-size:1.35em; margin:0 0 .15em; }}
 .band p {{ margin:0; color:var(--muted); }}
 .band .btn {{ display:inline-flex; }}
 footer {{ display:grid; grid-template-columns:1.6fr repeat(3,1fr); gap:1.5em; border-top:1px solid var(--line);
          margin-top:4em; padding:2.5em 0 3em; color:var(--muted); font-size:.93em; }}
 footer p {{ margin:.7em 0 0; font-size:.92em; }}
 footer h4 {{ color:var(--text); margin:.3em 0 .7em; font-size:.8em; text-transform:uppercase; letter-spacing:.08em; }}
 footer ul {{ list-style:none; padding:0; margin:0; }}
 footer li + li {{ margin-top:.4em; }}
 footer ul a {{ color:var(--muted); text-decoration:none; }}
 footer ul a:hover {{ color:var(--text); }}
 @media (max-width:1060px) {{ .features {{ grid-template-columns:repeat(2,1fr); }} }}
 @media (max-width:900px) {{
  .hero {{ grid-template-columns:1fr; gap:2em; padding-top:1em; }}
  .art {{ max-width:30em; margin:0 auto; }}
  .grid {{ grid-template-columns:1fr; }}
 }}
 @media (max-width:760px) {{ footer {{ grid-template-columns:1fr 1fr; }} footer > div:first-child {{ grid-column:1/-1; }} }}
 @media (max-width:620px) {{
  .features {{ grid-template-columns:1fr; }}
  .cta .btn {{ flex:1 1 100%; }}
  .band {{ padding:1.4em; }}
 }}
</style></head>
<body><div class="wrap">
<header class="top">
 <a class="brand" href="./">{brand()}</a>
 <nav><a href="#download">Download</a><a href="setup/">Setup guide</a><a href="internet-access/">Internet access</a><a href="{CLIENT_URL}">YarmiplayTV</a><a href="{REPO_URL}">Source</a></nav>
</header>
<main>
<section class="hero">
 <div>
  <p class="eyebrow">Free and open source &middot; Windows, macOS, Linux</p>
  <h1>Host movie night,<br><span>from your tray.</span></h1>
  <p class="lede">{NAME} runs a Syncplay server and a Jellyfin server on your computer: your friends get a room
  to watch in sync and a library to stream from.</p>
  <div class="cta">
   <a class="btn primary big" id="get" href="#download">{icon("download")}<span><b>Download</b><small>Windows, macOS, Linux</small></span></a>
   <a class="btn ghost big" href="{CLIENT_URL}">{icon("tv")}<span><b>Get YarmiplayTV</b><small>The player for your friends</small></span></a>
  </div>
  <p class="hero-meta">Version {html.escape(version)} &middot; Works with Syncplay {html.escape(syncplay_version())}</p>
 </div>
 {ILLUSTRATION}
</section>
<section class="section" aria-labelledby="what">
 <h2 id="what">One app, everything the room needs</h2>
 <p class="sub">Every server switches on and off from the control panel or the tray menu, on the ports you choose.</p>
 <div class="features">{feature_cards}</div>
</section>
<section class="section" id="download" aria-labelledby="get-it">
 <h2 id="get-it">Get {NAME}</h2>
 <p class="sub">Installed copies update themselves; store copies are updated by the store.</p>
 <div class="grid">
{chr(10).join(cards)}
 </div>
</section>
<section class="band">
 <span class="fi">{icon("tv")}</span>
 <div><h2>Need the player?</h2>
 <p>YarmiplayTV plays in your rooms on Google TV, Android, Windows, macOS and Linux, or straight from a browser.</p></div>
 <a class="btn" href="{CLIENT_URL}">Get YarmiplayTV<small>tv.yarmiplay.com</small></a>
</section>
</main>
<footer>
 <div><a class="brand" href="./">{brand()}</a>
 <p>Version {html.escape(version)} &middot; built {html.escape(built)}</p></div>
 {footer_columns}
</footer>
</div>
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
  // The heading's main button gets this computer's first download.
  var btn = card.querySelector(".btn"), get = document.getElementById("get");
  if (!btn || !get) return;
  get.setAttribute("href", btn.getAttribute("href"));
  if (btn.hasAttribute("download")) get.setAttribute("download", "");
  get.querySelector("b").textContent = "Get it for " + card.querySelector("h3").textContent;
  get.querySelector("small").textContent = btn.firstChild.nodeValue;
}})();
</script>
</body></html>
"""


def slugify(text):
    clean = re.sub(r'\[([^\]]+)\]\([^)]+\)', r'\1', text)
    clean = re.sub(r'[`*]', '', clean)
    clean = re.sub(r'[^a-zA-Z0-9\s-]', '', clean).strip().lower()
    return re.sub(r'[\s_]+', '-', clean)


def inline_markdown(text):
    """`code`, **bold**, *italic* and [text](url) in already-escaped text."""
    text = re.sub(r"`([^`]+)`", r"<code>\1</code>", text)
    text = re.sub(r"\*\*([^*]+)\*\*", r"<b>\1</b>", text)
    text = re.sub(r"(?<!\*)\*([^*]+)\*(?!\*)", r"<i>\1</i>", text)
    return re.sub(r"\[([^\]]+)\]\(([^)\s]+)\)", r'<a href="\2">\1</a>', text)


def markdown_to_html(md):
    """Parses markdown with headings (+ id slugs), code blocks, blockquotes, lists, tables and paragraphs."""
    blocks = []
    para = []
    list_items = []
    list_type = None  # "ul" or "ol"
    quote_lines = []
    table_lines = []
    in_code = False
    code_lines = []

    def flush_para():
        nonlocal para
        if para:
            blocks.append(f"<p>{inline_markdown(html.escape(' '.join(para), quote=False))}</p>")
            para = []

    def flush_list():
        nonlocal list_items, list_type
        if list_items:
            lis = "".join(f"<li>{inline_markdown(html.escape(i, quote=False))}</li>" for i in list_items)
            blocks.append(f"<{list_type}>{lis}</{list_type}>")
            list_items = []
            list_type = None

    def flush_quote():
        nonlocal quote_lines
        if quote_lines:
            q_text = " ".join(quote_lines)
            if any(k in q_text for k in ("**Tip", "**Note", "**Warning", "**Important", "**Security", "**Next step")):
                blocks.append(f'<div class="callout"><p>{inline_markdown(html.escape(q_text, quote=False))}</p></div>')
            else:
                blocks.append(f'<blockquote><p>{inline_markdown(html.escape(q_text, quote=False))}</p></blockquote>')
            quote_lines = []

    def flush_table():
        nonlocal table_lines
        if table_lines:
            rows = []
            for t_line in table_lines:
                cells = [c.strip() for c in t_line.split("|")[1:-1]]
                rows.append(cells)
            if len(rows) >= 2:
                header = rows[0]
                body = rows[2:] if rows[1][0].startswith(":") or rows[1][0].startswith("-") else rows[1:]
                th_html = "".join(f"<th>{inline_markdown(html.escape(c, quote=False))}</th>" for c in header)
                tr_html = []
                for row in body:
                    tds = "".join(f"<td>{inline_markdown(html.escape(c, quote=False))}</td>" for c in row)
                    tr_html.append(f"<tr>{tds}</tr>")
                blocks.append(f'<div class="table-wrap"><table><thead><tr>{th_html}</tr></thead>'
                              f'<tbody>{"".join(tr_html)}</tbody></table></div>')
            table_lines = []

    def flush_all():
        flush_para()
        flush_list()
        flush_quote()
        flush_table()

    for line in md.splitlines():
        stripped = line.strip()

        if in_code:
            if stripped == "```":
                in_code = False
                blocks.append(f"<pre><code>{html.escape(chr(10).join(code_lines))}</code></pre>")
                code_lines = []
            else:
                code_lines.append(line)
            continue

        if stripped.startswith("```"):
            flush_all()
            in_code = True
            code_lines = []
            continue

        if stripped == "---" or stripped == "***":
            flush_all()
            blocks.append("<hr>")
            continue

        if stripped.startswith("|") and stripped.endswith("|"):
            flush_para()
            flush_list()
            flush_quote()
            table_lines.append(stripped)
            continue
        elif table_lines:
            flush_table()

        heading = re.match(r"(#{1,4}) (.+)", stripped)
        if heading:
            flush_all()
            level = len(heading.group(1))
            h_text = heading.group(2).strip()
            slug = slugify(h_text)
            blocks.append(f'<h{level} id="{slug}">{inline_markdown(html.escape(h_text, quote=False))}</h{level}>')
            continue

        if stripped.startswith(">"):
            flush_para()
            flush_list()
            content = stripped.lstrip(">").strip()
            quote_lines.append(content)
            continue
        elif quote_lines:
            flush_quote()

        ul_match = re.match(r"^[-*]\s+(.+)", stripped)
        if ul_match:
            flush_para()
            if list_type != "ul":
                flush_list()
                list_type = "ul"
            list_items.append(ul_match.group(1))
            continue

        ol_match = re.match(r"^(\d+)\.\s+(.+)", stripped)
        if ol_match:
            flush_para()
            if list_type != "ol":
                flush_list()
                list_type = "ol"
            list_items.append(ol_match.group(2))
            continue

        if list_items and (line.startswith("   ") or line.startswith("\t") or line.startswith("  ")):
            list_items[-1] += " " + stripped
            continue
        elif list_items and not stripped:
            continue
        elif list_items:
            flush_list()

        if not stripped:
            flush_para()
            continue

        para.append(stripped)

    flush_all()
    if in_code and code_lines:
        blocks.append(f"<pre><code>{html.escape(chr(10).join(code_lines))}</code></pre>")

    return "\n".join(blocks)


def render_doc(md, description, site_url=None, path="", active_nav="", next_nav=None, prev_nav=None,
               version="dev", built="", windows_signed=False, privacy=True):
    """A Markdown document as a page in the download page's style, one folder below it."""
    lines = md.splitlines()
    title = next((l[2:].strip() for l in lines if l.startswith("# ")), NAME)
    body_md = "\n".join(l for l in lines if not l.startswith("# "))

    nav_cards = []
    if prev_nav:
        nav_cards.append(
            f'<a class="nav-card prev" href="{html.escape(prev_nav[1])}">'
            f'<span class="nav-dir">&larr; Previous</span>'
            f'<strong class="nav-title">{html.escape(prev_nav[0])}</strong>'
            f'<span class="nav-desc">{html.escape(prev_nav[2])}</span></a>'
        )
    if next_nav:
        nav_cards.append(
            f'<a class="nav-card next" href="{html.escape(next_nav[1])}">'
            f'<span class="nav-dir">Next &rarr;</span>'
            f'<strong class="nav-title">{html.escape(next_nav[0])}</strong>'
            f'<span class="nav-desc">{html.escape(next_nav[2])}</span></a>'
        )
    nav_html = f'<nav class="doc-nav" aria-label="Related guides">{"".join(nav_cards)}</nav>' if nav_cards else ""

    doc_columns = (
        ("Help", [("../setup/", "Setup guide"), ("../internet-access/", "Internet access"),
                  (f"{REPO_URL}/issues", "Report a problem")]),
        ("Project", [(HOME_URL, "yarmiplay.com"), (CLIENT_URL, "YarmiplayTV"), (REPO_URL, "Source code")]),
        ("Legal", ([("../privacy/", "Privacy")] if privacy else [])
         + ([(f"{REPO_URL}#code-signing-policy", "Code signing policy")] if windows_signed else [])),
    )
    footer_columns = "".join(
        f'<div><h4>{col_title}</h4><ul>'
        + "".join(f'<li><a href="{html.escape(href)}">{html.escape(label)}</a></li>' for href, label in links)
        + "</ul></div>" for col_title, links in doc_columns if links)

    active_setup = ' class="active"' if active_nav == "setup" else ''
    active_internet = ' class="active"' if active_nav == "internet-access" else ''

    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="theme-color" content="#0E1116">
<title>{html.escape(title)}</title>
{search_head(title, description, site_url, path)}
<link rel="icon" href="../logo.svg" type="image/svg+xml">
<style>
{BASE_CSS}
 body {{ line-height:1.65; }}
 .wrap {{ max-width:56em; }}
 main {{ padding:1.5em 0 3.5em; }}
 .doc-hero {{ margin-bottom:2em; padding-bottom:1.5em; border-bottom:1px solid var(--line); }}
 .crumb {{ font-size:.85em; color:var(--muted); margin-bottom:.8em; }}
 .crumb a {{ color:var(--muted); text-decoration:none; }}
 .crumb a:hover {{ color:var(--text); }}
 .eyebrow {{ display:inline-block; color:var(--accent); background:var(--tint); font-weight:600; font-size:.85em;
            padding:.25em .8em; border-radius:99px; margin:0 0 1em; }}
 h1 {{ font-size:clamp(2.1em,5vw,2.8em); letter-spacing:-.03em; line-height:1.15; margin:0 0 .35em; }}
 .doc-sub {{ color:var(--soft); font-size:1.15em; margin:0; line-height:1.5; }}
 h2 {{ font-size:1.5em; margin:2.2em 0 .5em; letter-spacing:-.02em; padding-bottom:.3em; border-bottom:1px solid var(--line); color:var(--text); }}
 h3 {{ font-size:1.22em; margin:1.6em 0 .4em; color:var(--text); }}
 h4 {{ font-size:1.05em; margin:1.3em 0 .3em; color:var(--soft); }}
 p, li {{ color:var(--soft); }}
 hr {{ border:0; border-top:1px solid var(--line); margin:2.5em 0; }}
 code {{ background:var(--surface); }}
 pre {{ background:#07090C; border:1px solid var(--line); border-radius:12px; padding:1em 1.25em; overflow-x:auto; margin:1.2em 0; }}
 pre code {{ background:transparent; padding:0; border-radius:0; color:#E8ECF2; }}
 .callout {{ background:linear-gradient(135deg,rgba(61,165,244,.12),var(--surface)); border-left:4px solid var(--accent);
           border-radius:0 12px 12px 0; padding:1em 1.3em; margin:1.5em 0; box-shadow:0 4px 16px -4px rgba(0,0,0,.3); }}
 .callout p {{ margin:0; color:var(--text); }}
 .callout b {{ color:var(--accent); }}
 .table-wrap {{ overflow-x:auto; margin:1.5em 0; border:1px solid var(--line); border-radius:12px; }}
 table {{ width:100%; border-collapse:collapse; text-align:left; font-size:.95em; }}
 th {{ background:var(--surface2); color:var(--text); font-weight:600; padding:.75em 1em; border-bottom:2px solid var(--line); white-space:nowrap; }}
 td {{ padding:.75em 1em; border-bottom:1px solid var(--line); color:var(--soft); }}
 tr:last-child td {{ border-bottom:0; }}
 tr:nth-child(even) td {{ background:rgba(255,255,255,.015); }}
 tr:hover td {{ background:var(--surface); }}
 ol, ul {{ padding-left:1.4em; }}
 li {{ margin:.35em 0; }}
 li::marker {{ color:var(--accent); }}
 #table-of-contents + ul, #table-of-contents + ol {{
   background:var(--surface); border:1px solid var(--line); border-radius:14px; padding:1.2em 1.8em; margin:1em 0 2em; list-style-type:none;
 }}
 #table-of-contents + ul li, #table-of-contents + ol li {{ margin:.4em 0; }}
 #table-of-contents + ul a, #table-of-contents + ol a {{ text-decoration:none; font-weight:500; }}
 #table-of-contents + ul a:hover, #table-of-contents + ol a:hover {{ text-decoration:underline; }}
 .doc-nav {{ display:grid; grid-template-columns:repeat(auto-fit,minmax(240px,1fr)); gap:1em; margin:3em 0 2em; }}
 .nav-card {{ display:flex; flex-direction:column; padding:1.2em 1.4em; background:var(--surface); border:1px solid var(--line);
             border-radius:14px; text-decoration:none; color:inherit; transition:border-color .15s, transform .15s; }}
 .nav-card:hover {{ border-color:var(--accent); transform:translateY(-2px); }}
 .nav-dir {{ font-size:.82em; font-weight:600; text-transform:uppercase; letter-spacing:.05em; color:var(--accent); margin-bottom:.3em; }}
 .nav-title {{ font-size:1.1em; color:var(--text); margin-bottom:.25em; }}
 .nav-desc {{ font-size:.88em; color:var(--muted); line-height:1.4; }}
 footer {{ display:grid; grid-template-columns:1.6fr repeat(3,1fr); gap:1.5em; border-top:1px solid var(--line);
          margin-top:4em; padding:2.5em 0 3em; color:var(--muted); font-size:.93em; }}
 footer p {{ margin:.7em 0 0; font-size:.92em; }}
 footer h4 {{ color:var(--text); margin:.3em 0 .7em; font-size:.8em; text-transform:uppercase; letter-spacing:.08em; }}
 footer ul {{ list-style:none; padding:0; margin:0; }}
 footer li + li {{ margin-top:.4em; }}
 footer ul a {{ color:var(--muted); text-decoration:none; }}
 footer ul a:hover {{ color:var(--text); }}
 @media (max-width:760px) {{ footer {{ grid-template-columns:1fr 1fr; }} footer > div:first-child {{ grid-column:1/-1; }} }}
</style></head>
<body><div class="wrap">
<header class="top">
 <a class="brand" href="../">{brand("../")}</a>
 <nav><a href="../#download">Download</a><a href="../setup/"{active_setup}>Setup guide</a><a href="../internet-access/"{active_internet}>Internet access</a><a href="{CLIENT_URL}">YarmiplayTV</a><a href="{REPO_URL}">Source</a></nav>
</header>
<main>
<div class="doc-hero">
 <div class="crumb"><a href="../">Home</a> &rsaquo; Documentation</div>
 <span class="eyebrow">{NAME} documentation</span>
 <h1>{html.escape(title)}</h1>
 <p class="doc-sub">{html.escape(description)}</p>
</div>
{markdown_to_html(body_md)}
{nav_html}
</main>
<footer>
 <div><a class="brand" href="../">{brand("../")}</a>
 <p>Version {html.escape(version)} &middot; built {html.escape(built)}</p></div>
 {footer_columns}
</footer>
</div></body></html>
"""


def build(dist, out, version, windows_signed=False, site_url=None):
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
    shots = os.path.join(ROOT, "docs", "store", "screenshots")
    if os.path.isdir(shots):
        shutil.copytree(shots, os.path.join(out, "screenshots"))
    now = datetime.datetime.now(datetime.timezone.utc)
    built = now.strftime("%Y-%m-%d %H:%M UTC")

    DOCS = [
        ("setup.md", "setup", f"Setup guide for {NAME}: first-run, Syncplay, Jellyfin, Plex and data storage.",
         ("Internet Access Guide", "../internet-access/", "Port forwarding, UPnP, DuckDNS, and automatic free HTTPS"),
         None),
        ("internet-access.md", "internet-access", f"Internet access guide for {NAME}: port forwarding, DuckDNS, and free HTTPS.",
         ("Downloads", "../#download", f"Download {NAME} for Windows, macOS, or Linux"),
         ("Setup Guide", "../setup/", "First-run setup, Syncplay, Jellyfin, and Plex configuration")),
        ("privacy.md", "privacy", f"Privacy policy of {NAME}: what the app stores and sends.",
         None,
         ("Setup Guide", "../setup/", "First-run setup, Syncplay, Jellyfin, and Plex configuration")),
    ]
    pages = [""]
    for filename, slug, desc, next_nav, prev_nav in DOCS:
        doc_src = os.path.join(ROOT, "docs", filename)
        if os.path.isfile(doc_src):
            doc_dir = os.path.join(out, slug)
            os.makedirs(doc_dir, exist_ok=True)
            with open(doc_src, encoding="utf-8") as src, \
                    open(os.path.join(doc_dir, "index.html"), "w", encoding="utf-8") as f:
                f.write(render_doc(src.read(), desc, site_url, f"{slug}/", active_nav=slug,
                                   next_nav=next_nav, prev_nav=prev_nav,
                                   version=version, built=built, windows_signed=windows_signed,
                                   privacy=os.path.isfile(os.path.join(ROOT, "docs", "privacy.md"))))
            pages.append(f"{slug}/")

    has_privacy = os.path.isfile(os.path.join(ROOT, "docs", "privacy.md"))
    with open(os.path.join(out, "index.html"), "w", encoding="utf-8") as f:
        f.write(render_page(downloads, version, built, windows_signed, has_privacy, site_url))
    if site_url:
        with open(os.path.join(out, "sitemap.xml"), "w", encoding="utf-8") as f:
            f.write(sitemap(site_url, pages, now.strftime("%Y-%m-%d")))
        with open(os.path.join(out, "robots.txt"), "w", encoding="utf-8") as f:
            f.write(f"User-agent: *\nAllow: /\n\nSitemap: {page_url(site_url, 'sitemap.xml')}\n")
    open(os.path.join(out, ".nojekyll"), "w").close()
    print(f"wrote {out} ({sum(len(v) for v in downloads.values())} downloads)")


def syncplay_version():
    """The Syncplay version the built-in server reports (protocol.rs SERVER_VERSION)."""
    with open(os.path.join(ROOT, "src-tauri", "src", "syncplay", "protocol.rs"), encoding="utf-8") as f:
        m = re.search(r'pub const SERVER_VERSION: &str = "([^"]+)";', f.read())
    if not m:
        sys.exit("SERVER_VERSION not found in src-tauri/src/syncplay/protocol.rs")
    return m.group(1)


def app_version():
    with open(os.path.join(ROOT, "src-tauri", "tauri.conf.json"), encoding="utf-8") as f:
        return json.load(f).get("version", "dev")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dist", required=True, help="folder with the built installers")
    ap.add_argument("--out", required=True, help="site folder to (re)create")
    ap.add_argument("--version", default=None, help="defaults to the version in tauri.conf.json")
    ap.add_argument("--windows-signed", action="store_true", help="the Windows installers are code-signed")
    ap.add_argument("--site-url", default=None, help="public URL of the site: canonical URLs and sitemap.xml")
    a = ap.parse_args()
    build(a.dist, a.out, a.version or app_version(), a.windows_signed, a.site_url)


if __name__ == "__main__":
    main()
