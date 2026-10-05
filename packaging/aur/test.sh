#!/usr/bin/env bash
# Builds and installs packaging/aur/PKGBUILD in an Arch Linux container, then runs the smoke test:
#   docker run --rm -v "$PWD:/repo:ro" archlinux:latest bash /repo/packaging/aur/test.sh
# REPO points elsewhere than /repo (the release workflow's checkout).
set -euo pipefail
repo="${REPO:-/repo}"

pacman -Syu --noconfirm --needed base-devel xorg-server-xvfb xorg-xauth dbus python curl > /dev/null
id builder > /dev/null 2>&1 || useradd -m builder
echo 'builder ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/builder

work=/home/builder/aur
rm -rf "$work"
mkdir -p "$work"
cp "$repo/packaging/aur/PKGBUILD" "$work/"
chown -R builder: "$work"
su builder -c "cd $work && makepkg -si --noconfirm"

pkgname=$(sed -n 's/^pkgname=//p' "$work/PKGBUILD")
test -x /usr/bin/yarmiplayservertv
test "$(cat /usr/lib/yarmiplayservertv/managed-by)" = aur
pacman -Qi "$pkgname" | grep -E '^(Name|Version|Depends On)'

cp -r "$repo/packaging/linux/smoke-test.sh" "$repo/scripts/fake_peer.py" /home/builder/
chown builder: /home/builder/smoke-test.sh /home/builder/fake_peer.py
su builder -c "SMOKE_HOME=/home/builder/smoke FAKE_PEER=/home/builder/fake_peer.py bash /home/builder/smoke-test.sh yarmiplayservertv"
