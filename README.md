# Forge

A macOS game library app. **Forge is a working name.** The interface uses SwiftUI and AppKit. A Rust library manages the local catalog and connects to Swift through UniFFI.

## What works

- Recent, Steam, Epic, GOG, and Other navigation, with Settings at the bottom of the sidebar.
- Manual import of `.app` bundles or `.exe` executables, with the catalog saved as JSON.
- Local search, removal from the catalog without deleting files, Mac app launches, and launch history.
- System, light, and dark appearance options.
- A shared fixed glass title bar for stores, with an avatar or initials, account menu, search, and sorting.
- Steam sign-in with a QR code or username, password, and Steam Guard. Sessions can be saved in the macOS Keychain.
- A Steam library with thumbnails, search, and sorting by last played, name, or playtime.
- Steam friends in a window-centered panel, with avatars, presence, current games, and changes received through the existing Steam connection.
- In-app game details with news, estimated size, and Windows or macOS downloads.
- One-click SteamPipe downloads in Rust, with file verification, cancellation, and resuming.
- A **Launch** button after installation. File verification and updates run before launch by default, with a preference under Settings → Steam games.
- macOS game launches using the paths and arguments declared by Steam.
- Epic Games sign-in in an embedded window with an automatic callback, optional session storage in the macOS Keychain, and a library with thumbnails, search, and name sorting.
- Epic friends in the same panel, with an initial friends summary and native STOMP/WebSocket presence events. See the [protocol notes](docs/epic-protocol.md#friends-and-live-presence) for live verification limits.
- Epic BuildPatch downloads in Rust, sharing Steam's progress controls, cancellation, file verification, and resumable installation behavior.
- Epic macOS game launches, with automatic file verification and updates before each launch.

Windows Steam games run in Forge's Wine runtime, built from the separate [forge-runtime](https://github.com/Zagrios/forge-runtime) repository. Each installation gets its own Wine prefix, the game reaches Forge's Steam host through Proton's lsteamclient, and Steam Auto-Cloud syncs the saves inside the prefix. Saves written through the SDK's RemoteStorage stay in the macOS Steam engine's storage, like a macOS game's. Development builds use a runtime installed by hand with forge-runtime's `install-dev.sh`; released builds will download it. GOG sign-in is still to be implemented. Multiplayer and controller support will have separate modules.

Steam Cloud's [Rust coordinator](docs/steam-cloud-coordinator.md) synchronizes supported AutoCloud and SDK RemoteStorage saves before launch and after game exit: AutoCloud saves of macOS games in the player's home, of Windows games in their Wine prefix. The detached supervisor completes the exit check even when Forge is closed. Divergent saves require an explicit Local/Cloud choice. An [authenticated lifecycle validation](docs/steam-cloud-lifecycle.md) exercised Silksong with the interface closed. FileShare and Dynamic Cloud remain unsupported.

## Languages

The interface supports English and French. It follows the system language and falls back to English when the requested language is unavailable. English is the source language.

User-facing text belongs in `apps/macos/Forge/Resources/Localizable.xcstrings` and is accessed through the shared `L10n` helper. Add both English and French translations when adding text. Any text that must remain hardcoded, such as technical diagnostics, must be in English. Run `python3 scripts/verify-localization.py` to validate the translation catalog and its references.

## Steam

Open the Steam tab. Scan the QR code with the Steam mobile app, or enter your credentials in Forge and complete Steam Guard confirmation. Forge never saves your password. The option to remember the session stores only the session in the macOS Keychain.

The account menu opens **Steam friends** and shows the online count once the initial list has loaded. The panel searches names locally, lists friends playing games first, then other online friends, and keeps offline friends in a collapsible section. Search includes that section even when collapsed. Presence and game activity remain separate, so an away friend can still be playing. Missing names, avatars, and game names have fallbacks.

Friends keep updating while the panel is closed or another section is selected. A connection interruption marks the last list as stale. Forge makes a bounded reconnection attempt and loads a fresh list; rejected authentication requires signing in again. This uses Forge's native Steam session without Steam, Wine, or a Steamworks SDK installation. Chat, invitations, friend management, and GOG friends are not implemented. See [the CM notes](docs/steam-cm-notes.md) for protocol references and verification limits.

The library shows the games Steam returns for the signed-in account, including free games that have been played. Games without a play history appear after recently played games. Click a thumbnail to open its details. **Download** automatically chooses macOS when available, otherwise Windows, using the system's preferred language. There is no platform, language, or folder picker.

Games are saved in `Games/Steam/steam-<appid>-<platform>` under Forge's data directory. Existing installations keep their location during verification and updates. Verified files and chunks remain available after cancellation and are reused when resuming.

The details page places the size next to playtime and the main button on the right. During an operation, a white bar at the bottom of the button shows progress. Its label distinguishes preparation, verification, and downloading. Hover over it for details, or use the adjacent cross to cancel. After installation, **Launch** verifies files and applies updates before starting the macOS game. Cancellation, an error, or signing out prevents the launch. You can disable this preference under Settings → Steam games, but an installation with an interrupted update must still be repaired before playing. An initial download never starts the game automatically.

Verification reads all existing file contents, with up to four readers and accelerated SHA-1 calculation on Apple Silicon. It keeps valid files and reports its own progress. Game bytes actually downloaded are counted separately. Forge still contacts Steam to check for updates. A cached manifest is reused if it is still current. An invalid cache is fetched again.

On Apple silicon, launching starts Forge's Rust host with the already-loaded session, then the macOS game. Silksong has been validated with the genuine Steamworks library, account/license checks and stats received in the game, without opening Steam or requesting Keychain access again. Forge now bundles the pinned Valve libraries, acquired directly from fixed Valve CDN packages during the build. It never imports an installed Steam client, including on first launch. Other games and SDK versions need compatibility testing. See [native Steam launches](docs/steam-native-launch.md). AutoCloud transfers belong to Rust; the native host cannot read the game's ordinary save roots.

This first version downloads the public branch, including depots for the selected platform and owned DLC. If Steam does not recognize the system language, or a translated depot is missing, Forge uses English. Private branches, Workshop, SDK/Dynamic Cloud, and install script execution are not integrated. Depots containing symbolic links are explicitly rejected. Updates retain old files that are no longer used.

The protocols are implemented in `steam-kit` based on a study of the public SteamKit and DepotDownloader sources. Forge runs neither of these programs nor SteamCMD. See [the protocol and dependencies](docs/steam-protocol.md) for sources, validation, and exact limitations.

## Epic Games

Open the Epic tab, then choose **Sign in to Epic Games**. A window displays the Epic Games website. Sign in and complete any security prompts on that site. Forge automatically finishes signing in, closes the window, and loads the library. Closing the window cancels the attempt.

The option to remember the session stores the refresh token in a macOS Keychain item separate from Steam. Forge refreshes the session and replaces the saved token when Epic rotates it. Sign-in pages use a temporary web session. Forge neither reads nor saves the password. The account menu lets you refresh the library or sign out.

The account menu also opens **Epic friends**. It shows accepted friends and the presence/game data returned by Epic, using the same panel, local search, keyboard navigation and avatar fallbacks as Steam. Both stores can stay connected while another library is selected. Closing the panel keeps the social connection alive. Only one account's panel is visible at a time.

After the STOMP welcome, Forge reads the friends summary and registers the connection to receive initial presence events. Relationship events refresh membership; reconnects establish a new presence registration. There is no periodic friends/status polling. Unknown presence stays explicit, and an interrupted connection marks cached rows as stale. Social access errors leave the library and downloads available. Retry and macOS wake restart only the social transport. This feature needs neither Epic Games Launcher nor Wine. Epic requires an automatic online registration for this connection. Forge sends no manual presence settings, party or game activity. Unreported friends remain unknown.

Live validation on September 16, 2026 returned 21 accepted friends with resolved names and delivered an already-connected friend's away status in the first ready state, about one second after connection. This uses the registration observed in the official launcher and replaces the denied legacy initial-presence read. The other 20 unreported statuses remained unknown. Real subsequent status and game transitions remain unverified. See [the protocol evidence and remaining checks](docs/epic-protocol.md#friends-and-live-presence).

The library shows owned games, their cover art, and the metadata available from Epic. It offers search, alphabetical sorting, and a details page using the Steam layout, with a banner, information, and a working **Download** button. Opening a game loads its account playtime and estimated installation size from Epic. Size uses the Mac build when available, otherwise Windows, and includes all files in that build's manifest. Both stores share the same persistent scrollbar. Unavailable values appear as a dash. Epic's playtime response does not provide a last-launch date, so that field remains unavailable. Unreal content and add-ons are not shown as standalone games. See [the Epic protocol and its validation](docs/epic-protocol.md).

**Download** chooses the Mac build when available, otherwise Windows, and saves it in `Games/Epic/epic-<identity-hash>-<platform>` under Forge's data directory. The identity includes the namespace, catalog item, and app name. Progress and cancellation use the same controls as Steam. One Epic download can run at a time, even when browsing another game. Cancelled or failed transfers can resume, including after restarting Forge, by verifying existing files and reusing validated chunks. After installation, **Launch** checks the current build, repairs missing or changed files, applies available updates, then starts the macOS game. Cancellation, an error, or signing out prevents launch. An initial download never starts the game automatically. All manifest files and languages are included; encrypted content and symbolic-link installations are rejected explicitly.

Epic and Steam share the verification scheduler, with up to four disk readers and accelerated SHA-1 on Apple Silicon. Epic retains validated manifests on disk across app restarts. Each verification checks ownership and the current manifest hash with Epic before reusing the cache. A changed hash or corrupt cache triggers a fresh manifest download; a response without a manifest hash is downloaded without cache reuse. Cached manifests do not skip checking the game files, and cached bytes are not counted as network traffic.

Launching uses the executable and arguments declared by Epic, a fresh exchange code for the current account, and an ownership token when required by the game. Steam and Epic share the native process launcher. Epic's Windows games remain downloadable, but launching them through Wine is not wired yet.

## Getting started

You need macOS, Xcode with its command-line tools, stable Rust with Cargo, and a valid Apple Development identity with its private key in the Keychain. The app targets macOS 26 and later. Builds use the host Mac's architecture.

Signing settings shared by Debug and Release live in `apps/macos/Config/Signing.xcconfig`. To select a specific certificate, copy `Signing.local.xcconfig.example` to `Signing.local.xcconfig` in the same directory, then set the certificate and team. Git ignores this local file. `security find-identity -v -p codesigning` lists available certificates. Use the fingerprint to avoid an ambiguous selection.

```sh
./scripts/run.sh
```

Alternatively, open `apps/macos/Forge.xcodeproj` in Xcode, select the **Forge** scheme, and choose Run. The Xcode project is included, so XcodeGen is not needed for this step. The first build downloads Cargo dependencies and generates the Swift bridge.

Builds use a stable Apple Development signature. The build, test, and DMG scripts reject ad hoc signatures. Notarized distribution still needs configuration. The app does not use App Sandbox, so it can open local apps selected by the user and eventually manage Wine processes. No driver, privileged entitlement, or system-level helper is included.

On the first signed launch after an older ad hoc build, macOS may still ask for permission to read the Steam session. Choose "Always Allow" to trust this identity across builds. The dialog asks for the "login" Keychain password, usually the password for your Mac account. Keeping the same signing identity preserves that trust across rebuilds. A locked Keychain or a change of identity may still require user action. Forge does not move the token to a file or change permissions for other apps. See [Apple's code signing requirements](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements).

## Verification

```sh
./scripts/test.sh
```

This command checks English and French localization, Rust formatting, Clippy, catalog and kit tests, Swift → UniFFI → Rust integration tests in Xcode, and the app's signature. Tests use temporary directories and do not launch games.

## Building the installer disk image

```sh
./scripts/build-dmg.sh
```

This command builds Release and creates `build/dist/Forge-<version>-<architecture>.dmg` with its SHA-256 checksum. Open the DMG, then drag **Forge** to **Applications**. The disk image shows the two icons side by side.

The current build targets the host Mac's architecture, Apple Silicon on the development machine. Developer ID signing and notarization are not yet configured for public distribution. See [macOS distribution](docs/distribution-macos.md).

## Structure

```text
apps/macos/
  Forge.xcodeproj       Shared Xcode project
  project.yml          XcodeGen definition
  Config/              Display name, version, bundle identifier
  Forge/
    App/               Swift lifecycle, state, and coordination
    Models/            Presentation types independent of UniFFI
    Features/          Library, navigation, and settings
    DesignSystem/      Shared colors and visual components
    Bridge/            Swift adapter for the Rust core and generated code
  ForgeTests/          Integration tests between both languages
crates/                Catalog, UniFFI bridge, and store kits
docs/                  Architecture and scope
scripts/               Building, launching, and verification
```

The library is stored in `~/Library/Application Support/com.zagrios.forge`. To use an isolated development catalog, set `FORGE_LIBRARY_DIRECTORY` before launching the binary. This setting does not change the Keychain identifier. Store sessions start when their tab is opened or a store operation is requested. Once connected, Steam keeps its friends observation active across section changes until sign-out. Download receipts are separated by SteamID or Epic account ID and contain no secrets. Friends and their avatar cache stay in memory.

## Editing the project

After changing the Swift file structure or Xcode settings, install XcodeGen 2.46.0 or later, then run:

```sh
./scripts/generate-project.sh
```

`build-rust.sh` regenerates files in `Bridge/Generated`. Do not edit them manually. `Cargo.lock` pins Rust dependency versions. This repository redistributes neither the proprietary Steamworks SDK nor a Wine engine.

The display name is configured in `apps/macos/Config/App.xcconfig`. Keep the storage identifier stable when renaming the app, or provide an explicit migration. See [the architecture](docs/architecture.md).
