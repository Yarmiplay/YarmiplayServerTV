# YarmiplayServerTV privacy policy

Effective 7 October 2026. This policy covers the YarmiplayServerTV app for Windows, macOS and Linux, including
the copy from the Microsoft Store.

## Summary

YarmiplayServerTV doesn't collect any data. It has no accounts, no analytics, no advertising and no crash
reporting, and the developer runs no servers that the app talks to. Every server and network feature is off
until you switch it on, and each one only talks to what it needs.

## What the app sends, and to whom

- **Updates:** only when you click **Check for updates** or switch on **Automatic updates**, the app downloads
  `latest.json` and then the new installer from the project's GitHub releases. The request contains nothing
  about you or your settings; like any download it reaches GitHub from your IP address, and GitHub handles it
  under the
  [GitHub Privacy Statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement).
  The copy from the Microsoft Store doesn't check: the Store updates it.
- **Jellyfin:** the first time you switch Jellyfin on, the app downloads Jellyfin and ffmpeg from
  `repo.jellyfin.org`. The copy from the Microsoft Store has Jellyfin built in and downloads nothing. Jellyfin
  then runs on your computer as its own program; see Jellyfin's documentation for what it does. The
  administrator account you create and the libraries you add stay in Jellyfin on your computer.
- **UPnP:** when you switch it on, the app asks your router on the local network to add and remove port
  forwards, and reads your public IP address from it.
- **DuckDNS and HTTPS:** when you switch it on, the app sends your DuckDNS token and public IP address to
  [DuckDNS](https://www.duckdns.org), requests certificates from
  [Let's Encrypt](https://letsencrypt.org/privacy/) (with the contact email if you enter one), and checks the
  DNS record through
  [Google Public DNS](https://developers.google.com/speed/public-dns/privacy) and
  [Cloudflare DNS](https://developers.cloudflare.com/1.1.1.1/privacy/public-dns-resolver/).
- **Syncplay server:** people you give the address to connect to it. The names they enter, file names,
  playback state and chat are passed between them and not stored; the app's log on your computer notes who
  joined and left which room.
- **File relay** (on by default, off in vanilla Syncplay mode): YarmiplayTV users in a room can play a video
  file that someone else in the room has. The file's bytes travel from that person's device through your
  computer to the people watching, and are cached on your computer's disk so each part is sent only once. The
  cache only holds what was relayed, is limited to the size you set, is deleted when the app starts, after a
  day without use, or when you click **Clear cache**, and is never sent anywhere else.
- **Sharing Jellyfin with Syncplay users** (off until you switch it on): the Syncplay server tells YarmiplayTV
  users that your Jellyfin is available and approves their Quick Connect sign-ins for a hidden guest account
  that can watch but not change anything. Their Jellyfin traffic can reach Jellyfin through the Syncplay port.
  Switching sharing off disables the guest account, which signs every guest out.

## What stays on your device

Your settings, the ACME account and certificates, Jellyfin (program, database, cache and logs), the file
relay cache and the app's log are stored only on your computer. The DuckDNS token and the Jellyfin sign-in are kept in the system
credential store (Windows Credential Manager, macOS Keychain or the Secret Service on Linux). Your media folders
are read by Jellyfin and never changed or uploaded by the app. The control panel can only be reached from your
own computer.

Uninstalling the Microsoft Store copy deletes its data folder. For the other installers, the folders are listed
in the [setup guide](https://github.com/Yarmiplay/YarmiplayServerTV#where-things-are-stored), and you can
delete them.

## Children

The app isn't directed at children. The Syncplay server relays chat between the people who connect to it.

## Changes and contact

Changes to this policy are published on this page and in the app's source repository,
[github.com/Yarmiplay/YarmiplayServerTV](https://github.com/Yarmiplay/YarmiplayServerTV). Questions can be
asked in its [issues](https://github.com/Yarmiplay/YarmiplayServerTV/issues).
