# Authenticated Cloud download and local restoration

September 19, 2026. The user authorized testing Silksong with the Steam account already connected to Forge. The local save directory initially contained only `AppConfig.ini`; the account's saves existed in Steam Cloud.

## Observed result

Forge's normal session restoration authenticated the new Rust Cloud reader. A complete changelist contained 18 persisted files, totaling 3,806,866 bytes, at revision 52. All files were downloaded, decompressed where required, and checked against Steam's SHA-1 and raw size. A second complete changelist was identical. An independent filesystem check verified all 18 backup files again.

The macOS resolver mapped the canonical remote root and account substitution through Steam's UFS root override. It placed all 18 files in the game-visible account directory using the Rust coordinator's explicit local restoration, preimage preservation, staging and readback. The existing `AppConfig.ini` stayed unchanged during this restoration.

Silksong was then launched through Forge's normal **Launch** action. The game's log confirmed Steam initialization, the expected account, `SteamOnlineSubsystem` and receipt of Steam stats. The automation tool could not reliably operate the Unity menu. The user opened the profile selector and confirmed that the saved game appeared. A subsequent screenshot showed slot 1 with an existing 21h02 profile.

No upload, remote deletion or Cloud preference change was performed. The complete original remote backup remains in a private `SteamCloudValidation` directory within Forge's Application Support directory. Game saves and account identifiers are not checked into this repository.

## Changes exercised

- `steam-kit` uses its existing authenticated CM connection for `Cloud.GetAppFileChangelist#1`, `Cloud.ClientGetAppQuotaUsage#1` and `Cloud.ClientFileDownload#1`. Complete listings require a revision and reject deltas, ambiguous file identities and invalid prefix references. Prefix-index presence is significant.
- Download plans use HTTPS with no redirects or inherited authentication cookies. The actual files came from Steam's Azure and Google storage endpoints. Signed URLs and authorization headers are never serialized into the report. A supplied Host must agree with the validated destination.
- Compressed downloads use a bounded, single-entry ZIP decoder. Archive names never become local paths. Both raw length and SHA-1 must match before a file becomes part of the completed backup. Unsupported encryption is an explicit error.
- `MacCloudPaths` resolves existing files for MacHome, MacAppSupport, MacDocuments and GameInstall, including supported macOS overrides and SteamID substitutions. Original remote names remain intact. Existing `.bak` files are retained even when the current upload wildcard no longer selects them. Unsupported rules, redirects, API-relative names, platform-rule syntax or ambiguous destinations stop resolution.
- The former `restore_cloud_backup` example (removed with manual restoration later on September 19) validated a downloaded manifest, produces an explicit plan and could apply it with `--apply-stopped-game`. It used `steam-cloud::Vault`, then reads every restored file back. The operator must establish that the game is stopped. That example was not a production launch adapter.
- A Debug-only `--steam-cloud-backup APP_ID DIRECTORY` argument runs the read-only probe after normal in-app connection. It has no effect in ordinary launches and does not obtain a second credential copy. The destination must already exist.

## Limits of this validation

This proves authenticated Cloud-to-Mac restoration for the supplied game and account, plus recognition of that save in the game's profile selector. It does not prove loading and playing the entire saved game, uploading a new save, finalization after closing Forge, another device receiving a change, RemoteStorage API sharing with a game process, or Dynamic synchronization.

Cloud remains disconnected from the production launch lifecycle. The resolver currently handles a validated subset of metadata and only existing remote files. Local upload discovery, additional override forms and platform rules, shared Cloud App ID resolution, remote writes, native storage ownership, preferences, supervisor finalization and Dynamic remain required before activation. See the [coordinator contract](steam-cloud-coordinator.md).

Source fingerprints and check results are recorded in the [sanitized observation report](evidence/steam-cloud-runtime-2026-09-19/authenticated-read-observations.json).

The final checks passed: 300 main-workspace Rust tests, 4 native-workspace tests, the separate runtime-isolation test, and 209 Swift tests with 9 skipped and no failures. Rust formatting, Clippy, all 519 localization keys and app signatures passed. The first Swift compilation exposed the new protocol requirement in test doubles; their shared default was updated. A scrollbar test failed twice with Silksong open and passed unchanged after the game exited. The complete Swift suite then passed with the game stopped. All 18 restored files still matched their original Cloud hashes after the menu check and game exit.

## Protocol references

The message declarations follow the [pinned Cloud schema](https://github.com/SteamDatabase/Protobufs/blob/d35adb23d7d48ad9673261f9572b60d1bfe06c12/steam/steammessages_cloud.steamclient.proto). Path rules and substitutions follow [Valve's Cloud documentation](https://partner.steamgames.com/doc/features/cloud); the macOS file flag is documented in [ERemoteStoragePlatform](https://partner.steamgames.com/doc/api/ISteamRemoteStorage#ERemoteStoragePlatform).

[GameNative's synchronization implementation](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/app/gamenative/service/SteamAutoCloud.kt) provided protocol evidence for prefix-index presence, canonical root mapping and ZIP downloads. The Rust implementation was written independently; no GameNative code was copied.
