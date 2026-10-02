# YarmiplayServerTV

Host your own **Syncplay server** and **Jellyfin server** from the system tray on Windows, macOS or Linux.
It's the companion to [YarmiplayTV](https://github.com/Yarmiplay/YarmiplayTV): one app on your computer
gives your friends a room to watch in sync and a media library to stream from.

**[Download YarmiplayServerTV](https://yarmiplay.github.io/YarmiplayServerTV/)**

- A Syncplay 1.7-compatible server built in: rooms, passwords, chat, readiness, shared playlists, and
  encrypted connections (TLS) when a certificate is available.
- Jellyfin, downloaded and set up for you. The first-run setup and library management happen in the
  control panel, so you don't need Jellyfin's own setup wizard.
- Optional UPnP port forwarding, so friends outside your home network can connect. It's off by default.
- Optional free HTTPS: a [DuckDNS](https://www.duckdns.org) name that follows your home IP, with a
  Let's Encrypt certificate that is renewed automatically.
- You choose the ports. Every server can be switched on and off from the control panel or the tray menu.

The control panel is only reachable from the app window on your own computer. There is no web control
port, so nobody else on the network or internet can change your settings.

## Getting started

1. Install YarmiplayServerTV from the [download page](https://yarmiplay.github.io/YarmiplayServerTV/) and start it.
   It lives in the system tray; closing the window keeps the servers running. Use **Quit** in the tray
   menu to stop everything.
2. On the **Dashboard**, switch on the Syncplay server, the Jellyfin server, or both.
3. Give your friends the addresses shown on the dashboard. **On your network** works at home; **Over the
   internet** needs the steps in [Internet access](#internet-access).

**Start with system** (Dashboard, or the tray menu) starts the app minimized to the tray when you log in.

### Syncplay

The default port is 8999. On the **Syncplay** page you can set a server password, a message of the day,
isolated rooms (users only see their own room), and switch off chat or readiness. Changing the port
restarts the server and disconnects everyone.

Connect from YarmiplayTV or any Syncplay client with `address:port`, for example `192.168.1.20:8999`. When
DuckDNS and HTTPS are set up, connect with the DuckDNS name (`myname.duckdns.org:8999`) so clients can
verify the certificate and use an encrypted connection.

### Jellyfin

The first time you switch Jellyfin on, the app downloads Jellyfin (about 150–250 MB, checked against
pinned SHA-256 checksums) from `repo.jellyfin.org`. The download includes ffmpeg for transcoding.

When Jellyfin is running, the **Jellyfin** page asks you to create the administrator account. That's the
whole first-run setup: the app configures the language, remote access and Quick Connect for you. Then add
libraries: give each one a name, pick the kind of content (movies, shows, music…) and choose a folder.
You can add more folders, remove libraries and start a scan from the same page.

To change other Jellyfin settings, open Jellyfin (**This PC** → **Open**) and sign in with the
administrator account.

Default ports: 8096 for HTTP, 8920 for HTTPS (HTTPS is only used once a DuckDNS certificate is ready).

## Internet access

At home, friends can always connect with the LAN address. For friends elsewhere, your router has to
forward the server ports to your computer.

### Port forwarding

Either forward the ports yourself in your router's settings (TCP, to this computer's LAN address shown on
the **Network** page), or switch on **UPnP** for Syncplay and/or Jellyfin on the **Network** page. With UPnP
the app asks the router to add the forwards, checks them every 10 minutes, and removes them again when you
switch UPnP off or quit. A forward you already made by hand is detected and left alone.

If the Network page reports **double NAT**, your router sits behind another router or your provider uses
carrier-grade NAT. Forwards on your own router then can't be reached from the internet; ask your provider
for a public IP, or forward the ports on the outer router too.

### DuckDNS and HTTPS

1. Sign in at [duckdns.org](https://www.duckdns.org) and create a subdomain, for example `myname`.
2. On the **Network** page, switch on **Use DuckDNS and HTTPS**, enter the domain (`myname` or
   `myname.duckdns.org`) and save, then paste the **token** from the DuckDNS page and choose **Save token**.
   The token is stored in your system's credential store (Windows Credential Manager, macOS Keychain or the
   Secret Service on Linux) and never shown again.
3. The app points the name at your public IP (and keeps it updated every 5 minutes), then gets a Let's
   Encrypt certificate using a DNS challenge. This takes about a minute and works without any open port.

The certificate is renewed automatically halfway through its lifetime. Syncplay uses a renewed certificate
right away; Jellyfin restarts briefly to load it.

**Test certificates (Let's Encrypt staging)** is useful while trying things out: staging has much higher
rate limits, but browsers and clients don't trust those certificates. Let's Encrypt allows 5 identical
certificates per week, so switch staging off once everything works.

## Where things are stored

Settings, the ACME account, certificates and Jellyfin (program, database, cache and logs) are kept in the
per-user app-data folder; **Open data folder** on the dashboard opens it.

- Windows: `%APPDATA%\Yarmiplay\YarmiplayServerTV` (settings) and `%LOCALAPPDATA%\Yarmiplay\YarmiplayServerTV` (data)
- macOS: `~/Library/Application Support/com.Yarmiplay.YarmiplayServerTV`
- Linux: `~/.config/yarmiplayservertv` and `~/.local/share/yarmiplayservertv`

Tokens and passwords never appear in the logs; the **Logs** page shows the app's activity and Jellyfin's
warnings.

## Building from source

You need [Node.js](https://nodejs.org) 22, [Rust](https://rustup.rs) (stable) and the
[Tauri prerequisites](https://tauri.app/start/prerequisites/) for your OS. On Debian/Ubuntu:

```sh
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libdbus-1-dev libxdo-dev libssl-dev build-essential
```

Then:

```sh
git clone https://github.com/Yarmiplay/YarmiplayServerTV
cd YarmiplayServerTV
npm install
npx tauri dev      # run with hot reload
npx tauri build    # installers in src-tauri/target/release/bundle
```

Tests: `cd src-tauri && cargo test` for the Rust side (Syncplay protocol and rooms, UPnP, ACME, Jellyfin
installer and configuration), `npm run check` for the UI. CI also runs two scripted Syncplay clients
(`scripts/fake_peer.py`) against the built-in server.

`YARMIPLAYSERVERTV_HOME=<folder>` keeps all settings and data in another folder, which is handy for
testing. The Jellyfin version and checksums are pinned in `src-tauri/jellyfin-manifest.json`; regenerate
it with `python scripts/update_jellyfin_manifest.py`.

### Project layout

- `src-tauri/src/syncplay/`: the Syncplay server (protocol, rooms, TCP/STARTTLS listener).
- `src-tauri/src/jellyfin/`: Jellyfin download, process supervision, `network.xml` and the REST API client.
- `src-tauri/src/net/`: UPnP port mapping, DuckDNS and ACME.
- `src-tauri/src/orchestrator.rs`: keeps every service in line with the settings.
- `ui/`: the control panel (Svelte).

## License

GPL-3.0-or-later. Syncplay and Jellyfin are separate projects by their respective authors; this app
speaks the Syncplay protocol and downloads unmodified Jellyfin releases.
