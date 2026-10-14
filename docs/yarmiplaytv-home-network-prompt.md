# YarmiplayTV home network prompt

This is a self-contained prompt for an agent working in the **YarmiplayTV** client repository. It describes how
YarmiplayServerTV (1.7.2 and later) lets clients on the host's own network connect with TLS when the router
doesn't loop the public address back in (no NAT loopback), and the client work needed. Paste everything below the
line into the agent.

---

## Problem

A host with DuckDNS and a Let's Encrypt certificate gives out `myname.duckdns.org:8999`. That name resolves to
the router's public IPv4. From inside the host's network, many routers don't loop the connection back to the
host, so the connection times out or is refused.

Dialling the LAN IP (`192.168.1.20:8999`) does reach the server. But `SocketTransport.startTls()`
(`Transport.kt:97-107`) verifies the certificate against the dialled host, so the handshake fails.
`negotiateTls` (`SyncplayClient.kt:451-473`) treats that as fatal. Today the user's only way out is to switch TLS
off at home.

## Goal

Without the user doing anything:

- **At home,** a server the user saved as `myname.duckdns.org` connects through its LAN address, with the
  certificate still verified against `myname.duckdns.org`.
- **Away from home,** nothing changes.
- **A typed LAN IP** connects with TLS too, verified against the name the server says it has.
- **Other servers** see no change: stock Syncplay, vanilla mode, older YarmiplayServerTV, and any server without
  a certificate.

## Hard rules

- **Never skip certificate verification.** The trust store stays the platform default, with no custom
  TrustManager, no "accept any", and no pinning shortcuts. The only change is **which name** is checked:
  - the name the user typed, or
  - for a typed IP, the server's own `tls.name`, which the UI then shows as "Verified as …".
- Only connect to a discovered or saved LAN address **with TLS verified against the expected name**. If
  verification fails there, drop that address; don't fall back to plaintext on it.
- Send discovery broadcasts only on Wi-Fi or Ethernet, never over mobile data.

## Server side (already in YarmiplayServerTV 1.7.2)

**`tls` field.** While a certificate is loaded and vanilla mode is off, the server sends `tls`:

```json
"tls": {"name": "myname.duckdns.org", "lan": ["192.168.1.20:8999", "[2a02:6b8::20]:8999"]}
```

- **Where it appears:**
  - the Hello's `features.yarmiplay`, next to `capabilities`,
  - every `{"Yarmiplay": {...}}` state message (with `capabilities`, `jellyfin`, `plex`); it changes when a
    certificate appears or goes away, or the addresses change,
  - `GET http://<host>:<port>/yarmiplay/info`, over plain HTTP on the Syncplay port, before any login: since
    1.8.3 with `name` only, the home addresses going to admitted clients.
- **Values:** it's `null` without a certificate. `capabilities.https` stays as before (true exactly when `tls`
  isn't null).
- **`lan` addresses:**
  - private IPv4 addresses first, the primary one first
  - then this PC's public IPv6, if any
  - each one is `ip:port`, with IPv6 in brackets

**Discovery.** Send the UDP datagram `{"yarmiplay":"discover","protocol":1}` to the **Syncplay port number** of
the server (default 8999), as a broadcast on the local network. The reply is a datagram from the server:

```json
{"yarmiplay":"server","server":"YarmiplayServerTV","version":"1.7.2","serverId":"9f…","port":8999,
 "tls":{"name":"myname.duckdns.org","lan":["192.168.1.20:8999"]}}
```

- `serverId` is the same id the device-approval challenge uses, so it matches `KnownServer.serverId`.
- `tls` is `null` without a certificate.
- The server only answers private, link-local and loopback senders, at most 10 replies a second, and never in
  vanilla mode.

**IPv6.** The server also publishes its public IPv6 on DuckDNS and listens on IPv6. On networks with IPv6, the
name may simply work at home over IPv6. `SocketTransport` already alternates IPv6 and IPv4, so keep that.

## Client changes

**`syncplay-protocol`**

- **`SocketTransport(host, port, tlsName = host)`:** dial `host`, but use `tlsName` for SNI and hostname
  verification in `createSocket(plain, tlsName, port, true)` with `endpointIdentificationAlgorithm = "HTTPS"`.
  Add a `transportFactory` overload that takes `tlsName`.
- **`SyncplayConfig`:** add `tlsName: String? = null`, meaning "verify as" (default: `host`). The error text at
  `SyncplayClient.kt:462-467` names `tlsName`.
- **Parse `tls`:**
  - from `features.yarmiplay.tls` in `handleHello`, and from `Yarmiplay` state messages, into
    `ServerTls(name, lan)` on `RoomState`
  - next to `capabilities` (`SyncplayClient.kt:1164`)
- **Relay HTTP** (`httpBase`, `SyncplayClient.kt:846-850`; `RelayHttp.kt`):
  - Build the URL with `tlsName` as the host.
  - Give OkHttp a `Dns` that maps `tlsName` to the address the Syncplay connection actually dialled. Then HTTPS
    to the relay is verified against the same name and reaches the same box.
  - Same for the Jellyfin proxy base in `HostJellyfinSharing.kt`.

**`media-source` or `shared`:** a new `SyncplayDiscovery`, modeled on `JellyfinDiscovery.kt`.

- `discover(port, timeout = 1500 ms): List<DiscoveredServer>`:
  - Send the datagram to `255.255.255.255:<port>` and to each up interface's IPv4 broadcast address.
  - Collect replies until the timeout.
  - Ignore anything that isn't valid JSON with `"yarmiplay":"server"`.
- On Android, hold a `WifiManager.MulticastLock` while discovering, if broadcasts are filtered on the device.

**`shared`:** the connect logic, in `SyncController` / the place that builds the config.

1. **Remember.** After every successful login, save `tls.name`, `tls.lan` and `serverId` with the known server.
   - `KnownServer` (`SettingsStore.kt:105-111`) gains `tlsName: String?` and `lanAddresses: List<String>`.
   - Extend the tab-separated `sp_known_servers` format with new trailing columns, and parse old lines without
     them.
2. **Connect, for a server saved by name:**
   - Race the typed `host:port` against each saved LAN address, all with TLS verified against the **typed
     name**. Give the typed host a 300 ms head start, then start the LAN attempts.
   - The first attempt whose TLS handshake verifies wins. Cancel the rest. Only the winner sends Hello.
   - If everything fails (or nothing is saved), run discovery on the server's port. Use replies whose `tls.name`
     equals the typed host (case-insensitive) or whose `serverId` equals the saved one. Try their `lan`
     addresses the same way.
   - If that fails too, show today's error.
3. **Connect, for a typed IP (IPv4 or IPv6 literal), with TLS on:**
   - Fetch `http://<ip>:<port>/yarmiplay/info` (2 s timeout). If `tls.name` is present, connect to the IP with
     `tlsName = tls.name`. Store the server under that name from now on, so later connections take path 2 and
     work away from home too.
   - Without `tls` (stock server, no certificate, vanilla mode), behave as today.
4. **Reconnect** (`SyncController.reconnect`, :187-189): run the same logic again, not just `lastConfig`, so moving
   between home and away works.

**UI**

- In the server or room info, show "Connected at home via 192.168.1.20 · verified as myname.duckdns.org" when the
  winning address isn't the typed host. Otherwise show nothing new.
- For a typed IP that became a name: "This server's name is myname.duckdns.org; saved for use away from home."

## Tests

- **Transport:** with a test CA and a certificate for `test.example`, `SocketTransport("127.0.0.1", port,
  tlsName = "test.example")` verifies. With `tlsName = "other.example"` it fails. Without `tlsName` it fails
  against the IP, as today.
- **Connect race:** with fake transports, a LAN address that verifies wins. A LAN address that fails
  verification is dropped and never used in plaintext. The typed host wins when it answers within the head
  start.
- **Discovery:**
  - Parsing, including junk replies and the wrong `yarmiplay` value.
  - Matching by `tls.name` and by `serverId`.
  - No broadcast on a metered or mobile network.
- **`KnownServer`:** round-trips with the new columns and parses old lines.
- **Typed IP:** `/yarmiplay/info` with `tls` gives a verified connection and the server stored under its name.
  Without `tls`, behavior is unchanged.
- **Manual,** on a home network whose router has no NAT loopback, with YarmiplayServerTV 1.7.2 and a real
  DuckDNS certificate:
  - The saved DuckDNS name connects with TLS at home, and the UI shows the LAN address.
  - The same saved server connects over mobile data away from home.
  - A typed LAN IP connects with TLS and is saved under the DuckDNS name.
