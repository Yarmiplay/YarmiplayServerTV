#!/usr/bin/env python3
"""
Scriptable Syncplay peer for testing YarmiplayTV without a desktop client.

It speaks the same protocol as Syncplay 1.7 (plain TCP), keeps a virtual playback clock,
follows the room like a real client and prints what the room does. Standard library only.

Examples:
  python scripts/fake_peer.py --room tvtest --file "Show - S01E01.mkv" --size 123 --duration 1422
  python scripts/fake_peer.py --room tvtest --script "wait 3; playlist A.mkv; select 0; wait 8; ready; play; wait 10; seek 300; wait 5; pause"

Script / interactive commands (separated by ';' or one per line on stdin):
  wait <s>                 sleep
  playlist <f1>|<f2>|...   replace the shared playlist
  add <file>               append to the shared playlist
  select <index>           select a playlist entry (everyone switches to it)
  file <name> [size] [dur] report the file we are "playing"
  play | pause             unpause / pause the room
  seek <seconds>           seek the room
  drift <seconds>          shift our clock without telling the room (simulates a lagging player)
  ready | unready          set readiness
  chat <text>              send a chat message
  status                   print the room as we see it
  expect-user <name> <key>=<value>   fail unless the user's field matches (file, ready)
  expect-room <key>=<value> [within=<s>]  e.g. paused=false, position~300 (±3s)
  quit

With --yarmiplay the peer declares the YarmiplayServerTV extensions (docs/client-integration-prompt.md)
and answers the server's upload requests for the files it offers:
  offer <path>[|<path>...]  offer local files to the room's file relay
  expect-files <n> [within=<s>]  fail unless the room's relay lists at least n files
  fetch <name> <out> [stream|download]  download a relayed file over HTTP into <out>
  expect-same <a> <b>       fail unless two local files have the same contents
"""
import argparse
import hashlib
import json
import os
import socket
import sys
import threading
import time
import urllib.request

MIB = 1 << 20


def quick_hash(path):
    """SHA-256 over the first and last MiB and the size (u64 little endian), as the server expects."""
    size = os.path.getsize(path)
    m = min(MIB, size)
    with open(path, "rb") as f:
        first = f.read(m)
        f.seek(size - m)
        last = f.read(m)
    return hashlib.sha256(first + last + size.to_bytes(8, "little")).hexdigest()


def file_sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(MIB), b""):
            h.update(block)
    return h.hexdigest()

VERSION = "1.2.255"
REAL_VERSION = "1.7.6"


def log(msg):
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


class Peer:
    def __init__(self, host, port, name, room, password=None, verbose=False, yarmiplay=False):
        self.host, self.port, self.name, self.room = host, port, name, room
        self.password = password
        self.verbose = verbose
        self.yarmiplay = yarmiplay
        self.sock = None
        self.send_lock = threading.Lock()
        # extension state
        self.token = None
        self.capabilities = {}
        self.relay_files = []
        self.offered = {}  # (size, quickHash) -> local path
        self.lock = threading.RLock()
        # virtual player
        self.paused = True
        self.pos = 0.0
        self.pos_at = time.time()
        self.file = None
        # protocol state
        self.client_ignoring = 0
        self.server_ignoring = 0
        self.last_latency_calc = None
        self.rtt = 0.0
        self.users = {}
        self.playlist = []
        self.playlist_index = None
        self.global_paused = True
        self.global_pos = 0.0
        self.global_set_by = None
        self.synced = False
        self.logged_in = threading.Event()
        self.failed = False
        self.running = True

    # --- virtual player -------------------------------------------------------------
    def position(self):
        with self.lock:
            if self.paused:
                return self.pos
            return self.pos + (time.time() - self.pos_at)

    def set_position(self, p):
        with self.lock:
            self.pos = max(0.0, p)
            self.pos_at = time.time()

    def set_paused(self, paused):
        with self.lock:
            self.pos = self.position()
            self.pos_at = time.time()
            self.paused = paused

    # --- networking -------------------------------------------------------------------
    def connect(self):
        self.sock = socket.create_connection((self.host, self.port), timeout=10)
        self.sock.settimeout(None)
        threading.Thread(target=self.reader, daemon=True).start()
        hello = {
            "username": self.name,
            "room": {"name": self.room},
            "version": VERSION,
            "realversion": REAL_VERSION,
            "features": {"sharedPlaylists": True, "chat": True, "readiness": True, "featureList": True,
                         "managedRooms": True, "uiMode": "CLI"},
        }
        if self.yarmiplay:
            hello["features"]["yarmiplay"] = {"protocol": 1}
        if self.password:
            hello["password"] = hashlib.md5(self.password.encode()).hexdigest()
        self.send({"Hello": hello})
        if not self.logged_in.wait(10):
            raise RuntimeError("no Hello from server")

    def send(self, obj):
        line = json.dumps(obj)
        if self.verbose:
            log(f">> {line}")
        with self.send_lock:
            self.sock.sendall((line + "\r\n").encode())

    def reader(self):
        buf = b""
        try:
            while self.running:
                chunk = self.sock.recv(65536)
                if not chunk:
                    log("server closed the connection")
                    break
                buf += chunk
                while b"\n" in buf:
                    raw, buf = buf.split(b"\n", 1)
                    raw = raw.strip()
                    if raw:
                        self.handle(json.loads(raw.decode()))
        except OSError as e:
            if self.running:
                log(f"connection error: {e}")
        self.running = False

    # --- incoming ------------------------------------------------------------------------
    def handle(self, msg):
        if self.verbose:
            log(f"<< {json.dumps(msg)}")
        for cmd, val in msg.items():
            if cmd == "Hello":
                self.name = val.get("username", self.name)
                log(f"joined room '{self.room}' as {self.name} (server {val.get('realversion')})")
                self.logged_in.set()
                self.send({"Set": {"ready": {"isReady": False, "manuallyInitiated": False}}})
                self.send_file()
                self.send({"List": None})
            elif cmd == "Set":
                self.handle_set(val)
            elif cmd == "List":
                room = val.get(self.room, {})
                with self.lock:
                    self.users = {n: {"file": u.get("file") or None, "ready": u.get("isReady")} for n, u in room.items()}
            elif cmd == "State":
                self.handle_state(val)
            elif cmd == "Chat":
                log(f"CHAT {val.get('username')}: {val.get('message')}")
            elif cmd == "Error":
                log(f"ERROR from server: {val}")
                self.failed = True
            elif cmd == "Yarmiplay":
                self.handle_ext(val)

    def handle_ext(self, val):
        for key, v in val.items():
            if key == "session":
                self.token = v.get("token")
                log(f"extension session (protocol {v.get('protocol')})")
            elif key == "capabilities":
                self.capabilities = v
                log(f"capabilities: {v}")
            elif key == "files":
                with self.lock:
                    self.relay_files = v
                log("relay files: " + (", ".join(f"{f['name']} ({f['sources']} sources, {f['cachedBytes']} cached)"
                                                 for f in v) or "none"))
            elif key == "upload":
                threading.Thread(target=self.upload, args=(v,), daemon=True).start()
            elif key == "uploadCancel":
                log(f"upload {v.get('id')} cancelled")
            elif key == "jellyfin":
                log(f"jellyfin: {v}")
            elif key == "jellyfinAuthorize":
                log(f"Quick Connect {v.get('code')}: {'approved' if v.get('ok') else v.get('error')}")

    def http_url(self, path):
        return f"http://{self.host}:{self.port}{path}"

    def upload(self, req):
        path = self.offered.get((req.get("size"), req.get("quickHash")))
        if not path:
            self.send({"Yarmiplay": {"uploadFailed": {"id": req["id"], "error": "not offered"}}})
            return
        with open(path, "rb") as f:
            f.seek(req["offset"])
            data = f.read(req["length"])
        r = urllib.request.Request(self.http_url(f"/yarmiplay/upload/{req['id']}"), data=data, method="PUT",
                                   headers={"Authorization": f"Bearer {self.token}"})
        try:
            with urllib.request.urlopen(r, timeout=60) as resp:
                log(f"uploaded {len(data)} bytes at {req['offset']} (HTTP {resp.status})")
        except Exception as e:  # the server may cancel; it re-requests elsewhere
            log(f"upload {req['id']} failed: {e}")

    def handle_set(self, val):
        for key, v in val.items():
            if key == "user":
                for name, info in v.items():
                    ev = info.get("event", {})
                    f = info.get("file")
                    with self.lock:
                        if "left" in ev:
                            self.users.pop(name, None)
                            log(f"{name} left")
                            continue
                        u = self.users.setdefault(name, {"file": None, "ready": None})
                        if f:
                            u["file"] = f
                            if name != self.name:
                                log(f"{name} is playing '{f.get('name')}' size={f.get('size')} duration={f.get('duration')}")
                        if "joined" in ev and name != self.name:
                            log(f"{name} joined")
            elif key == "ready":
                name = v.get("username")
                with self.lock:
                    self.users.setdefault(name, {"file": None, "ready": None})["ready"] = v.get("isReady")
                if name != self.name:
                    log(f"{name} is {'ready' if v.get('isReady') else 'not ready'}")
            elif key == "playlistChange":
                self.playlist = v.get("files", [])
                log(f"playlist changed by {v.get('user')}: {self.playlist}")
            elif key == "playlistIndex":
                self.playlist_index = v.get("index")
                log(f"playlist index -> {self.playlist_index} (by {v.get('user')})")

    def handle_state(self, val):
        ignore = val.get("ignoringOnTheFly", {})
        if "server" in ignore:
            self.server_ignoring = ignore["server"]
            self.client_ignoring = 0
        elif "client" in ignore and ignore["client"] == self.client_ignoring:
            self.client_ignoring = 0
        ping = val.get("ping", {})
        self.last_latency_calc = ping.get("latencyCalculation")
        if "clientLatencyCalculation" in ping and ping["clientLatencyCalculation"]:
            self.rtt = time.time() - ping["clientLatencyCalculation"]
        ps = val.get("playstate")
        if ps is not None and self.client_ignoring == 0:
            pos, paused, do_seek, set_by = ps.get("position", 0.0), ps.get("paused", True), ps.get("doSeek", False), ps.get("setBy")
            if paused != self.global_paused and set_by and set_by != self.name:
                log(f"ROOM {'paused' if paused else 'unpaused'} by {set_by} at {pos:.1f}")
            if do_seek and set_by and set_by != self.name:
                log(f"ROOM seek to {pos:.1f} by {set_by}")
            self.global_paused, self.global_pos, self.global_set_by = paused, pos, set_by
            # adopt the room position on join, then follow like a real client (only when ahead)
            if not self.synced or do_seek or self.position() - pos > 4:
                self.set_position(pos)
                self.synced = True
            if paused != self.paused:
                self.set_paused(paused)
                if not paused:
                    self.set_position(pos)
        self.reply_state()

    def reply_state(self, state_change=False, do_seek=False):
        state = {}
        if self.client_ignoring == 0 or self.server_ignoring != 0 or state_change:
            ps = {"position": self.position(), "paused": self.paused}
            if do_seek:
                ps["doSeek"] = True
            state["playstate"] = ps
        ping = {"clientLatencyCalculation": time.time(), "clientRtt": self.rtt}
        if self.last_latency_calc:
            ping["latencyCalculation"] = self.last_latency_calc
        state["ping"] = ping
        if state_change:
            self.client_ignoring += 1
        if self.server_ignoring or self.client_ignoring:
            ig = {}
            if self.server_ignoring:
                ig["server"] = self.server_ignoring
                self.server_ignoring = 0
            if self.client_ignoring:
                ig["client"] = self.client_ignoring
            state["ignoringOnTheFly"] = ig
        self.send({"State": state})

    # --- commands ---------------------------------------------------------------------------
    def send_file(self):
        if self.file:
            name, size, dur = self.file
            self.send({"Set": {"file": {"name": name, "size": size, "duration": dur}}})

    def do(self, line):
        parts = line.strip().split(" ", 1)
        if not parts or not parts[0]:
            return True
        cmd, arg = parts[0].lower(), (parts[1].strip() if len(parts) > 1 else "")
        if cmd == "wait":
            time.sleep(float(arg))
        elif cmd == "playlist":
            files = [f.strip() for f in arg.split("|") if f.strip()]
            self.send({"Set": {"playlistChange": {"files": files}}})
        elif cmd == "add":
            self.send({"Set": {"playlistChange": {"files": self.playlist + [arg]}}})
        elif cmd == "select":
            self.send({"Set": {"playlistIndex": {"index": int(arg)}}})
        elif cmd == "file":
            bits = arg.rsplit(" ", 2)
            name = bits[0] if len(bits) < 3 else bits[0]
            size = int(bits[1]) if len(bits) >= 2 else 0
            dur = float(bits[2]) if len(bits) == 3 else 0.0
            self.file = (name, size, dur)
            self.set_position(0)
            self.send_file()
        elif cmd in ("play", "pause"):
            self.set_paused(cmd == "pause")
            self.reply_state(state_change=True)
            log(f"we {'paused' if cmd == 'pause' else 'unpaused'} at {self.position():.1f}")
        elif cmd == "seek":
            self.set_position(float(arg))
            self.reply_state(state_change=True, do_seek=True)
            log(f"we seeked to {float(arg):.1f}")
        elif cmd == "drift":
            # Silently fall behind (negative) or jump ahead without telling the room, like a lagging player.
            self.set_position(self.position() + float(arg))
            log(f"drifted by {float(arg):+.1f}s to {self.position():.1f}")
        elif cmd in ("ready", "unready"):
            self.send({"Set": {"ready": {"isReady": cmd == "ready", "manuallyInitiated": True}}})
        elif cmd == "chat":
            self.send({"Chat": arg})
        elif cmd == "status":
            self.send({"List": None})
            time.sleep(0.5)
            self.print_status()
        elif cmd == "expect-user":
            name, cond = arg.split(" ", 1)
            return self.expect_user(name, cond)
        elif cmd == "expect-room":
            return self.expect_room(arg)
        elif cmd == "offer":
            files = []
            for p in (p.strip() for p in arg.split("|") if p.strip()):
                size, qh = os.path.getsize(p), quick_hash(p)
                self.offered[(size, qh)] = p
                files.append({"name": os.path.basename(p), "size": size, "duration": 0.0, "quickHash": qh})
            self.send({"Yarmiplay": {"offer": {"files": files}}})
            log(f"offered {len(files)} file(s)")
        elif cmd == "expect-files":
            return self.expect_files(arg)
        elif cmd == "fetch":
            return self.fetch(*arg.split())
        elif cmd == "expect-same":
            a, b = arg.split()
            if file_sha256(a) == file_sha256(b):
                log(f"PASS {a} matches {b}")
            else:
                log(f"FAIL {a} differs from {b}")
                self.failed = True
        elif cmd == "quit":
            return False
        else:
            log(f"unknown command: {cmd}")
        return True

    def print_status(self):
        with self.lock:
            log(f"room={self.room} paused={self.global_paused} pos={self.global_pos:.1f} (ours {self.position():.1f}) "
                f"setBy={self.global_set_by} playlist={self.playlist} index={self.playlist_index} rtt={self.rtt*1000:.0f}ms")
            for n, u in self.users.items():
                f = u.get("file") or {}
                log(f"   {n}: ready={u.get('ready')} file={f.get('name')} size={f.get('size')} dur={f.get('duration')}")

    def expect_user(self, name, cond):
        key, want = cond.split("=", 1)
        deadline = time.time() + 15
        got = None
        while time.time() < deadline:
            self.send({"List": None})
            time.sleep(1)
            with self.lock:
                u = next((v for n, v in self.users.items() if n.lower().startswith(name.lower())), None)
            if u is None:
                continue
            if key == "ready":
                got = str(u.get("ready")).lower()
            elif key in ("file", "size", "duration"):
                f = u.get("file") or {}
                got = str(f.get("name" if key == "file" else key))
            if got is not None and (got == want or (key == "duration" and abs(float(got or 0) - float(want)) < 2.5)):
                log(f"PASS user {name} {key}={got}")
                return True
        log(f"FAIL user {name} {key}: wanted {want}, got {got}")
        self.failed = True
        return True

    def expect_files(self, arg):
        bits = arg.split()
        want = int(bits[0])
        within = float(next((b.split("=", 1)[1] for b in bits[1:] if b.startswith("within=")), 10))
        deadline = time.time() + within
        while time.time() < deadline:
            with self.lock:
                n = len(self.relay_files)
            if n >= want:
                log(f"PASS relay lists {n} file(s)")
                return True
            time.sleep(0.25)
        log(f"FAIL relay lists {len(self.relay_files)} file(s), wanted {want}")
        self.failed = True
        return True

    def fetch(self, name, out, mode="stream"):
        with self.lock:
            f = next((f for f in self.relay_files if f["name"] == name), None)
        if f is None or not self.token:
            log(f"FAIL no relayed file named {name}")
            self.failed = True
            return True
        url = self.http_url(f"/yarmiplay/files/{f['id']}?mode={mode}")
        r = urllib.request.Request(url, headers={"Authorization": f"Bearer {self.token}"})
        start = time.time()
        got = 0
        with urllib.request.urlopen(r, timeout=120) as resp, open(out, "wb") as dst:
            for block in iter(lambda: resp.read(MIB), b""):
                dst.write(block)
                got += len(block)
        secs = max(time.time() - start, 1e-3)
        log(f"fetched {got} bytes of {name} in {secs:.1f}s ({got * 8 / secs / 1e6:.1f} Mbit/s)")
        if got != f["size"]:
            log(f"FAIL fetched {got} bytes, expected {f['size']}")
            self.failed = True
        return True

    def expect_room(self, arg):
        conds = arg.split()
        within = 10.0
        checks = []
        for c in conds:
            if c.startswith("within="):
                within = float(c.split("=", 1)[1])
            else:
                checks.append(c)
        deadline = time.time() + within
        while True:
            ok = True
            detail = []
            for c in checks:
                if "~" in c:
                    key, want = c.split("~", 1)
                    val = self.global_pos if key == "position" else None
                    good = val is not None and abs(val - float(want)) <= 3
                else:
                    key, want = c.split("=", 1)
                    val = {"paused": str(self.global_paused).lower(), "setby": str(self.global_set_by)}.get(key.lower())
                    good = val == want
                detail.append(f"{key}={val}")
                ok = ok and good
            if ok:
                log(f"PASS room {' '.join(detail)}")
                return True
            if time.time() > deadline:
                log(f"FAIL room expected {' '.join(checks)}, got {' '.join(detail)}")
                self.failed = True
                return True
            time.sleep(0.25)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", default="localhost")
    ap.add_argument("--port", type=int, default=8999)
    ap.add_argument("--name", default="DeskPeer")
    ap.add_argument("--room", required=True)
    ap.add_argument("--password")
    ap.add_argument("--file", help="file name to report")
    ap.add_argument("--size", type=int, default=0)
    ap.add_argument("--duration", type=float, default=0.0)
    ap.add_argument("--script", help="commands separated by ';' (otherwise read from stdin)")
    ap.add_argument("--verbose", action="store_true")
    ap.add_argument("--yarmiplay", action="store_true", help="declare the YarmiplayServerTV extensions")
    args = ap.parse_args()

    peer = Peer(args.host, args.port, args.name, args.room, args.password, args.verbose, args.yarmiplay)
    if args.file:
        peer.file = (args.file, args.size, args.duration)
    peer.connect()

    commands = args.script.split(";") if args.script else sys.stdin
    for line in commands:
        if not peer.running:
            break
        try:
            if not peer.do(line):
                break
        except Exception as e:  # keep going in interactive use
            log(f"command failed: {line.strip()}: {e}")
            peer.failed = True
    peer.running = False
    try:
        peer.sock.close()
    except OSError:
        pass
    if peer.failed:
        log("RESULT: FAILED")
        sys.exit(1)
    log("RESULT: OK")


if __name__ == "__main__":
    main()
