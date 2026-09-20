# Steam integration

## Scope

Forge contains a Rust implementation of Steam sign-in and SteamPipe. Swift code manages the interface, Keychain, and app storage directory. No .NET executable, SteamCMD, or Steamworks SDK is part of the execution path.

The workspace remains organized into five crates: the shared catalog, UniFFI bridge, Steam, Epic, and GOG. Steam owns its protocol modules within its crate. Future multiplayer messages can reuse the CM connection, but their implementation remains separate from file transfers.

## Sources reviewed

These references were reviewed at the following revisions. Their source repositories and programs are not bundled with Forge.

- [SteamKit, revision 9c7f3c3](https://github.com/SteamRE/SteamKit/tree/9c7f3c3fab6a8cf4ddd6cc7fe11bee70154e82d4), under LGPL-2.1, for Steam messages, authentication, formats, and services.
- [DepotDownloader, revision e7474cb](https://github.com/SteamRE/DepotDownloader/tree/e7474cb9ee8a87c0d917489b84c75667a1364aa1), under GPL-2.0, for the sequence of licenses, depots, manifests, and content servers.
- Valve's documentation for the [news API](https://partner.steamgames.com/doc/webapi/ISteamNews), [player library](https://partner.steamgames.com/doc/webapi/IPlayerService), and [library artwork](https://partner.steamgames.com/doc/store/assets/libraryassets).
- [Steam language codes](https://partner.steamgames.com/doc/store/localization/languages), for mapping macOS preferences to depot languages.

Forge's protobuf declarations contain only the fields it uses. Message numbers, fields, and formats are documented separately from the application interface. Detailed notes identify the source files reviewed for [authentication](steam-auth-notes.md), [CM connections](steam-cm-notes.md), and [content](steam-content-notes.md).

## Sign-in and library

`SteamLogin` represents a sign-in request. QR codes and credentials use Steam's HTTPS authentication service. The password is encrypted with the RSA key supplied by Steam before transmission. Steam decides which confirmations are available: mobile approval, a mobile code, or an emailed code. The client follows the supplied polling interval and QR changes.

After authorization, `SteamSession` has the CM servers verify the token. The SteamID read from the token is used for routing and is never presented as local signature verification. The saved macOS session contains the refresh token in the Keychain, not the password.

`Player.GetOwnedGames` supplies names, play history, and icons for the signed-in account. The grid sorts by last played, then by name. Downloading checks licenses separately, then Steam authorizes each depot key and manifest access code. An error is never treated as a successfully loaded empty library.

Once the library arrives, Forge loads declared operating systems in the background
through PICS, in batches of up to 50 games. Each batch uses the existing metadata
cache and publishes its results to the library model, including macOS, Windows,
and Linux declarations. Hovering a card only reveals its cached logo and makes no
request. These library-wide values are also available for filtering. An empty
declaration means unknown compatibility; a failed batch remains unresolved and
does not discard other batches or hide the library. Refreshing the library or
changing the session cancels the old load and rejects late responses.

Thumbnails come from Steam's CDN, with a visual fallback when a game has no cover art. The details page loads community announcements through the public `GetNewsForApp` API. Excerpts are displayed as text without executing HTML. The full article is available through its HTTPS link.

## Cargo dependencies

Exact versions, including transitive dependencies, are recorded in `Cargo.lock`. No ready-made Steam client crate is used.

| Direct Steam dependencies | Role |
| --- | --- |
| `tokio` | Asynchronous networking, timers, synchronization, and the CM task |
| `reqwest` with Rustls, `tokio-tungstenite`, `futures-util` | HTTPS and TLS WebSocket, without system OpenSSL |
| `prost` | Protobuf encoding and parsing |
| `rsa`, `rand_core` | RSA password encryption with system randomness |
| `aes`, `cbc`, `base64` | Filename and chunk decryption, authentication exchange encoding |
| `sha1` 0.11, `crc32fast` | Hashes required by Steam formats. SHA-1 uses ARM instructions available on Apple Silicon |
| `flate2`, `lzma-rs`, `ruzstd` | Steam's three compression families: ZIP/DEFLATE, VZip/LZMA, and VSZa/Zstd |
| `serde`, `serde_json` | HTTPS directory, news, session information, and receipts without secrets |
| `thiserror`, `tempfile` | Structured errors and atomic receipt publication |
| `forge-core` | Shared store description |

Encryption algorithms use dedicated crates. Rustls uses the `ring` cryptographic provider, which includes optimized native routines. "Rust" here refers to the client and protocol implementation, without a .NET service or external Steam binary.

The game verification SHA-1 dependency uses RustCrypto's ARM backend. The older version 0.10 remains a transitive dependency of the WebSocket transport. The cache and parallel readers use the standard library and the existing `tempfile` dependency, without adding a direct dependency.

## Downloads and limitations

The **Download** button prefers macOS when the game's metadata and accessible depots allow it, otherwise Windows. Swift passes the system's first preferred language to the Rust `language` module, which maps it to a Steam code, including Chinese, Portuguese, and Spanish variants. Unknown languages and missing translated depots fall back to English. The interface offers no platform or language picker.

The client selects public-branch depots, including authorized DLC. The default directory is `Games/Steam/steam-<appid>-<platform>` under Forge's storage directory. An existing installation keeps its location. Windows and macOS versions use separate directories. Shared depots are resolved through their Steam metadata. A new manifest code is requested before each network attempt that needs one, avoiding reuse of a code that expired during a long transfer.

During an update, the Swift receipt keeps the installation location and carries the `needsVerification` marker. If the transfer fails or the app closes, **Launch** repairs the installation in its directory before starting the game. Success replaces the receipt and removes the marker. Older receipts without the marker remain readable. An interrupted first download uses **Resume**.

Data travels over HTTPS. Filenames are decrypted, paths are checked, and chunks are decrypted, decompressed, and verified. Complete files are validated before atomic replacement. Valid existing files and verified partial chunks are reused when resuming. The manifest's RSA signature is not verified locally. Transport and integrity checks are separate from signature verification.

This version handles one download at a time per session. Updates do not remove old files. Depot symbolic links and legacy v4 manifests are rejected. Private branches, Workshop, install scripts, Steam Cloud, and differential downloads are not included. Downloading a game's files does not demonstrate compatibility with macOS or Wine.

## Launching

An installation has a **Launch** button in the row containing playtime, last played, and size. By default, this action calls the existing SteamPipe download flow to verify files and apply updates. It waits for success before requesting a launch plan. An error, cancellation, or sign-out interrupts the sequence. An initial download stops after installation without starting the game.

The global `forge.steam.verifyBeforeLaunch` setting is enabled by default and can be changed under Settings → Steam games. When disabled, files are not verified again at every launch, but the `needsVerification` marker still forces repair after an interrupted update. The launch plan continues to request Steam metadata, so this setting does not add an offline mode.

Verification requests current Steam metadata, then reuses the local manifest if its identifier matches. Archives received from the CDN are stored unchanged under `.forge-content/manifests/<depot>/<manifest>.zip`, without keys or tokens. The cache is published atomically and checked when read for sizes, CRC, structure, identifiers, decryption, and paths. A corrupt cache or a new version triggers a new authorized request. A valid cache avoids the manifest-code request and manifest download. It never bypasses license or local file checks.

File verification rereads every byte and compares SHA-1 hashes, using at most four readers depending on the available CPU count. All readers finish before any repair, including after cancellation or an error. Valid files are retained without downloading their chunks. Progress distinguishes checking for updates, local verification, and actual downloading. The verified amount can therefore reach the game's full size with zero content bytes downloaded. Manifest traffic is separate from the received-content counter.

`SteamSession.launchPlan` checks the receipt and licenses, then rereads the [publisher's declared launch options](https://partner.steamgames.com/doc/sdk/uploading). The Rust `launch` module selects an entry compatible with the platform, checks that paths stay inside the installation, and splits arguments without a command interpreter. Install, configuration, and uninstall entries are not selected as the default launch action.

`SteamGameLauncher` runs macOS games with their working directory and arguments. An `.app` path uses the bundle's executable. An explicit path inside the bundle is preserved. `SteamAppId` and `SteamGameId` are passed to the process. Windows launching awaits Wine engine integration and currently returns an explicit message. Some macOS games may also require the official client or Steamworks services that Forge does not implement.

## Verification

Offline tests cover authentication and Steam Guard, protobuf fields, VDF formats, depot metadata, the three codecs, paths, file verification, and resuming after cancellation. Swift tests cover the asynchronous bridge, Keychain access using an isolated item, library sorting, late details responses, and automatic downloading with macOS preference, the system language, and reuse of an existing location.

Launch tests cover executable selection, arguments, verification before execution, the saved setting, errors, cancellation, and changing the details page during verification. Two harmless test executables, including one inside an `.app` bundle, confirm the received working directory and arguments. No real macOS game has been launched as part of this validation.

Network tests are ignored by default to avoid contacting Steam during routine checks. They create a QR request without an account, use an anonymous CM connection for public metadata, and read news. The CM test also downloaded a complete public Steamworks SDK Redist file, 340,120 bytes in one chunk, then decrypted, decompressed, and SHA-1 verified it in memory. Fixtures contain no real credentials.

QR sign-in, the library, and a details page with news were also verified in the app with a real account. This check revealed CM notifications in the legacy binary format, whose rejection was interrupting the connection. A test reproduces a batch containing a legacy notification and a protobuf login response, then verifies that the response is still processed. Legacy login rejections and disconnections remain handled.

An owned game was downloaded in full to the `build/steam-smoke` test directory: 522,734,781 bytes, with cancellation during transfer and then resuming through the interface. The final receipt confirms download completion. The game was not run.

A later **Launch** click confirmed that all 69 installation files kept their inode, size, and modification time after verification. The old interface nevertheless displayed "Downloading" because the coordinator assigned that state to every report, including local reads. The second-pass test on an unchanged depot now verifies that the manifest cache prevents both CDN requests and file replacements.

After the fix, the same in-app operation reports "Verifying files" and "No game data downloaded" while reading Megabonk's files. Presentation tests also use a manifest received over the network with zero game-content bytes to keep those two amounts distinct.

Cache tests cover corruption, a manifest change, and unsafe paths. One test alters an installed file while preserving its size and modification time. Verification with a cached manifest still detects and repairs the file. Reader tests check the concurrency limit, result ordering, and termination of every reader after cancellation.

The optional local benchmark uses the same file verifier as installation. Expected hashes are calculated before timing starts, and it changes no files. Run it from the workspace root:

```sh
FORGE_VERIFY_BENCH_DIRECTORY="$PWD/build/steam-smoke/steam-3405340-windows" \
  cargo test --locked -p steam-kit content::verification::tests::benchmark_installed_files \
  -- --ignored --exact --nocapture
```

Add `--release` before the `--` separator to measure the distributed build profile. For this installation of 69 files and 522,734,781 bytes, measurements from September 15, 2026 were:

| Profile | 1 reader | 4 readers |
| --- | ---: | ---: |
| Debug | 1.860 s | 0.550 s |
| Release | 0.200 s | 0.057 s |

These measurements use recently read files and exclude Steam requests. They do not measure the complete Launch action or disk performance when data is absent from the system cache.

Credentials sign-in and Steam Guard variants are covered by protocol tests, but not all have been tried with a real account. The completed validations do not prove support for every account or depot.
