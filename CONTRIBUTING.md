# Contributing to Obelisk

This guide covers building Obelisk from source, testing it and finding your way around the code. To install Obelisk as a player, see the [README](README.md).

## Contents

- [Requirements](#requirements)
- [Building and running](#building-and-running)
- [Testing](#testing)
- [Windows games in development](#windows-games-in-development)
- [Project layout](#project-layout)
- [Interface text and translations](#interface-text-and-translations)
- [Editing the Xcode project](#editing-the-xcode-project)
- [Releases](#releases)
- [License](#license)

## Requirements

- A Mac with Apple silicon and macOS 26 or later. Builds target the Mac's own architecture.
- Xcode with its command-line tools.
- Stable Rust with Cargo, through [rustup](https://rustup.rs), with both Mac targets: `rustup target add aarch64-apple-darwin x86_64-apple-darwin`. Part of the Steam host is built for Intel, for the Windows games Wine runs under Rosetta.
- An Apple Development signing identity, with its private key in your Keychain. The build, test and DMG scripts refuse ad hoc signatures.

Obelisk does not use the App Sandbox, so that it can open games and manage Wine processes. It ships no driver, privileged entitlement or system helper.

## Building and running

Signing settings shared by Debug and Release live in `apps/macos/Config/Signing.xcconfig`. To pick your own certificate and team:

```sh
cp apps/macos/Config/Signing.local.xcconfig.example apps/macos/Config/Signing.local.xcconfig
security find-identity -v -p codesigning   # lists your certificates
```

Set the identity and team in `Signing.local.xcconfig`, which git ignores. Use a certificate's fingerprint to avoid an ambiguous match.

Then build and launch Obelisk:

```sh
./scripts/run.sh
```

You can also open `apps/macos/Obelisk.xcodeproj` in Xcode, select the **Obelisk** scheme and choose **Run**. The first build downloads the Cargo dependencies and generates the Swift bindings of the Rust core.

Keep the same signing identity across builds: macOS then keeps trusting it for the Keychain items Obelisk saves sessions in. After a change of identity, macOS asks once for your login Keychain password; choose **Always Allow**.

Debug builds save sessions in their own Keychain items, `com.zagrios.obelisk.debug.steam`, `.debug.epic` and `.debug.gog`, so sign in to each store once in a Debug build. A release is signed by another team, and macOS would otherwise ask for your password each time one build reads an item the other saved.

By default, a build uses the same library as the installed app, in `~/Library/Application Support/com.zagrios.obelisk`. Set `OBELISK_LIBRARY_DIRECTORY` before launching the binary to use another one.

## Testing

```sh
./scripts/test.sh
```

This checks the English, French and Italian translations, runs `cargo fmt`, Clippy and the Rust tests, runs the Swift ↔ Rust integration tests in Xcode, and verifies the app's signature. Tests use temporary folders and never launch games.

## Windows games in development

Windows games run in [Voile](https://github.com/Zagrios/voile), Obelisk's Wine runtime, in `<library>/Voile/<version>`. Obelisk runs the newest release installed of its contract, the paths, variables and programs it relies on in a runtime, which [docs/CONTRACT.md](docs/CONTRACT.md) lists and `crates/game-runtime/voile-release.json` numbers. As a Windows game launches, Obelisk reads Voile's signed `voile-feed.json`: a newer release of its contract downloads once no game runs, and a game whose profile needs a newer release than the one in place waits for it. The first Windows game gets a release with its download. `voile-release.json` also names the release a build installs before it has read the feed; the Release workflow writes the newest one there with `scripts/pin-voile.sh` before it builds, so it needs no commit.

- **A local Voile build:** run Voile's `./scripts/install-dev.sh`. It installs the runtime in `<library>/Voile/current`, which takes precedence over every release.
- **A local copy of the release in `voile-release.json`:** a Debug build given `OBELISK_VOILE_ARCHIVE=<path to the archive>` installs that file instead of downloading it, after checking its SHA-256.

Steam games also need Valve's Steam components, pinned in `crates/game-runtime/runtime-manifest.json` and downloaded from Valve's CDN into `~/Library/Caches/com.zagrios.obelisk/SteamRuntime`. Obelisk never ships a Valve binary.

What Obelisk knows about particular Windows games is in `crates/game-runtime/profiles.json`. A Release build reads it again from `main` as it starts and as a Windows game launches and keeps the last one in its library, so a change pushed to `main` reaches players within minutes; a Debug build keeps the file it was built with, as edited. An entry names its game by Steam app ID (`steam`), Epic namespace and app name (`epic`) or GOG product ID (`gog`), and by a file only that game's installation holds (`path`, relative to the installation), which finds it on any storefront. Obelisk looks for the storefront's ID first, then for the file. A game no entry names runs on the Linux engine when its installation holds Easy Anti-Cheat, which refuses Rosetta's Wine: an `EasyAntiCheat` folder whose `Settings.json` names the game's program, beside the bootstrapper, the program whose version resource describes it as "Easy Anti-Cheat Bootstrapper" whatever its file's name, in the installation's folder or two levels below. It renders with D3DMetal when the game's program imports Direct3D 12 without Direct3D 11, else with DXMT. On the Linux engine, which runs neither 32-bit nor .NET programs, a storefront's launch program of either kind gives way to that bootstrapper, with the same arguments, unless the profile names an `executable`. Any entry for the game wins over this, even one that changes nothing. The entry can then give the `engine` (`Rosetta` or `Linux`) and the `graphics` backend (`D3DMetal` or `Dxmt`) the game runs with, the `executable` started instead of the publisher's, relative to the installation, the `arguments` added after the storefront's, the environment `variables` and `dll_overrides` its Wine needs, and the oldest Voile release it runs in, `minimum_voile`, such as `v26.3.0-16`: the game still runs in the newest release installed, and its launch waits for a download only when an older one is in place. Its `note` says why. Obelisk checks each entry as it reads the file and leaves out one that sets what a profile may not: another key, one of Obelisk's own variables or DLLs, a `DYLD_` or `LD_` variable, a path outside the installation. The file's `format` changes only when an older Obelisk would read it wrong. A player's launch settings apply over it. A Visual C++ runtime installer, which storefronts run as a prerequisite, runs with `CX_HIDE_BUILTIN_VERSION_RESOURCES=1`: otherwise it keeps Wine's own `msvcp140`, `msvcp140_2` and `vcruntime140_1`, whose versions match its own, and a `n,b` override of them would find no Microsoft DLL. `cargo test -p game-runtime profile` checks the file.

What Obelisk tells players about particular games is in `crates/game-runtime/notes.json`: a few sentences a game's page shows under its metrics, such as what works of the game and what does not yet. Like the profiles, a Release build reads it again from `main`, as it starts, and keeps the last one in its library, so a note pushed to `main` reaches players without a release; a Debug build keeps the file it was built with. An entry names its game by Steam app ID (`steam`), Epic namespace and app name (`epic`) or GOG product ID (`gog`), and gives its `text` by language code, `en` always: a page shows the note in Obelisk's language, else in English. Each text holds at most 400 characters. Obelisk leaves out an entry without English or with a longer or empty text, and ignores a file that does not parse or holds no note it can show: pages then show the last notes it read, else those it was built with. It leaves other keys aside. A note changes nothing about how its game runs, and stays out of `profiles.json`, whose older readers would leave out an entry with a key they do not know. `cargo test -p game-runtime notes` checks the file.

Logs are in `~/Library/Logs/Obelisk`, one folder per store. A file named `winedebug` at the root of an installed Voile runtime replaces Obelisk's default Wine debug channels, for example `err+all,+steamclient`.

Fall Guys' Epic build also needs Epic's EOS social overlay for friend requests and party invitations. Before launching it, Obelisk downloads and verifies Epic's standalone Windows overlay asset into `drive_c/obelisk/EOSOverlay` in the selected engine's Wine prefix and registers its C: path in that prefix's user registry. This replaces the overlay setup its 32-bit EOS bootstrapper would perform; the game still starts through its 64-bit anti-cheat bootstrapper. The in-game overlay's **Invite friends…** action returns to the game and sends EOS's Shift+F3 shortcut. If macOS does not allow posting keyboard input, it shows the shortcut to press manually. A successful build or download does not confirm that the EOS UI renders or that a remote player receives an invitation; check those in a live game.

## Project layout

```text
apps/macos/
  Obelisk.xcodeproj     Xcode project, generated from project.yml
  project.yml           XcodeGen definition
  Config/               Name, version, bundle identifier and signing
  Obelisk/
    App/                Lifecycle, app state, overlay and menu bar
    Bridge/             Swift side of the Rust core; Generated/ is written by the build
    DesignSystem/       Shared visual components
    Features/           Home, store libraries, game pages, settings, overlay
    Models/             Store accounts, downloads, games and Cloud saves
    Resources/          Assets and Localizable.xcstrings
  ObeliskTests/         Swift ↔ Rust integration tests
crates/
  obelisk-core/         Catalog, library cache, verification and the shared download engine
  obelisk-bridge/       UniFFI bridge between Swift and Rust
  game-runtime/         Installations, download queue, launches, Wine, Cloud sync
  cloud-vault/          Local archive of each account's save versions
  steam-native/         Host that runs Valve's Steam components for games
  storefronts/          steam-kit, epic-kit and gog-kit: each store's protocols
packaging/              DMG layout and helper apps
scripts/                Build, run, test and release scripts
```

`CONTEXT.md` defines the words the code uses, such as Transfer, Game session or Reservation. Read it before changing how games install or launch.

The Steam, Epic Games and GOG protocols are implemented in Rust, from a study of public projects such as SteamKit, DepotDownloader, Legendary and Heroic. Obelisk runs none of them, nor SteamCMD. This repository redistributes neither the Steamworks SDK nor Wine.

## Interface text and translations

The interface is in English, French and Italian. It follows the system language and falls back to English, unless the player picks a language in **Settings → Language**. That choice is saved as Obelisk's own `AppleLanguages`, as macOS's per-app language setting does, and applies when Obelisk restarts.

- Every text shown to the player lives in `apps/macos/Obelisk/Resources/Localizable.xcstrings` and is read through the `L10n` helper.
- Add the English, the French and the Italian translation together.
- Text that has to stay in code, such as technical diagnostics, is in English.
- `python3 scripts/verify-localization.py` checks the catalog and every key the code uses.
- The language picker lists every translation the app bundle contains. Each one needs its flag in `Assets.xcassets`, as an image set named `Flag-<code>`, taken from the same [Free Flat Flags](https://github.com/Zagrios/bs-manager/blob/chore/ressources/Img/free_flat_flags_227.zip) set as the others. A language several countries speak shows two flags split across the diagonal, as English shows the United States' and the United Kingdom's: `AppLanguage.countries` names them.

## Editing the Xcode project

After adding, moving or removing Swift files, or changing Xcode settings, install [XcodeGen](https://github.com/yonaskolb/XcodeGen) 2.46.0 or later and regenerate the project:

```sh
./scripts/generate-project.sh
```

Never edit the files in `Bridge/Generated`: `scripts/build-rust.sh` regenerates them. `Cargo.lock` pins the Rust dependencies.

The app's display name and version are in `apps/macos/Config/App.xcconfig`. The storage identifier, `com.zagrios.obelisk`, must stay the same when the app is renamed, or come with a migration.

## Releases

```sh
./scripts/build-dmg.sh
```

This builds the Release app, signs it with the Developer ID set as `DISTRIBUTION_CODE_SIGN_IDENTITY`, and creates `build/dist/Obelisk-<version>-arm64.dmg` with its SHA-256. It then notarizes and staples the disk image, with the notarytool keychain profile `obelisk-notary` (or `$OBELISK_NOTARY_PROFILE`), or the App Store Connect API key given by `OBELISK_NOTARY_KEY`, `OBELISK_NOTARY_KEY_ID` and `OBELISK_NOTARY_ISSUER`. `--no-notarize` skips that step for a local test.

To publish a version, set `MARKETING_VERSION` in `apps/macos/Config/App.xcconfig`, commit, and push a tag `v<version>`, such as `v0.2.0`. The Release workflow checks the tag against the app version, builds the notarized DMG and drafts the GitHub release with it.

Obelisk updates itself with [Sparkle](https://sparkle-project.org). It reads `appcast.xml` from the latest release, so players get a version only once its release is published. Publishing a release, or editing its notes afterwards, runs the Appcast workflow: it signs the DMG with the EdDSA key in the `SPARKLE_ED_PRIVATE_KEY` secret, turns the release notes into the HTML Sparkle shows, and attaches `appcast.xml`. `scripts/write-appcast.sh` writes the same feed locally, with the key that `generate_keys --account zagrios` keeps in the login Keychain. The key's public half is `SUPublicEDKey` in `apps/macos/Config/Info.plist`. Without the private key, Obelisks already installed accept no new version, so keep a copy of it outside this Mac. Debug builds never update.

## License

Obelisk is under the [GNU General Public License v3.0](LICENSE). By sending a contribution, you agree that it is shared under the same license.
