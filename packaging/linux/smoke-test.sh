#!/usr/bin/env bash
# Starts an installed YarmiplayServerTV headless (Xvfb and a private D-Bus session) with the Syncplay server on,
# then joins it with scripts/fake_peer.py. With SMOKE_JELLYFIN=1 it also switches Jellyfin on and waits for its
# health endpoint, which checks the Jellyfin built into the Flatpak and the Snap.
#
#   SMOKE_HOME=~/smoke packaging/linux/smoke-test.sh yarmiplayservertv
#   SMOKE_HOME=~/smoke packaging/linux/smoke-test.sh flatpak run --filesystem=~/smoke \
#     --env=YARMIPLAYSERVERTV_HOME=$HOME/smoke com.yarmiplay.servertv
#
# The app gets SMOKE_HOME as YARMIPLAYSERVERTV_HOME (a sandbox needs it passed in, as above). FAKE_PEER is the
# path of scripts/fake_peer.py when this script runs from a copy. Needs xvfb-run, dbus-run-session, python3, curl.
set -euo pipefail

home=${SMOKE_HOME:?set SMOKE_HOME to a folder the app can read and write}
fake_peer=${FAKE_PEER:-$(cd "$(dirname "$0")/../.." && pwd)/scripts/fake_peer.py}
syncplay_port=18999
jellyfin_port=18096

mkdir -p "$home/config"
if [ "${SMOKE_JELLYFIN:-0}" = 1 ]; then
  jellyfin=true
else
  jellyfin=false
fi
printf '{"syncplay":{"enabled":true,"port":%d},"jellyfin":{"enabled":%s,"httpPort":%d,"httpsPort":18920},"browser":{"enabled":false}}\n' \
  "$syncplay_port" "$jellyfin" "$jellyfin_port" > "$home/config/settings.json"

log=$(mktemp)
export YARMIPLAYSERVERTV_HOME="$home"
setsid dbus-run-session -- xvfb-run -a "$@" --minimized > "$log" 2>&1 &
pid=$!
cleanup() { kill -- "-$pid" 2> /dev/null || true; }
trap cleanup EXIT

fail() {
  echo "::error::$1"
  echo "--- app output"; cat "$log"
  if [ -d "$home/data/logs" ]; then echo "--- app logs"; tail -n 200 "$home"/data/logs/* || true; fi
  exit 1
}

alive() { kill -0 "$pid" 2> /dev/null; }

syncplay_ok=false
for _ in $(seq 90); do
  alive || fail "the app exited"
  if python3 "$fake_peer" --port "$syncplay_port" --room smoke \
      --script "wait 1; expect-room paused=true; quit" > /dev/null 2>&1; then
    syncplay_ok=true
    break
  fi
  sleep 2
done
$syncplay_ok || fail "the Syncplay server didn't answer on port $syncplay_port"
echo "Syncplay server answered on port $syncplay_port"

if [ "$jellyfin" = true ]; then
  for _ in $(seq 120); do
    alive || fail "the app exited"
    if [ "$(curl -fsS "http://127.0.0.1:$jellyfin_port/health" 2> /dev/null)" = Healthy ]; then
      echo "Jellyfin answered on port $jellyfin_port"
      exit 0
    fi
    sleep 2
  done
  fail "Jellyfin didn't become healthy on port $jellyfin_port"
fi
