# Epic Cloud Saves

Forge synchronizes EGS launcher Cloud Saves for native Mac games with supported publisher rules. EOS Player Data Storage remains owned by the game. The launcher supplies its existing exchange-code, account, sandbox, deployment and ownership arguments; those arguments alone do not establish EOS persistence.

Epic's [EOS integration documentation](https://dev.epicgames.com/documentation/unreal-engine/online-subsystem-eos-plugin-in-unreal-engine) describes the game's SDK and exchange-code login. This path does not require Forge to provide a Steamworks-style local API host for EOS. The reused `forge-steam-host` binary enters its Epic supervision branch before Steam engine initialization; its Epic role is process ownership and launcher savesync. Game-specific prerequisites still require a real witness.

The protocol research is in [Epic Cloud research](research-epic-cloud-saves.md) and [GameNative comparison](research-gamenative-epic-cloud.md). Forge implements the protocol in Rust and does not execute Legendary, GameNative or the official launcher.

## Ownership and admission

`epic-kit::cloud` owns publisher rules, safe Mac path resolution and BuildPatch serialization. `epic-kit::Client` owns authenticated savesync requests and remote verification. `steam-runtime::epic` owns synchronization policy, process lifetime and recovery. Swift presents snapshots and sends explicit decisions through `forge-bridge::EpicGameSession`.

Swift reserves an Epic session before installation verification. It completes the existing verify/update pipeline, then asks Rust to create a detached `forge-steam-host epic-supervise` process. A one-use exchange code travels through a private pipe. The helper obtains an independent Epic OAuth session and revokes only that session when finished; it never persists credentials or revokes the UI's remembered token.

The helper persists its identity and acknowledges the handoff before doing Cloud work. It waits for `begin()` from the caller. Swift receives the client, finishes credential persistence, checks cancellation and account revocation, and only then begins work. An abandoned handoff expires after 120 seconds. Observing a recovered session does not begin it.

For a launch, the helper prepares Cloud saves before requesting fresh launch arguments. It records a durable capture marker and game guard before opening a private exec pipe to its child. The target cannot execute until the helper has recorded the child's kernel identity. Launch admission, cancellation and account revocation share the same admission lock.

After resolving the native executable, Epic uses the shared Mach-O selection
and [Rosetta requirement](steam-rosetta-intel.md#shared-steam-and-epic-requirement).
An Intel-only game waits for the signed probe before admission. If installation
was required, the supervisor refreshes the short-lived launch plan and ownership
token, then validates the new executable before admission. Steam and Epic
share `game_session` for exec confirmation and asynchronous child exit. The
helper reports exec failure explicitly; it is reaped before releasing a failed
launch. The presence task runs on the supervisor's executor until game exit or
account revocation.

The helper waits for actual child exit before taking the exit snapshot and uploading. Closing Forge only detaches the observer. Cancelling preparation cannot release a running game's installation. Signing out cancels Cloud transfers; a running game stays tracked and its eventual local exit snapshot is archived for the original account.

Recovery compares PID, UID and kernel birth time. A process can change executable with `exec()` without losing ownership. Failed inspection or lost IPC remains an unknown live state and keeps mutations blocked. A dead helper alone does not prove that its game exited.

Terminal failures carry a typed reason through the persisted snapshot and UniFFI. Both runtime and transport cancellations become `Cancelled`; the display message does not determine Swift behavior. Rust recognizes the two historical cancellation keys only when reading a terminal snapshot without a typed reason. A failure while a child is still owned keeps the session running and has no terminal failure reason.

## Identity and local storage

The shared archive uses `StoreScope::Steam` and `StoreScope::Epic`. Existing Steam JSON and directory paths remain unchanged. Epic identity contains the complete account ID, namespace and app name. Epic archives live under `SteamCloud/epic/<account>/<game-key>`; the game key hashes the namespace/app tuple, whose original identifiers remain in the journals. Account identities are not converted to Steam IDs or app IDs.

`steam-cloud::Vault` provides content-addressed snapshots, local projection journals, physical-path ownership and upload settlement. Ownership fences also apply when two stores or accounts select the same physical save directory. Capture does not adopt another account's bytes.

The shared journal reader rejects files with group or other permission bits, checking the opened file before reading JSON. Missing journals remain absent; symlinks and nonregular files are refused. Atomic writes create private files, and the existing size limits remain unchanged.

`epic-context.json` stores configuration, rules and pending work beside the vault. It contains no credentials. Pending work has three states:

- `Capture` is persisted before admission and stays until the actual exit bytes are archived.
- `Upload` refers to an immutable archived local manifest.
- `Applying` records a verified remote choice before changing local files or the baseline, so a crash cannot later upload an older exit snapshot.

Conflicts persist their local snapshot before prompting, including the first prelaunch conflict. Pending discovery checks the account, game identity, directory and configuration before resuming.

Before uninstall, Forge archives selected saves and persists an installation-removal marker. A partial uninstall preserves already archived missing files. Later synchronization can finish remote work without recreating an absent installation. Reinstall compares preserved local content against the fresh remote view and requires a new choice when they differ. The archive has no user-facing history screen or old-schema migration path.

## Comparison and transfers

Forge compares verified contents against the common baseline. It never chooses a winner from timestamps. A conflict offers Local, Cloud or Cancel. The choice is bound to the account, session, decision revision, local contents and remote view. Changes invalidate the choice and cause another comparison.

The savesync transport lists manifests, downloads the newest snapshot and its exact referenced chunks, verifies file contents, then rechecks the inventory. Requests for chunk links are bounded batches. Publisher include/exclude rules select the Mac files; uploads retain remote files excluded from that selection.

Manifest identity accepts the native EGS `namespace:catalog-item:app:account` form and the Legendary `app+account` form. Expected game identifiers come from the validated launch configuration, and the account comes from the active credentials. The native form must match every component exactly. The Legendary form does not encode namespace or catalog item; it is accepted only within the authenticated app-scoped transaction. Forge's writer retains that compatible format.

An upload first verifies the expected remote inventory. It uploads every required chunk successfully before publishing a new BuildPatch manifest. It then downloads the published snapshot, compares every file's bytes, and rechecks the inventory before reporting Synced. An interrupted upload is never blindly replayed. Forge preserves its journal and compares a fresh verified view before starting a new operation.

The remote inventory fingerprint is not an atomic compare-and-swap token. No atomic savesync write reservation was established. Concurrent external writers can still race publication. Forge detects observable changes and does not claim stronger server guarantees.

Transient preparation failures get three bounded attempts within a 120-second budget, then explicit Retry or Cancel. Launch Anyway is available only for a retryable Cloud failure with safe local ownership. If fresh metadata fails, it additionally requires previously validated unchanged rules. It captures pending local work and does not infer permission from elapsed time. Missing or changed rules with an existing archive stop preparation.

Post-exit failures remain pending. The UI attempts pending work for the current account at connection, wake, network return and explicit Retry. It avoids repeating the same failed game continuously. Deferred work does not launch the game.

## Supported rules and limits

The catalog must publish `CloudSaveFolder_MAC`. `CloudSaveFolder`, `CloudIncludeList` and `CloudExcludeList` are also retained. Forge's Cloud activation for supported rules is its own policy; it does not claim to read the official launcher's local enable/disable preference.

The Mac resolver supports `Home`, `UserProfile`, `UserLibrary`, `UserDir`, `InstallDir`, `EpicId` and `AppName`, plus `~/`. It validates the resulting directory before collection or projection. Traversal outside its anchor, broad home/library roots, symlinks, hard links, unsafe names and case/Unicode normalization collisions are rejected.

`AppData` remains unsupported because the inspected sources disagree about its Mac base directory. Forge does not guess between Application Support and Application Support/Epic. A fresh game with absent or unsupported rules may launch with Cloud marked unsupported. Existing Cloud state prevents silently disabling protection when rules change or become unsupported.

Current local bounds are 10,000 files, 64 MiB per file and 256 MiB in total. They are memory safety limits, independent of a publisher quota. An inventory reaching 1,000 manifests is rejected because completeness is uncertain. Published manifests are not pruned automatically. Windows execution and Wine are outside this implementation.

## Verification and remaining live evidence

Controlled Rust tests exercise authenticated HTTP fixtures, chunk/manifest failures, full readback, inventory races, account isolation, rules, paths, journal recovery and real disposable child processes. An independent Legendary fixture is checked into `crates/storefronts/epic-kit/src/cloud/fixtures/legendary-42f6bdea`; both Forge-to-Legendary and Legendary-to-Forge decoding were checked with synthetic files, including empty files and Unicode paths.

Swift tests exercise launch reservation, conflict responses, account changes, cancellation before/after handoff, pending recovery, network-return triggers and archive failure before uninstall. The full validation command is `./scripts/test.sh`; opt-in production tests must be kept disabled for fixture-only runs.

Local validation on September 22, 2026:

- `scripts/test.sh` passed localization for 581 English/French keys, workspace/native formatting and strict Clippy, and 492 Rust tests with seven optional/subprocess cases ignored. These include 135 `epic-kit` tests and the legacy Steam archive fixtures.
- The full macOS suite was rerun after correcting duplicate cancellation delivery during the Swift handoff: 279 tests, 12 opt-in skips, zero failures.
- The Debug app built successfully and passed the Apple certificate signature/architecture checks.
- The separate bundled Steam runtime isolation test passed without access to a Steam installation. Steam service/game Cloud behavior was not retested live.
- An opt-in native renderer passed and its two PNGs were inspected: the Epic detail synchronization button and the actual attached conflict sheet. The fixture uses a fake account and supervisor. These captures verify the displayed controls, not live service behavior.

The first installed-game attempt exposed a local IPC defect before game admission: the temporary socket directory inherited mode `0755`, while the observer correctly required `0700`. Begin and Observe were refused, leaving the initial Checking snapshot in reconnecting state until admission expired. The server now creates the directory privately before binding, under `/tmp` to keep the Unix socket path short. The ownership-token directory is also explicitly private.

The regression test `real_supervisor_channel_is_observable_before_game_admission` runs the actual server and client together. It reproduced the stale reconnecting snapshot before the fix, then passed with the correction. All ten supervisor tests pass. A separate check against the helper packaged in the rebuilt app verified private permissions, kernel identity, Observe and cancellation using only a disposable session, with no Begin, authentication or game launch. Strict Clippy, the Debug build and signature verification passed after the correction.

The next installed-game attempt reached Epic but failed on an overly narrow manifest identity check. A read-only diagnostic confirmed Kingdom New Lands' Mac rule, `{UserLibrary}/Application Support/nl.noio.kingdom-new-lands/`, and a valid three-entry manifest inventory. The latest 827-byte manifest uses BuildPatch feature level 17 and identifies the game with the exact native EGS tuple. Forge had accepted only the shorter Legendary form, causing `backend.epic.invalid_response` before any save-file transfer or game admission. The diagnostic read the inventory and manifest metadata only; it did not download save chunks, change local saves or publish Cloud content.

After the correction, the same live metadata check passed the production manifest parser, including identity, paths and size limits. An opt-in XCTest asserted an error-free `manifest_validated_no_chunks_read` result. All temporary diagnostic APIs, logging and test hooks were then removed. A separate regression covers encoding preserved remote files outside the local Mac filters, so the transport does not reject files intentionally retained by the synchronization policy.

Validation after this correction passed 22 protocol Cloud tests, 21 Epic supervisor/session tests with one subprocess fixture ignored, and 14 macOS Cloud cases with the optional renderer skipped. Strict Clippy and formatting passed. The rebuilt Debug app passed signature verification and was reopened on Kingdom New Lands with the Launch action available; the game itself was not started by this check.

A further installed-game retry still failed. The metadata-only check above had not exercised chunk retrieval. A full read-only call through the production `cloud_download` reproduced `Protocol` at decoding the chunk-link response: Epic returned `hash: null` and `lastModified: null` alongside valid string links. The existing deserializer accepted missing fields but rejected explicit nulls. A fixture reproduces this exact response through the complete download path. Link metadata now accepts missing or null values; manifest inventories still require nonempty hashes and modification values, and invalid scalar types remain rejected.

After this fix, the authenticated production download completed: the 22,714-byte chunk decoded to its declared 1 MiB window; header, chunk and file hashes passed; all nine files were reconstructed, totaling 271,383 bytes; and the three-entry inventory remained stable. A private copy of all nine downloaded files was re-read and compared byte-for-byte and by hash. Four existing local files totaling 329,802 bytes were independently backed up and verified unchanged during the download. This establishes complete remote read evidence, not upload or an in-game roundtrip. Temporary diagnostics and their bridge/test APIs were removed after this result.

The rebuilt app then exercised the real Kingdom New Lands launch. The user confirmed choosing a save version in the conflict prompt and closing the game. The detached supervisor recorded exit code 0 and terminal `Finished`/`Synced` without an error. Seven of the nine final file hashes differed from the prelaunch Cloud copy. All nine current local files, totaling 272,042 bytes, matched the settled Cloud readback baseline by SHA-256 and size; local archive and baseline matched, and upload, projection, game and context pending markers were clear. The UI displayed "Sauvegardes Cloud à jour". This establishes a real launch/exit cycle with changed bytes and verified Epic upload/readback for this game; retrieval by another launcher/device remains untested.

The nullable-metadata correction passed 25 Cloud protocol tests, strict Clippy, formatting, the 14-case macOS Cloud suite with one optional renderer skipped, and Debug signature verification. The private pre-test local and remote copies were retained for recovery.

Kingdom New Lands now has a verified Forge launch/exit synchronization cycle, including real remote downloads and changed-file upload/readback. A cross-client/device EGS roundtrip and an EOS Player Data Storage witness have not been established.

A live EGS check must first establish the candidate's publisher rules and take a private verified backup. Evidence then requires a changed in-game save, actual process exit, verified remote bytes and a controlled restore/roundtrip. The test must account for the official launcher and other writers. EOS requires a separate game with established Player Data Storage use and game-driven persistence evidence.
