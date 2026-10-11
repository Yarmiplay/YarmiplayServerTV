# YarmiplayTV Plex sharing prompt

This is a self-contained prompt for an agent working in the **YarmiplayTV** client repository. It describes
how YarmiplayServerTV (1.7.0 and later) shares the host's Plex Media Server with the people on its Syncplay
server, and the client work needed to support it. It builds on the extension handshake in
[client-integration-prompt.md](client-integration-prompt.md) (see "Plex sharing" there). Paste everything below
the line into the agent.

---

## Goal

YarmiplayTV already plays from Plex: it links a plex.tv account with a `plex.tv/link` code
(`PlexClient.link()`), finds servers through `/api/v2/resources`, probes each connection's `/identity` by
`machineIdentifier`, and direct-plays `library/parts`. Today a host's Plex can only be used if the host shared
it with the user by hand in Plex.

YarmiplayServerTV can now offer its host's Plex to the people in its Syncplay rooms, the way it already offers
Jellyfin (`HostJellyfinSharing`). The host's Plex is shared through a normal **Plex friend invite** to the
user's own plex.tv account, for the libraries the host picked. Add this to YarmiplayTV:

- When the Syncplay server says Plex is available, and the user has a linked plex.tv account, ask the server to
  invite that account, accept the invite with the user's own token, and add the server to the Plex sources as
  "<name> (via <Syncplay server>)".
- Nothing changes against a stock Syncplay server, a YarmiplayServerTV in vanilla mode, or an older
  YarmiplayServerTV without Plex sharing.

## Hard rules

- **Never send a plex.tv token to the Syncplay server**, and never send any `Yarmiplay` command unless the server
  sent one first (same rule as the rest of the extensions).
- Only ask for an invite when the setting "Add media servers shared by Syncplay hosts" is on (it already exists,
  key `add_shared_servers`, default on) and the user has linked a plex.tv account. Without a linked account, show
  a one-line hint ("Link your Plex account to watch <host>'s Plex") with a button to the existing link screen;
  don't start linking on your own.
- Against the server, use the server resource's `accessToken` from `/api/v2/resources`, never the account token.

## Protocol (extension protocol 1)

The server's capabilities gain `plex`, and its `Yarmiplay` messages gain a `plex` object, next to `jellyfin`:

```json
{"Yarmiplay": {
  "capabilities": {"fileRelay": true, "jellyfin": false, "plex": true, "https": true},
  "plex": {"available": true, "machineId": "5f3e…", "serverName": "Yarmi's Plex",
           "proxy": true, "addresses": ["http://203.0.113.7:32400", "http://192.168.1.20:32400"]}
}}
```

- `machineId`: the Plex server's `machineIdentifier`.
- `proxy`: Plex also answers on the Syncplay host and port (same base URL rule as Jellyfin: `https://` when the
  Syncplay connection used STARTTLS, else `http://`). YarmiplayServerTV 1.7.0 always sends `false`, because Plex
  counts relayed viewers as local; support it anyway for later versions.
- `addresses`: direct URLs, public first. They can be unreachable from outside the host's network.
- When sharing stops, the server sends `"plex": {"available": false}` and `capabilities.plex: false`.
- Older servers send neither key: treat that as unavailable.

Ask for an invite:

```json
{"Yarmiplay": {"plexInvite": {"username": "ana_watches"}}}
```

`username` is the linked account's plex.tv username (or its email). The reply is
`{"Yarmiplay": {"plexInvite": {"ok": true}}}`, or `{"ok": false, "error": "…"}`. A second request for an account
that was already invited succeeds without making a new invite. Limit: 3 requests per minute per connection.

## Client changes

**`syncplay-protocol`** (`Yarmiplay.kt`, `SyncplayClient.kt`):

- `YarmiplayCapabilities` gets `plex: Boolean = false`, parsed from `capabilities.plex`. Include it in `allOff`,
  otherwise a server sharing only Plex looks like vanilla mode.
- In `handleYarmiplay`, parse `plex` into a `SharedPlex(available, machineId, serverName, proxy, addresses)`
  (same shape as `SharedJellyfin`), and add `"plex"` to the keys that mark a YarmiplayServerTV (next to
  `"jellyfin"` and `"session"` at the vanilla check).
- `invitePlex(username)` sends `plexInvite`; it requires `capabilities.plex` and an active session. Parse the
  reply into `SyncplayEvent.PlexInvited(ok, error)`.

**`media-source`** (`PlexClient.kt`):

- `pendingInvites(accountToken)`: `GET https://plex.tv/api/invites/requests` (XML), returning each invite's id,
  `friend`/`server`/`home` flags and its servers' `machineIdentifier`.
- `acceptInvite(accountToken, invite)`: `PUT https://plex.tv/api/invites/requests/{id}?friend=1&server=1&home=0`.
- These are the endpoints Plex's apps use; confirm them against two live accounts before shipping, and keep
  them in one place so they're easy to adjust.

**`shared`** (`HostPlexSharing.kt`, modeled on `HostJellyfinSharing.kt`):

1. Trigger when `capabilities.plex && shared.available && machineId != ""`, the setting is on, a plex.tv account
   is linked, and this `machineId` wasn't handled yet on this session.
2. If the user's resources already contain `machineId` (the host shared it by hand earlier), skip to step 5.
3. Send `invitePlex(<linked username>)` and wait up to 30 seconds for the reply. On `ok: false`, post the error
   to the room locally and stop.
4. Poll `pendingInvites` for up to 30 seconds for one whose server `machineIdentifier == machineId`, then
   `acceptInvite`.
5. `servers(accountToken)`, take the one with `machineId`, and build candidate URLs:
   `(syncplay base URL if proxy) + addresses + its plex.tv connections`, deduplicated. Probe them with the
   existing `probeAll` (`/identity` must match `machineId`).
6. Save it as a `PlexSession` named "<serverName> (via <Syncplay server>)" with `sharedBy` and
   `candidateUrls`, and post "Added <name>, shared by the Syncplay host".

**`PlexSession`** gains `sharedBy: String? = null`, `candidateUrls: List<String> = emptyList()` and
`noLongerShared: Boolean = false`. `refresh()` for a shared server:

- A 401 from the server marks it `noLongerShared` instead of removing it. Offer **Remove** in the UI.
- An unreachable server tries its `candidateUrls` before asking plex.tv again.
- If `plex.available` turns `true` again on a later visit to that host, re-run the flow.
- `ServerLabels.isSharedServer` should cover `PlexSource` too.

**Docs:** in YarmiplayTV's `docs/privacy.md`, say that when a host shares Plex, the app sends the host's Syncplay
server the linked plex.tv username so the host can invite it, and accepts the invite with the user's own token.

## Tests

- **Unit tests:**
  - `capabilities.plex` parsing, including servers that don't send it, and `allOff` with only `plex` on.
  - `SharedPlex` parsing and the `available: false` form.
  - `invitePlex` is never written to a connection whose server isn't YarmiplayServerTV, or when `plex` is off.
  - Candidate URL order and deduplication.
  - A 401 marks a shared server `noLongerShared` and doesn't delete it.
- **Against YarmiplayServerTV 1.7.0** with Plex installed and two plex.tv accounts (host and guest):
  - Host: switch on Plex on the Plex page, link the host account, pick libraries, turn on "Share with Syncplay
    users", and turn on the Syncplay server.
  - Guest: link the guest account in YarmiplayTV, join the room. The host's Plex appears as
    "<name> (via <server>)" and only the picked libraries are listed. It also shows in the host's Plex page under
    "Invited from Syncplay".
  - Host removes the guest there, or switches sharing off: the guest's server becomes "No longer shared".
  - Vanilla mode on: no `plexInvite` is ever sent.
- **Against the official Syncplay server**: no change in behavior.
