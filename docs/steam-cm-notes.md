# Steam CM and download metadata

The `steam-kit::cm` module uses Steam services directly through a TLS WebSocket
connection. It bundles neither SteamKit, SteamCMD, nor a .NET executable.

## Scope

- The `ISteamDirectory/GetCMListForConnect` directory, followed by CM protobuf messages.
- Connection with a Steam client session token supplied by the Auth module.
- Request IDs, multiple responses, gzip envelopes, and heartbeats in a Tokio
  task independent of HTTP downloads.
- Coexistence with legacy non-protobuf CM notifications found in account
  sessions. Their headers are validated, then notifications unrelated to
  downloading are ignored. Login rejections and disconnections are still handled.
- Library and play history through `Player.GetOwnedGames#1`.
- The signed-in account's public profile through `ClientRequestFriendData` with
  the `PlayerName | Presence` flags, then the `player_name` and `avatar_hash`
  fields of `ClientPersonaState`. Field groups can arrive separately. The
  request waits for an update to each group without clearing absent fields.
  The image URL uses `avatars.steamstatic.com`. The wait is limited to five
  seconds and releases the CM lock after sending. A separate channel preserves
  game responses.
- Accepted Steam friends through `ClientFriendsList` and `ClientPersonaState`
  on that same connection. Full and incremental relationship changes feed an
  account-scoped reducer. A full list or newly accepted friend triggers the
  read-only `Chat.RequestFriendPersonaStates#1` service to request a status
  push. Names and avatars, including offline friends, are hydrated with
  `ClientRequestFriendData` in batches of at most 100 using only
  `PlayerName | Presence = 18`. Relationship bursts coalesce while a status
  request is in flight. Forge sends no command that changes the account's presence.
- Revisioned full social snapshots through a Tokio watch channel and UniFFI.
  The observer suspends until a revision changes, independently of the mutex
  used by library and download queries. The reader publishes once per received
  envelope after applying its social messages, so bursts retain the final state
  without queuing every intermediate snapshot.
- Licenses and PICS packages to identify owned applications, including DLC.
- PICS application metadata, OS, architecture, and language filters, shared
  depot references, DLC, and public manifests. Platforms declared in
  `common/oslist` are retained, with a fallback to those in the launch
  configurations. Shared files without a declared OS are not enough to claim a
  macOS version. This case was confirmed in the public metadata for ELDEN RING
  and The Witcher 3.
- Depot keys and ContentServerDirectory services for HTTPS servers, manifest
  authorization, and CDN tokens.

Steam checks access rights on every key or manifest request. The module provides
no mechanism to bypass that validation.

## Social state and recovery

Only individual accounts with relationship `Friend = 3` are displayed. The
connected account, clans, blocked accounts, and pending requests are excluded.
Persona updates cannot recreate a removed relationship. A bounded temporary
cache handles persons received before the first full list. Each full list
requests all retained friends again rather than relying on spontaneous online
notifications.

Presence and activity are separate presentation fields. Updates follow the
message's field groups: an absent `Status` group preserves presence and game
activity, while a present `Status = 1` replaces both using protobuf defaults.
An omitted persona state in that group means offline, and omitted game fields
clear the previous activity. Identity-only responses therefore cannot erase
status or game data. Name updates can clear the name; absent avatar bytes keep
the previous avatar. Explicit fields in the historical game groups are also
accepted as partial updates. Offline or invisible clears activity. Busy, away,
and snooze remain distinct, and unrecognized states become unknown. Game names
come from persona messages and do not depend on owning the same game. If Steam
supplies only an identifier, Forge displays a localized generic in-game label;
it does not make a catalog request per friend.

Forge bounds a social packet at 4 MiB, a relationship list at 10,000 entries,
the pre-list persona cache at 512 entries, and display labels at 1,024 UTF-8
bytes. These are client resource limits, not Steam account-limit claims.
Optional malformed persona bodies are ignored without failing content queries.
An unreadable relationship list keeps the social state in error until a valid
full list restores membership; an unrelated persona update cannot certify the
old list as current.
Transport envelope validation, gzip limits, and response-queue overflow checks
remain enforced.

Initial hydration requires a valid full relationship list and name, avatar,
and status groups for each retained friend; an empty full list is ready
immediately. It has a 20-second deadline. Missing information produces an
error state rather than a reliable-looking zero. Heartbeats request a reply;
90 seconds without a received CM frame marks the session disconnected. These
timers detect transport or initial-load failure and do not refresh friend
statuses. `SteamModel` retains stale rows, closes the old connection, and makes
up to three reconnect attempts with delays of 1, 2, and 4 seconds. A replacement
session receives a full relationship list and hydrates it before replacing
stale rows. System wake and the panel's retry action can start another bounded
cycle. Authentication or session revocation requires signing in again.

Closing the panel does not close the observation. Sign-out cancels the observer
and any pending connection attempt, aborts the CM task, and clears account
presentation and avatar state. Social recovery does not resume an interrupted
download automatically; the existing transfer error, cancellation, and resume
paths still apply.

## Validation and limitations

Local tests check text VDF and binary KeyValue parsing, detection of truncated
input, depot metadata, and protobuf/gzip envelopes. Nesting depth, decompressed
size, and message count are bounded.

Social tests include manually encoded wire fixtures, complete and incremental
membership, group defaults, partial identity and game updates, unknown states,
account isolation, bounded input, watch cancellation, and a local WebSocket
server sharing social and content traffic. Swift tests inject Rust events
through UniFFI into `SteamModel` and exercise a closed panel, reconnect
exhaustion, authentication rejection, sign-out, and library recovery while an
old request is suspended.

On September 16, 2026, an authenticated live session confirmed the initial
accepted-friend list, names, avatars, away/offline states, and the online count
without changing the account's presence. Local search and Escape dismissal
were also checked in the running app. Identity requests alone initially
returned no status fields. Adding the read-only Chat request supplied the
status snapshot; reducing identity requests to mask 18 prevented those responses
from overwriting it. A spontaneous live presence transition and a live game's
start or stop were not observed in that check; those paths are covered by
controlled event tests.

The `cm::tests::live_anonymous_cm_public_content_metadata` test is ignored by
default. It contacts Steam servers without a user account and checks Steamworks
SDK Redist metadata, public package 17906 in binary KeyValue format, CDN servers,
a key, and a manifest authorization. It then retrieves the depot's smallest
nonempty file, limited to 8 MiB, and verifies decryption, decompression, chunks,
and the complete SHA-1 hash in memory. It uses the production HTTP client and
the CDN VHost in the URL, without overriding the Host header. It creates no
installation files.

The September 15, 2026 run **passed** for application 1007, depot 1004, with
30 HTTPS servers offered and one 340,120-byte file verified in a single chunk.
This public test does not validate a user account's sign-in or access rights.

```sh
cargo test -p steam-kit cm::tests::live_anonymous_cm_public_content_metadata -- --ignored --nocapture
```

Password-protected beta branches, CDN proxies, and Steam China services are not
implemented. Protocol or access errors are reported to the client. Social
reconnection does not replay an interrupted content request. A complete
authenticated library still requires validation with a test account. Metadata
alone does not prove a game can run on macOS.

## References and provenance

Formats, field numbers, and network sequences were checked in the following
primary sources, reviewed on September 15, 2026:

- [SteamKit](https://github.com/SteamRE/SteamKit), particularly the declarations
  in `SteamMsgBase.cs`, `SteamMsgClientServerLogin.cs`,
  `SteamMsgClientServerAppInfo.cs`, `SteamMsgContentSystem.cs`,
  `SteamMsgPlayer.cs`, the SteamUser/SteamApps/SteamContent handlers, and
  `CMClient.cs`, at revision `9c7f3c3fab6a8cf4ddd6cc7fe11bee70154e82d4`.
  The project is distributed under LGPL 2.1 or later.
- [SteamFriends](https://github.com/SteamRE/SteamKit/blob/master/SteamKit2/SteamKit2/Steam/Handlers/SteamFriends/SteamFriends.cs)
  for the profile request and the signed-in account's `player_name` and `avatar_hash` fields.
- [DepotDownloader](https://github.com/SteamRE/DepotDownloader), particularly
  shared depot identification and download filters in `ContentDownloader.cs`,
  at revision `e7474cb9ee8a87c0d917489b84c75667a1364aa1`.
  The repository contains a GPL version 2 license.
- The Steam directory was queried directly to verify the JSON format and the
  names of the WebSocket endpoints it offers.

The Rust modules were written for Forge from protocol facts. They redistribute
no source files or binaries from the reference projects. This note describes
provenance, not a legal opinion on a future product distribution.

### Friends protocol references

The friends implementation was checked against these revisions on September
16, 2026. Community protocol references and mirrored Valve client code do not
imply Valve support for Forge.

- SteamKit revision `84c990c3982eedb5abd733116b987c9870a0dccb`:
  [SteamFriends handlers and requests](https://github.com/SteamRE/SteamKit/blob/84c990c3982eedb5abd733116b987c9870a0dccb/SteamKit2/SteamKit2/Steam/Handlers/SteamFriends/SteamFriends.cs),
  [callback payloads](https://github.com/SteamRE/SteamKit/blob/84c990c3982eedb5abd733116b987c9870a0dccb/SteamKit2/SteamKit2/Steam/Handlers/SteamFriends/Callbacks.cs),
  [message and state enums](https://github.com/SteamRE/SteamKit/blob/84c990c3982eedb5abd733116b987c9870a0dccb/SteamKit2/SteamKit2/Base/Generated/SteamLanguage.cs),
  [default persona request flags](https://github.com/SteamRE/SteamKit/blob/84c990c3982eedb5abd733116b987c9870a0dccb/SteamKit2/SteamKit2/Steam/SteamClient/Configuration/SteamConfigurationBuilder.cs),
  and [the game ID layout](https://github.com/SteamRE/SteamKit/blob/84c990c3982eedb5abd733116b987c9870a0dccb/SteamKit2/SteamKit2/Types/GameID.cs).
- SteamTracking/Protobufs revision
  `08f0f630b3ada21a6a48ffede2116df3ef3c918d`:
  [friends messages](https://github.com/SteamTracking/Protobufs/blob/08f0f630b3ada21a6a48ffede2116df3ef3c918d/steam/steammessages_clientserver_friends.proto),
  [Chat persona request](https://github.com/SteamTracking/Protobufs/blob/08f0f630b3ada21a6a48ffede2116df3ef3c918d/steam/steammessages_chat.steamclient.proto),
  and [logon messages](https://github.com/SteamTracking/Protobufs/blob/08f0f630b3ada21a6a48ffede2116df3ef3c918d/steam/steammessages_clientserver_login.proto).
- Valve's shipped friends interface, mirrored by SteamDatabase/SteamTracking at
  revision `c5b71401ef2f8989a67ceb59eaa750e9763bf8e5`:
  [persona reducer and initial status request](https://github.com/SteamDatabase/SteamTracking/blob/c5b71401ef2f8989a67ceb59eaa750e9763bf8e5/steamcommunity.com/public/javascript/webui/friends.js#L11491)
  and [field-group constants and service declaration](https://github.com/SteamDatabase/SteamTracking/blob/c5b71401ef2f8989a67ceb59eaa750e9763bf8e5/steamcommunity.com/public/javascript/webui/steammessages.js#L341).
- Valve's [ISteamFriends documentation](https://partner.steamgames.com/doc/api/isteamfriends)
  describes persona states and visible friend activity. This is SDK documentation
  for games, not a launcher HTTP API or a Forge dependency.

The checked wire identifiers are `ClientPersonaState = 766`,
`ClientFriendsList = 767`, and `ClientRequestFriendData = 815`.
`FriendsList` uses `bincremental` at field 1 and repeated relations at field 2;
each relation carries a fixed64 SteamID at field 1 and relationship at field 2.
Only relationship `Friend = 3` on an individual account belongs in the panel.

`PersonaState` has message-wide `status_flags` at field 1 and repeated persons
at field 2. Each person uses fixed64 ID 1, persona state 2, app ID 3, name 15,
avatar hash 31, game name 55, and fixed64 game ID 56. The relevant group flags
are `Status = 1`, `PlayerName = 2`, `Presence = 16`, `GameExtraInfo = 256`, and
`GameDataBlob = 512`. These are distinct from the person's `persona_state_flags`.

SteamKit's historical cache gates persona state on `Presence` and game fields
on `GameDataBlob`, but its default request includes `GameExtraInfo` and excludes
`GameDataBlob`. Forge follows the shipped Valve interface for current group
semantics: `Status = 1` replaces presence and activity, applying zero/empty
defaults to absent fields. Valve also requests the initial status push through
`Chat.RequestFriendPersonaStates#1` after receiving the full relationship list,
then hydrates missing identity information without the `Status` bit.

This distinction matters on a session that does not publish its own presence:
an identity request with a broad mask can echo the requested `Status` bit while
omitting state values, which would overwrite a valid status snapshot with
offline defaults. Forge requests only names and avatars through that path.
The request mask is field 1 of `ClientRequestFriendData`; the current
`ClientLogon` schema has no persona request-mask field. The Chat request has an
empty protobuf body. Forge neither sends `ClientChangeStatus` nor forces the
local account online or invisible.
