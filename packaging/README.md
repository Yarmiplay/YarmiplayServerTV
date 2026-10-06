# Linux packages

| Folder | What |
|---|---|
| `linux/` | AppStream metainfo and desktop file under the app ID `com.yarmiplay.servertv`, and `smoke-test.sh`, which starts an installed copy headless and checks that its servers answer (`snap-smoke-test.sh` runs it for the snap on a CI runner) |
| `../snap/` | `snapcraft.yaml`: the release `.deb` plus the pinned Jellyfin |

The snap tells the app that the Snap Store updates it (through `SNAP`), so the app hides its own updater. Any
distribution package can do the same by writing its name into `/usr/lib/yarmiplayservertv/managed-by`. The snap
has Jellyfin built in at `usr/lib/yarmiplayservertv/jellyfin`, from the same pinned, checksummed downloads as the
Microsoft Store package (`src-tauri/jellyfin-manifest.json`), so nothing executable is downloaded into it.

The release workflow publishes the snap, and the Linux packages workflow builds, installs and smoke-tests it when
anything here changes.

## Local build

Needs snapd and LXD or Multipass. Put a `.deb` from `npx tauri build --bundles deb` at
`snap/local/yarmiplayservertv.deb`, then:

```sh
snapcraft pack
sudo snap install --dangerous yarmiplayservertv_*.snap
```

## One-time Snap Store setup

1. A [Snapcraft account](https://snapcraft.io/account) and the name: `snapcraft register yarmiplayservertv`
   (or [register it on the website](https://snapcraft.io/register-snap)).
2. Credentials for the release workflow, limited to this snap and to uploading and releasing:

   ```sh
   snapcraft export-login --snaps yarmiplayservertv \
     --acls package_access,package_push,package_update,package_release --expires 2099-12-31 snap-creds
   gh secret set SNAPCRAFT_STORE_CREDENTIALS < snap-creds && rm snap-creds
   ```

   Without snapcraft installed, the `ghcr.io/canonical/snapcraft:8_core24` Docker image runs the same command
   (`--entrypoint snapcraft`, with the output folder mounted). It asks for the account's password and 2FA code:
   the Snap Store no longer accepts browser (Candid) logins, so don't set `SNAPCRAFT_STORE_AUTH`.
3. The repository variable `SNAP_NAME` = `yarmiplayservertv` (and optionally `SNAP_CHANNEL`).
