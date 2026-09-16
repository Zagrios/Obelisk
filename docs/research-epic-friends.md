# Epic friends and live presence

Initial research on 2026-09-16 used public sources without account credentials. Later that day, implementation and live checks used Forge's normal saved-session path. The [implementation and live validation record](epic-protocol.md#friends-and-live-presence) records the initial HTTP 403 investigation, the later verified registration that delivers initial presence, and the remaining live-transition checks.

## Existing Forge authentication

`crates/storefronts/epic-kit/src/auth.rs` already implements the launcher OAuth flow used by Legendary, using the `launcherAppClient2` client and `token_type=eg1`. The session has an account ID, access token and serialized refresh path. Reusing this session is a plausible starting point for launcher social services, but successful library access does not prove permission to access every social endpoint.

## Concrete implementation reference

[EGL3](https://github.com/WorkingRobot/EGL3) is an independently implemented Epic launcher with REST friends queries and a STOMP connection for launcher presence. Its relevant transport files date from July 2021. They provide concrete protocol evidence, but their age means endpoint behavior, authentication and schemas need live verification.

The [REST client](https://github.com/WorkingRobot/EGL3/blob/master/src/web/epic/EpicClientAuthed.cpp) fetches `/friends/api/v1/<accountId>/summary?displayNames=true` using launcher authentication. The [response model](https://github.com/WorkingRobot/EGL3/blob/master/src/web/epic/responses/GetFriendsSummary.h) contains accepted friends, incoming requests, outgoing requests, suggestions and a blocklist. Other endpoints retrieve individual friends and requests.

The [host definitions](https://github.com/WorkingRobot/EGL3/blob/master/src/web/Hosts.h) select `wss://connect.ol.epicgames.com/` for launcher STOMP, distinct from EOS STOMP and Fortnite XMPP. The [STOMP client](https://github.com/WorkingRobot/EGL3/blob/master/src/web/stomp/StompClient.cpp) sends the launcher token through the WebSocket `Authorization` header, offers STOMP subprotocols, performs CONNECT/SUBSCRIBE and negotiates heartbeats. This native client uses headers; an older browser-based wiki's cookie-auth observation is not the only implementation route.

The [friends client](https://github.com/WorkingRobot/EGL3/blob/master/src/web/epic/friends/FriendsClient.cpp) subscribes to a destination ending in `/account/<accountId>`, then handles:

- `core.connect.v1.connected`, which supplies a connection ID.
- `presence.v1.UPDATE`.
- `friends.v1.FRIEND_ENTRY_ADDED`, `REMOVED` and `UPDATED`.
- `friends.v1.FRIEND_INVITE_ENTRY_ADDED` and `REMOVED`.
- Blocklist changes.

The [presence model](https://github.com/WorkingRobot/EGL3/blob/master/src/web/stomp/messages/Presence.h) contains an account status and multiple namespace presences, each with optional product/app IDs, activity, properties and active connections. Forge should aggregate these, rather than treating the disappearance of one connection as the friend going offline everywhere.

## Official SDK versus launcher protocol

Epic officially documents `AddNotifyOnPresenceChanged` and `AddNotifyFriendsUpdate`. Its EAS friends view follows application consent: it does not automatically expose the complete launcher friends list. These official SDK callbacks therefore establish an event-driven service model, not blanket permission for a Forge EOS application to behave as the Epic launcher. [Epic's friends and presence guide](https://onlineservices.epicgames.com/news/querying-for-epic-friends-and-their-status)

The launcher REST/STOMP route above comes from independently written source, not an official, stable public API contract. Fortnite XMPP libraries are useful secondary implementation references, but Fortnite-specific presence cannot establish full Epic launcher feature parity. [fortnitepy source](https://github.com/Terbau/fortnitepy)

## Implementation follow-up

The existing launcher session returned 21 accepted friends and completed the STOMP subscription. EGL3's July 2021 files did not establish the current initial-presence request. A newer [Presence Service endpoint observation](https://github.com/LeleDerGrasshalmi/FortniteEndpointsDocumentation/blob/a9517ca054949f9ad917f00a4549fd84f9eff71a/EpicGames/PresenceService/Presence/GetPresence.md), dated June 5, 2026, documents a `circle=friends` snapshot at `presence-public-service-prod.ol.epicgames.com/presence/api/v1/_/<accountId>/presence`. This is an independent protocol observation, not an Epic API guarantee. Forge's account received HTTP 403 there. A valid library session and successful subscription do not establish access to initial presence.

The implemented `epic-kit` service subscribes before merging the roster, buffers intervening events, resynchronizes after reconnect, shares serialized OAuth refresh and rejects old account generations. The common Steam/Epic panel retains the returned roster on a presence permission error. Controlled HTTP/WebSocket tests exercise the state transitions; they do not prove Epic's live delivery or coverage across products.

Separate transport reconnection from account sign-out. Closing or replacing a STOMP connection must not call Forge's account disconnect path, which revokes the OAuth access token. A temporary social connection failure should leave library and download authentication usable.

Keep this first scope to reading friends and status. Friend management, chat and joining games need their own supported request and delivery paths.

## Installed launcher comparison

The installed macOS launcher's current social component provided newer primary-source evidence. It registers the connection with the presence service, consumes `presence.v1.UPDATE`, and displays offline as the UI fallback for a missing presence record. Forge's normal launcher token successfully performed the same registration in a temporary diagnostic, but the reply contained only the local account's presence and no friend update followed during the observation. Direct initial reads remained forbidden. See the [exact sources, request, observed results and remaining witness test](epic-protocol.md#follow-up-installed-macos-launchers-social-component). An all-offline screenshot cannot distinguish successful initialization from the launcher's fallback display.

## Connected-friend verification

A controlled friend already shown as away allowed a direct comparison. Without connection registration, Forge received no presence; with the official registration, the same token received an initial away event in under one second. The production service now follows that path, removes the denied legacy collection read, and keeps unreported states unknown. Current relationship event names are also accepted. The witness could not perform a later controlled transition. See the [witness and correction record](epic-protocol.md#connected-friend-witness-and-correction) for the exact distinction between verified initial presence and remaining coverage.
