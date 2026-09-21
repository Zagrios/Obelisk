# Epic Games integration

## Scope

Forge displays the Epic Games website in an embedded sign-in window, captures the authorization code automatically, and loads the account's library. Password and two-factor authentication are handled on the Epic website. Forge downloads and verifies Epic games using BuildPatch manifests, and launches native Mac games after checking for updates.

The Rust implementation lives in `epic-kit`. Forge neither runs Legendary or the Epic launcher nor requires them to be installed.

## Protocol

Endpoints and formats were verified on September 15, 2026 against Legendary's public sources at commit `42f6bdeadde3a9526dc8eb713763476999ac217b`:

- [Epic API](https://github.com/legendary-gl/legendary/blob/42f6bdeadde3a9526dc8eb713763476999ac217b/legendary/api/egs.py) for the sign-in URL, OAuth grants, refresh, session revocation, library pages, platform assets, and catalog metadata.
- [Legendary catalog](https://github.com/legendary-gl/legendary/blob/42f6bdeadde3a9526dc8eb713763476999ac217b/legendary/core.py) for distinguishing standalone games from add-ons, mods, and Unreal content.
- [Heroic embedded sign-in](https://github.com/Heroic-Games-Launcher/HeroicGamesLauncher/blob/a7046d7742dd478fffd2e00bff600d2542c53cdb/src/frontend/screens/WebView/index.tsx) for the `id/login?responseType=code` URL, the `EpicGamesLauncher` User-Agent marker, and the automatic localhost callback.

These launcher APIs may change independently of the public Epic Online Services APIs. The launcher's public OAuth client credentials are used by the protocol. They are not the user's account credentials.

The `WKNavigationDelegate` intercepts only the main frame's HTTPS `localhost/launcher/authorized` callback, with a single `code` parameter. No local server or global URL scheme is needed. Only the still-active attempt can accept the callback. The window closes before exchanging the code with the account service.

The old URL explicitly used `/id/api/redirect`, which produces a JSON response intended for the launcher. It is no longer presented to the user. If Epic still uses this variant, Forge hides the page, validates the HTTPS origin and client parameters, then extracts `authorizationCode`. No JavaScript reads are performed on sign-in forms. Cookies stay in a `WKWebsiteDataStore.nonPersistent()` shared only with that attempt's secondary windows.

The library follows `responseMetadata.nextCursor` through the final page and rejects pagination loops. The catalog supplies names, descriptions, developers, and cover art. Windows and Mac assets indicate known platforms. Add-ons and Unreal content are not displayed as standalone games.

## Game metrics

Opening a game's details requests its account playtime and build manifest in parallel, using the existing session. These reads do not download game files. Installation starts only through the Download action.

- Playtime comes from `GET /library/api/public/playtime/account/{accountId}/all` on the library service. The matching `accountId` and `artifactId` identify the record; `artifactId` matches the library's `appName`. `totalTime` is in seconds. An absent record stays unknown, while an explicit zero means never played.
- Installation size comes from `GET /launcher/api/public/assets/v2/platform/{platform}/namespace/{namespace}/catalogItem/{catalogItemId}/app/{appName}/label/Live`. Forge selects Mac when the library advertises it, otherwise Windows. It fetches the returned manifest over HTTPS without forwarding the account's bearer token to the CDN, then sums the uncompressed file parts. Legacy JSON and supported binary BuildPatch manifests are handled. The estimate includes optional files and languages and is not a measurement of an existing installation or the compressed download size.
- The examined playtime response contains no last-launch timestamp. Forge leaves this metric unavailable instead of substituting an acquisition or update date.

Protocol sources checked on September 16, 2026:

- [Playnite's Epic client](https://github.com/JosefNemec/PlayniteExtensions/blob/master/source/Libraries/EpicLibrary/Services/EpicAccountClient.cs) and [playtime response model](https://github.com/JosefNemec/PlayniteExtensions/blob/master/source/Libraries/EpicLibrary/Models/PlaytimesResponse.cs).
- [GOG's Epic integration](https://github.com/FriendsOfGalaxy/galaxy-integration-epic/blob/master/src/plugin.py), which converts Epic seconds to Galaxy minutes and supplies no last-played timestamp.
- Legendary's [manifest endpoint](https://github.com/legendary-gl/legendary/blob/42f6bdeadde3a9526dc8eb713763476999ac217b/legendary/api/egs.py), [binary parser](https://github.com/legendary-gl/legendary/blob/42f6bdeadde3a9526dc8eb713763476999ac217b/legendary/models/manifest.py), and [JSON parser](https://github.com/legendary-gl/legendary/blob/42f6bdeadde3a9526dc8eb713763476999ac217b/legendary/models/json_manifest.py).

Unavailable history or an unavailable manifest does not hide the other metric. Authentication rejection is retried once after refreshing the session. Account and selection checks prevent late responses from updating a different game or a signed-out view. Refreshed credentials are saved even if the user closes the details page during the request.

## Achievements

Game details show playtime after installation size and a trophy button with the unlocked/total count. Steam and Epic share the achievement panel, including its scroll area at the right edge, circular close button, and dismissal when clicking the parent window outside the sheet.

`epic-kit::Client::achievements` reads definitions and the current account's unlocks through `https://launcher.store.epicgames.com/graphql`, using the launcher's store User-Agent. The read-only queries follow Legendary's [achievement query definitions](https://github.com/legendary-gl/legendary/blob/master/legendary/models/gql.py) and [API requests](https://github.com/legendary-gl/legendary/blob/master/legendary/api/egs.py), checked September 21, 2026. Definition requests use the game's namespace and the language code; unlock requests use the same namespace and the active Epic account ID.

Responses are bounded and checked for GraphQL errors, namespace/account mismatches, duplicate identifiers, and incomplete definitions. Unsupported games have an empty list; failed requests remain unavailable and can be retried. Locked hidden achievements do not expose their names, descriptions, or icons. Only HTTPS icons without embedded credentials are accepted. Unlock dates are parsed separately from display text.

Requests reuse the launcher `Api` helper, so bounds, status mapping and redirect policy stay in one place.

The UI request goes through `EpicRememberedSession`, preserving credential rotation and account checks. Failures are reported through `EpicModel.message`, as elsewhere in the store. Closing the game or replacing the selection invalidates pending achievement responses. This feature performs no achievement mutations or game launches.

Live verification on September 21, 2026: Cyberpunk 2077 displayed 2.5 hours and 2/57 achievements from the signed-in account, including icons and unlock dates. The native panel's scrollbar and outside-click dismissal were checked visually. Fixture tests cover merging unlocks, hidden achievements, unsupported games versus failed responses, account mismatch, and authentication refresh.

## Sessions and errors

With the remember option enabled, `EpicKeychain` saves only the account identity and refresh token. The `epic-session` item uses the `com.zagrios.forge.epic` Keychain service. Steam keeps its own item and service. No token is written to the catalog, preferences, or logs.

Rust REST calls use HTTPS without automatic redirects. The web window follows HTTPS navigation required for sign-in. Displayed errors include neither OAuth response bodies nor revocation URLs, which contain a token. Swift operations reject responses that become stale after sign-out. A temporary network failure does not delete a saved session.

Signing out clears the local session and asks Epic to revoke it. If the network prevents revocation, the interface reports it. Closing the sign-in window cancels only the current attempt.

Some sign-in providers may reject embedded browsers, including [Google](https://developers.google.com/identity/protocols/oauth2/policies). Their acceptance still needs separate validation. Authentication with Epic credentials takes place directly on the Epic website.

## Validation

Rust tests use a local HTTP server and mock library responses. Swift tests check the UniFFI bridge, storage in an isolated Keychain item, restoration errors, and concurrent responses during sign-out.

Metrics validation on September 16, 2026:

- `./scripts/test.sh` passed formatting, Clippy, localization validation, 103 Rust tests with five optional tests ignored, and the 101-test macOS suite with four tests skipped and no failures. App signature verification passed.
- Regression tests cover account/game matching, absent versus zero playtime, authentication refresh, late responses after selection changes or sign-out, and credential rotation after closing a game.
- Manifest tests cover JSON, binary, zlib compression, encrypted-name metadata, integrity checks, truncation, malformed lengths, overflow, and requests that omit account credentials on the CDN.
- The rebuilt app displayed both account playtime and installation-size data for Rocket League using an existing signed-in Epic session. No game installation was started.

Local validation on September 15, 2026:

- Rust formatting and Clippy passed across the workspace.
- 82 Rust tests passed, including 13 for Epic. Five Steam network tests were ignored by default.
- 61 macOS tests passed, including 37 for Epic. One optional Steam rendering test was ignored.
- The built app's Apple Development signature was verified.
- The localhost callback, hidden JSON response, secondary window, closing, and cancellation were verified using the actual WebKit engine and local test pages.
- Opening the real Epic page was verified in the macOS app without entering credentials.

Automated tests do not demonstrate end-to-end sign-in with a real Epic account. That validation requires completing sign-in in the Epic window, checking the account's games, then restarting Forge to verify session restoration.

## Downloads

The details page starts installation using the same progress button and cancellation controls as Steam. It chooses the Mac build when advertised by the library, otherwise Windows, under `Games/Epic` in Forge's data directory. All manifest files are selected, including optional language files. An installed game offers Launch, which verifies files, applies updates, and then starts the game. The initial download never launches automatically.

Before fetching content, the Rust client verifies the full game identity against the signed-in account's paginated library. A rejected account token is refreshed once. The manifest request reuses the size-estimation endpoint and its HTTPS CDN validation. Signed manifest query parameters are not copied to chunk requests, and the account bearer token never reaches a CDN.

`content_manifest` parses binary BuildPatch metadata, chunk tables and file-part tables, as well as legacy JSON manifests. The installer supports unencrypted chunk-based manifests and validates chunk identity, size, compression, SHA-1 and rolling hash before assembling each file. A file's final SHA-1 must match before an atomic replacement makes it visible at its destination. Path traversal, reserved Forge paths, case collisions, existing symlinks and hardlinks are rejected. Manifests requiring encrypted filenames/chunks, legacy file-data delivery, pre-SHA1 chunk formats or symbolic links fail explicitly.

A bounded network pump runs alongside a blocking disk worker. Cancellation wakes pending requests and stops disk work between blocks. The same cancellation token and atomic-file helper serve Steam and Epic. Existing files are verified first using the shared `forge-core` scheduler, with at most four readers and accelerated SHA-1 on Apple Silicon. All readers finish before any repair, including after cancellation or an error. Progress callbacks stay serialized. Validated chunk data stays in `.forge-content/epic` after an interrupted attempt and is reused on retry, including after restarting the app. The chunk cache for the completed build is removed after all files succeed. A per-directory lock prevents overlapping writers. Installation receipts are written only on success, with no tokens or signed URLs. Swift keeps a separate receipt list per account and rejects late progress or results after sign-out.

Validated raw manifests persist separately in `.forge-content/manifests`, including after successful installation. The key includes the full game identity, platform, and server-provided SHA-1. Forge still checks current ownership and requests the Live asset metadata before reading a cache entry. It validates the cached archive's hash, parsed app identity, format, and installation paths. A changed hash or invalid cache causes a CDN fetch; responses without a hash do not use this cache. Fresh asset metadata supplies the CDN location and deployment ID even on a cache hit. Signed URLs and account tokens are never persisted. Only actual CDN manifest bytes contribute to download network counters. Game files are still checked in full, and opening details for a size estimate does not write a cache.

When a repair needs chunks, the installer can try the other validated CDN locations from the current response if a mirror fails. This also works with a cached manifest, without downloading that manifest again to select a mirror. Cancellation stops retries immediately, and manifest query parameters are never forwarded to chunk requests.

Download format references:

- Legendary [binary manifests](https://github.com/legendary-gl/legendary/blob/42f6bdeadde3a9526dc8eb713763476999ac217b/legendary/models/manifest.py) and [JSON manifests](https://github.com/legendary-gl/legendary/blob/42f6bdeadde3a9526dc8eb713763476999ac217b/legendary/models/json_manifest.py).
- Legendary [chunk headers](https://github.com/legendary-gl/legendary/blob/42f6bdeadde3a9526dc8eb713763476999ac217b/legendary/models/chunk.py) and [rolling hash](https://github.com/legendary-gl/legendary/blob/42f6bdeadde3a9526dc8eb713763476999ac217b/legendary/utils/rolling_hash.py).
- EpicManifestParser [versioned chunk tables](https://github.com/NotOfficer/EpicManifestParser/blob/master/src/EpicManifestParser/UE/FChunkInfo.cs) and [encrypted-metadata arrays](https://github.com/NotOfficer/EpicManifestParser/blob/master/src/EpicManifestParser/UE/FEncryptedData.cs), which reference Epic's C++ serialization types.

The local fixtures exercise content transfer and reconstruction, corruption rejection, cancellation and resumed downloads, authentication refresh, account isolation, and the absence of credentials on CDN requests. These tests do not by themselves demonstrate a complete installation from Epic's production CDN.

A production smoke test on September 16, 2026 restored the saved session and downloaded the Mac build of Kingdom New Lands into an isolated temporary directory. It verified 1,048,576 bytes of game data after receiving 1,401,165 bytes of content and 1,436,603 network bytes including the manifest. Cancellation completed without reaching the deadline, and the temporary installation was removed. This validates real authentication, the manifest, CDN chunks, assembly and cancellation; it does not claim that the entire game was installed or launched.

To repeat this explicitly, run the macOS test `ForgeTests/EpicDownloadTests/testLiveEpicDownloadSmokeWhenRequested` with `TEST_RUNNER_FORGE_EPIC_DOWNLOAD_SMOKE=1` passed to `xcodebuild`. The test is skipped in the normal suite, persists rotated credentials, and stops after the first verified content or a 55-second download deadline.

Final validation for the download integration on September 16, 2026:

- `./scripts/test.sh` passed formatting, Clippy with warnings denied, 131 Rust tests with five optional network tests ignored, and the 115-test macOS suite with five intentional/host skips and no failures. App signature verification passed.
- Localization validation and catalog compilation passed with 433 complete English/French keys.
- The separate opted-in production smoke test passed with one test executed and none skipped.

## Launching

Every Epic launch first runs the existing installation pipeline against the current Live manifest. Valid files remain in place, while updates or repairs download the required chunks. Cancellation, verification failure, sign-out, or an account change prevents process creation. The operation keeps the game selected when Launch was clicked, even if the user opens another details page.

The Rust launch plan validates the completed receipt's namespace, catalog item, app name, platform, account, and canonical installation directory. It rechecks ownership through the current library, then obtains a fresh code from `GET /account/api/oauth/exchange`. Manifest arguments and the catalog's `AdditionalCommandLine` are parsed into literal arguments using the same helper as Steam. The plan includes Epic's account, app, sandbox, locale, and optional deployment arguments; deployment comes from the asset sidecar configuration. The executable's parent is the working directory.

When the catalog sets `OwnershipToken` to true, Forge posts `nsCatalogItemId={namespace}:{catalogItemId}` to `/ecommerceintegration/api/public/platforms/EPIC/identities/{accountId}/ownershipToken`. The native launcher writes the returned bytes to a private temporary file, passes its path through `-epicovt`, and deletes the directory after process termination or launch failure. Exchange codes and ownership tokens are never stored in installation receipts or logged. Rejected authentication is refreshed once, and the active account is checked again before returning the plan.

Steam and Epic share executable validation and the macOS process executor. Arguments are passed directly without a shell. Native Mac builds can run; Windows builds report the same missing-Wine limitation as Steam.

Launch protocol references checked on September 16, 2026: Legendary's [launch arguments and working directory](https://github.com/derrod/legendary/blob/master/legendary/core.py#L747-L841), [exchange and ownership token endpoints](https://github.com/derrod/legendary/blob/master/legendary/api/egs.py#L135-L147), and [additional catalog arguments](https://github.com/derrod/legendary/blob/master/legendary/models/game.py#L143-L147).

The optional macOS test `ForgeTests/EpicDownloadTests/testLiveEpicLaunchPlanSmokeWhenRequested`, enabled with `TEST_RUNNER_FORGE_EPIC_LAUNCH_SMOKE=1`, restores the saved account and requests a real launch plan for an existing installation. It saves rotated credentials and checks the executable and required arguments without starting the game or modifying its installation.

Launch validation on September 16, 2026:

- `./scripts/test.sh` passed formatting, Clippy with warnings denied, 143 Rust tests with five optional tests ignored, and the 127-test macOS suite with six intentional/host skips and no failures. Localization validation passed for 452 complete English/French keys, and app signature verification passed.
- Launch regressions cover ownership, receipt identity, path confinement, literal arguments, deployment metadata, token renewal, sign-out, cancellation, selection changes, and temporary ownership-file cleanup. Native execution tests run temporary fixtures, not installed games.
- The opted-in production launch-plan test passed for the installed Windows build of Among Us. Epic returned launch authorization, and the executable and required arguments were verified. The game was not executed; Windows execution remains unavailable without Wine.

Verification and cache validation on September 16, 2026:

- `./scripts/test.sh` passed formatting, Clippy with warnings denied, 161 Rust tests with five optional tests ignored, and the 127-test macOS suite with six intentional/host skips and no failures. Localization validation and app signature verification passed.
- HTTP fixtures cover persistent cache reuse after creating a new client, current ownership and authentication, changed hashes, corrupt archives, missing hashes, fresh CDN locations and deployment metadata, and zero manifest-network bytes on a cache hit.
- Shared verification tests cover bounded parallel readers, full-file checksums, monotonic progress, unsafe paths, cancellation, worker errors, and joining every reader before repair. CDN fixtures exercise repair through a second mirror after missing or corrupt chunk responses, without forwarding account credentials or manifest query parameters.

## Friends and live presence

### Implemented path

The native `epic-kit` social service reuses Forge's launcher OAuth session. REST uses Bearer authentication. The WebSocket at `wss://connect.ol.epicgames.com/` uses the raw access token in `Authorization`, without the REST prefix. Tokens remain in Rust, with refresh credentials saved only through the existing Keychain path.

Forge negotiates STOMP 1.2 with `CONNECT`, then subscribes to `<random-id>/account/<accountId>` with `id:sub-0`, automatic acknowledgments and a receipt. It waits for `core.connect.v1.connected` on that subscription: a receipt alone does not supply the connection ID needed for presence registration. The setup deadline remains active until the welcome arrives.

After the welcome, two bounded HTTP requests run concurrently while the socket reader continues:

- `GET https://friends-public-service-prod.ol.epicgames.com/friends/api/v1/<accountId>/summary?displayNames=true` replaces accepted relationships, including an empty result. Self, pending requests, suggestions and blocked accounts are excluded. An unexpected nonempty pagination cursor fails explicitly instead of silently truncating the list.
- `POST https://api.epicgames.dev/epic/presence/internal/v1/_/<accountId>/presence/<connectionId>`, with `X-HTTP-Method-Override: PATCH`, registers this connection. The body supplies automatic `status: online` and empty `conn.props`, matching the installed launcher's social component. Its `own` response is checked against the signed-in account. Friend states arrive through `presence.v1.UPDATE`; the response itself is not a friends snapshot. Registration is sent once per connection, including reconnects, and is not repeated for metadata changes.

Live comparison established that registration triggers delivery of an already-present friend. Without it, the same normal Forge session received no presence. The old initial collection GET returned HTTP 403 and is no longer used. A failed registration still preserves the accepted roster and reports the social error without revoking library authentication.

Epic rejected registration without a status with HTTP 400, code 1040. Forge therefore sends the launcher's automatic online status. This can make Forge an active launcher presence session. It omits `statusSettings`, party data and game activity; it does not request a manual preference change or overwrite another connection's properties. The official source distinguishes this automatic request from manual updates that include `statusSettings`. A manual-away override could not be tested because the user could not find that control in the launcher. Exact externally visible cleanup timing after closing Forge remains unverified.

A successful registration acknowledges the connection; it does not provide an authoritative complete circle or a marker proving every offline friend has been reported. Forge keeps unreported friends unknown, rather than copying the official component's offline fallback. Closing the socket or loading relationship metadata does not turn unknown friends into offline friends.

The live summary omitted display names despite the query flag. Missing names therefore use a bounded, optional `GET https://account-public-service-prod.ol.epicgames.com/account/api/public/account` with repeated `accountId` parameters, at most 100 per request. This route and response are implemented by [EGL3's GetAccounts](https://github.com/WorkingRobot/EGL3/blob/master/src/web/epic/responses/GetAccounts.h) and described in the [Account Service bulk lookup observation](https://github.com/LeleDerGrasshalmi/FortniteEndpointsDocumentation/blob/main/EpicGames/AccountService/Account/Lookup/AccountIds.md). Forge prefers the Epic display name, then a returned external display name, then the common unknown-name fallback. Positive and absent results are cached per connection for accepted friends. A friend-entry update invalidates that metadata cache. One request runs at a time under a ten-second enrichment budget while the socket continues reading. Failure preserves membership and presence instead of breaking social observation. Late profile responses cannot recreate deleted friends.

Presence events never create friendships. Namespace arrays replace previous arrays; an absent field preserves its group, while null or an empty array clears it. Active connections are aggregated deterministically. A closing connection does not remove another active connection's game. Offline, invisible and unknown account states clear activity. A later availability-only update cannot revive that old game. Available game names come from presence payloads, including EOS product properties and launcher running-app metadata, without consulting the user's owned games or inventing avatar URLs.

Relationship events update membership immediately and trigger a one-time reconciliation. Events received while HTTP is pending are replayed over its result. Metadata reconciliation preserves presence already received on the current connection. Removal/block tombstones prevent stale HTTP data from restoring deleted friends. A relationship change during an existing read schedules one coalesced follow-up for metadata. This is event-triggered reconciliation, not polling. Membership responses and socket events have no shared sequence number; removal tombstones and ordered event replay prevent a stale metadata response from restoring removed relationships.

The decoder accepts split and concatenated frames, LF/CRLF, byte-counted bodies, escaping and heartbeats. It rejects invalid framing and terminal STOMP errors. An unknown optional business event is ignored. Frame/message limits are 1 MiB, bootstrap responses 8 MiB each, and the reconciliation buffer 512 events or 4 MiB. Overflow discards the incomplete attempt and reconnects with bounded backoff. The full snapshot watch coalesces unchanged state and slow UI readers without dropping relation deltas from the reducer.

Social stop, retry and macOS wake do not revoke OAuth or cancel downloads. Voluntary sign-out still does both. A shared refresh mutex serializes library, download and social renewals; an independent credential revision saves rotation even if the following social operation fails. Account and observation generations reject late Swift results. A bounded OAuth refresh already in progress may finish after social cancellation so a rotated refresh token is not lost; its result cannot republish an obsolete social generation.

### Source evidence and current gateway differences

The [EGL3 REST client](https://github.com/WorkingRobot/EGL3/blob/master/src/web/epic/EpicClientAuthed.cpp), [summary response](https://github.com/WorkingRobot/EGL3/blob/master/src/web/epic/responses/GetFriendsSummary.h), [hosts](https://github.com/WorkingRobot/EGL3/blob/master/src/web/Hosts.h), [friends client](https://github.com/WorkingRobot/EGL3/blob/master/src/web/epic/friends/FriendsClient.cpp) and [presence model](https://github.com/WorkingRobot/EGL3/blob/master/src/web/stomp/messages/Presence.h) establish the launcher route and payload shapes. Its [transport implementation at the inspected revision](https://github.com/WorkingRobot/EGL3/blob/9490441d79bf4c8e58d421cd086d0bb41217cd3b/src/web/stomp/StompClient.cpp) dates from July 3, 2021. It is an independent implementation, not a stable Epic public API contract.

A newer [Presence Service observation](https://github.com/LeleDerGrasshalmi/FortniteEndpointsDocumentation/blob/a9517ca054949f9ad917f00a4549fd84f9eff71a/EpicGames/PresenceService/Presence/GetPresence.md), updated June 5, 2026, supplies the initial `circle=friends` request and response array. The Forge live check confirms that this service refused the current session, not that the session has the required permission.

The September 16, 2026 gateway accepted an HTTP upgrade but omitted `Sec-WebSocket-Protocol`. Advertising `v12.stomp` therefore failed tungstenite's subprotocol check. Forge leaves the WebSocket offer absent and validates the STOMP `CONNECTED` version instead. An incoming-heartbeat request of `30000,30000` produced `ERROR` with `Invalid heartbeat`. The accepted CONNECT sends both the standard `heart-beat` and EGL3's `heartbeat` spelling with `30000,0`. That combination was observed working; the independent necessity of the alias was not established.

Outgoing STOMP heartbeats follow the server's requested interval and the [STOMP 1.2 negotiation rules](https://stomp.github.io/stomp-specification-1.2.html#Heart-beating). No incoming STOMP timeout is enabled when the client requested zero. WebSocket Ping runs every 30 seconds with a 20-second Pong deadline to detect a silent transport. Setup, HTTP and writes have finite deadlines. Retry uses 1, 2, 4 and 8 seconds for five unsuccessful attempts, honors numeric `Retry-After`, and stops automatically for delays over an hour or unsupported date syntax. Permission denial does not cause logout. Authentication rejection refreshes once; a second refusal stops social retry.

Forge does not open EOS STOMP or Fortnite XMPP connections. Product-specific coverage and whether those transports would be needed require real observations. The [official Epic friends/presence guide](https://onlineservices.epicgames.com/news/querying-for-epic-friends-and-their-status) describes application consent restrictions; creating a Forge EAS application would not establish launcher-wide friend visibility.

### Follow-up: launcher permissions and EOS authentication

Further diagnosis on September 16, 2026 separated OAuth scope from legacy service permissions. The [launcher client permission inventory](https://github.com/Jaren8r/EpicClients/blob/prod/clients/34a02cf8f4414e29b15921876da36f9a.md) lists `basic_profile`, `country`, `friends_list` and `presence` as allowed scopes. Its legacy account permissions list `presence:_:{accountId}` with UPDATE and `presence:_:{accountId}:lastOnline` with READ, but not READ on the initial presence resource. This independent inventory explains the observed 403; an allowed EOS scope is not proof of access to a legacy REST operation.

A temporary probe used Forge's normal restored session and the same launcher client identity:

1. `GET /account/api/oauth/exchange` returned HTTP 200. The exchange code stayed in Rust memory.
2. `POST https://api.epicgames.dev/epic/oauth/v2/token` with `grant_type=exchange_code` and `scope=basic_profile friends_list presence` returned HTTP 200. The response explicitly granted both social scopes. No additional sign-in or consent screen was needed for this account. This is the exchange flow described by [Epic's Auth Web APIs](https://dev.epicgames.com/docs/web-api-ref/authentication#requesting-an-access-token).
3. The EOS token was rejected by the legacy Presence Service as unauthorized. A diagnostic GET on the corresponding EOS presence collection returned `errors.com.epicgames.social.presence.not_found`, code 24002. This did not establish a friends snapshot route or demonstrate that all friends were offline.

The existing legacy token also returned 146 permission entries when requested with `includePerms=true`. Neither that count nor the successful EOS grant constitutes a universal token or proves availability of future features. Each operation must use the appropriate service, token type and granted permissions.

The current launcher GraphQL host is `launcher.store.epicgames.com`, as used by [Legendary](https://github.com/legendary-gl/legendary/blob/master/legendary/api/egs.py), rather than EGL3's old `store-launcher.epicgames.com`. Its GraphQL server disables schema introspection. The launcher site's served JavaScript delegates `getFriendsList` to the native `social.getfriendslist` bridge; this source does not reveal the native initial-presence request.

These were diagnostic probes only. They did not publish the user's presence, export account credentials, or implement a new runtime dependency. The temporary Rust probes and shortened smoke-test observation were removed. At that stage the 40-second live test still failed and initial presence remained unresolved. The later installed-launcher and connected-friend comparisons below established the working registration; successful token issuance alone had been insufficient.

### Follow-up: installed macOS launcher's social component

After Epic Games Launcher was installed on September 16, its local request log identified a separate social component, version 2.4.0, deployment `1a9be8fc42ed298cdbbf89ec4b9cf5445fbb6f33`. The following evidence comes from Epic's publicly served [application module](https://social-overlay.ol.epicgames.com/1a9be8fc42ed298cdbbf89ec4b9cf5445fbb6f33-social-web-component/assets/app-Bo_FzhvT.js) and [shared client module](https://social-overlay.ol.epicgames.com/1a9be8fc42ed298cdbbf89ec4b9cf5445fbb6f33-social-web-component/assets/client-OP1MhDqo.js), rather than the older launcher store page. These deployment-specific implementation details are not a stable API contract.

- Launcher account data comes from `common.getsignedinuser`. The component accepts an `eg1~` access token as its elevated token. The source does not establish that switching Forge to an EOS token is necessary.
- Friends are read through `https://api.epicgames.dev/overlay-graphql/v1/graphql`. The `Friends.summary` query selects relationships, account names and external identities, without presence fields.
- Epic Connect uses `wss://connect.ol.epicgames.com`, a random UUID prefix for `/account/<accountId>`, and the connection ID from `core.connect.v1.connected`. The browser passes authentication in its WebSocket URL; Forge's native Authorization header was separately observed working for setup.
- Once connected, the component registers presence with `POST https://api.epicgames.dev/epic/presence/internal/v1/_/<accountId>/presence/<connectionId>` and `X-HTTP-Method-Override: PATCH`. It supplies `status` and `conn.props`. Its automatic registration omits `statusSettings`; an explicit manual status change supplies that field. The response's `own` field updates the component's own-account state.
- Friend presence is updated from `presence.v1.UPDATE`. The friend-view selector substitutes `offline` when no presence entry is available. An all-offline screenshot therefore does not, by itself, prove receipt of an authoritative initial presence snapshot. Forge continues to distinguish unknown presence from offline.

A temporary probe reproduced that registration through Forge's normal Rust session while the official launcher displayed the user as online. It sent `status: online`, empty connection properties, and no manual status settings, party or game activity. HTTP 200 returned an object containing only `own`. No friend presence event was observed during the 40-second check. This establishes registration acceptance, not a friends snapshot or a proven externally visible presence effect. The corresponding collection GET under the same internal gateway was also tested as a diagnostic hypothesis and returned HTTP 403, code 1003; it is not an implemented or verified read route.

At the end of that all-offline diagnostic, the baseline still returned 21 accepted friends, 21 resolved names and zero known initial presences. The temporary probes were removed pending an already-connected friend witness. The later witness check below established the registration needed by the implemented path.

The current component handles `friends.v1.FRIENDSHIP_REQUEST`, `FRIENDSHIP_REMOVE`, `FRIENDSHIP_ENTRY_UPDATE` and `USER_BLOCKLIST_UPDATE`. Forge now accepts those names as well as the older EGL3 names, preserves the pending/accepted distinction, and invalidates profile names for the new entry-update event. Its existing activity parser already handles `conn.props.running_app_id` and `running_app_name`; no activity-parser extension was needed.

### Connected-friend witness and correction

A friend already displayed as away in the official client supplied a controlled initial-state witness on September 16. The unmodified Forge runtime returned 21 accepted friends, all with unknown presence. A temporary registration using the same token returned HTTP 200, then `presence.v1.UPDATE` for an away friend; the first roster snapshot contained that state after 933 ms. Repeating the registration without `status` returned HTTP 400 and did not deliver friend presence. This comparison separated the missing registration from the refused legacy collection read.

The corrected runtime removes that collection read, registers using the welcome's connection ID, buffers initial events while loading membership/names, and retains them during subsequent metadata reads. Its live test reached `ready` after 1004 ms with 21 accepted friends, 21 names and one away friend already present in the first state. The other 20 initial statuses remained unknown. This proves an initial presence delivery for the available witness, not complete offline coverage. The friend could not perform a controlled status change, so spontaneous availability/game changes remain a live-validation limit.

### Verification record

Live checks on September 16, 2026 used `EpicSession.restore`, the existing Keychain item and the normal Rust service. Only stage names, HTTP status codes and aggregate counts were logged. No access token, refresh token, account ID or raw social payload was exported.

| Check | Observed result |
| --- | --- |
| Accepted-friend summary | HTTP 200, 21 accepted friends returned through the normal session |
| Display names | Summary supplied none; bounded Account Service lookup resolved all 21 |
| STOMP authentication and subscription | HTTP upgrade, `CONNECTED`, matching `RECEIPT` and `core.connect.v1.connected` received |
| Initial presence | Corrected registration returned HTTP 200; first ready state after 1004 ms included one away friend, with 20 unreported friends kept unknown |
| Spontaneous real availability/game transition | Initial-state witness was available but could not perform a controlled change |
| Cross-product activity coverage | Not demonstrated |
| Effect on the local account's externally visible presence | Automatic online registration required and implemented; manual settings and activity omitted; external preference/cleanup timing not independently verified |

The opt-in `EpicFriendsIntegrationTests.testLiveEpicFriendsWhenRequested` observes the service for 40 seconds by default and prints only counts and connection enums. `TEST_RUNNER_FORGE_EPIC_FRIENDS_SMOKE_SECONDS` can extend this to at most 300 seconds. With a controlled already-present friend, `TEST_RUNNER_FORGE_EPIC_FRIENDS_EXPECT_PRESENT=1` additionally requires a present friend in the first ready state. It requires `TEST_RUNNER_FORGE_EPIC_FRIENDS_SMOKE=1` when launched through `xcodebuild`. It fails if Epic cannot establish a ready initial state. A successful check does not treat unreported friends as offline or establish an unobserved live transition.

Controlled Rust tests use actual HTTP and WebSocket peers with independently written STOMP fixtures and virtual time. They exercise initial reads, concurrent events, relation filtering, removal/block reconciliation, namespace transitions, malformed framing, buffer bounds, revision races, coalescing, cancellation, heartbeat liveness, backoff, access refusal, shared refresh and credential revision. Swift tests cover the adapter, independent stores, modal lifetime, social-only retry, sign-out generations and credential persistence. These fixtures establish local behavior, not Epic delivery.

The opt-in AppKit renderer covers Steam and Epic, light and dark, at 860 × 600 and 1100 × 760, with a long scrolling list, offline search and empty state. It uses the actual common modal and keyboard close path. Rendered fixtures do not substitute for the real-account checks above.

Before the registration correction, `./scripts/test.sh` validation passed on September 16, 2026: 503 complete English/French localization keys, Rust formatting, strict Clippy, 209 Rust tests passed with five optional checks ignored, and 175 macOS cases with nine skipped and no failures. Apple certificate signature verification passed. The separate friends renderer generated 24 PNGs and passed. That earlier live Epic check failed on initial presence; the connected-friend correction and later results supersede that failure.

The rebuilt app was also checked with Steam and Epic connected together. Both real-account panels were inspected in light and dark themes. Epic names, scrolling, local search and keyboard dismissal worked; Steam still displayed two online friends and eleven offline friends. The system theme setting was restored afterward. Minimum-size and long-name coverage comes from the native 860 × 600 renderer fixtures, not a successful manual window resize.

After the registration correction, `./scripts/test.sh` passed: 503 localized keys, Rust formatting, strict Clippy, 211 Rust tests passed with five optional checks ignored, and 175 macOS cases with nine skipped and no failures. Apple certificate signature verification passed. The separate 180-second live check with `EXPECT_PRESENT=1` also passed, with no subsequent presence change observed. The rebuilt Debug app was relaunched and its actual Epic friends panel showed the away witness, the partial online count and no access-error banner. Unreported friends remained in the unknown-presence section.
