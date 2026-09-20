# Authenticated Steam Cloud upload validation

September 19, 2026. Forge's Rust CM transport completed an upload, full verified download and deletion of one disposable file in Silksong's Cloud store. The logged-in Forge session supplied authentication. The game, native host and detached supervisor were stopped throughout the test.

The original 18 persisted remote files remained unchanged. The 18 local saves, totaling 3,806,866 bytes, also remained unchanged. This establishes the transport for this account and app. Normal game launch and exit still do not invoke Cloud synchronization.

## Observed sequence

1. Read the account and app preferences through app 7's checksum-verified `sharedconfig.vdf`. Both were enabled for app 1030300, at roaming revision 241. No preference was changed and the complete roaming file was not retained.
2. Preserve a complete verified remote backup at revision 52. Independently copy and hash all 18 existing local saves before testing.
3. Open a durable Rust upload transaction for one unique `__forge_cloud_validation_*.bin` file. Steam acknowledged the file commit and batch completion. A full remote readback verified its contents and all original files at revision 53.
4. Open a second transaction deleting only that disposable filename. Steam acknowledged the batch. Full remote readback found the original set at revision 54 and no persisted disposable file.
5. Compare the original remote identities, SHA-1 hashes, lengths and platform flags. All matched. Independently recompute local SHA-256 hashes. All 18 matched their pretest copies.
6. Run the new metadata-driven local scanner against the verified backup and Forge's installation receipt. It selected 18 files, all matching the remote names, lengths, hashes and platform flags. No game-specific rule is encoded in the scanner.

Private evidence is located through `build/steam-cloud-transfer-root.txt`. That directory contains `cloud-transfer-result.json`, two durable upload journals in `Verified` state, the complete remote backup, `local-before.json`, local pretest copies and `local-discovery.json`. The public [observation record](evidence/steam-cloud-runtime-2026-09-19/authenticated-upload-observations.json) omits account identifiers, machine paths, credentials and signed URLs.

## Transport contract

`steam-kit::cloud::UploadTransaction` owns an exclusive OS lock, immutable numbered payload files and a durable phase journal. It stores no credentials or signed transfer information. It persists uncertainty before sending a mutating request. A potentially sent request is never blindly repeated after reopening.

The concrete CM adapter validates the authenticated account and response result. It follows each server-provided HTTP block plan with validated HTTPS hosts, methods, headers, offsets and optional explicit bodies. It checks authorization between operations, including after obtaining a download plan. Sign-out invalidates the client's authorization flag before aborting its CM session.

Completion requires every file's positive commit result, the blocking batch acknowledgement, matching complete remote metadata, checksum-verified downloads of every persisted file and a second unchanged full listing. A best-effort failed-batch completion does not imply rollback. Steam's batch protocol does not provide an atomic compare-and-swap for the complete Cloud store. [Valve upload contract](https://partner.steamgames.com/doc/webapi/ICloudService), [pinned CM schema](https://github.com/SteamDatabase/Protobufs/blob/d35adb23d7d48ad9673261f9572b60d1bfe06c12/steam/steammessages_cloud.steamclient.proto).

The Debug trigger reserves `SteamGameSessions` while the probe runs, excluding game launch and installation mutations within Forge. The probe creates and removes only its own unique disposable file. It is not an automatic save uploader.

## Verification and remaining work

The full `scripts/test.sh` run passed with 313 main-workspace Rust tests, four native-workspace tests and 210 Swift tests, nine skipped and no failures. Localization, formatting, Clippy, signing and the separately enabled bundled-runtime isolation test passed. The log is `build/steam-cloud-transfer-tests.log`.

After the final authorization guard changes, `scripts/build.sh` passed and the authenticated probe above succeeded. The subsequent focused Cloud suite passed 21 tests, including interrupted HTTP transfer and failed journal persistence after batch admission. A review then reproduced a scanner collision with a platform-excluded remote file. The added regression failed before the fix and passed afterward. Such identities, including supported alternate root spellings and tombstones, are now reserved; they cannot become new all-platform uploads. Tombstone resurrection remains unsupported.

The final `steam-kit` run passed 95 tests with five ignored, and compiled all examples. Clippy with `-D warnings` passed for all `steam-kit` targets and the native Cloud probe. Logs are `build/steam-cloud-transfer-build.log`, `build/steam-cloud-transfer-unit-final.log` and `build/steam-cloud-transfer-final-rust.log`. The read-only scanner was rerun after the collision fix against the real local files and still selected the same 18 matching files. A final app build passed in `build/steam-cloud-transfer-build-final.log`. The earlier native local-storage write/read/restart fixture also passed after the runner changes, recorded in `build/steam-cloud-local-regression.log`.

Forge was reopened normally on the connected Steam library. Process inspection confirmed no diagnostic argument, native host, supervisor, game or probe remained. Local SHA-256 hashes still matched all 18 pretest files. No commit or push was made.

The following are still unimplemented or unproven:

- Product integration before launch, after actual game exit, after closing Forge and after sign-out.
- Server-side settlement and safe automatic retry of an ambiguous old batch. The current journal blocks resending it.
- Complete metadata coverage, account switching, conflict UI and preservation before uninstall. Save history was subsequently removed from the product scope.
- Exclusive transfer ownership while a native game runs, separate SDK RemoteStorage persistence and Dynamic coherence, callbacks and game reload.

The [coordinator activation list](steam-cloud-coordinator.md#still-required-before-activation) and [native ownership analysis](steam-cloud-transfer-ownership.md) remain separate from this authenticated transport result.
