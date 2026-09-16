# Store services during game launch

Research checked on 2026-09-16. This note describes service boundaries for a macOS launcher. It does not establish compatibility for any particular game, Wine build, or graphics backend.

## Steam

Valve documents that `SteamAPI_Init` needs a running Steam client to supply Steamworks implementations, the correct App ID, the same OS user context, and a valid game license. `SteamAPI_RestartAppIfNecessary` can relaunch through `steam://run/<AppID>`. Logging into the Steam network and downloading a game therefore does not, by itself, provide the runtime that the game's Steamworks DLL expects. [Steamworks API overview](https://partner.steamgames.com/doc/sdk/api)

The official Steam user interface is a separate question. GameNative's Android implementation supplies an authenticated Steam runtime through a helper process and Proton's `lsteamclient` bridge, without requiring the user to install the desktop Steam application separately. This is concrete evidence for investigating the same user experience in Forge. It is not a complete independent Steamworks reimplementation or a verified macOS solution. Its critical bootstrap binary is proprietary and its source is withheld. See [the GameNative investigation](research-gamenative-steam.md), [bootstrap source](https://github.com/utkarshdalal/GameNative/blob/f17309da1add0bf6d74b9f96cd99579e5101cb13/app/src/main/java/app/gamenative/SteamBootstrap.kt), and [component notices](https://github.com/utkarshdalal/GameNative/blob/f17309da1add0bf6d74b9f96cd99579e5101cb13/THIRD_PARTY_NOTICES).

Design inference: investigate a managed Steam runtime and a Windows-to-host Steamworks bridge as the preferred Forge experience. Use Windows Steam with the game in the same Wine environment as a reference or fallback during validation. Do not assume native macOS Steam communicates with a Windows Steamworks DLL without a specifically implemented and tested bridge. This prefix recommendation is an architectural inference, not an explicit Valve compatibility guarantee. CodeWeavers' supported installation path installs Steam into a CrossOver bottle. [Installing Steam in CrossOver](https://support.codeweavers.com/en_US/installing-steam-in-crossover)

The game remains responsible for reporting achievements and game-specific rich presence through its existing integration. Forge should preserve that path and verify the results in the real Steam account. A local Forge session timer is a separate measurement from the store's authoritative playtime.

Steam invitations have distinct delivery paths:

- A closed game receives connection arguments at launch. Lobby invitations use `+connect_lobby <id>`.
- An already running game receives `GameRichPresenceJoinRequested_t` or `GameLobbyJoinRequested_t`.
- The game's implementation decides how to process the request.

Forge must preserve these arguments and callbacks, including the correct game, store account, and Wine environment. Starting a second copy of a running game is not a substitute for delivering its callback. [ISteamFriends](https://partner.steamgames.com/doc/api/isteamfriends), [Steam matchmaking and lobbies](https://partner.steamgames.com/doc/features/multiplayer/matchmaking)

## Epic

The official Epic launcher is not universally required for game authentication. Epic's Unreal documentation describes browser account-portal authentication and launcher exchange-code authentication. Legendary explicitly implements authenticated game launch for multiplayer and DRM, including Wine on macOS, and manages the EOS overlay separately. These establish a viable direct-launch route, not support for every title. [Epic authentication flows](https://dev.epicgames.com/documentation/unreal-engine/online-subsystem-eos-plugin-in-unreal-engine), [Legendary source and documented features](https://github.com/legendary-gl/legendary)

GameNative independently uses the same exchange-code and ownership-token launch pattern, and installs the EOS overlay into the game's Wine environment. Forge already prepares comparable launch credentials in `crates/storefronts/epic-kit/src/client/launch.rs`; overlay provisioning and end-to-end online validation remain separate work. [GameNative launch parameters](https://github.com/utkarshdalal/GameNative/blob/f17309da1add0bf6d74b9f96cd99579e5101cb13/app/src/main/java/app/gamenative/service/epic/EpicGameLauncher.kt), [GameNative overlay installation](https://github.com/utkarshdalal/GameNative/blob/f17309da1add0bf6d74b9f96cd99579e5101cb13/app/src/main/java/app/gamenative/service/epic/EpicOverlayManager.kt)

A Forge Epic account session is distinct from the game's EOS session. The game retains its own product configuration, identity-provider configuration and existing integration. Forge must prepare the launch credentials that this integration expects. Creating a separate Forge EOS application does not give it universal authority over other games' achievements, lobbies or social state. This is an architectural conclusion from the per-product configuration documented by Epic. [Epic authentication flows](https://dev.epicgames.com/documentation/unreal-engine/online-subsystem-eos-plugin-in-unreal-engine)

A unified friends interface also has an authorization boundary. Epic's current account-support explanation says applications can receive the user's friends who have authorized that game, with explicit consent. This is not automatically the unrestricted friends list available to the Epic Games Launcher. [Epic account information shared with other services](https://www.epicgames.com/help/c-45487929/c-40721840/a16686016)

Do not equate working EOS authentication, achievements or matchmaking with synchronized Epic launcher playtime. The current Heroic playtime setting explicitly states that store synchronization supports GOG only. Its Epic implementation does not establish an equivalent Epic playtime path. This is evidence about Heroic's implementation, not proof that every possible Epic integration is impossible. [Heroic playtime setting](https://github.com/Heroic-Games-Launcher/HeroicGamesLauncher/blob/main/src/frontend/screens/Settings/components/PlaytimeSync.tsx)

## GOG

GOG separates launching a DRM-free game from providing Galaxy services. Its official SDK feature matrix documents authenticated multiplayer, achievements, rich presence and game-time tracking when Galaxy is running and the user owns the game, even if launched externally. The matrix distinguishes overlay availability when launched through Galaxy. Offline support also differs by feature. [GOG SDK feature matrix](https://docs.gog.com/sdk-galaxy-feats-and-states/), [GOG SDK overview](https://docs.gog.com/sdk/)

This describes GOG's official implementation; it does not prove a replacement is impossible. Heroic already implements GOG playtime submission and presence updates directly. [Heroic GOG playtime implementation](https://github.com/Heroic-Games-Launcher/HeroicGamesLauncher/blob/main/src/backend/storeManagers/gog/games.ts), [Heroic GOG presence implementation](https://github.com/Heroic-Games-Launcher/HeroicGamesLauncher/blob/main/src/backend/storeManagers/gog/presence.ts)

Comet provides another concrete alternative. Its implementation checklist marks achievements and statistics as supported, and release v0.3.2 includes both Apple Silicon and Intel macOS artifacts. Its `GAME_JOIN_REQUEST_NOTIFICATION` and `GAME_INVITE_SENT_NOTIFICATION` entries remain unchecked. It is therefore a candidate for preserving achievements without Galaxy, but this evidence does not establish complete multiplayer invitations. Shipping macOS binaries also does not prove every Windows game's integration works through Wine. [Comet source and feature checklist](https://github.com/imLinguin/comet), [Comet v0.3.2 release](https://github.com/imLinguin/comet/releases/tag/v0.3.2)

GOG invitations likewise preserve a game-specific connection string. Galaxy passes it as a launch argument when the game is closed; a running game handles SDK listeners or the overlay callback. [GOG invitation examples](https://docs.gog.com/demo-invitations-implementation/), [GOG IFriends API](https://docs.gog.com/galaxyapi/classgalaxy_1_1api_1_1IFriends.html)

## Multiplayer and anti-cheat

Wine compatibility, store login, matchmaking, crossplay and anti-cheat acceptance are separate requirements. Forge does not need to relay normal gameplay traffic to preserve an existing game's multiplayer implementation. It needs to launch the unmodified game in the environment its client libraries expect. This is the recommended architecture, not a universal compatibility claim.

Valve describes Proton as a Linux compatibility layer. Its EAC and BattlEye support needs action by each game's developer; kernel-space anti-cheat is not supported by the documented Proton model. This does not establish equivalent support for macOS Wine. [Valve Proton documentation](https://partner.steamgames.com/doc/steamhardware/proton)

Epic's anti-cheat announcement explicitly distinguishes support for native Linux and Mac builds from Wine and Proton support **on Linux**. A Steam Deck compatibility result therefore cannot be used as proof of macOS Wine multiplayer compatibility. [Epic anti-cheat platform announcement](https://onlineservices.epicgames.com/news/epic-online-services-launches-anti-cheat-support-for-linux-mac-and-steam-deck)

## Recommended product contract

The user prefers installing only Forge. Aim for one user-facing Forge installation that prepares required runtimes and store components on demand. Investigate GameNative's separation of Steam runtime from desktop interface before making a full official client a product requirement. A full client remains an engineering reference or fallback, not the user's preferred experience. Whether each component can be redistributed or installed automatically must be checked against its distribution terms before shipping.

Track capabilities per game, store build and runtime version: launch, online session, invitations with the game open and closed, achievements, authoritative store playtime, friends presence and overlay. Do not reduce these to a single compatibility flag. Local playtime needs process/session tracking through child-process handoffs, crashes and sleep; store playtime needs independent remote verification.

For a first proof of concept, validate one Steam Windows title with online co-op and achievements and one Epic title with authenticated online play. Test invitations in both directions and compare store accounts before and after play. A successful executable launch alone is insufficient evidence.
