# YarmiplayServerTV Setup Guide

YarmiplayServerTV lets you host your own **Syncplay server** and **Jellyfin server** directly from your computer's system tray on Windows, macOS, and Linux. It pairs with [YarmiplayTV](https://tv.yarmiplay.com/) so your friends and family get a private room to watch video together in sync and a media library to stream from.

> **Tip:** If you want your friends to connect over the internet from outside your home network, see the companion [Internet Access Guide](../internet-access/).

---

## Table of contents

- [Installation](#installation)
  - [Windows](#windows)
  - [macOS](#macos)
  - [Linux](#linux)
- [First run & dashboard](#first-run--dashboard)
  - [System tray operation](#system-tray-operation)
  - [Web control panel in a browser](#web-control-panel-in-a-browser)
  - [Start with system](#start-with-system)
- [Syncplay server](#syncplay-server)
  - [Configuring the port](#configuring-the-port)
  - [Access control (Who can join)](#access-control-who-can-join)
  - [Approving YarmiplayTV devices](#approving-yarmiplaytv-devices)
  - [Managed rooms & room operators](#managed-rooms--room-operators)
- [YarmiplayTV extras](#yarmiplaytv-extras)
  - [File relay](#file-relay)
  - [Jellyfin sharing with room guests](#jellyfin-sharing-with-room-guests)
  - [Plex sharing with friend invites](#plex-sharing-with-friend-invites)
  - [Vanilla Syncplay mode](#vanilla-syncplay-mode)
- [Jellyfin media server](#jellyfin-media-server)
  - [Automatic zero-config setup](#automatic-zero-config-setup)
  - [Automatic admin session recovery](#automatic-admin-session-recovery)
  - [Adding libraries](#adding-libraries)
  - [Sort into shows](#sort-into-shows)
- [Plex Media Server integration](#plex-media-server-integration)
- [Updates](#updates)
- [Where things are stored](#where-things-are-stored)
- [Uninstalling](#uninstalling)

---

## Installation

### Windows

You have two easy ways to install on Windows 10 or 11 (64-bit):

1. **Microsoft Store (Recommended):** Install from the [Microsoft Store](https://apps.microsoft.com/detail/9P6B9C1KXFFQ). The Store edition has Jellyfin pre-bundled (no extra downloads needed) and updates automatically in the background.
2. **Standalone installer (.exe):** Download it from the [download page](../#download). It upgrades an older .msi install (Windows asks once) and keeps your settings. If Windows SmartScreen displays a warning, click **More info** and select **Run anyway**. Standalone copies check for updates and update themselves with your permission. A .msi is still published so older installs can update.

### macOS

Supported on macOS 11 (Big Sur) or newer on both Apple Silicon (M1/M2/M3/M4) and Intel Macs:

1. Download the disk image (`.dmg`) from the [download page](../#download).
2. Open the disk image and drag **YarmiplayServerTV** into your **Applications** folder.
3. The first time you launch the app, right-click (or Control-click) YarmiplayServerTV in Applications and choose **Open**, then confirm with **Open**.
4. If macOS reports the application is damaged, open Terminal and run:
   ```sh
   xattr -cr /Applications/YarmiplayServerTV.app
   ```

### Linux

Packages are provided for 64-bit Linux distributions with a desktop environment that supports system tray icons:

- **Snap Store (Recommended):**
  ```sh
  sudo snap install yarmiplayservertv
  ```
  The Snap edition includes Jellyfin pre-packaged and updates automatically. If you want it to store passwords in your desktop keyring rather than an owner-only file, run:
  ```sh
  sudo snap connect yarmiplayservertv:password-manager-service
  ```
- **Debian / Ubuntu (.deb):** Download the `.deb` package and install it via:
  ```sh
  sudo apt install ./YarmiplayServerTV.deb
  ```
- **Standalone AppImage:** Download the `.AppImage`, make it executable, and run it:
  ```sh
  chmod +x YarmiplayServerTV.AppImage
  ./YarmiplayServerTV.AppImage
  ```

> **Note for GNOME users:** Install the **AppIndicator and KStatusNotifierItem Support** GNOME Shell extension to view the system tray icon.

---

## First run & dashboard

### System tray operation

When launched, YarmiplayServerTV minimizes to your system tray (menu bar on macOS). Closing the control panel window leaves the servers running uninterrupted in the background.

Click the tray icon anytime to bring up the control panel, or right-click to view the context menu:
- **Show Window:** Brings the control panel to the foreground.
- **Open in Browser:** Opens the web-based control panel in your default browser.
- **Syncplay / Jellyfin:** Quick toggles to start or stop individual servers.
- **Quit:** Gracefully stops all active servers and quits the application.

### Web control panel in a browser

You can manage your servers from any web browser on your computer:
1. Right-click the tray icon and select **Open in Browser**.
2. This connects securely to `127.0.0.1:8097` using a single-use authentication token.
3. The web control panel is strictly bound to `127.0.0.1` (localhost), ensuring no one on your local network or the internet can ever reach or modify your server settings.

### Start with system

Toggle **Start with system** on the Dashboard or via the tray context menu to have YarmiplayServerTV start automatically and quietly in the tray whenever you log in to your computer.

---

## Syncplay server

The built-in Syncplay server is fully compatible with official Syncplay 1.7 clients as well as YarmiplayTV.

### Configuring the port

The default Syncplay port is `8999`. You can change this on the **Syncplay** page. Changing the port will restart the Syncplay server and disconnect any active clients.

### Access control (Who can join)

You can configure exactly who is permitted to connect to your Syncplay server:

- **Anyone:** Anyone who knows your address can connect and join any room.
- **Password only:** All users must provide the server password you define.
- **Password or approved devices:** Friends with YarmiplayTV devices you have approved connect seamlessly without typing a password; other clients must enter the password. **Approve devices that join with the password** is on by default: a YarmiplayTV device that enters the password is added to the approved list, so it can join without the password next time.
- **Approved devices only:** Only explicitly approved YarmiplayTV devices can join. Standard Syncplay clients without approval keys are turned away.

### Approving YarmiplayTV devices

When a friend attempts to connect using YarmiplayTV under an approved-device access policy:
1. The device presents a verification code on screen (e.g. `3F2A-91BC-04DE-7710`).
2. A pending approval notification appears on your **Dashboard** and **Syncplay** page showing the matching code, device name, username, and IP address.
3. Compare the codes and click **Approve** or **Deny**.
4. Once approved, the client saves a cryptographically signed device key unique to your server. It will remain approved across server restarts and updates until you click **Remove**.

### Managed rooms & room operators

Managed rooms give room operators exclusive control over play, pause, seeking, and the shared playlist—ideal for large movie watch parties:
- Any user can create a managed room from Syncplay (**Room → Create managed room**) or you can create one directly from the **Syncplay** page in the control panel.
- Managed rooms are assigned a secure operator password. Anyone who knows the password can become an operator by choosing **Identify as room operator**.
- Room passwords persist across server restarts. Click **Reset all managed-room passwords** if you ever wish to revoke existing operator passwords.

---

## YarmiplayTV extras

When connecting with YarmiplayTV, clients unlock powerful integrated features on the same Syncplay port with no extra configuration:

### File relay

When watching a video file that only one person in the room possesses, the **File relay** feature automatically streams parts of the video from that user's device through your server to other viewers.
- **Intelligent chunk caching:** Relayed video chunks are cached locally on your disk so that identical chunks are never downloaded more than once, even if five people join the room at different times.
- **Disk limits:** You can set the maximum relay cache size on the Syncplay page (10 GB default). YarmiplayServerTV guarantees at least 2 GB of free disk space remains untouched.
- **Automatic cleanup:** The cache is cleaned on application start and whenever files have gone unused for 24 hours. You can also click **Clear cache** at any time.

### Jellyfin sharing with room guests

Share your home media library with friends in your watch room without handing out your personal credentials:
- Toggle **Share with Syncplay users** on the **Jellyfin** page.
- YarmiplayTV clients in your Syncplay rooms can connect to your Jellyfin in one tap.
- The server provisions a restricted guest account via Jellyfin Quick Connect that can stream media but cannot delete files or modify server configurations.
- Turning off Jellyfin sharing instantly revokes the guest account and logs out all guest sessions.

### Plex sharing with friend invites

If you run a Plex Media Server, you can invite room members directly:
- Turn on **Share with Syncplay users** on the **Plex** page.
- When a room member requests access, your server automatically sends an invite to their `plex.tv` username granting access to the specific Plex libraries you selected.
- Turning off Plex sharing automatically withdraws the invited shares without affecting friends you added manually in Plex.

### Vanilla Syncplay mode

If you only want standard Syncplay functionality without any YarmiplayTV extensions, turn on **Vanilla Syncplay mode**. This disables file relaying, voice chat, Jellyfin sharing, Plex sharing, and device key negotiations, giving you a 100% standard Syncplay server.

**Voice chat** (on by default) lets people in a room talk through this PC. Audio is not recorded. It listens on UDP 9000, with TCP 9000 as a fallback, and browsers use a WebSocket on the Syncplay port. With Syncplay's UPnP switch on, those voice ports are forwarded too. Either this switch or the switch in YarmiplayTV being off means no voice connection.

---

## Jellyfin media server

YarmiplayServerTV makes running Jellyfin effortless.

### Automatic zero-config setup

On first launch (for standalone packages), YarmiplayServerTV downloads an official, unmodified Jellyfin build with ffmpeg transcoder support from `repo.jellyfin.org`, verifies its cryptographic checksum, and starts it on port `8096` (HTTP) and `8920` (HTTPS). Microsoft Store and Linux Snap packages already have Jellyfin included.

You do not need to walk through Jellyfin's initial setup wizard. The control panel automatically configures language settings, remote access permissions, and Quick Connect support.

### Automatic admin session recovery

YarmiplayServerTV **owns and manages Jellyfin**:
- On initial run, create your administrator username and password in the control panel.
- The server stores the authentication token securely in your operating system's credential vault.
- If the token is ever lost or Jellyfin is restarted, YarmiplayServerTV queries Jellyfin's local database directly and seamlessly re-provisions an administrator session. You never get locked out.

### Adding libraries

1. On the **Jellyfin** page, click **Add library**.
2. Give your library a display name, choose the content type (Movies, TV Shows, Music, etc.), and select the folder on your hard drive where the media is stored.
3. Click **Refresh libraries** whenever you add new media files. Jellyfin scans every library, and the list on the page reloads. A library whose folders are missing or empty is removed from the list. The files themselves are left where they are.

### Sort into shows

Loose episode downloads in a folder can appear disorganized in media centers. YarmiplayServerTV includes a built-in **Sort into shows** organizer:
- Analyzes filenames in standard scene naming (`Show.S01E05`), numbered release conventions (`[Group] Show - 05 (1080p).mkv`), and episode prefixes (`Show Ep 5`).
- Shows you an exact preview of planned moves into structured directories (`Show Name/Season 01/`).
- Moves matching subtitle files (`.srt`, `.ass`) along with the video.
- Preserves Syncplay file hashes and never overwrites existing files.
- Enable **Keep sorted** to monitor your download folder and organize incoming files automatically once they have finished downloading.

---

## Plex Media Server integration

If you prefer Plex Media Server, you can install it from [plex.tv](https://www.plex.tv/media-server-downloads/):
1. Switch on **Plex Media Server** on the **Plex** page. YarmiplayServerTV detects when Plex is running locally on port `32400`.
2. Click **Link your Plex account**. The control panel displays a 4-letter link code.
3. Visit [plex.tv/link](https://plex.tv/link) in your browser, enter the code, and confirm.
4. Manage which Plex libraries are visible and shared with your Syncplay watch parties.

---

## Updates

- **Microsoft Store & Snap:** Automatically kept up-to-date by your operating system.
- **Standalone Windows (.msi / .exe) & Linux (.deb / AppImage):**
  - Click **Check for updates** on the Dashboard or tray menu to check for new releases.
  - Turn on **Automatic updates** to check periodically and automatically update when no active Syncplay rooms are in use.
  - Every update binary is cryptographically verified against the developer's release signing key before installation.

---

## Where things are stored

Your media files are never moved or modified without your permission. Configuration files, databases, and logs are kept in standard per-user directories:

- **Windows:**
  - Settings: `%APPDATA%\Yarmiplay\YarmiplayServerTV`
  - Jellyfin data, database & relay cache: `%LOCALAPPDATA%\Yarmiplay\YarmiplayServerTV`
- **Windows (Microsoft Store):**
  - Isolated inside `%LOCALAPPDATA%\Packages\Yarmiplay.YarmiplayServerTV_<id>\LocalState`
- **macOS:**
  - `~/Library/Application Support/com.Yarmiplay.YarmiplayServerTV`
- **Linux:**
  - Settings: `~/.config/yarmiplayservertv`
  - Data: `~/.local/share/yarmiplayservertv`
- **Linux (Snap):**
  - `~/snap/yarmiplayservertv/current/`

Authentication tokens (DuckDNS API token, Jellyfin admin session, and Plex tokens) are saved securely in your operating system's native credential vault:
- Windows Credential Manager
- macOS Keychain
- Linux Secret Service (libsecret / GNOME Keyring / KWallet)

---

## Uninstalling

To cleanly remove YarmiplayServerTV:
1. If enabled, toggle off **Start with system** on the Dashboard.
2. Select **Quit** from the system tray menu to stop all servers and cleanly release any active UPnP port mappings.
3. Uninstall the app through your operating system:
   - **Windows:** Open **Settings > Apps > Installed apps**, find YarmiplayServerTV, and select **Uninstall**.
   - **macOS:** Drag YarmiplayServerTV from Applications to Trash.
   - **Linux:** Run `sudo apt remove yarmiplay-server-tv` or `sudo snap remove yarmiplayservertv`.
4. (Optional) Delete the data folders listed in [Where things are stored](#where-things-are-stored) to remove all server settings, Jellyfin databases, and cached files.
