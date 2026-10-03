# Microsoft Store listing and Partner Center answers

Everything Partner Center asks for, ready to paste. The app is
[9P6B9C1KXFFQ](https://apps.microsoft.com/detail/9P6B9C1KXFFQ) (package family
`Yarmiplay.YarmiplayServerTV_yxgrjh3bzf9p8`). The package is the unsigned `.msix` from the Release run's
`windows-store-msix-<version>` artifact (or `scripts/make-msix.ps1` locally); the Store signs it. Its identity
in [src-tauri/msix/AppxManifest.xml](../../src-tauri/msix/AppxManifest.xml) matches Product management >
Product identity.

The Store package has Jellyfin built in: `make-msix.ps1` downloads the release pinned in
`src-tauri/jellyfin-manifest.json`, checks its SHA-256 and unpacks it into the package's `jellyfin` folder with
a `NOTICE.txt` pointing at the GPL sources. The Store copy never downloads Jellyfin itself (Store policy 10.2.2
doesn't allow apps to download code), so a new Jellyfin version reaches it with the next Store release.

## Files

| Partner Center field | File |
|---|---|
| Package | `src-tauri/target/msix/YarmiplayServerTV-<version>.msix` |
| Desktop screenshots (1366x768 or larger) | take them of the control panel: Dashboard, Syncplay, Jellyfin, Network |
| Privacy policy URL | `https://yarmiplay.github.io/YarmiplayServerTV/privacy/` (from `docs/privacy.md`) |

## Pricing and availability

**Markets:** all. **Visibility:** public. **Pricing:** free, no trial. **Release:** as soon as it passes
certification.

## Properties

**Category:** Photo & video (no subcategory), next to YarmiplayTV.
**Privacy policy URL:** the URL above. **Website:** `https://yarmiplay.github.io/YarmiplayServerTV/`
**Support contact info:** `https://github.com/Yarmiplay/YarmiplayServerTV/issues`
**Product declarations:** none apply (no accessibility claim, not for Xbox, installable on removable storage
is fine). **System requirements:** x64 processor; 2 GB memory minimum, 4 GB recommended (Jellyfin
transcoding wants more).

## Age ratings (IARC)

Category "All other app types". Answer **Yes** only to "users can interact or exchange content" (the Syncplay
server relays room chat between the people who connect) and, among its follow-ups, to "interactions can be
limited to invited friends" (server password). Everything else is No. The result is 3+ / Everyone with
"Users Interact".

## Store listing (English)

**Product name:** `YarmiplayServerTV` (the reserved name; also `DisplayName` in the manifest).

**Description:**

> YarmiplayServerTV hosts your own Syncplay server and Jellyfin server from the system tray. It's the companion
> to YarmiplayTV: one app on your computer gives your friends a room to watch in sync and a media library to
> stream from.
>
> SYNCPLAY SERVER
> • A Syncplay 1.7-compatible server built in: rooms, passwords, chat, readiness and shared playlists
> • Works with YarmiplayTV and the Syncplay desktop client
> • Encrypted connections (TLS) once a certificate is set up
>
> JELLYFIN BUILT IN
> • The official, unmodified Jellyfin server (with jellyfin-ffmpeg) comes with the app; switch it on and it runs
>   for you
> • Create the administrator account and add libraries right in the control panel
>
> FRIENDS OUTSIDE YOUR HOME
> • Optional UPnP port forwarding, off by default
> • Optional free HTTPS: a DuckDNS name with a Let's Encrypt certificate that renews itself
>
> Every server can be switched on and off from the control panel or the tray menu, and the control panel can
> only be reached from your own computer.
>
> No accounts, no ads, no tracking. YarmiplayServerTV is open source:
> https://github.com/Yarmiplay/YarmiplayServerTV
>
> Jellyfin and jellyfin-ffmpeg are free software under the GPL; their licence and source links are in the
> jellyfin folder of the app.
>
> YarmiplayServerTV is an independent app. It is not affiliated with or endorsed by the Syncplay project,
> Jellyfin, DuckDNS or Let's Encrypt, which are named only to describe compatibility. It doesn't provide any
> videos: you share your own media.

**Product features** (one per line, 200 characters max each):

- Syncplay 1.7-compatible server with rooms, passwords, chat and shared playlists
- The official Jellyfin server built in, ready to switch on
- Jellyfin setup and libraries in the control panel
- Optional UPnP port forwarding for friends outside your home network
- Optional free HTTPS with DuckDNS and Let's Encrypt
- Lives in the system tray and can start with Windows
- Control panel only reachable from your own computer
- No accounts, no ads, no tracking; open source

**Search terms** (7 max): `syncplay`, `jellyfin`, `media server`, `watch party`, `watch together`,
`yarmiplaytv`, `upnp`

**Copyright and trademark info:** `© 2026 Yarmiplay`

## Submission options

**Notes for certification** (on the separate Additional testing information page, not in Submission options):

> No account is needed. The app opens its control panel and lives in the system tray; closing the window keeps
> it running, and Quit in the tray menu exits. To try it, switch on "Syncplay server" on the Dashboard; any
> Syncplay client (or YarmiplayTV) can then connect to the "On your network" address shown. Switching on
> "Jellyfin server" starts the Jellyfin server that ships inside the package (the official, unmodified
> Jellyfin release; the app downloads no code) and keeps its data in the app's LocalState folder; the
> Jellyfin page then walks through creating the administrator account. Windows may ask to allow the servers
> through the firewall. UPnP and DuckDNS are optional and off by default.

**Restricted capability runFullTrust** (why it's needed, 500 characters max):

> YarmiplayServerTV is a desktop server app. It needs full trust to listen for Syncplay connections on the
> port the user picks, run and supervise the Jellyfin server process that ships in the package, read the
> media folders the user adds to Jellyfin, ask the router for UPnP port forwards, and store the DuckDNS token
> in Windows Credential Manager.

## Each release

1. Bump the version as usual (`src-tauri/tauri.conf.json`, `package.json`, `src-tauri/Cargo.toml`; the Store
   needs a higher package version than the last one) and push the version tag.
2. Download the `windows-store-msix-<version>` artifact from the Release run.
3. In Partner Center, open the app, choose **Update** on the published submission (it copies everything),
   replace the package under Packages, add "What's new" to the listing if you like, and submit.

The Store copy doesn't check GitHub for updates; the Store updates it. It keeps its settings and data in its
own `LocalState` folder, so it doesn't share them with a copy installed from the download page.
