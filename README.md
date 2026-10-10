# YarmiplayServerTV

Host your own **Syncplay server** and **Jellyfin server** from the system tray on Windows, macOS or Linux.
It's the companion to [YarmiplayTV](https://github.com/Yarmiplay/YarmiplayTV): one app on your computer
gives your friends a room to watch in sync and a media library to stream from.

**[Download YarmiplayServerTV](https://servertv.yarmiplay.com/)**, or on Windows get it from the
[Microsoft Store](https://apps.microsoft.com/detail/9P6B9C1KXFFQ) (Jellyfin built in, updated by the Store).

- A Syncplay 1.7-compatible server built in: rooms, passwords, chat, readiness, shared playlists, and
  encrypted connections (TLS) when a certificate is available. You can also admit only the YarmiplayTV
  devices you approve.
- Extras for YarmiplayTV on the same port: a file relay, so everyone in a room can play the file one
  person has, and one-tap access to your Jellyfin for the people on your Syncplay server. Official
  Syncplay clients see a normal server, and **Vanilla Syncplay mode** turns the extras off entirely.
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

1. Install YarmiplayServerTV from the [download page](https://servertv.yarmiplay.com/) (on Windows,
   the [Microsoft Store](https://apps.microsoft.com/detail/9P6B9C1KXFFQ) works too) and start it.
   It lives in the system tray; closing the window keeps the servers running. Use **Quit** in the tray
   menu to stop everything.
2. On the **Dashboard**, switch on the Syncplay server, the Jellyfin server, or both.
3. Give your friends the addresses shown on the dashboard. **On your network** works at home; **Over the
   internet** needs the steps in [Internet access](#internet-access).

**Start with system** (Dashboard, or the tray menu) starts the app minimized to the tray when you log in.

### Install on Linux

- [Snap Store](https://snapcraft.io/yarmiplayservertv): `sudo snap install yarmiplayservertv`
- Debian and Ubuntu: the `.deb` from the [download page](https://servertv.yarmiplay.com/),
  `sudo apt install ./YarmiplayServerTV.deb`
- Anything else: the AppImage from the download page, `chmod +x YarmiplayServerTV.AppImage` and run it

The Snap Store keeps the snap up to date and it has Jellyfin built in. It can reach your home folder and
removable drives for Jellyfin libraries, and keeps tokens in an owner-only file unless you allow it into your
keyring with `sudo snap connect yarmiplayservertv:password-manager-service`. On GNOME, the AppIndicator
extension is needed to see the tray icon.

### Updates

Copies from the Microsoft Store or the Snap Store are updated by those stores and don't show these controls.

**Check for updates** (Dashboard, or the tray menu) looks for a newer release here on GitHub and downloads
it; **Install** then restarts the app into the new version. Switch on **Automatic updates** on the Dashboard to
check every few hours and install new versions while nobody is using Syncplay. Copies installed with the
`.msi` or `.deb` package need administrator permission to update, so for those the new version is downloaded
and waits for you to click **Install**. Every update is checked against the app's update signing key before
it installs. Versions before 1.1.0 can't update themselves; install 1.1.0 or later from the download page once.

### Syncplay

The default port is 8999. On the **Syncplay** page you choose who can join, and set a welcome message
(Syncplay clients show it when they join, up to 2000 characters), isolated rooms (users only see their own
room), and switch off chat or readiness. Changing the port restarts the server and disconnects everyone.
The server reports itself as Syncplay 1.7.6.

**Who can join:**

- **Anyone** who has the address.
- **Password only:** every client needs the server password, including YarmiplayTV devices you approved.
  Switching to this mode disconnects anyone who got in with an approved device instead of the password.
- **Password or approved devices:** clients need the server password, except YarmiplayTV devices you
  approved.
- **Approved devices only:** only YarmiplayTV devices you approved. Official Syncplay clients are turned
  away with a message saying so. Switching to this mode disconnects everyone who isn't on an approved device.

When a YarmiplayTV device asks to join, it appears under **Devices** on the Syncplay page (and the Dashboard
says so) with its name, username, address and a code such as `3F2A-91BC-04DE-7710`. YarmiplayTV shows the same
code; compare them, then **Approve** or **Deny**. The device waits up to 10 minutes. Approved devices stay
approved until you **Remove** them, which also disconnects them. Each device keeps a key for this server only,
so your LAN address and DuckDNS name share one approval and other servers can't recognize the device.

Connect from YarmiplayTV or any Syncplay client with `address:port`, for example `192.168.1.20:8999`. When
DuckDNS and HTTPS are set up, connect with the DuckDNS name (`myname.duckdns.org:8999`) so clients can
verify the certificate and use an encrypted connection.

#### YarmiplayTV extras

YarmiplayTV clients announce themselves when they join and get a few extras. Other Syncplay clients never
see them. All of it runs on the Syncplay port: no extra port to forward.

- **File relay** (on by default): when someone in the room has the file that's playing and you don't,
  YarmiplayTV streams it from them through this computer. Each part is cached on this computer's disk, so it
  is sent only once however many people watch, and people who join later are served from the cache. Set
  the cache size on the **Syncplay** page (10 GB by default; at least 2 GB of the disk always stays free).
  The cache is emptied when the app starts, files unused for a day are dropped, and **Clear cache** empties
  it now. The **Rooms** list shows what's being relayed.
- **Jellyfin sharing** (off by default, on the **Jellyfin** page): YarmiplayTV users on the Syncplay
  server can add your Jellyfin with one tap. The app signs them in through Quick Connect as a hidden
  "Syncplay guests" account that can watch but can't delete or manage anything, and Jellyfin is reachable
  through the Syncplay port too. Switching sharing off disables that account, which signs every guest out.
  Set "Who can join" to a password option or approved devices only before you share, or anyone who finds the
  server can watch your libraries.
- **Vanilla Syncplay mode** makes the server behave exactly like the official one for everyone: no file
  relay, no Jellyfin sharing, no device approvals (approved devices need the password like everyone else),
  and nothing but Syncplay on its port. It can't be combined with **Approved devices only**. Switching it on
  takes effect right away.

The protocol is described in [docs/client-integration-prompt.md](docs/client-integration-prompt.md) and
[docs/yarmiplaytv-device-access-prompt.md](docs/yarmiplaytv-device-access-prompt.md).

### Jellyfin

The first time you switch Jellyfin on, the app downloads Jellyfin (about 150–250 MB, checked against
pinned SHA-256 checksums) from `repo.jellyfin.org`. The download includes ffmpeg for transcoding. The
Microsoft Store and Snap copies come with that same Jellyfin built in and download nothing.

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

Settings, approved Syncplay devices (`syncplay-devices.json`), the ACME account, certificates, Jellyfin
(program, database, cache and logs) and the file relay cache (`relay-cache`) are kept in the per-user app-data
folder; **Open data folder** on the dashboard opens it.

- Windows: `%APPDATA%\Yarmiplay\YarmiplayServerTV` (settings) and `%LOCALAPPDATA%\Yarmiplay\YarmiplayServerTV` (data)
- Windows, Microsoft Store copy: `%LOCALAPPDATA%\Packages\Yarmiplay.YarmiplayServerTV_<id>\LocalState`, which
  Windows deletes when you uninstall it. It doesn't share settings with a copy from the download page; don't run
  both at once, since they would want the same ports.
- macOS: `~/Library/Application Support/com.Yarmiplay.YarmiplayServerTV`
- Linux: `~/.config/yarmiplayservertv` and `~/.local/share/yarmiplayservertv`
- Linux, Snap: the same two under `~/snap/yarmiplayservertv/current/`

Tokens and passwords never appear in the logs; the **Logs** page shows the app's activity and Jellyfin's
warnings.

## Uninstalling

1. If you switched on **Start with system**, switch it off first, so no login entry is left behind.
2. Choose **Quit** in the tray menu. This stops Jellyfin and removes any UPnP port forwards the app made.
3. Remove the app:
   - Windows: **Settings > Apps > Installed apps**, then YarmiplayServerTV > **Uninstall**.
   - macOS: move YarmiplayServerTV from Applications to the Trash.
   - Linux: `sudo apt remove yarmiplay-server-tv`, or delete the AppImage. The snap:
     `sudo snap remove yarmiplayservertv`.
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

Tests: `cd src-tauri && cargo test` for the Rust side (Syncplay protocol and rooms, access modes and
approved devices, the YarmiplayTV extensions and file relay, UPnP, ACME, Jellyfin installer, configuration and sharing), `npm run check` for the
UI. CI also runs scripted clients (`scripts/fake_peer.py`) against the built-in server: two plain Syncplay
clients, and a seeder and a viewer (`--yarmiplay`) that pass a file through the relay.

`YARMIPLAYSERVERTV_HOME=<folder>` keeps all settings and data in another folder, which is handy for
testing. The Jellyfin version and checksums are pinned in `src-tauri/jellyfin-manifest.json`; regenerate
it with `python scripts/update_jellyfin_manifest.py`.

### Project layout

- `src-tauri/src/syncplay/`: the Syncplay server (protocol, rooms, TCP/STARTTLS listener), the YarmiplayTV
  extensions (`ext.rs`), approved devices (`devices.rs`) and the first-byte switch that serves HTTP on the same
  port (`mux.rs`).
- `src-tauri/src/relay/`: the file relay (offers, chunk cache, scheduler, HTTP endpoints).
- `src-tauri/src/jellyfin/`: Jellyfin download, process supervision, `network.xml`, the REST API client,
  and sharing with Syncplay users (guest account and reverse proxy).
- `src-tauri/src/net/`: UPnP port mapping, DuckDNS and ACME.
- `src-tauri/src/orchestrator.rs`: keeps every service in line with the settings.
- `ui/`: the control panel (Svelte).

### Releases

Bump the version with `python scripts/bump-version.py 1.3.0 --note "What changed."`, which writes it into
`src-tauri/tauri.conf.json`, `Cargo.toml`, `package.json`, the lock files and a new release entry in the
AppStream metainfo (`packaging/linux/com.yarmiplay.servertv.metainfo.xml`, shown by the Snap Store). Commit,
then push a matching tag:

```sh
git tag v1.3.0 && git push origin v1.3.0
```

With the repository variable `AUTO_TAG` set to `true` and a `RELEASE_TAG_TOKEN` secret (a fine-grained token
with Contents read and write on this repository), `.github/workflows/tag-on-version-bump.yml` pushes that tag
itself once the Build of a version bump on `main` is green.

`.github/workflows/release.yml` builds the installers for Windows, macOS and Linux and attaches them to the
tag's GitHub release together with `latest.json`, which installed copies read to update themselves
(`scripts/updater_manifest.py` writes it). Updates are signed with the updater key in the secrets
`TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`; its public half is
`plugins.updater.pubkey` in `src-tauri/tauri.conf.json`. Keep a backup of that key: without it, installed
copies can't receive updates anymore.

The app is [YarmiplayServerTV on the Microsoft Store](https://apps.microsoft.com/detail/9P6B9C1KXFFQ) (Store ID
`9P6B9C1KXFFQ`). The same run builds the Store package (`scripts/make-msix.ps1`, an unsigned `.msix` that the
Store signs, with Jellyfin built in) as the `windows-store-msix-<version>` artifact. Upload it in Partner Center as described in
[docs/store/README.md](docs/store/README.md), which also has the listing text. The app notices when it runs
from that package: it leaves updates to the Store, keeps its data in the package's `LocalState` folder and uses
the package's startup task for **Start with system**. `./scripts/make-msix.ps1 -Register` installs the package
locally for a test (needs Developer Mode).

After the GitHub release, the same workflow's snap job builds the snap from the release's `.deb`
(`snap/snapcraft.yaml`, with the metainfo and desktop file from `packaging/linux/`), checks that it starts, and
uploads it to the [Snap Store](https://snapcraft.io/yarmiplayservertv). It does nothing until the repository
variable `SNAP_NAME` is `yarmiplayservertv`, and needs the secret `SNAPCRAFT_STORE_CREDENTIALS`; the optional
variable `SNAP_CHANNEL` picks the channel (default `stable`). See [packaging/README.md](packaging/README.md) for
a local build and the one-time setup. The Linux packages workflow builds, installs and starts the snap whenever
its files change. To redo a store for the latest release, run the Release workflow by hand with only that
store's box ticked.

## Privacy

This program will not transfer any information to other networked systems unless specifically requested by
the user or the person installing or operating it. There is no telemetry or account. Every
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
- **Approved devices:** for each YarmiplayTV device you approve, the app keeps its public key, the name it
  sent, when it was approved and last seen, and the username it last used, on your computer only. Requests
  you haven't answered are kept in memory, with the device's IP address, and forgotten after a day or when
  the app quits.
- **File relay:** relayed video files pass through your computer and are cached on its disk until they go
  unused for a day, and never past a restart.
- **Jellyfin sharing:** when you switch it on, Syncplay users can sign in to your Jellyfin as a guest.

The full privacy policy is [docs/privacy.md](docs/privacy.md), published at
<https://servertv.yarmiplay.com/privacy/>.

## License

GPL-3.0-or-later. Syncplay and Jellyfin are separate projects by their respective authors; this app
speaks the Syncplay protocol and downloads unmodified Jellyfin releases.
