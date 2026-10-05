#!/usr/bin/env bash
# smoke-test.sh for the installed snap, on a CI runner:
#   packaging/linux/snap-smoke-test.sh [snap name]
# A strict snap that uses devices only starts inside its own cgroup scope, which "snap run" asks the user's
# systemd for over the session bus, as in any desktop session. A CI job has neither, so this starts the user's
# systemd instance (lingering) and runs the smoke test as one of its services, with Jellyfin switched on.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
name=${1:-yarmiplayservertv}
uid=$(id -u)

sudo apt-get install -y -qq dbus-user-session > /dev/null
sudo loginctl enable-linger "$(id -un)"
export XDG_RUNTIME_DIR="/run/user/$uid"
for _ in $(seq 30); do
  if [ -S "$XDG_RUNTIME_DIR/bus" ]; then break; fi
  sleep 1
done
[ -S "$XDG_RUNTIME_DIR/bus" ] || { echo "::error::the user's D-Bus session didn't start"; exit 1; }

home="$HOME/snap-smoke"
rm -rf "$home"
systemd-run --user --pipe --wait --collect --quiet \
  --setenv=SMOKE_HOME="$home" --setenv=SMOKE_JELLYFIN=1 --setenv=SMOKE_SESSION_BUS=1 \
  --setenv=DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus" \
  --setenv=FAKE_PEER="$here/../../scripts/fake_peer.py" \
  bash -c 'bash "$1" "$2" && touch "$SMOKE_HOME/passed"' _ "$here/smoke-test.sh" "/snap/bin/$name"
[ -f "$home/passed" ] || { echo "::error::the snap's smoke test failed"; exit 1; }
