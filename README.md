# YarmiplayServerTV

Host your own **Syncplay server** and **Jellyfin server** from the system tray on Windows, macOS or Linux.
It's the companion to [YarmiplayTV](https://github.com/Yarmiplay/YarmiplayTV): one app on your computer
gives your friends a room to watch in sync and a media library to stream from.

**[Download YarmiplayServerTV](https://yarmiplay.github.io/YarmiplayServerTV/)**, or on Windows get it from the
[Microsoft Store](https://apps.microsoft.com/detail/9P6B9C1KXFFQ) (Jellyfin built in, updated by the Store).

- A Syncplay 1.7-compatible server built in: rooms, passwords, chat, readiness, shared playlists, and
  encrypted connections (TLS) when a certificate is available.
- Jellyfin, downloaded and set up for you. The first-run setup and library management happen in the
  control panel, so you don't need Jellyfin's own setup wizard.
- Optional UPnP port forwarding, so friends outside your home network can connect. It's off by default.
- Optional free HTTPS: a [DuckDNS](https://www.duckdns.org) name that follows your home IP, with a
  Let's Encrypt certificate that is renewed automatically.
- You choose the ports. Every server can be switched on and off from the control panel or the tray menu.

The control panel is only reachable from your own computer: in the app window, or in a browser through
**Open in Browser** in the tray menu. Browser access listens on `127.0.0.1:8097` only, needs the one-time
sign-in link that menu item opens, and can be switched off on the Dashboard, so nobody else on the network
or internet can change your settings.

## Getting started

1. Install YarmiplayServerTV from the [download page](https://yarmiplay.github.io/YarmiplayServerTV/) (on Windows,
   the [Microsoft Store](https://apps.microsoft.com/detail/9P6B9C1KXFFQ) works too) and start it.
   It lives in the system tray; closing the window keeps the servers running. Use **Quit** in the tray
   menu to stop everything.
2. On the **Dashboard**, switch on the Syncplay server, the Jellyfin server, or both.
3. Give your friends the addresses shown on the dashboard. **On your network** works at home; **Over the
   internet** needs the steps in [Internet access](#internet-access).

**Start with system** (Dashboard, or the tray menu) starts the app minimized to the tray when you log in.

### Updates

The Microsoft Store copy is updated by the Store and doesn't show these controls.

**Check for updates** (Dashboard, or the tray menu) looks for a newer release here on GitHub and downloads
it; **Install** then restarts the app into the new version. Switch on **Automatic updates** on the Dashboard to
check every few hours and install new versions while nobody is using Syncplay. Copies installed with the
`.msi` or `.deb` package need administrator permission to update, so for those the new version is downloaded
and waits for you to click **Install**. Every update is checked against the app's update signing key before
it installs. Versions before 1.1.0 can't update themselves; install 1.1.0 or later from the download page once.

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
- Windows, Microsoft Store copy: `%LOCALAPPDATA%\Packages\Yarmiplay.YarmiplayServerTV_<id>\LocalState`, which
  Windows deletes when you uninstall it. It doesn't share settings with a copy from the download page; don't run
  both at once, since they would want the same ports.
- macOS: `~/Library/Application Support/com.Yarmiplay.YarmiplayServerTV`
- Linux: `~/.config/yarmiplayservertv` and `~/.local/share/yarmiplayservertv`

Tokens and passwords never appear in the logs; the **Logs** page shows the app's activity and Jellyfin's
warnings.

## Uninstalling

1. If you switched on **Start with system**, switch it off first, so no login entry is left behind.
2. Choose **Quit** in the tray menu. This stops Jellyfin and removes any UPnP port forwards the app made.
3. Remove the app:
   - Windows: **Settings > Apps > Installed apps**, then YarmiplayServerTV > **Uninstall**.
   - macOS: move YarmiplayServerTV from Applications to the Trash.
   - Linux: `sudo apt remove yarmiplay-server-tv`, or delete the AppImage.
4. To remove your settings, certificates and Jellyfin (with its library database) too, delete the folders
   listed in [Where things are stored](#where-things-are-stored) (the Microsoft Store copy's folder is deleted
   with the app). The DuckDNS token and Jellyfin sign-in are
   kept in the system credential store under `YarmiplayServerTV`; **Remove** next to the DuckDNS token on the
   Network page and
   **Sign out** on the Jellyfin page delete them, or remove them in Windows Credential Manager, Keychain Access
   (macOS) or your Linux keyring app.

Your media folders are never changed or deleted.

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

### Releases

Bump the version in `src-tauri/tauri.conf.json` (and `package.json` / `src-tauri/Cargo.toml` to match), then
push a matching tag:

```sh
git tag v1.0.1 && git push origin v1.0.1
```

`.github/workflows/release.yml` builds the installers for Windows, macOS and Linux and attaches them to the
tag's GitHub release together with `latest.json`, which installed copies read to update themselves
(`scripts/updater_manifest.py` writes it). Updates are signed with the updater key in the secrets
`TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`; its public half is
`plugins.updater.pubkey` in `src-tauri/tauri.conf.json`. Keep a backup of that key: without it, installed
copies can't receive updates anymore.

The Windows installers are submitted to [SignPath](https://signpath.io) for code signing first. An approver
accepts the request in SignPath (the job waits up to 6 hours), then the signed `.msi` and `.exe` are verified,
signed for the updater and released, and the download page is republished with them. Signing needs the repository variable `SIGNPATH_ORGANIZATION_ID` and the secret `SIGNPATH_API_TOKEN` (a
SignPath CI user with submitter rights), a SignPath project with the slug `YarmiplayServerTV` linked to the
GitHub.com trusted build system, its `release-signing` policy, and
[.github/signpath/artifact-configuration.xml](.github/signpath/artifact-configuration.xml) as its default
artifact configuration. Without the variable the release gets the unsigned installers, and the download page
keeps offering the ones from `main`. SmartScreen may still warn about a newly signed release until it has been
downloaded enough.

The app is [YarmiplayServerTV on the Microsoft Store](https://apps.microsoft.com/detail/9P6B9C1KXFFQ) (Store ID
`9P6B9C1KXFFQ`). The same run builds the Store package (`scripts/make-msix.ps1`, an unsigned `.msix` that the
Store signs, with Jellyfin built in) as the `windows-store-msix-<version>` artifact. Upload it in Partner Center as described in
[docs/store/README.md](docs/store/README.md), which also has the listing text. The app notices when it runs
from that package: it leaves updates to the Store, keeps its data in the package's `LocalState` folder and uses
the package's startup task for **Start with system**. `./scripts/make-msix.ps1 -Register` installs the package
locally for a test (needs Developer Mode).

## Code signing policy

Free code signing provided by [SignPath.io](https://signpath.io), certificate by
[SignPath Foundation](https://signpath.org).

- Committers and reviewers: [Yarmiplay](https://github.com/Yarmiplay)
- Approvers: [Yarmiplay](https://github.com/Yarmiplay)

Only the Windows installers and the YarmiplayServerTV program in them are signed, built by
`.github/workflows/release.yml` from a version tag of this repository. Jellyfin isn't part of the installers;
the app downloads unmodified Jellyfin releases, as their project publishes them, when you first switch it on.

Privacy: this program will not transfer any information to other networked systems unless specifically
requested by the user or the person installing or operating it. There is no telemetry or account. Every
server and network feature is off until you switch it on, and each one only talks to what it needs:

- **Updates:** only when you click **Check for updates** or switch on **Automatic updates**, the app downloads
  `latest.json` and then the new installer from this project's GitHub releases. GitHub sees your IP address,
  as with any download.
- **Jellyfin:** downloads Jellyfin and ffmpeg from `repo.jellyfin.org` the first time you switch it on.
  Jellyfin then runs on your computer as its own program; see Jellyfin's documentation for what it does.
- **UPnP:** asks your router on the local network to add and remove port forwards.
- **DuckDNS and HTTPS:** sends your DuckDNS token and public IP address to [DuckDNS](https://www.duckdns.org),
  requests certificates from [Let's Encrypt](https://letsencrypt.org/privacy/) (with the contact email if you
  enter one), and checks the DNS record through [Google Public DNS](https://developers.google.com/speed/public-dns/privacy)
  and [Cloudflare DNS](https://developers.cloudflare.com/1.1.1.1/privacy/public-dns-resolver/).
- **Syncplay server:** people you give the address to connect to it. File names, playback and chat are
  passed between them and not stored; the app's log on your computer notes who joined and left which room.

The full privacy policy is [docs/privacy.md](docs/privacy.md), published at
<https://yarmiplay.github.io/YarmiplayServerTV/privacy/>.

## License

GPL-3.0-or-later. Syncplay and Jellyfin are separate projects by their respective authors; this app
speaks the Syncplay protocol and downloads unmodified Jellyfin releases.
