# YarmiplayTV managed rooms prompt

This is a self-contained prompt for an agent working in the **YarmiplayTV** client repository. It describes
Syncplay's managed (controlled) rooms, which YarmiplayServerTV supports from 1.7.1 exactly as the official
server does, and the client work needed to use them. Nothing here is a Yarmiplay extension: it works the same
against the official Syncplay server. Paste everything below the line into the agent.

---

## Goal

In a Syncplay **managed room**, only the room's *operators* can play, pause, seek or change the playlist.
Everyone else follows, and the server puts them back if they try. Official Syncplay clients offer **Create
managed room** and **Identify as room operator**. Add the same to YarmiplayTV on TV and mobile/desktop:

- Create a managed room and get its name and operator password to share.
- Join a managed room and identify as operator with its password, typed in or saved from earlier.
- See who the operators are. Operators can set other users ready or not ready.
- As a non-operator, get a clear message instead of a player that jumps back without explanation.
- Nothing changes against servers whose Hello says `managedRooms: false`, or doesn't say it at all.

## What YarmiplayTV already has (don't redo)

- The client Hello already sends `managedRooms: true` and `setOthersReadiness: true` (`SyncplayClient.kt`
  around :876-884).
- The server's Hello `features` are stored in `RoomState.serverFeatures` (`Models.kt:97`), but nothing reads
  `managedRooms` yet.
- `RoomUser.isController` exists (`Models.kt:66-75`) and is filled from `List` (`handleList`, :545-552). The user
  lists show a ★ for it (`Panels.kt:192`, `MobilePanels.kt:181`).
- **Gaps:**
  - `handleSet` (:477-488) drops `controllerAuth` and `newControlledRoom`.
  - `handleSetUsers` (:490-524) doesn't keep `isController` on `joined` (:501) or on updates (:514).
  - `handleSetReady` (:526-535) ignores `setBy`.
  - There's no way to send `ready` for another user.
  - Room names are shown raw everywhere, so a managed room would show its hash.

## Protocol (official Syncplay 1.7, identical on YarmiplayServerTV 1.7.1+)

**Server features:** the server's Hello has `features.managedRooms: true` when it supports managed rooms.
Treat a missing key as `false`, and hide every managed-room action when it's false.

**Room names:**
- A managed room's name is `+<name>:<HASH>`, matching `^\+(.*):(\w{12})$`, e.g. `+movie night:D6A85289CDB5`.
- The server computes `HASH` from the name, the password and its own secret salt. **Clients never compute
  it.**
- Room names are at most 35 characters, so the base name can be at most 21.

**Passwords:** passwords look like `AB-123-456`: two uppercase letters, three digits, three digits, matching
`[A-Z]{2}-\d{3}-\d{3}`. Generate them with a secure random source. Show them as-is, and uppercase what users
type in.

**Create a managed room** (what the official client does):

1. Client → `{"Set": {"controllerAuth": {"room": "movie night", "password": "AB-123-456"}}}`, while in any
   room. `room` is the base name the user typed.
2. Server → sender only: `{"Set": {"newControlledRoom": {"roomName": "+movie night:D6A85289CDB5",
   "password": "AB-123-456"}}}`.
3. Client changes room to `roomName` (the normal `Set.room`), then sends
   `{"Set": {"controllerAuth": {"room": "+movie night:D6A85289CDB5", "password": "AB-123-456"}}}`.
4. Server → everyone who can see the user (the whole server, or just the room if rooms are isolated):
   `{"Set": {"controllerAuth": {"user": "ana", "room": "+movie night:D6A85289CDB5", "success": true}}}`.

**Identify as operator** in a managed room you're already in: step 3 with the password, then step 4.
- A wrong password gives `success: false`, broadcast the same way.
- A password that doesn't match the format gives `success: false`, sent to the room only.
- On YarmiplayServerTV the password must be for the room you're in.

**Operators:**
- `controllerAuth` with `success: true` makes `user` an operator of `room`.
- `List` entries carry `"controller": true|false` for every user.
- Operator status ends when the user leaves the room or disconnects. Rejoining needs the password again. The
  official client sends it automatically when it has one saved.

**Non-operators in a managed room:**
- A pause, unpause or seek isn't applied or forwarded. The server sends that client two forced `State`s:
  1. `(room position, the client's own paused, doSeek: false, setBy: <the client>)`
  2. `(room position, the room's paused, doSeek: true, setBy: <who last set it>)`

  Both carry `ignoringOnTheFly.server`.
- A `playlistChange` or `playlistIndex` from a non-operator is answered, to that client only, with the room's
  current `playlistChange` / `playlistIndex`. The `user` field of that reply is the room name.

**Setting other users' readiness** (operators only; ignored otherwise):
- Client → `{"Set": {"ready": {"username": "bob", "isReady": true, "manuallyInitiated": true}}}`.
- Server → the room: `{"Set": {"ready": {"username": "bob", "isReady": true, "manuallyInitiated": true,
  "setBy": "ana"}}}`.
- Clients that didn't declare `setOthersReadiness` also get a chat line from `ana`: "I have set bob as ready.".
  YarmiplayTV declares it, so it gets none and should show `setBy` itself.

## Client changes

**`syncplay-protocol`** (`SyncplayClient.kt`, `SyncplayEvent.kt`, `Models.kt`):

- `RoomState.managedRooms: Boolean`, read from `serverFeatures["managedRooms"] == true`.
- `handleSet`:
  - `newControlledRoom`: if it answers a pending create (same password), `changeRoom(roomName)` and send
    `controllerAuth {room: roomName, password}`. Emit `ManagedRoomCreated(room, password)`.
  - `controllerAuth`: emit `ControllerAuth(user, room, success)`. On success, set `isController = true` for
    that user in that room and `publishUsers`.
- `handleSetUsers`:
  - Keep `isController` across updates.
  - When a user leaves or changes room, clear their `isController`, matching the server.
- `handleSetReady`: read `setBy` and emit `ReadySetBy(user, setBy, isReady)` when it's present and not the user
  themself.
- New methods, each a no-op unless `managedRooms`:
  - `createManagedRoom(baseName)`: validate the length (1..21), generate a password, remember it as pending, and
    send step 1.
  - `identifyAsOperator(password)`: send `controllerAuth {room: <current room>, password}`.
  - `setOthersReady(username, isReady)`.
- `isOperator()` for the local user: the current room is managed (matches the regex) and the local user's
  `RoomUser.isController` is true.

**Room-name display:**
- Add a shared helper, e.g. `RoomNames.display(name)`, that turns `+movie night:D6A85289CDB5` into
  `movie night` (shown with a lock) and leaves other names alone.
- Use it wherever a room name is shown:
  - TV: `Panels.kt:91,154`, `PlayerScreen.kt:513`, `HomeScreen.kt:134`, `BrowseScreen.kt:207`,
    `SyncplayConnectScreen.kt:71`.
  - Mobile: the matching lines in `MobilePanels.kt`, `MobilePlayer.kt`, `MobileHome.kt` and `MobileBrowse.kt`.
  - The "Joined room" feed line in `SyncController.kt:134`.
- Keep the full name for joining, `room_playlist:` keys and the desktop file name (`DesktopRoomPanel.kt:317`).

**Saved operator passwords** (`SettingsStore.kt`):
- Store them under `room_password:<host:port>/<full room name>`, following `room_playlist:`. Use
  `SyncController.serverKey` for `host:port`.
- Save on `ManagedRoomCreated`, and on `ControllerAuth(success = true)` for the local user after a typed
  password.
- Delete on `success = false` for the local user.
- After joining or reconnecting to a managed room with a saved password, send `identifyAsOperator` once.

**UI** (TV `Panels.kt` and mobile/desktop `MobilePanels.kt`, next to **Change room**; shown only when
`managedRooms`):
- **Create managed room:** a name field. The result shows the room name (display form plus the full name) and
  the password, with Copy on mobile/desktop.
- **Identify as operator:** a password field, shown in a managed room when you aren't an operator.
- In the user list:
  - Operators keep the ★, with the label "operator".
  - Operators get **Mark ready** / **Mark not ready** on other users.
- Connect screens (`SyncplayConnectScreen.kt`, `MobileConnect.kt`):
  - Accept a full `+name:HASH` room as-is.
  - Add an optional "Operator password" field. It's saved like the above and sent after joining.

**Non-operator playback** (`SyncEngine.kt`). In a managed room, when the local user isn't an operator:
- A local pause, unpause or seek isn't sent as a state change: put the player back to the global state locally.
  The server would revert it anyway, but this avoids a flash of the wrong state.
- Don't flip readiness on a blocked pause (`toggleReady`, :116-131), and don't post "You jumped to …".
- Post "Only room operators can control playback" with `postLocal`, at most once every 10 seconds.
- Still accept the server's forced states as today. `serverUnpaused` and `serverSeeked` already snap the player
  back.

**Notices** (`postLocal`, `SyncController.kt:216`):
- "<user> is now a room operator"
- "You are now a room operator"
- "Wrong room password"
- "<setBy> marked you ready" / "not ready"
- "<setBy> marked <user> ready" / "not ready"

## Tests

- **`syncplay-protocol` unit tests**, in the style of `YarmiplayClientTest.kt` with `ScriptedTransport`:
  - The create flow: the steps 1-3 messages in order, and `ManagedRoomCreated`.
  - `controllerAuth` success and failure update `isController` and emit events. A leave clears the flag.
  - `isController` survives `Set.user` updates.
  - `ready` with `setBy` emits `ReadySetBy`.
  - Auto-identify after joining a managed room with a saved password, once only.
  - Without `managedRooms` in the server's Hello, none of the methods send anything.
- **`SyncEngineTest`:**
  - A non-operator's local pause and seek are not sent, readiness doesn't change, and the notice is rate-limited.
  - An operator's are sent as today.
- **Room-name helper:** names with `:` in the base, wrong hash lengths, and ordinary names.
- **`FakeSyncplayServer.kt` / `scripts/fake_peer.py`:** add `controllerAuth` / `newControlledRoom` handling and a
  `controller` flag in `List`, so `Scenarios.kt` can cover a managed room.
- **Interop:** against YarmiplayServerTV 1.7.1, and against the official server started with
  `--salt SOMESALT`:
  - An official 1.7.x client creates a managed room. YarmiplayTV joins it, can't control playback, identifies
    with the password, and then can.
  - The reverse: YarmiplayTV creates it, and the official client identifies.
  - Restart YarmiplayServerTV: the saved password still works (its salt persists).
