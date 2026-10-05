# Linux packages

| Folder | What |
|---|---|
| `linux/` | AppStream metainfo and desktop file under the app ID `com.yarmiplay.servertv` (Flatpak, Snap), and `smoke-test.sh`, which starts an installed copy headless and checks that its servers answer |
| `aur/` | `PKGBUILD` of `yarmiplayservertv-bin`, which repackages the release `.deb`, and `test.sh` |
| `flatpak/` | The Flathub manifest, built from source offline, and `build.sh` |
| `../snap/` | `snapcraft.yaml`: the release `.deb` plus the pinned Jellyfin |

Every package tells the app who updates it, so the app hides its own updater: Flatpak and Snap through their
environment (`FLATPAK_ID`, `SNAP`), the AUR package through the file `/usr/lib/yarmiplayservertv/managed-by`
(any other distribution package can write its name there too). The Flatpak and the Snap have Jellyfin built in
at `lib/yarmiplayservertv/jellyfin`, from the same pinned, checksummed downloads as the Microsoft Store package
(`src-tauri/jellyfin-manifest.json`); the AUR package downloads it on first use like the `.deb`.

The release workflow publishes all three (see the table in [docs/store/README.md](../docs/store/README.md#linux-stores)),
and the Linux packages workflow builds, installs and smoke-tests them when anything here changes.

## Local builds

Docker is enough for all but the Snap.

```sh
# AUR: build, install and start the PKGBUILD in Arch
docker run --rm -v "$PWD:/repo:ro" archlinux:latest bash /repo/packaging/aur/test.sh

# Flatpak: generate the offline sources and build into $BUILD_ROOT/repo
docker run --rm --privileged -v "$PWD:/repo" -v flatpak-cache:/work -e HOME=/work/home \
  -e BUILD_ROOT=/work/out -e TOOLS_DIR=/work/tools ubuntu:24.04 bash -c \
  "apt-get update && apt-get install -y flatpak git python3-venv dbus && mkdir -p /run/dbus &&
   dbus-daemon --system --fork && bash /repo/packaging/flatpak/build.sh"

# Snap (needs snapd and LXD or Multipass): put a .deb from npx tauri build --bundles deb at
# snap/local/yarmiplayservertv.deb, then
snapcraft pack && sudo snap install --dangerous yarmiplayservertv_*.snap
```

`build.sh` writes `app-source.json` (this checkout), `jellyfin-sources.json`, `cargo-sources.json`,
`node-sources.json` and `shared-modules/` next to the manifest; they're ignored by git. With
`APP_SOURCE=git TAG=v1.2.1 COMMIT=<sha> NO_BUILD=1` it writes them for a release instead, as the flathub job does.

Lint as Flathub does (`build.sh` installs `org.flatpak.Builder`):

```sh
flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest packaging/flatpak/com.yarmiplay.servertv.yml
flatpak run --command=flatpak-builder-lint org.flatpak.Builder repo "$BUILD_ROOT/repo"
```

## First Flathub submission

Once per app; afterwards the release workflow sends every version to the app's own Flathub repository.

1. Generate the files for the latest release:
   `APP_SOURCE=git TAG=v<version> COMMIT=$(git rev-parse v<version>^{commit}) NO_BUILD=1 packaging/flatpak/build.sh`
2. Fork [flathub/flathub](https://github.com/flathub/flathub), make a branch from its `new-pr` branch, add
   `com.yarmiplay.servertv.yml`, the four `.json` files and `shared-modules` as a git submodule
   (`git submodule add https://github.com/flathub/shared-modules.git`, checked out at the commit `build.sh` used),
   and open a pull request against `new-pr` titled "Add com.yarmiplay.servertv".
3. Answer the reviewers; comment `bot, build` to run a test build.
4. After the merge Flathub creates `flathub/com.yarmiplay.servertv` and invites you. Then set the variable
   `FLATHUB_REPO` and the secret `FLATHUB_TOKEN`, and allow auto-merge in that repository's settings so
   release pull requests merge themselves once their test build passes.
5. Verify the app on [Flathub's developer portal](https://flathub.org/) by serving the token it gives at
   `https://yarmiplay.com/.well-known/org.flathub.VerifiedApps.txt`, which earns the verified checkmark.
