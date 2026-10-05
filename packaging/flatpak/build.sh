#!/usr/bin/env bash
# Generates the offline sources next to the manifest and builds the Flatpak into an OSTree repo next to it.
#   packaging/flatpak/build.sh                      this checkout
#   APP_SOURCE=git TAG=v1.2.1 COMMIT=<sha> packaging/flatpak/build.sh    a release, as Flathub builds it
#   NO_BUILD=1 ...                                  only generate the sources (the flathub job)
#   BUILD_ROOT=/somewhere/else ...                  build-dir, repo and cache there instead
#   FLATPAK_BUILDER=flatpak-builder ...             a native flatpak-builder instead of Flathub's (see below)
# Needs git, python3 with venv, flatpak and a D-Bus system bus; Flathub's remote and org.flatpak.Builder are
# added for the user.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
dir="$repo/packaging/flatpak"
tools_rev=74697c75b630d7330e77250fc13cb5ea688d9479
cd "$repo"

args=(--app-source "${APP_SOURCE:-dir}")
if [ "${APP_SOURCE:-dir}" = git ]; then args+=(--tag "$TAG" --commit "$COMMIT"); fi
python3 scripts/linux_packaging.py flatpak-sources "${args[@]}"

tools="${TOOLS_DIR:-$HOME/.cache/flatpak-builder-tools}"
if [ ! -d "$tools/.git" ]; then git clone --quiet https://github.com/flatpak/flatpak-builder-tools.git "$tools"; fi
git -C "$tools" fetch --quiet origin "$tools_rev"
git -C "$tools" checkout --quiet "$tools_rev"
venv="$tools/.venv"
if ! { [ -x "$venv/bin/flatpak-node-generator" ] && "$venv/bin/python" -c "import aiohttp, tomlkit" 2> /dev/null; }; then
  rm -rf "$venv"
  python3 -m venv "$venv"
  "$venv/bin/pip" install --quiet aiohttp tomlkit "$tools/node"
fi
"$venv/bin/python" "$tools/cargo/flatpak-cargo-generator.py" src-tauri/Cargo.lock -o "$dir/cargo-sources.json"
"$venv/bin/flatpak-node-generator" npm package-lock.json -o "$dir/node-sources.json"

if [ "${NO_BUILD:-0}" = 1 ]; then exit 0; fi

out="${BUILD_ROOT:-$dir}"
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
# Flathub's own flatpak-builder and linter, since distributions may ship an older builder. Inside a container,
# where that nested sandbox misbehaves, FLATPAK_BUILDER=flatpak-builder uses a recent (1.4.9+) native one.
flatpak install --user -y --noninteractive flathub org.flatpak.Builder
read -ra builder <<< "${FLATPAK_BUILDER:-flatpak run org.flatpak.Builder}"
session=()
if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then session=(dbus-run-session --); fi
"${session[@]}" "${builder[@]}" --user --install-deps-from=flathub --force-clean --ccache \
  --mirror-screenshots-url=https://dl.flathub.org/media/ \
  --state-dir="$out/.flatpak-builder" --repo="$out/repo" "$out/build-dir" "$dir/com.yarmiplay.servertv.yml"
# What Flathub's build does with the screenshots, so "flatpak-builder-lint repo" judges them the same way
if [ -d "$out/build-dir/screenshots" ] && command -v ostree > /dev/null; then
  ostree commit --repo="$out/repo" --canonical-permissions --branch="screenshots/$(uname -m)" \
    "$out/build-dir/screenshots"
fi
