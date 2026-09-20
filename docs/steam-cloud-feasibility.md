# Steam Cloud feasibility and Rust ownership

September 19, 2026. Historical research and proposed architecture. The history and manual-restoration feature described below was removed at the user's request later that day; the current contract is in [the coordinator documentation](steam-cloud-coordinator.md). The subsequent [Rust coordinator implementation](steam-cloud-coordinator.md) adds tested local storage, reconciliation and durable pending operations. It is not wired to the launcher or a transfer adapter; Steam Cloud is not active in Forge.

Forge should own the Cloud coordinator in Rust, inside the detached supervisor. It should own account attribution, reconciliation, history, conflicts, deadlines and recovery. Prefer Rust CM transfers where they can coexist correctly with the game's Steamworks storage. Keep the Valve runtime behind a narrow adapter for the game-facing functions that require it. Swift should present state and user choices, and retain its existing Keychain role.

The main unresolved dependency is control of Valve's own synchronizer. We have not proved that Forge can suppress its independent transfers without changing the user's Cloud preference. Offline projection of files and their index before initialization works. While the runtime is open, native file operations preserve private/public coherence; replacing its index externally fails. A second valid direction is a Valve transfer adapter explicitly driven by Forge. Explicit start and offline failure callbacks are now established; conflict interception, active cancellation and successful authenticated completion still need proof. Neither direction is ready for production writes.

This study preserves Auto-Cloud, Cloud API and Dynamic Cloud Sync as separate required capabilities. Silksong is the first metadata sample, not a special case in the proposed implementation.

## Evidence obtained

| Check | Result and scope |
| --- | --- |
| Checkout at study start | Only `docs/steam-native-launch.md` was modified before this study. That change and the September 18 manual lifecycle report were preserved. Subsequent metadata and coordinator implementation is tracked in [the coordinator note](steam-cloud-coordinator.md). |
| Anonymous Steam CM request | A disposable copy of the current Rust `steam-kit` requested PICS metadata for App ID 1030300. One focused probe passed in 1.71 seconds and returned 11,043 bytes. This used no account credentials and downloaded no game content. |
| Silksong configuration | Steam returned Auto-Cloud rules, a 100,000,000-byte quota and a 256-file limit. See the captured [VDF](evidence/steam-cloud-feasibility-2026-09-19/silksong-appinfo.vdf) and [observation report](evidence/steam-cloud-feasibility-2026-09-19/observations.json). |
| Bundled Valve binary | Verified the bundled SHA-256, established guarded interface access and file/control signatures, then executed the isolated Rust probe. Public and private RemoteStorage interfaces respond without installed-Steam access. See [runtime control evidence](steam-cloud-runtime-controls.md). |
| Disposable offline storage | With synthetic app quota metadata and a disconnected test identity, private RemoteStorage writes, lists, reads and survives process restart. A file and index prepared by Rust are also readable after restart. Bare file projection fails, as does replacing the index after it was loaded. Native mutations instead provide working private/public live coherence and persisted deletion. Explicit offline download/upload requests return correlated failure callbacks. This establishes local adapter behavior and disconnected control flow only. See [reproduction and results](evidence/steam-cloud-runtime-2026-09-19/README.md). |
| External contracts | Reviewed Valve documentation, SteamKit source, extracted protocol schemas and Apple's power-notification documentation. The [source research](steam-cloud-source-research.md) distinguishes documented behavior, source evidence and unknowns. |

The probe reused the anonymous sign-in sequence already present in `cm.rs`, then requested only public product information. Its temporary workspace and source hashes are recorded in the observation report. Local scratch artifacts are under `build/steam-cloud-feasibility/`; they are not a new launch workflow.

No authenticated Cloud call, real save access, game launch, installed-Steam read or transfer occurred. The new Rust example builds and passes targeted Clippy; its runner checks the actual pinned runtime. Neither the metadata probe nor the offline runtime probe is an end-to-end Cloud integration test.

### Silksong's published rules

The source rule is `WinAppDataLocalLow`, path `Team Cherry/Hollow Knight Silksong/{Steam3AccountID}`, pattern `*.dat`, recursive. A macOS override changes the root to `MacHome` and transforms the path to `/Library/Application Support/unity.Team-Cherry.Silksong/{Steam3AccountID}`. Linux has its own `LinuxHome` override. These values come from Steam's live response, not from inspecting the user's save directories. [Captured rules](evidence/steam-cloud-feasibility-2026-09-19/silksong-appinfo.vdf)

The intended macOS interpretation is beneath the user's home. The leading slash in the serialized transform must not become an unrestricted absolute path through `PathBuf::join`. Exact transform ordering and remote-name encoding need a fixture and a provider comparison before writes. `{Steam3AccountID}` means the numeric AccountID, distinct from the full SteamID64. [Valve path rules](https://partner.steamgames.com/doc/features/cloud)

No explicit Dynamic flag or `appidredirect` appeared in this capture. That does not prove the running game never uses the Cloud API, nor does it establish Dynamic support. The shared roots indicate a cross-platform configuration; actual save-format compatibility and another device receiving the files remain untested.

### What the runtime inspection establishes

The bundled binary matches version `1788652215`, SHA-256 `035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840`. Its strings include `IClientRemoteStorage`, `SynchronizeApp`, `RunAutoCloudOnAppLaunch`, `RunAutoCloudOnAppExit`, `ResolveSyncConflict`, `LoadLocalFileInfoCache`, `GetRemoteStorageSyncState`, `CancelCurrentAndPendingOperations`, and `GetLocalFileChange`. It also contains suspension-related names and `m_setAppCloudSyncInhibit`. [Static observations](evidence/steam-cloud-feasibility-2026-09-19/observations.json)

The follow-up established engine accessor 24, public RemoteStorage016 access, file and quota signatures, and root/state enums. It also verified that `LoadLocalFileInfoCache` queues work and that `FileRead` requires an indexed record before opening a file. A default unauthenticated user exposes an Individual-type identity with AccountID zero; the directory name `userdata/0` does not mean the full SteamID or account type is zero. A later job rejects the zero AccountID separately. A disposable identity with AccountID 1 passes that local guard without authentication. Synthetic UFS quota metadata removes write result 25. Initializing the cache before writing and waiting for its persisted record allows readback after process restart. [Static and runtime evidence](steam-cloud-runtime-controls.md)

The inhibition collection is used by a platform-change path; it is not a proven general Forge ownership switch. Direction masks 1 and 2, zero-option requests and ordinary failure callbacks are established in the follow-up. Other options, successful completion and Dynamic callbacks still require proof. Do not call guessed vtable slots or edit internal fields. The probe applies the existing hash, image and method-offset checks before calls. [Existing ABI checks](../crates/steam-native/src/abi.rs)

### GameNative as an implementation reference

At the user's suggestion, we inspected GameNative revision `88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52`. Its Kotlin Cloud coordinator performs transfers through JavaSteam and HTTP, tracks previous file state, and connects synchronization to launch/exit. This supplies an implementation precedent for Forge-owned transfers, beyond generated protocol declarations. [SteamAutoCloud source](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/app/gamenative/service/SteamAutoCloud.kt)

The transferable design ideas are a launcher-owned coordinator, explicit local/remote identities, durable comparison state and an intentional cache handoff to the game-facing runtime. Forge should implement these in Rust and verify server acknowledgements independently. The [comparison notes](steam-cloud-source-research.md) identify concrete protocol calls and limits.

GameNative's Windows-container paths and alternate Steam modes do not establish Forge's macOS behavior. Its Android Valve bootstrap is provided as a binary with withheld source. Static disassembly establishes a native app-metadata request and a wait for callback 1270003 before launch. The corresponding macOS accessor, request ABI and callback size are independently established in the [bootstrap investigation](steam-cloud-gamenative-bootstrap.md). This is a preparation precedent, not a Cloud inhibition contract. We found no usable inhibition control in the inspected launcher wrappers. This leaves Forge's single-owner and Dynamic gates open. [Bootstrap wrapper](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/app/gamenative/SteamBootstrap.kt), [component provenance](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/THIRD_PARTY_NOTICES)

## Current implementation gaps

| Current module | Verified gap | Required change if this design proceeds |
| --- | --- | --- |
| [`steam-runtime/files.rs`](../crates/steam-runtime/src/files.rs) and [`process.rs`](../crates/steam-runtime/src/process.rs) | The host gets an ephemeral HOME; cleanup removes it. The sandbox denies writes except `/dev/null`, and denies reading Forge's normal application-support directory as well as Steam's. | Separate persistent account storage from temporary launch data. Grant the host only its required paths, keeping history, journal and unrelated accounts inaccessible. Persistent HOME alone does not prove Valve uses it. |
| [`supervisor/server.rs`](../crates/steam-runtime/src/supervisor/server.rs), `run_game` | Actual child exit immediately calls `runtime.stopped`. Host shutdown waits about 700 ms before forced termination. | Add a Cloud finalization phase before host teardown when the selected adapter needs it. Keep game exit distinct from Cloud success. |
| [`supervisor/mod.rs`](../crates/steam-runtime/src/supervisor/mod.rs) | Reattachment supports `Observe` and preparation `Cancel`, with no Cloud commands or journal. | Versioned, identity-checked commands for decisions, sign-out and credential renewal; separate game, host and Cloud state. |
| [`steam-native/host.rs`](../crates/steam-native/src/host.rs) and [`abi.rs`](../crates/steam-native/src/abi.rs) | The host authenticates its own Valve connection. Callback payloads are discarded; only IDs and sizes are returned internally. There is no Cloud control interface. | A typed Rust adapter with bounded commands and decoded, validated Cloud events. Connection control must cover this host as well as Rust CM. |
| [`steam-native/sdk.rs`](../crates/steam-native/src/sdk.rs) and [`adapter`](../crates/steam-native/adapter/src/lib.rs) | The SDK check does not test RemoteStorage; the adapter redirects discovery only. | A disposable RemoteStorage consumer and explicit validation of persistence and callbacks. Discovery interposition is not a storage implementation. |
| [`steam-kit/cm.rs`](../crates/storefronts/steam-kit/src/cm.rs) | `AppInfo` discards `ufs`. Generic CM request/response exists, but no Cloud messages do; incoming dispatch does not route Cloud service notifications. | Preserve full required rules, implement typed CM operations and Cloud notification routing, and validate device/session identity. |
| [`SteamModel`](../apps/macos/Forge/Models/SteamModel.swift) and [`SteamGameLauncher`](../apps/macos/Forge/Models/SteamGameLauncher.swift) | Sign-out shuts the UI's CM connection; the detached host survives. Games receive the normal HOME. Installation receipts do not identify save ownership. | Explicit Cloud authorization revocation in the supervisor, plus Rust ownership records for physical save locations. |
| [`GameInstallationManager`](../apps/macos/Forge/Models/GameInstallationManager.swift) and [`GameManagementButton`](../apps/macos/Forge/DesignSystem/GameDetailHeader.swift) | Uninstall removes the whole installation directory. The entire gear menu is disabled when uninstall is unavailable. | Preserve matching in-installation saves before removal. Separate history access from uninstall/restore eligibility; retain Epic behavior. |

## Proposed ownership

Add a safe Rust `steam-cloud` module, preferably a crate shared by the supervisor and bridge. Its implementation contains rule expansion, content comparison, durable operations and four-version history. A small interface exposes preparation, user decisions, local restoration, pending-work resumption and observation. Lifecycle events enter through the supervisor. Callers receive decisions or typed state, not individual file-transfer steps.

| Responsibility | Owner |
| --- | --- |
| Synchronization decisions, baseline comparison, conflicts, retry policy, history and restoration | `steam-cloud`, Rust |
| Process lifetime, exclusive admission, post-exit work, detached recovery and platform events | `steam-runtime`, Rust |
| Steam metadata and CM/HTTP wire operations | `steam-kit`, Rust, under coordinator control |
| Real Steamworks interfaces, required Valve cache operations and callbacks | `steam-native`, Rust adapter calling the pinned Valve binary |
| macOS power and process APIs requiring unsafe FFI | A small native adapter outside the safe workspace, using I/O Kit/Core Foundation as appropriate |
| Typed commands and state through the UI interface | `forge-bridge` and the versioned supervisor protocol |
| Modals, localized labels and Keychain access | SwiftUI/AppKit and the existing Swift credential adapter |

The supervisor must own the networking needed after Forge closes. It cannot retain a pointer to the UI process's `steam-kit::Client`. It needs a bounded session handoff or renewal, using the existing private transport approach. Creating another authenticated CM session and choosing its client/device identity still require validation.

For restoration while no game runs, and for pending work at app startup, use the same Rust coordinator through a short-lived worker or an in-process bridge owner under the same locks. A live supervisor remains the sole owner for its game. Keep the current one-Steam-game admission rule until the existing runtime supports more.

### One transfer owner per Cloud store

Prefer this arrangement: Forge resolves Auto-Cloud files and performs CM transfers in Rust; Valve serves local RemoteStorage calls for API games. This requires all of the following to be proved:

1. Valve cannot independently transfer on launch, exit, reconnect or suspend while Forge owns that store.
2. Forge and the game see the same files, deletions, platform flags and cache state.
3. Forge can quiesce writes when needed and notify a Dynamic-capable game of applied changes.
4. Forge can discover and respect account and per-game Cloud preferences without reading an installed Steam client.

`SetCloudEnabledForApp(false)` is a user-preference change, not a hidden technical isolation mechanism. [Valve preference contract](https://partner.steamgames.com/doc/api/ISteamRemoteStorage#SetCloudEnabledForApp)

If the local-storage arrangement fails but Valve offers controllable synchronization, the coordinator may select a Valve transfer adapter for the whole store. Forge still owns the policy: explicit work requests, preserved preimages, user conflict decisions, deadlines, confirmed outcomes and durable recovery. Reject that adapter if it overwrites files or chooses conflicts before Forge can intervene.

Do not mix Rust and Valve transfers within one active account/Cloud-App-ID operation. Record the adapter generation and reconcile its caches before switching adapters. Auto-Cloud-only games also require this exclusion because their host has a live Valve connection. If neither control model meets the contract, report the missing capability rather than silently narrowing Cloud support.

### CM route and its limits

The existing CM session is a useful Rust foundation. The research identifies client changelist, download, upload, delete and blocking batch-completion messages. None has been authorized or executed through Forge in this study. The Web OAuth route is not an assumed fallback for arbitrary owned games. See the [protocol and authentication evidence](steam-cloud-source-research.md#cm-protocol-route-for-rust).

For a Rust transfer, validate the CM envelope and operation result, execute the server's actual upload block plan, confirm file commitment and batch completion, and re-read remote state. A sent exit notification and a successful SDK disk write are not upload acknowledgements. Persist ambiguous outcomes for reconciliation. No reviewed contract proves an atomic multi-file compare-and-swap against other devices; a remote change racing an upload remains an explicit validation gate. [Upload semantics and schema limits](steam-cloud-source-research.md#schema-details-that-affect-correctness)

## Persistent state and reconciliation

Proposed storage is beneath Forge's library directory, independent of installation and runtime caches. Bind every record to SteamID64, logical game App ID, resolved Cloud storage App ID, schema version and operation ID. Separate the game's persistent Valve working area from immutable history and the coordinator journal. AccountID path substitutions derive from the validated account identity.

The coordinator needs:

- A rule snapshot and its fingerprint, including roots, overrides, patterns, exclusions when supplied, OS masks, account substitutions and Cloud App ID redirects. Preserve canonical remote names independently of resolved local paths. Unknown roots or formats are explicit unsupported states, not an empty save set.
- A last confirmed baseline containing complete file identities, hashes, sizes, platform/persistence flags, deletions and the remote change cursor when available. A delta listing must be applied to a known complete baseline; omission from a delta is not deletion.
- An ownership record for actual save locations. Adopt a preexisting unowned save into the connected account automatically, as agreed. Never reassign a location already known to belong to account A when B signs in.
- Immutable complete snapshot manifests and verified content blobs, plus a journal for staging, replacement, remote batch progress and recovery. An intentional restoration records the remote revision it was based on.

Lock by account and resolved Cloud store, and also protect overlapping physical roots. This prevents two logical games sharing Cloud data, two accounts sharing a local folder, or an uninstall from bypassing each other's guard. For shared folders, capture A's data before projecting B's; when B has no data, do not leave A's files in the game-visible set. Persist the switch so a crash cannot silently attribute the remainder to B.

The Rust filesystem layer validates containment, symlink traversal, filename collisions, case and Unicode behavior on the destination filesystem. It operates only on selected save files and never scans an installed Steam tree. A root under the game's installation uses the recorded Forge installation path.

### Three-way decisions

Compare local and remote content against the last confirmed baseline, including deletions. Equal contents converge even if timestamps differ. Do not choose by newest timestamp.

| Observed state | Action |
| --- | --- |
| Neither side changed | Continue. |
| Only remote changed | Preserve the local preimage, stage and validate the remote set, then apply it before launch. |
| Only local changed | Keep the local set for play; publish at the agreed normal exit or Dynamic step. |
| Both changed to the same set | Reconcile metadata after verifying remote state. |
| Both changed differently | Await the local/Cloud choice; preserve the replaced set. |
| No baseline, both nonempty and different | Treat as an initial divergence, not evidence that either side is newer. |
| Remote unavailable or incomplete | Do not infer an empty Cloud or deletions. Offer retry, cancel or "Lancer quand même" before launch. |

Recheck the remote revision when accepting a conflict choice; a stale modal cannot authorize replacement of a newer remote version. Choosing local permits the same deferred publication policy as local-only changes. Choosing Cloud requires a recoverable local replacement. Recheck again after actual game exit because another device may have played meanwhile.

For local multi-file application, preserve preimages, write and hash all incoming data, persist the plan, then apply replacements and deletions under an exclusive guard. Journal progress and recover before permitting launch. Rename makes one file atomic, not an arbitrary set spanning multiple roots. File and parent-directory durability must be established for the chosen storage design; the current helper only fsyncs the file. Never claim a coherent snapshot of actively changing game files merely because two scans match.

### Four versions and local restoration (superseded)

Keep four most recent different complete versions per game/account, deduplicated by content manifest. Create versions at synchronization boundaries and preserve the current local set before Cloud replacement or restoration. Pending-operation bytes remain protected until resolution, even if they are not one of the four visible history entries. Garbage collection must respect both history and journal references.

Restore only while the game is stopped and no sync can replace the same files. Write a durable local-intent marker, preserve the current version, then restore through the same recoverable apply path. If the remote revision has not changed, next-launch preparation keeps this intentional local change. A different remote revision requires the normal comparison/conflict decision. Restoration itself issues no remote mutation; upload follows game exit or an applicable Dynamic step.

The installed Steam game's gear menu gains "Sauvegardes". While playing it allows consultation only. Uninstalled games expose no save action, but their history remains. Before uninstall, preserve matching saves inside the installation outside that directory; failure to preserve them must stop removal. Uninstall never submits Cloud deletes. Keep this policy in Rust even where the existing removal adapter is Swift.

## Detached lifecycle, accounts and deadlines

Represent game phase, Cloud phase and connection confidence separately. After the game exits, Cloud may be uploading, awaiting authentication, awaiting a decision, or durably deferred. A lost control connection is uncertainty, not proof of exit or synchronization success. Retain existing process-identity recovery guarantees. [Current supervisor contract](steam-native-launch.md#surviving-forge-closure-and-crash)

Closing Forge detaches the UI and allows the authorized supervisor to finish. Signing out explicitly revokes Cloud work for that account without stopping the game. Add a command that revokes new transfers in both Rust and the Valve adapter while preserving local game writes. An already-sent request may have an uncertain outcome; reconcile it only under the same account. A later B login cannot renew A's authorization. Reopening Forge resumes only pending jobs belonging to the connected account.

Do not persist credentials in the journal, snapshots or command arguments. A live supervisor may hold a bounded account credential lease in memory; resumed work receives fresh authorization through private IPC after the existing Keychain/session flow. Sign-out must reach the detached owner, with revocation recoverable if IPC fails. A small durable account-generation record can fence a future transfer, but cannot recall an already-sent request. UI closure alone is not revocation.

Set one 120-second deadline when post-exit synchronization starts, before its first request. After the first transient failure, schedule retries at 5, 15, 35, 65 and 95 seconds from that failure, capped by the remaining global budget. Connections, requests and confirmation all consume this budget. Do not overlap attempts or restart the deadline after each failure or wake. If an attempt spans scheduled instants, coalesce missed retries rather than launching a burst. Authentication rejection, quota exhaustion and other action-required errors enter a persistent waiting state immediately.

Before ending the worker, persist remaining work and any unknown remote transaction outcome. On a real network return while Forge is open, resume pending authorized work; when Forge is closed and the bounded worker has exited, wait for the next opening. Do not retain a permanent retry process. Conflicts discovered with the UI closed remain unresolved until it reconnects. Retention locks protect staged save data even after the process exits.

## Dynamic Cloud Sync on macOS

Keep power-event observation in the autonomous Rust supervisor through a small native adapter. I/O Kit has a usable C interface; a SwiftUI observer is insufficient when Forge is closed. Apple's sleep notification is not a guarantee that networking remains available until uploads finish. [Apple power-notification contract](https://developer.apple.com/library/archive/qa/qa1340/_index.html)

Dynamic validation must establish an ordered sequence: a coherent game write boundary, controlled suspension, upload outcome, remote recheck/download at wake, coherent local application, real SDK change notification, and the game's state reload. The actual suspension/recovery mechanism is undecided. Process signals alone can strand a game or interrupt an incomplete write; the supervisor's own crash and process handoffs need explicit treatment.

If sleep interrupts a transfer, persist and resume it after wake without reporting success. That is transfer recovery, not proof of Dynamic synchronization of a still-open game. Never apply a remote replacement while the resumed game can write concurrently. A restored version can upload during a validated Dynamic step, as accepted in the product scope. Silksong's current metadata is not a Dynamic test fixture.

## Proof matrix and next experiments

| Capability | Evidence now | Evidence still required |
| --- | --- | --- |
| Generic Auto-Cloud rules | Live Silksong metadata through Rust | Resolver fixtures for other roots/platforms/redirects; canonical remote filenames; correct real file selection. |
| Local Cloud API writes | Private/public creation, size-changing replacement, reads and deletion while the runtime stays open; persisted final state | Separate game/SDK process, authenticated metadata preparation, remote deletion semantics. |
| Persistence | Native-written and Rust-projected indexed fixtures read back after process restart | Production persistent storage, separate real-account roots and crash durability. |
| Rust CM Cloud transport | Existing CM transport and source message definitions | Authenticated read-only changelist/quota, then disposable transfers with server acknowledgements. |
| Single synchronization owner | Native mutations update the live index coherently; external index replacement fails. Explicit sync requests yield typed offline failures | No second transfers across lifecycle events; remote-baseline coherence, conflict interception and active cancellation. |
| Remote success | Unverified | File/batch acceptance plus fresh remote readback matching content; ambiguous-commit recovery. |
| Another device | Unverified | Independent client downloads and observes the exact version, with compatible game format. |
| Conflicts/history | Rust tests cover divergence, deletions, four-version retention, interrupted restore, intentional-local preservation and stale remote views | Authenticated remote races, runtime integration, account projection and UI. |
| Detached completion | Existing game supervisor is established separately | Cloud finalization with UI closed, 120-second deadline, durable waits and account-isolated resumption. |
| Dynamic | Source contracts and static runtime leads | Cooperative test game, real callbacks and reload, actual macOS power-cycle ordering, supervisor-crash recovery. |

The next discriminating experiment should target the Valve storage adapter before a broad implementation:

1. Continue from the established guarded private/public interface access. Sync admission, cancellation semantics and completion events remain open. Do not infer them from string order.
2. Extend the passing same-process public/private storage fixture to a separate SDK consumer. For production, request and verify native app metadata rather than injecting the synthetic fixture. Keep the temporary Forge-owned storage and installed-Steam read denial. Local process-restart readback is established; authenticated Cloud persistence is not.
3. Use the passing native mutation path for local coherence experiments, while distinguishing its local-change state from an acknowledged remote baseline. Rewriting `remotecache.vdf` while running remains invalid. Establish either exclusive Rust transfer ownership or the explicit Valve-transfer contract, including conflict interception and actual cancellation. Disconnected start/error callbacks do not settle that choice.
4. Separately authorize a disposable authenticated Cloud target. Read changelist/quota first, then upload/download/delete test-only data with interrupted commits and concurrent remote changes. Record sanitized result codes, hashes and revisions, never tokens or signed URLs.
5. Once an adapter passes, implement and test the Rust journal/reconciler with fault injection, then the detached lifecycle and UI. Add a dedicated Dynamic test game and power-cycle validation. Coordinate any real-game or real-save trial with the user.

A successful first adapter test determines how much transfer execution can remain in `steam-kit`; it does not move scheduling, conflict decisions or durable ownership out of Forge. No risky lifecycle changes, real-save experiment, commit or publication are part of this completed study.
