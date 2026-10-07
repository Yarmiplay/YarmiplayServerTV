# YarmiplayTV client integration prompt

This is a self-contained prompt for an agent working in the **YarmiplayTV** client repository
(`SyncplayTV`). It describes the extensions that YarmiplayServerTV (1.6.0 and later) adds to the
Syncplay 1.7 protocol, and what the client should do with them. Paste everything below the line
into the agent.

---

## Goal

YarmiplayServerTV is a Syncplay 1.7 server that can also:

1. **Relay video files between viewers in a room.** A viewer who doesn't have the room's file can
   stream it (or download it whole) from another viewer who does, through the server, which keeps
   a temporary cache.
2. **Share the host's Jellyfin.** The server hands out access to the host's Jellyfin through a
   hidden guest account, approved with Jellyfin Quick Connect, and proxies Jellyfin on the same
   port as Syncplay, so friends need only one port.

Add client support for both, without changing anything when the server is a stock Syncplay server
(or a YarmiplayServerTV in "vanilla Syncplay mode", which looks identical).

Relevant code in this repo:

- `syncplay-protocol/src/main/kotlin/com/yarmiplaytv/syncplay/SyncplayClient.kt`: the Hello is
  built around line 625 (`putJsonObject("features")`), and the server's Hello `features` are
  parsed around line 363 into `serverFeatures`. STARTTLS is handled around line 300.
- `media-source/src/main/kotlin/com/yarmiplaytv/media/jellyfin/JellyfinClient.kt`:
  `quickConnectEnabled(serverUrl)` and `quickConnect(serverUrl)`, which runs
  `QuickConnect/Initiate`, emits `QuickConnectState.WaitingForApproval(code)`, then polls
  `QuickConnect/Connect` and authenticates.
- `shared/src/commonMain/kotlin/com/yarmiplaytv/sync/MediaLocator.kt`: `locateForRoom(fileName)`
  returns `RoomLocation.Local`, `RoomLocation.Server` or `RoomLocation.Missing`.
- The players: `player-mpv` (Android, libmpv) and `player-mpv-desktop`.

## Hard rules

- **Never send a `Yarmiplay` command, and never make a `/yarmiplay/*` HTTP request, unless the
  server's Hello advertised `features.yarmiplay`.** A stock Syncplay server drops the connection
  with an error when it receives an unknown command.
- The session token is a secret for this connection. Don't log it, and don't put it in URLs that
  end up in logs or crash reports (prefer the `Authorization` header).
- Everything new must degrade to today's behaviour: when anything here fails, the room still
  syncs as a plain Syncplay room.

## Detection and handshake (protocol 1)

Declare the extension in the client Hello's `features`, next to the existing keys:

```json
{"Hello": {"username": "ana", "room": {"name": "movie night"}, "version": "1.2.255",
  "realversion": "1.7.4",
  "features": {"sharedPlaylists": true, "chat": true, "yarmiplay": {"protocol": 1}}}}
```

A YarmiplayServerTV that has the extensions on answers with a Hello whose `features` include:

```json
"yarmiplay": {
  "server": "YarmiplayServerTV",
  "version": "1.6.0",
  "protocol": 1,
  "capabilities": {"fileRelay": true, "jellyfin": false, "https": true}
}
```

If `features.yarmiplay` is missing, the server is stock Syncplay (or vanilla mode): turn all of
this off for the connection. The session protocol is `min(client, server)`; this document is
protocol 1.

Right after that Hello, the server sends the session:

```json
{"Yarmiplay": {
  "session": {"token": "9f2c…48 hex chars…", "protocol": 1},
  "capabilities": {"fileRelay": true, "jellyfin": true, "https": true},
  "jellyfin": {"available": true, "serverId": "b7c1…", "serverName": "Yarmi's Jellyfin",
               "proxy": true, "addresses": ["http://192.168.1.20:8096", "https://yarmi.duckdns.org:8920"]}
}}
```

A `Yarmiplay` message can carry several sub-commands at once (like `Set`); handle every key you
know and ignore the rest.

There is also a probe that needs no login: `GET /yarmiplay/info` on the Syncplay host and port
returns `{"server", "version", "protocol", "capabilities"}`. It returns 404 (and closes the
connection) in vanilla mode, and a stock server won't answer HTTP at all. You don't need it for
normal use; it's handy for a "test server" button.

### Mid-session changes

The server re-sends `capabilities` (and `jellyfin`) whenever they change:

```json
{"Yarmiplay": {"capabilities": {"fileRelay": false, "jellyfin": false, "https": false}}}
```

- **Everything `false`** means the host switched vanilla mode on. The token is revoked at the
  same moment: stop relay work and Jellyfin authorization, cancel uploads, and expect HTTP 401
  from `/yarmiplay/*`. Treat the connection as plain Syncplay until you reconnect. Reconnecting
  later gets a fresh Hello that says whether the extensions are back.
- **`fileRelay` turning `false`**: stop seeding and fetching, and drop the relay entries from the
  UI. When it turns `true` again, the server has forgotten all offers: **send your offer again.**
- **`jellyfin` turning `false`**: the host stopped sharing Jellyfin (or it isn't running). Don't
  delete the saved server; see the Jellyfin section.

### Peers

A peer's `features`, echoed in `Set` `user` `joined` events and in `List` replies, contain
`"yarmiplay": {"protocol": 1}` exactly when that peer has an extension session. Use it to show
which viewers are on a stock client (they can't seed or fetch relayed files). Vanilla clients
never see this key.

## HTTP on the Syncplay port

The same host and port serve HTTP. Build the base URL from where the Syncplay connection went:

- `https://<host>:<port>` when the Syncplay connection upgraded to TLS with STARTTLS (and
  `capabilities.https` is `true`). It's the same certificate, so the same hostname check applies.
- `http://<host>:<port>` otherwise. Plain HTTP is always accepted.

Authenticate with `Authorization: Bearer <token>`. If a component can't set headers, `?t=<token>`
works too. The token is valid only while this Syncplay connection is logged in, and it's scoped to
the room the connection is in: file ids from another room return 404. A reconnect gets a new
token.

Status codes: `401` unknown or revoked token, `404` relay off or no such file in your room,
`416` unsatisfiable range.

## File relay

### Wire messages

**Client to server: offer the files you can serve.** Each offer replaces the previous one; send
`{"files": []}` to withdraw. At most 200 files.

```json
{"Yarmiplay": {"offer": {"files": [
  {"name": "Show - S01E01.mkv", "size": 1534098432, "duration": 1422.5,
   "quickHash": "3b0c…64 hex chars…"}
]}}}
```

`quickHash` is lowercase hex SHA-256 over, in this order: the first `min(1 MiB, size)` bytes of
the file, the last `min(1 MiB, size)` bytes, and `size` as an unsigned 64-bit little-endian
integer. (For files under 1 MiB, the whole file goes in twice.) It lets the server merge seeders
that hold the same file, so compute it once per file and cache it by path, size and mtime.

```kotlin
fun quickHash(file: RandomAccessFile): String {
    val size = file.length()
    val n = minOf(1L shl 20, size).toInt()
    val sha = MessageDigest.getInstance("SHA-256")
    val buf = ByteArray(n)
    file.seek(0); file.readFully(buf); sha.update(buf)
    file.seek(size - n); file.readFully(buf); sha.update(buf)
    sha.update(ByteBuffer.allocate(8).order(ByteOrder.LITTLE_ENDIAN).putLong(size).array())
    return sha.digest().joinToString("") { "%02x".format(it) }
}
```

Offers belong to the room you're in when you send them. **The server forgets your offers when you
change rooms, so offer again after every room change** (and after `fileRelay` comes back on, as
above).

**Server to client: the room's relay list.** Sent on joining a room and whenever it changes
(`cachedBytes` and `rate` updates arrive at most every 2 seconds):

```json
{"Yarmiplay": {"files": [
  {"id": "5d41…32 hex chars…", "name": "Show - S01E01.mkv", "size": 1534098432,
   "duration": 1422.5, "quickHash": "3b0c…", "sources": 2, "cachedBytes": 268435456,
   "rate": 5242880}
]}}
```

- `id` is the file's id in this room; use it in the URLs below.
- `sources` is the number of connected viewers offering it. With `sources: 0` the entry is only
  the server's cache: reads past the cached part will stall and fail, so only use such an entry
  when `cachedBytes == size`.
- `rate` is the server's measured throughput from seeders for this file, in bytes per second
  (0 when idle).

**Server to client: upload requests,** sent only to seeders:

```json
{"Yarmiplay": {"upload": {"id": "a1b2…24 hex chars…", "file": "5d41…", "size": 1534098432,
  "quickHash": "3b0c…", "offset": 268435456, "length": 33554432}}}
{"Yarmiplay": {"uploadCancel": {"id": "a1b2…"}}}
```

Answer an `upload` by sending exactly the bytes `[offset, offset + length)` of the file matching
`size` and `quickHash`:

```
PUT /yarmiplay/upload/<upload id>
Authorization: Bearer <token>
Content-Length: <length>

<bytes>
```

- `204` means done. `409` means the server cancelled it (stop quietly), `404` means it's unknown
  or already reassigned, and `400` means you sent too few or too many bytes.
- Stream the body from disk; don't load it into memory. Requests are up to 32 MiB, aligned to
  4 MiB.
- Expect up to 2 requests at a time and run them in parallel. Keep data flowing: an upload with
  no progress for 10 seconds is reassigned and you're skipped for 30 seconds.
- On `uploadCancel`, abort that PUT.
- If you can't serve it (file gone, read error, no match), tell the server so it moves on at once:
  `{"Yarmiplay": {"uploadFailed": {"id": "a1b2…", "error": "file not found"}}}`

**Reading:** `GET` (or `HEAD`) `/yarmiplay/files/<id>?mode=stream|download` with an optional
`Range: bytes=start-end` returns `200` or `206` with `Content-Range`, `Accept-Ranges: bytes` and a
`Content-Type` based on the file extension. Bytes arrive as seeders deliver them. The response ends with an error if no
data arrives for 60 seconds.

- `mode=stream` (the default) asks for priority on the 64 MiB after the read position.
- `mode=download` is background work for fetching the whole file, scheduled after every stream.

### What to offer

Offer the room playlist's entries that this device has as **local files** (the
`RoomLocation.Local` matches), with their real size, duration and `quickHash`. Re-send the offer
when the playlist changes, when local folders are rescanned, and after joining or changing rooms.
Don't offer files you only partly have.

Optionally, as a second step, also offer playlist entries that resolve to a Jellyfin item whose
original file can be read with byte ranges (`/Items/{id}/Download` when the user has download
permission), and serve uploads by range-reading from Jellyfin. Skip this when downloads aren't
allowed.

### MediaLocator

Add a relay source after local folders and media servers: when `locateForRoom(fileName)` would
return `Missing`, look in the latest `files` list for an entry whose `name` equals the room file's
name (and whose `size` matches when the room reports one) and that has `sources > 0` or
`cachedBytes == size`. Return a new `RoomLocation.Relay(entry)` (or a `PlayableMedia` with a relay
source) instead of `Missing`. When the list changes, re-run the lookup for the current file, so a
seeder joining turns "missing" into "playable".

Show it to the user, for example "Streaming from Ana via the Syncplay server", and the peer
badges from the Peers section.

### Stream or download

Use a temporary **client cache** for relayed files, and let the player read through it. A good
shape is a small loopback HTTP server inside the app (`http://127.0.0.1:<port>/relay/<id>`) that
answers the player's range requests from the cache file and fetches missing ranges from the
server. That gives both modes one code path, and libmpv seeking just works.

1. Estimate the bitrate as `size * 8 / duration` bits per second.
2. Start in **stream mode**: fetch from the play position with `mode=stream` and let the player
   start.
3. Measure throughput over a 10–20 second window, from your own reads and the list's `rate`. If
   it's below about **1.3 × bitrate**, switch to **download mode**:
   - Mark yourself not ready: `{"Set": {"ready": {"isReady": false, "manuallyInitiated": false}}}`.
   - Fetch the rest of the file with `mode=download` into the cache.
   - Show the progress and an ETA.
   - Become ready again once the remaining download fits in the remaining playback time with a
     margin: `(size - haveBytes) / rate <= (duration - position) * 0.8`.
4. Switch back to stream mode if throughput recovers well above the threshold.

Cache rules:

- Keep the cache in the platform cache folder (Android `context.cacheDir`, desktop app cache),
  never in the user's media folders.
- Cap its size (for example 4 GB, or a setting) with least-recently-used eviction.
- Delete a file's data when leaving the room or when the room moves on to another file and the old
  one isn't in the playlist. Also delete files older than 24 hours, and wipe the cache at startup.
- A fully downloaded file whose `quickHash` checks out may be offered as a seed while it's still
  cached.

## Jellyfin sharing

When `jellyfin.available` is `true`, add the host's Jellyfin automatically (behind a setting
"Add media servers shared by Syncplay hosts", on by default):

1. **Find a working URL.** Try the proxy first when `proxy` is `true`: the Syncplay base URL from
   the HTTP section. Then try each of `addresses`. For each candidate, `GET /System/Info/Public`
   and accept it only if its `Id` equals `serverId`.
2. **Deduplicate.** If a saved server has the same server id and a working token, don't add it
   again. Remember the candidate URLs on the saved server so it can move between them later.
3. Check `quickConnectEnabled(url)`.
4. **Authorize with Quick Connect.** Run `JellyfinClient.quickConnect(url)`. When it emits
   `WaitingForApproval(code)`, send the code to the Syncplay server instead of showing it:
   `{"Yarmiplay": {"jellyfinAuthorize": {"code": "123456"}}}`
5. **Wait for the result.** The server approves the code for its guest account and replies:
   `{"Yarmiplay": {"jellyfinAuthorize": {"code": "123456", "ok": true}}}`. The `Connect` poll then
   succeeds and `quickConnect` finishes with the access token. On `"ok": false`, show `error` and
   stop the flow. You get 5 attempts per minute per connection.
6. **Save the server** as "<serverName> (via <Syncplay server name>)", marked as shared by a
   Syncplay host, and tell the user it was added.

Later on:

- The guest token stays valid across Syncplay sessions. When the host stops sharing, the guest
  account is disabled and Jellyfin answers 401. Mark the server "No longer shared" and offer to
  remove it. If `jellyfin.available` turns `true` again on a later visit, re-authorize
  automatically.
- If the saved URL stops answering, re-probe the stored candidates (the proxy URL depends on how
  this device reached the Syncplay server).
- Shared servers then take part in `MediaLocator` like any other media server.

## Tests

- **Unit tests:**
  - `quickHash` against a known vector. Generate one with `scripts/fake_peer.py`'s `quick_hash`
    in the YarmiplayServerTV repo.
  - Hello `features` parsing, with and without `yarmiplay`.
  - Capability transitions, including everything going `false`.
  - The re-offer triggers: room change, `fileRelay` back on, playlist change.
  - The stream/download decision and the readiness formula.
  - Cache eviction.
- **Against a stock Syncplay server** (`syncplay-server`, or YarmiplayServerTV with vanilla mode
  on): no `Yarmiplay` command is ever sent, no HTTP request is made, and the room behaves exactly
  as before.
- **Against YarmiplayServerTV:**
  - Run it headless from that repo: `cargo run --example syncplay_server -- 18999` (relay on, no
    Jellyfin).
  - Seed from a scripted peer:
    `python scripts/fake_peer.py --port 18999 --room t --name seeder --yarmiplay --script "offer C:/videos/a.mkv; wait 600"`
  - Check that the client streams `a.mkv` and that the bytes match.
  - Run the reverse with
    `python scripts/fake_peer.py --port 18999 --room t --name leecher --yarmiplay --script "expect-files 1 within=30; fetch a.mkv out.mkv download"`
    to test the client as a seeder.
- **Jellyfin:** use the full YarmiplayServerTV app with "Share with Syncplay users" switched on,
  join from the client, and check that the server appears and plays both through the proxy and on
  the LAN address.
