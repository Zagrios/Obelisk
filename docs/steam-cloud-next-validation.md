# Current follow-up, September 20, 2026

[Current compatibility implementation and evidence](steam-cloud-compatibility-2026-09-20.md) supersede the SDK and metadata status below. Ordinary SDK RemoteStorage persistence and Rust-owned transfers are implemented and passed an authenticated two-cache roundtrip. Additional observed AutoCloud forms are supported. Real-game SDK launch/exit and Mac/Steam Deck confirmation remain unverified. FileShare and Dynamic Cloud are explicitly outside this follow-up, not completion gates. No history or format migration was added.

The remainder records earlier trials and their scope at the time.

# Follow-up validation, September 19, 2026

This continues the five workstreams from the latest handoff. It does not replace the earlier authenticated upload and unchanged-save lifecycle evidence.

1. **Modified-byte automatic upload, controlled conflict and post-exit recovery are verified.** After save-history removal, the user loaded Silksong, produced a normal save and returned to its main menu. Two of the 18 files changed. Forge was closed first, then the game quit normally. The detached supervisor uploaded exactly those two files, deleted none, received acknowledgement and completed full verified readback at remote revision 55, up from 54. All 18 local files match the verified remote set and the preserved game-produced copy; the other 16 remote identities stayed unchanged. The supervisor finished with exit 0, no pending operation and no game guard. The real Steam Deck to Mac download and a later two-device conflict resolved with Local were also verified; see the completed trials below. A controlled stale-baseline/local-file fixture then triggered the real conflict UI. Selecting Cloud downloaded and restored the user's complete latest progress before the game started. This was not a second-device race. A later supervisor crash after normal game exit and before any new upload transaction left durable reconciliation work that Forge recovered. That recovery exposed a main-session CM login collision, fixed and verified separately below. Interrupted HTTP upload recovery was subsequently verified as described below. The user has no second account; that real-account trial remains unvalidated. See [modified-save observations](evidence/steam-cloud-runtime-2026-09-19/modified-save-cycle-observations.json).
2. **Interrupted HTTP recovery and retry after missing confirmation are implemented.** The user selected GameNative-style fresh comparison instead of permanently blocking an unknown result. After a failed attempt, Forge preserves the old transaction, verifies a complete fresh Cloud view and rebuilds its decision from the last confirmed baseline. Matching content converges; divergence prompts a local/Cloud choice. An earlier failed exit is retried before the next launch. Each cycle makes at most three attempts. The existing authenticated interrupted-HTTP trial remains valid; missing-confirmation recovery is newly covered by automated fixtures, not a new authenticated provider trial. See [the current recovery policy](steam-cloud-upload-settlement.md#current-recovery-policy-compare-again-after-failure).
3. **SDK RemoteStorage and FileShare remain unimplemented in production.** The existing isolated probes and restricted host are unchanged. Do not enable a writable native cache without establishing ownership for FileShare's own Cloud commit.
4. **Dynamic Cloud remains unimplemented.** No suspend barrier, compatible-game reload or real Dynamic callback cycle was tested here.
5. **Additional metadata forms remain unimplemented.** Unknown rules and redirects continue to fail explicitly.

The new negative-commit regression failed before the fix because the transport retained `Interrupted` despite receiving a positive close acknowledgement. All 15 upload tests now pass, including lost mutation responses, interrupted HTTP before commit, repeated closure failure, revocation, reopen and legacy-journal handling. The main Rust workspace passed 354 tests and the native workspace passed four. Focused strict Clippy passed for `steam-kit`, `steam-cloud` and `steam-runtime`. The final error-preservation adjustment was followed by another successful run of all 15 upload tests.

The settlement checks are automated and source-review results, and do not establish authenticated settlement of an uncertain batch. The changed-byte automatic upload and rebuilt-app checks described here were performed afterward. [Machine-readable observations](evidence/steam-cloud-runtime-2026-09-19/next-cycle-observations.json) include source hashes for the changed Rust files.

The inherited dirty checkout was copied before edits. The current-turn diff is in `build/steam-cloud-next-settlement.diff`. The private save-copy pointer is `build/steam-cloud-modified-cycle-root.txt`; the observer is `build/observe-modified-cloud-cycle.py`. Neither save bytes nor account identifiers were added to this report. No commit or push was made.


## Save-history removal requested before the gameplay trial

The user removed save history from the product scope. The gear-menu action, version list, manual-restore UI, Swift state, localization strings, bridge records and Rust history/restore APIs are gone. The standalone manual-restoration example was removed as well. One current account snapshot remains for account switching; baseline, pending uploads and recoverable local replacement plans keep their own references. Schema-1 journals migrate to schema 2 without altering physical saves, with compatibility for an already admitted replacement. Retrying a partial uninstall retains missing archived files and captures updated surviving files without scheduling an upload.

`./scripts/test.sh` passed: 354 main-workspace Rust tests, 4 native tests, 216 Swift tests executed with 9 skipped and no failures, plus the separate bundled-runtime isolation test. Formatting, strict Clippy, all 538 localization keys and app signature verification passed. Tests cover migration with pending uploads and an active game guard, rejection of malformed legacy state, interrupted legacy replacements, account switching and partial-uninstall retries. These tests used temporary fixtures; all 18 real Silksong saves still matched the pretest hashes before reopening Forge.

The inherited worktree was archived before this removal. Its isolated diff is `build/remove-save-history.diff`; the full test output is `build/remove-save-history-tests.log`. The private gameplay-copy pointer remains unchanged. No commit or push was made.

`./scripts/run.sh` rebuilt and reopened the Debug app successfully. UI inspection of Silksong's gear menu showed only **Désinstaller**. The real archive migrated to schema 2 at the next launch; all 18 saves were still unchanged, with no pending upload and remote revision 54. The subsequent game-produced change and detached upload succeeded as recorded above.

Silksong then reached its main menu through Forge, with the supervisor running and Cloud ready. The user subsequently performed the gameplay step recorded above.

## Follow-up from the real recovery trial

The conflict trial exposed one stale history promise in the error text. Both French and English now omit the claim that the other version will remain available.

Cloud maintenance completed after the injected supervisor crash, but the main Steam session lost authentication. The same remembered credentials worked after reopening with no pending work. The login request lacked a distinct identifier for each physical CM connection; the fix supplies both protocol fields with a nonzero random identifier. A regression failed before the fix and passed afterward. In the rebuilt app, an equal-content recovery replay completed while the profile, friends and game detail remained usable. All 18 files still match the user's current progress. See [CM session isolation](steam-cloud-cm-session-isolation.md) for the source contract, controls and evidence limits.

The final `./scripts/test.sh` passed 355 main-workspace Rust tests, four native tests and 216 Swift tests executed, with nine skipped and no failures, plus the separate runtime isolation test, formatting, strict Clippy, localization and app signatures. `./scripts/run.sh` rebuilt and opened the tested app. No real second account has been supplied; no SDK, FileShare or Dynamic result is claimed.

## Cloud progress in the launch button

The subsequent UI request moves Cloud checking and synchronization into the launch button, labeled **Synchro Cloud** in French, with the existing indeterminate bar moving back and forth. Only conflicts present the Cloud sheet. Network and other recoverable Cloud failures appear inline with retry, cancel and, when permitted by the coordinator, **Lancer quand même**. The prelaunch cancellation control remains available; finalization keeps its existing launch/uninstall guards.

The focused Steam presentation, Cloud model, session and shared Epic presentation suites passed: 58 tests executed, one skipped, no failures. The lifecycle fixture covers checking, failed retry, conflict choice, running, finalization and return to launch. Its rendered view was inspected at `build/cloud-button-previews/steam-cloud-sync-button.png`; this is an isolated fixture, not another real-game Cloud trial. Localization passed for 539 keys, app signatures passed, and `./scripts/run.sh` rebuilt and reopened Forge. The scoped diff is `build/cloud-button.diff`.


## Interrupted upload and actual restart

The first two authenticated disposable-file trials exposed `ClientBeginFileUpload` returning result 10 after failed-batch closure. The correction records the staged file, reports its failed transfer, retains the explicit negative file receipt and only then closes the batch. Steam returns that negative receipt as result 2 with `file_committed=false`; a typed adapter handles this exact response. Unknown positive-commit outcomes remain blocked. Intermediate older journals are not retroactively assumed safe.

The final real trial interrupted a 2 MiB HTTP upload after reqwest consumed 64 KiB. This proves client-side body interruption, not receipt of those bytes by the provider. Forge then exited completely and restarted. It received the negative file receipt, closed the old batch, read a fresh complete remote view, uploaded the same file through a fresh transaction, verified it, deleted only that fixture and verified the complete remaining set. Revisions were 61 before interruption, 62 after settlement and 64 after verified cleanup. All 18 real local files still match their pretest hashes, and all 18 remote files retain their original contents. The final transaction phases are `Closed`, `Verified` and `Verified` with no staged or unanswered mutation. This is the production transport driven by an opt-in Debug diagnostic, not another detached-supervisor recovery test.

The final `./scripts/test.sh` passed 360 main-workspace Rust tests, four native tests and 217 Swift tests executed with nine skipped, plus the separate bundled-runtime isolation check, formatting, strict Clippy, 539 localization keys and app signatures. An earlier parallel workspace run hit an unrelated transient `Busy` in an uninstall archival test; its isolated rerun and the complete final script passed without changing that test or implementation.

See [the redacted interrupted-upload evidence](evidence/steam-cloud-runtime-2026-09-19/interrupted-upload-observations.json) and [the updated settlement contract](steam-cloud-upload-settlement.md#authenticated-http-interruption-and-schema-3). Private copies and diagnostic journals remain under the directory referenced by `build/steam-cloud-interruption-root.txt`. No save content, account identifier, credential or signed URL is included in the public evidence.

The user has a Steam Deck on the same account. A real Deck-to-Mac save transfer is now being prepared; it is not yet claimed as verified. SDK/FileShare, Dynamic and additional metadata forms remain separate unfinished workstreams.


## SDK preparation while awaiting the Steam Deck trial

A read-only query found existing data in App 480: one file, 3,366 bytes, with a 4,096-byte quota. That app is excluded from the proposed native whole-app transfer probe. A bounded search of 51 owned library entries then found three empty Cloud namespaces without AutoCloud `savefiles` metadata. Project Zomboid, App 108600, is the preferred candidate: zero remote files and bytes, a 1,000,000-byte quota and a 1,000-file limit. Its UFS metadata includes `hidecloudui=1` and `ignoreexternalfiles=1`. These observations establish a candidate, not native SDK support or current availability at the next run. See [the redacted candidate evidence](evidence/steam-cloud-runtime-2026-09-19/sdk-candidate-observations.json).

The bounded discovery is an opt-in Debug command, `--steam-cloud-sdk-candidates 480 <private-output-directory>`. The numeric argument is retained by the existing diagnostic argument parser; the discovery searches owned library entries rather than mutating that AppID. This reuses the normal Forge session, makes only read requests and writes its report privately. No native cache permission or production ownership rule was changed.

Next sequence:

1. Wait for the user's Steam Deck Silksong play/save/quit and final Cloud-up-to-date confirmation. Keep the Mac game stopped. Observe the new remote set through normal Forge authentication, preserve a fresh private copy, then verify the normal Mac prelaunch download against it. A second account is not available and is not required for this same-account device trial.
2. Only after the Deck game is closed, recheck the SDK candidate's empty remote state and enabled preferences. Do not run another native game/SDK session while the user is playing on the Deck.
3. For SDK validation, use the genuine Valve SDK in a separate process with the existing authenticated account/license check. Give its native host an isolated, persistent working area limited to this account and app. Request and verify real native metadata; do not substitute fixture identity, quotas or SDK return values. Keep credentials and native configuration writes out of the working area.
4. First establish FileWrite/FileRead and indexed persistence across two fully terminated native generations. Then establish FileShare's callback and verified remote bytes with Rust CM writes excluded while the native generation runs. Stop the native generation before any diagnostic cleanup. Native cancellation and conflict admission remain required before choosing a production transfer owner.
5. SDK integration, Dynamic suspend/resume with a compatible game, and new metadata forms remain unfinished. The interrupted-upload fix does not establish these capabilities.

Private candidate output is referenced by `build/steam-cloud-sdk-candidate-root.txt`. The opt-in diagnostic never extracts credentials from Keychain. The Steam Deck action is the next user-dependent step.


The post-review suite also passed after adding the read-only candidate discovery and an explicit legacy-Prepared upgrade regression: 361 main-workspace Rust tests, four native tests, 217 Swift tests executed with nine skipped, plus isolation, localization, formatting, strict Clippy and signing. Legacy Prepared journals now persist schema 3 before any newly admitted mutation, preserving recovery information if that fresh attempt is interrupted.

The user subsequently saved on the Steam Deck. A complete, stable, verified read-only Cloud backup observed revision 65 with three changed files, no additions or deletions, and all 18 Mac files still equal to the preserved pretest copy. Confirmation that the Deck game is closed is pending before Mac launch; automatic local recovery has not yet been claimed. The private backup pointer is `build/steam-cloud-deck-cycle-root.txt`; the [redacted device-trial observation](evidence/steam-cloud-runtime-2026-09-19/deck-to-mac-observations.json) records this intermediate state.


## Completed Steam Deck to Mac recovery

The user confirmed Silksong was closed on the Steam Deck. Forge was restarted without diagnostic arguments, then Silksong was launched through its normal button. The button displayed **Synchro Cloud** without a modal. A read-only observer captured all 18 physical Mac files and all 18 baseline entries matching the independently downloaded Deck backup at revision 65. The production observer still reported the game process absent at that point, proving projection preceded game startup. Three files changed compared with the preserved Mac copy; there were no additions or deletions.

The game process and its startup save notice were observed. It subsequently exited with code 0; the agent did not initiate that exit. The supervisor reached Finished/Synced and stopped its host. All 18 physical files still matched the Deck backup, revision stayed 65, and no pending operation, game guard or reconciliation work remained. Forge returned to **Lancer** and displayed **Sauvegardes Cloud à jour**. This proves automatic Deck-to-Mac recovery and finalization, not visual inspection of a loaded save slot.

The [device-trial observation](evidence/steam-cloud-runtime-2026-09-19/deck-to-mac-observations.json) now records completion. Private pretest and remote copies, the captured baseline and hash comparison remain under `build/steam-cloud-deck-cycle-root.txt`. The second-account trial is still unavailable.


## Inspection-induced Steam launch after the device trial

The user reported that official Steam opened and confirmed having closed Silksong first. The first game process, owned by Forge, exited at 19:52:20 local time. Its preserved `Player-prev.log` records Steam initialization, received stats and API shutdown. At 19:52:23.695, the agent called `game.getAXStateAndScreenshot()` on the app binding after exit. LaunchServices created a second game process at 19:52:23.706, outside the Forge supervisor. Its log reports that it was not launched through Steam and then fails SDK initialization; official Steam starts at 19:52:24.404.

This corrects the intermediate diagnosis that the normal Forge launch failed. Two different game processes produced the two logs. The second launch was induced by the inspection, with no Forge Steam environment. No production launch code was changed. Future post-exit checks must use supervisor/process/file observations without invoking the game app UI binding. At the final check, official Steam, the game and native host were absent, all 18 Mac files still matched the Deck backup at revision 65, and no Cloud work remained. See [the redacted incident evidence](evidence/steam-cloud-runtime-2026-09-19/native-relaunch-observations.json).


## Real device conflict resolved with Local

The user reported creating a real Mac/Steam Deck divergence, seeing the conflict sheet and selecting Local. Read-only inspection afterward found revision 67, all 17 physical files matching the baseline established by production verified Cloud readback, and no pending upload, game guard or reconciliation work. The latest transaction is Verified with two uploads and one deletion; the local choice mirrored the game-produced file set. This observation made no additional remote request and did not inspect the selected save on the Deck. See [the redacted device-conflict evidence](evidence/steam-cloud-runtime-2026-09-19/device-conflict-observations.json). The private copy pointer is `build/steam-cloud-device-conflict-root.txt`.

## Immediate conflict sheet after game exit

The same trial exposed an intermediate inline conflict message followed by an approximately five-second delay before the sheet. The supervisor had already detected the conflict, but the Swift presentation waited for maintenance to reconnect and repeat the comparison. The supervisor now includes the existing durable decision token with its conflict status; Swift opens the sheet as soon as that status arrives and adopts the captured decision after the supervisor releases the archive. The inline conflict explanation is removed; the explanation inside the sheet remains. Resolution still validates the token and fresh remote view after the user's choice.

The presentation regression failed before the UI fix and passed afterward. Tests cover immediate presentation before supervisor completion, no preliminary maintenance replay, suppression of the inline message, cancellation before and after handoff, explicit retry after a failed choice, account scoping and old snapshots without the optional token. The current-turn snapshot pointer is `build/immediate-cloud-conflict-before.txt`; the isolated source diff is `build/immediate-cloud-conflict.diff`. These are automated lifecycle checks, not a second real conflict trial after the fix.

The final `./scripts/test.sh` passed 363 main-workspace Rust tests, four native tests and 220 Swift tests executed with nine skipped and no failures, plus bundled-runtime isolation, formatting, strict Clippy, 539 localization keys and app signatures. See [the immediate-conflict regression evidence](evidence/steam-cloud-runtime-2026-09-19/immediate-conflict-observations.json) for source hashes and validation scope.

`./scripts/run.sh` rebuilt and reopened Forge normally. The Silksong detail screen shows **Lancer** with no stale inline conflict message. No game was launched during this correction.

## GameNative-style retry selected by the user

Following the source comparison, the user explicitly requested the same failure/retry approach. Forge no longer requires terminal proof of every old file mutation before it may rebuild a sync decision. The old transaction journal remains unchanged, the confirmed baseline and local choice survive, and a fresh complete remote read is required before retiring the old pending attempt. Known interrupted HTTP transfers retain their negative-commit cleanup. New uploads still check Steam replies and verified content.

A pending prior exit now synchronizes before the next launch when the local copy is selected. A Cloud choice returns the download decision and cannot accidentally re-upload the previous local files. Prelaunch synchronization retains its existing checking state and cancellation control. Up to three transient attempts run with one- and two-second waits; a conflict, authorization error or local integrity error stops automatic retries. The total budget remains 120 seconds.

The inherited checkout was preserved via `build/cloud-retry-before.txt`. No commit, push, real-game launch or save mutation was performed for this change. Existing provider trials do not prove the new missing-confirmation retry path; that path is validated with synthetic journals and local fixtures.

The full `./scripts/test.sh` passed 369 main-workspace Rust tests, four native tests and 220 Swift tests executed with nine skipped and no failures, plus isolation, formatting, strict Clippy, 539 localization keys and app signatures. The [retry evidence](evidence/steam-cloud-runtime-2026-09-19/cloud-retry-observations.json) records source hashes and scope. The isolated diff is `build/cloud-retry.diff`.

`./scripts/run.sh` rebuilt and reopened Forge normally. The authenticated Steam library and Silksong detail show **Lancer**, with no error or conflict sheet. This verifies the updated application opens; it is not an authenticated missing-confirmation trial.


## Cloud maintenance and transfer reuse

The three approved follow-ups add automatic collection of unreferenced archive blobs, verified-byte reuse for remote comparisons, and Rust-owned UI choices. Collection keeps the Vault's recovery references as well as the durable context, current in-memory context and outstanding conflict views. Reading the durable context matters after a failed context write: newer in-memory values must not free bytes still needed on restart. Active game/replacement guards prevent collection, and cleanup failure is deferred. Existing upload journals, including lost-confirmation evidence, are outside this blob cleanup.

Remote comparisons still obtain and recheck complete fresh Cloud listings. Archived bytes are reusable only if their SHA-256 remains valid and they match the current Steam SHA-1 and length. A changed or missing file is downloaded, and post-upload byte verification deliberately bypasses the cache. The supervisor retains one HTTP client across attempts. Parsed preferences can be reused only for the same account, app and freshly listed configuration file; absent configuration remains unknown.

Rust publishes permitted choices with each status. Swift's launch-anyway button follows that list, and Rust immediately rejects an unadvertised choice without republishing a new decision. This removes the duplicated error-key policy. Old serialized statuses remain readable and grant no choices by default.

The inherited worktree was preserved via `build/cloud-maintenance-before.txt`; the scoped diff is `build/cloud-maintenance.diff`. Tests use synthetic Cloud sources and temporary saves. No authenticated Steam trial or real save edit is part of this change.

Final `./scripts/test.sh` passed 376 main-workspace Rust tests, four native tests and 220 Swift tests executed with nine skipped and no failures, plus bundled-runtime isolation, formatting, strict Clippy, all 539 localization keys and app signatures. The [maintenance evidence](evidence/steam-cloud-runtime-2026-09-19/cloud-maintenance-observations.json) records source hashes, synthetic cache/cleanup checks and their limits.

`./scripts/run.sh` rebuilt the signed Debug app successfully. The previous process was then closed through Forge's Quit menu and the rebuilt application reopened. The authenticated Steam library and Silksong detail display **Lancer** without an error. No real game was launched.


## Cloud transfer performance, September 20

The post-upload adapter readback now stages verified bytes directly in the coordinator's archive. Every surviving remote file is downloaded once, and only the final stable listing plus the durable Verified receipt permits completion. Provisional callbacks cannot advance the baseline. Ordinary comparisons and readback use bounded HTTP concurrency: at most four files per group, with larger-than-32-MiB raw files handled alone. CM requests remain serialized; cancellation drops all readers and authorization checks remain in place.

Normal preflight establishes ownership early and captures current local bytes after the remote view is ready. Recovery of pending work retains the early snapshot, and disabled/offline launches still capture. Duplicate archive identities and impossible cache candidates avoid redundant reads, while integrity checks remain active on subsequent transitions and restarts.

The removed pending-status field exposed a compatibility regression in existing journals. The reader now migrates its five known historical values in either schema generation, preserving the pending operation and uncertainty fence. Unknown fields, invalid values and missing fences still fail without rewriting the original journal. Authentication classification was left unchanged.

`./scripts/test.sh` passed with 384 main-workspace Rust tests at that point, four native tests, 220 Swift tests executed with nine skipped and no failures, plus isolation, strict Clippy, formatting and signatures. The final workspace run includes one additional corruption regression and passes 385 Rust tests with strict Clippy. The macOS app build and tests completed successfully. The [performance evidence](evidence/steam-cloud-runtime-2026-09-20/cloud-performance-observations.json) records source hashes and validation scope. No authenticated Steam request, real-game run or real save edit was performed; there is no measured Steam/Silksong wall-clock speedup claim.


## Current development formats only, September 20

The user confirmed that Forge has one development user and requested removal of migration support. This supersedes the compatibility repair recorded in the preceding section. Vault journals now deserialize directly into archive schema 2. History migration, obsolete pending-status conversion and the old manual-restore effect are removed. Upload journals accept only schema 3. Ownership metadata and the context's removal marker are required rather than inferred from older records. Unsupported journals fail without an automatic rewrite or reset; current-format interrupted operations retain their normal recovery.

The active local archive, ownership record and context were already current, with no running game, pending upload, replacement or unfinished synchronization. Under the archive and transaction locks, one unreferenced Verified schema-2 upload directory was removed from Forge's active storage. A private pre-cleanup copy is beside the source snapshot referenced by `build/cloud-current-format-before.txt`. Current state files remained byte-identical, and the before/after checks of all 18 recorded save paths were unchanged, including absent paths. No game was launched and no Steam request was made.

The scoped source changes are in `build/cloud-current-format.diff`; local cleanup details are in `build/cloud-current-format-cleanup.json`.

The final `./scripts/test.sh` passed 378 main-workspace Rust tests, four native tests and 220 Swift tests executed with nine skipped and no failures, plus bundled-runtime isolation, strict Clippy, formatting and app signature verification. The lower Rust count reflects removal of migration-specific tests and their replacement with current-format acceptance/rejection checks. See [the current-format cleanup evidence](evidence/steam-cloud-runtime-2026-09-20/current-format-observations.json).

## Real Mac-to-Deck cycle after optimization, September 20

`./scripts/run.sh` rebuilt and opened the current Debug app successfully. The user confirmed Silksong was closed and Cloud up to date on the Steam Deck. Before launch, a private copy preserved all 17 existing saves, the 18 recorded paths including one absent file, and Forge's current Cloud records. The normal Forge launch displayed **Synchro Cloud**, completed preflight at revision 67 with unchanged local files, and started Silksong without a conflict.

The user produced a normal save on the Mac and quit the game. Two files changed. The supervisor uploaded those two files, deleted none, and advanced to revision 68 only after verified remote readback. All 17 physical files match the confirmed baseline and remote manifest. The transaction is schema 3 and Verified, with no unanswered mutation or staged upload; the session is Finished/Synced with exit 0. No pending operation, game guard or reconciliation work remains. Forge returned to **Lancer** and **Sauvegardes Cloud à jour**, with the game and helper stopped.

The user then launched Silksong on the Steam Deck and confirmed the Mac's new progress was present. That final device check is user-reported; no Deck filesystem was inspected. This validates the normal Mac-to-Cloud-to-Deck cycle after the recent changes. The Mac preflight required no changed remote bytes, and this trial does not repeat conflict, offline, interruption or lost-confirmation tests, nor establish SDK/FileShare or Dynamic Cloud support. No performance speedup is claimed without a comparative benchmark.

No implementation change was needed. The [redacted device-cycle evidence](evidence/steam-cloud-runtime-2026-09-20/post-optimization-device-cycle-observations.json) records the result. Private before/after copies and lifecycle observations are referenced by `build/cloud-retest-root.txt`. No commit or push was made.
