# Steam Cloud source research

Research date: September 19, 2026. This note records documentation and public-source evidence for the feasibility study. No authenticated Cloud requests, game launches, save-file reads, or transfers were performed. It does not establish that Forge's embedded Valve runtime can already synchronize files.

The requested direction is a Rust implementation under Forge's control. Valve may provide necessary game-facing compatibility, but Forge must own scheduling, reconciliation, account attribution, history, conflict decisions, and recovery. The architecture note should treat the unverified controls below as prerequisites, not assumed capabilities.

## Reading the evidence

- **Documented** means Valve or Apple describes the behavior.
- **Source evidence** means a named implementation or extracted protocol schema contains the operation. SteamKit is an independent implementation. SteamDatabase's extracted schemas describe observed protocol structure, not a supported Valve launcher contract.
- **Inference** identifies a design consequence rather than a proven server or runtime behavior.
- **Unverified** requires an experiment or a stronger contract.

## Game configuration and storage methods

Documented: Steam normally downloads before launch and uploads after exit. Auto-Cloud discovers files through configured groups; the Cloud API gives games `ISteamRemoteStorage` access. Dynamic Cloud Sync extends both methods during an active session. [Steam Cloud overview](https://partner.steamgames.com/doc/features/cloud)

Auto-Cloud rules include root, subdirectory, wildcard pattern, OS filter, and recursion. macOS roots include home, Application Support, Documents, and the install directory. Paths can substitute `{64BitSteamID}` and `{Steam3AccountID}`, the latter is the numeric AccountID. Root overrides remap directories between platforms. Separate OS roots can partition saves; cross-platform compatibility must follow actual overrides and file flags. API-created files default to all platforms. A shared Cloud App ID can make two games use one store. Quotas apply per user and game. [Configuration and platform rules](https://partner.steamgames.com/doc/features/cloud)

Dynamic sync uploads around suspension and retrieves changes before resuming the game. The game must handle changes after notification. Valve's example uses Steam Deck; it does not prove Forge can reproduce the sequence on macOS. [Dynamic Cloud Sync](https://partner.steamgames.com/doc/features/cloud)

Inference: preserve original remote names separately from resolved macOS paths. A resolver that invents remote names from local paths can lose cross-platform identity. Shared Cloud App IDs require one coordination lock for the underlying store, even when Forge exposes separate games.

## What the public game SDK proves

Documented: `FileWrite` can fail when disk is read-only. `RemoteStorageFileWriteAsyncComplete_t` reports local-write success. Stream close commits to disk. None of these results establishes remote upload completion. `FileDelete` removes locally and propagates deletion; `FileForget` removes Cloud persistence while retaining local data. [RemoteStorage operations](https://partner.steamgames.com/doc/api/ISteamRemoteStorage)

`BeginFileWriteBatch` and `EndFileWriteBatch` provide hints about related writes or deletes, including direct Auto-Cloud disk writes. They are optional. Cloud preferences have separate account and app getters. `SetCloudEnabledForApp` must follow an explicit user request. It is not an appropriate hidden switch for disabling a competing synchronizer. File platform flags and quota queries also exist. [Batches and preferences](https://partner.steamgames.com/doc/api/ISteamRemoteStorage#BeginFileWriteBatch)

For supported Dynamic games, `RemoteStorageLocalFileChange_t` announces applied changes. `GetLocalFileChangeCount` and `GetLocalFileChange` enumerate updates and deletions, with filename or absolute path semantics. The game must reload appropriate state. This reference provides no documented external-launcher control to set Auto-Cloud roots, force a complete synchronization, wait for its completion, or inject the change list into another process. That is a limit of the reviewed public interface, not proof that Valve's private runtime has no such controls. [Dynamic callbacks](https://partner.steamgames.com/doc/api/ISteamRemoteStorage#RemoteStorageLocalFileChange_t)

## Web API authentication and completion

Documented: the OAuth route requires a Client ID assigned by Valve. Cloud permissions are restricted to the App IDs declared for that client. A normal Steam login token therefore cannot be assumed to grant Web Cloud access for an arbitrary library. The documentation does not establish that Forge has this registration or these scopes. [Steam OAuth setup and Cloud scopes](https://partner.steamgames.com/doc/webapi_overview/OAuth)

`ICloudService` requires `read_cloud` for enumeration and `write_cloud` for modifications. Enumeration is paginated and can return file hashes, platform flags, and download URLs. An HTTP 200 response still requires successful `x-eresult`. [ICloudService](https://partner.steamgames.com/doc/webapi/ICloudService)

The documented mutation sequence is `BeginAppUploadBatch`, then `BeginHTTPUpload`, HTTP PUT, `CommitHTTPUpload` for each upload, any deletes, and `CompleteAppUploadBatch`. Completion must run after success or failure; omitting it can block later uploads for several minutes. `CommitHTTPUpload` returns `file_committed`. Batch completion releases the pending batch; the docs describe reduced conflict likelihood, not atomic rollback. Its `app_change_number` is documented as unused by this Web API. [Upload and batch protocol](https://partner.steamgames.com/doc/webapi/ICloudService#BeginAppUploadBatch)

Inference: persist outstanding batch cleanup separately from successful-file progress. A lost reply after PUT or commit is an unknown outcome, not permission to mark the save synchronized. Re-read remote state before deciding whether to resend. No reviewed Web contract proves a compare-and-swap upload that excludes another device between comparison and commit.

## CM protocol route for Rust

Source evidence is pinned to SteamKit commit `84c990c3982eedb5abd733116b987c9870a0dccb`, dated September 16, 2026. The generated Cloud service contains these version-1 operations. [SteamKit Cloud bindings](https://github.com/SteamRE/SteamKit/blob/84c990c3982eedb5abd733116b987c9870a0dccb/SteamKit2/SteamKit2/Base/Generated/SteamMsgCloud.cs#L2313)

| Purpose | CM method names |
| --- | --- |
| Remote file state | `Cloud.GetAppFileChangelist#1`, `Cloud.GetSingleFileInfo#1` |
| Download | `Cloud.ClientFileDownload#1` |
| Upload | `Cloud.BeginAppUploadBatch#1`, `Cloud.ClientBeginFileUpload#1`, `Cloud.ClientCommitFileUpload#1` |
| Delete | `Cloud.ClientDeleteFile#1` |
| Batch acknowledgement | `Cloud.CompleteAppUploadBatchBlocking#1` |
| Quotas | `Cloud.ClientGetAppQuotaUsage#1` |
| Lifecycle | `Cloud.SignalAppLaunchIntent#1`, `Cloud.SuspendAppSession#1`, `Cloud.ResumeAppSession#1` |
| Outgoing notifications | `Cloud.CompleteAppUploadBatch#1`, `Cloud.SignalAppExitSyncDone#1`, `Cloud.ClientConflictResolution#1` |
| Incoming state notification | `CloudClient.NotifyAppStateChange#1` |

The outgoing exit notification reports the client's conclusion. It is not an acknowledgement from Steam. The blocking batch variant has a response, unlike its notification counterpart. The Cloud service also contains Web-style HTTP upload operations; their presence does not settle which path Forge's session can use. [Generated requests and notifications](https://github.com/SteamRE/SteamKit/blob/84c990c3982eedb5abd733116b987c9870a0dccb/SteamKit2/SteamKit2/Base/Generated/SteamMsgCloud.cs#L2374)

SteamKit sends unified requests using `ServiceMethodCallFromClient` after login, with a target method and job ID. It uses the non-authenticated message form when no SteamID exists. Notifications omit the job ID. This is CM session transport, not the Web OAuth flow. It does not prove backend authorization for Forge's account and each Cloud method. [Unified-message transport](https://github.com/SteamRE/SteamKit/blob/84c990c3982eedb5abd733116b987c9870a0dccb/SteamKit2/SteamKit2/Steam/Handlers/SteamUnifiedMessages/SteamUnifiedMessages.cs#L49)

Response success comes from the protobuf header's `eresult`, independently of the typed body. Forge must validate both the envelope and any operation-specific result. [Response callback implementation](https://github.com/SteamRE/SteamKit/blob/84c990c3982eedb5abd733116b987c9870a0dccb/SteamKit2/SteamKit2/Steam/Handlers/SteamUnifiedMessages/Callbacks.cs#L24)

SteamKit's public `SteamCloud` helper exposes UGC details, single-file information, and sharing. It is not a complete save synchronization engine. The generated service is the relevant protocol reference. [SteamCloud helper](https://github.com/SteamRE/SteamKit/blob/84c990c3982eedb5abd733116b987c9870a0dccb/SteamKit2/SteamKit2/Steam/Handlers/SteamCloud/SteamCloud.cs)

## Schema details that affect correctness

Source evidence: SteamDatabase Protobufs commit `d35adb23d7d48ad9673261f9572b60d1bfe06c12`, dated September 19, 2026, contains these fields. [Cloud schema](https://github.com/SteamDatabase/Protobufs/blob/d35adb23d7d48ad9673261f9572b60d1bfe06c12/steam/steammessages_cloud.steamclient.proto)

- Changelists carry `current_change_number`, `is_only_delta`, path prefixes, hashes, platform masks, persistence state, and reupload requests. Absence from a delta is not deletion.
- Client upload responses contain block requests, HTTP method, headers, offsets, lengths, explicit body data, and `may_parallelize`. Upload cannot assume one arbitrary PUT.
- Upload and download schemas distinguish raw size, transfer size, and encryption. File commit returns `file_committed`.
- Explicit deletion has its own fields. Quota responses expose byte and file limits.
- Launch intent returns pending remote operations. Suspension reports `cloud_sync_completed`; exit reports `uploads_required` and `uploads_completed`.
- A state-change notification carries an app ID and change number, not downloaded file bytes.

Inference: these fields identify implementation obligations, but do not establish their complete server semantics, compression format, encryption procedure, timeout behavior, or availability to Forge. Preserve unknown states and reject unsupported transfer formats rather than interpreting them as empty saves.

The client-object schema distinguishes active sessions, uploads in progress, uploads pending, and suspended sessions. Each pending operation can include machine, client ID, timestamp, OS, and device type. [Pending-operation schema](https://github.com/SteamDatabase/Protobufs/blob/d35adb23d7d48ad9673261f9572b60d1bfe06c12/steam/steammessages_client_objects.proto#L5)

Unverified: whether the existing Forge and embedded-runtime connections may share an appropriate device identity, how simultaneous sessions affect these signals, and which pending states require waiting or an explicit user choice. A launch signal is not a substitute for a file-state comparison.

The generic `CloudConfigStore` source has versioned key/value namespaces and download/upload methods. It does not identify a Cloud-preference key or establish global/per-game setting availability. Do not claim that this name proves preference synchronization. [CloudConfigStore schema bindings](https://github.com/SteamRE/SteamKit/blob/84c990c3982eedb5abd733116b987c9870a0dccb/SteamKit2/SteamKit2/Base/Generated/WebUI/SteamMsgCloudConfigStore.cs)

## macOS sleep and wake

Documented: Apple provides `NSWorkspace` sleep/wake notifications and I/O Kit power notifications. Cocoa observers use the workspace notification center, not the default center. An I/O Kit observer can use `IORegisterForSystemPower` with a run loop independently of SwiftUI. Apple's archived QA describes up to 30 seconds for acknowledgement, and distinguishes idle sleep from forced sleep. Lid closure or explicit sleep cannot be prevented by rejecting the forced-sleep notification. [Apple QA1340](https://developer.apple.com/library/archive/qa/qa1340/_index.html)

Inference: a Rust supervisor can own a small platform adapter over the C I/O Kit/Core Foundation APIs. Receiving an event does not guarantee a network window, coherent game files, or that Forge executes before a waking game. The QA is an archived contract and must be tested on the supported macOS versions.

Unverified: game write quiescence, process-tree suspension, safe recovery if the supervisor dies while a game is suspended, download before game execution resumes, and genuine delivery of the SDK change notification. Sending SIGSTOP and SIGCONT alone would not prove those properties. The 120-second post-exit retry policy cannot be treated as an available sleep delay.

## Runtime controls that remain unverified

Rust-owned transfers require proof that Forge can prevent autonomous Valve transfers while preserving local RemoteStorage behavior, preferences, and cache consistency. Filesystem copies alone cannot establish that a running SDK client sees external changes. A Valve transfer adapter is an alternative only if Forge can request work, observe completion, control conflicts, and recover interrupted operations. Neither control model is proven here. The feasibility note should specify disposable-data experiments for these gaps and separately validate CM authorization, remote readback, and Dynamic callbacks.

## GameNative implementation comparison

The [official site](https://gamenative.app/) links `utkarshdalal/GameNative`. This inspection pins commit [`88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52`](https://github.com/utkarshdalal/GameNative/tree/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52), dated September 17, 2026. It is source evidence, not an Android execution test. The repository carries [GPL version 3](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/LICENSE). This research records behavior and protocol facts; no implementation was copied into Forge.

`SteamService` logs the user into CM through JavaSteam using the refresh token. Cloud calls use the resulting `SteamCloud` handler. Launch and close functions serialize synchronization per app; offline close records pending work. Launch checks Steam's pending remote operations after successful reconciliation. Their bodies contain no real-Steam/Bionic-mode exclusion, so callers and native internals still determine which modes actually use them. [Authentication](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/app/gamenative/service/SteamService.kt#L3417), [launch and exit](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/app/gamenative/service/SteamService.kt#L3105)

`SteamAutoCloud.syncUserFiles` is a launcher-owned synchronizer. It reads changelists, compares local SHA-1 hashes with a persisted snapshot/change number, requests explicit local/remote conflict choices, and performs HTTP downloads/uploads through JavaSteam Cloud metadata. It scans Windows Auto-Cloud rules and `SteamUserData`, including games without Auto-Cloud patterns. Original remote roots/paths remain separate from overridden local paths. After success it writes `remotecache.vdf`. These are concrete references for Forge's protocol, resolver, and baseline model. [Synchronization and path mapping](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/app/gamenative/service/SteamAutoCloud.kt#L146)

Several details need stronger handling in Forge. Scanning has a fixed depth of five; some path comparisons lowercase names. Downloads write directly to destination files. Upload success counts successful HTTP blocks, but `commitFileUpload`'s returned boolean is only logged. Batch completion is not protected by a `finally` in this function. `isOnlyDelta` appears in logging without a branch that merges delta metadata into a complete manifest. These observations identify gaps in this source path, not reproduced production failures. [Upload completion](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/app/gamenative/service/SteamAutoCloud.kt#L733), [changelist use](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/app/gamenative/service/SteamAutoCloud.kt#L803)

The declared dependency is JavaSteam `1.8.0.1-26-SNAPSHOT`, pointing to the `gamenative-latest` branch. At inspected source commit `433f2ad15c36d5e690a4fe77401ec3f6b960641e`, `commitFileUpload` returns `fileCommitted`; `completeAppUploadBatch` calls the blocking CM method. The wrapper methods shown do not explicitly inspect `response.result`. The inspected branch is not proof of the exact published SNAPSHOT artifact's contents. [Dependency declaration](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/gradle/libs.versions.toml#L21), [JavaSteam Cloud wrapper](https://github.com/joshuatam/JavaSteam/blob/433f2ad15c36d5e690a4fe77401ec3f6b960641e/src/main/java/in/dragonbra/javasteam/steam/handlers/steamcloud/SteamCloud.kt#L281)

GameNative's tests cover root overrides, offline edits, conflicting changes, missing cache, and cloud-only files. They mock the Cloud service, including successful commits and batch completion. They provide useful fixtures and scenarios, not proof of Steam acceptance or another device receiving the data. No tests were run in this study. [Cloud tests](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/test/java/app/gamenative/service/SteamAutoCloudTest.kt#L221)

Bionic mode launches a separate bootstrap executable, which hosts genuine Android `libsteamclient.so`; Windows games bridge through Proton `lsteamclient`. The bootstrap is proprietary and its source is withheld. Public wrappers do not establish a Cloud inhibition control or cache-coherence contract with the Kotlin synchronizer. [Current process wrapper](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/app/gamenative/SteamBootstrap.kt#L121), [component notice](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/THIRD_PARTY_NOTICES#L105)

The reviewed Cloud service path contains no Dynamic suspend/resume or `RemoteStorageLocalFileChange` delivery. `UFS` retains quota, file count, and patterns only. `CloudConfigStoreService` is used for library collections, not Cloud preferences. These source paths therefore support a launcher-driven ordinary sync design, while genuine-runtime coexistence and Dynamic behavior remain unverified. [UFS model](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/app/gamenative/data/UFS.kt), [CloudConfigStore caller](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/app/gamenative/service/SteamService.kt#L4964)
