# Steam Cloud transfer ownership

Research date: 2026-09-19. This follow-up used static analysis and existing offline evidence. It did not execute the runtime, authenticate, inspect real saves or change a Cloud preference.

## Decision supported by current evidence

Keep Rust Cloud mutations disconnected from the production native host. There is still no established preference-preserving switch that makes this Valve runtime a local-only RemoteStorage provider. The shortest useful next experiment is an authenticated, disposable Valve transfer-adapter probe, with Rust CM access limited to observation during that probe. Its first pass should prove one explicit upload and download with verified remote bytes. It must not be described as production-ready ownership until conflict admission and cancellation also pass.

The existing local work does not need repeating. Native local writes, private/public coherence, persisted deletion, pre-initialization file/index projection, and disconnected directional callbacks already pass. Live external index replacement already fails. Sources: [runtime control evidence](steam-cloud-runtime-controls.md), [offline observations](evidence/steam-cloud-runtime-2026-09-19/README.md), [GameNative bootstrap investigation](steam-cloud-gamenative-bootstrap.md).

## Pinned input and reproduction

The universal `build/steam-cloud-feasibility/steamclient.dylib` was hashed again for this investigation. SHA-256 is `035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840`, matching the [runtime manifest](../crates/steam-runtime/runtime-manifest.json). Addresses below refer to its ARM64 image, not universal-file offsets or process addresses.

The previously extracted ARM64 image remains at `/var/folders/bj/bjxppt2d0jg9ppj9_bkdsz580000gn/T/forge-cloud-static-w0ewwrzw/arm64.dylib`. `parsed.json` there records Mach-O sections and `LC_FUNCTION_STARTS`. The evidence can be reproduced with bounded disassembly, for example:

```sh
shasum -a 256 build/steam-cloud-feasibility/steamclient.dylib
xcrun llvm-objdump -d --start-address=0xacf310 --stop-address=0xacf654 /var/folders/bj/bjxppt2d0jg9ppj9_bkdsz580000gn/T/forge-cloud-static-w0ewwrzw/arm64.dylib
xcrun llvm-objdump -d --start-address=0xacfb48 --stop-address=0xacfde4 /var/folders/bj/bjxppt2d0jg9ppj9_bkdsz580000gn/T/forge-cloud-static-w0ewwrzw/arm64.dylib
```

Do not add `--macho` to those range-limited commands. The installed objdump otherwise ignores the requested bounds. Surviving export names printed beside stripped instructions are not method identities.

## The identified inhibitor is part of platform migration

The previous note established a collection named `m_setAppCloudSyncInhibit` at server member `+0x228`, read by queue runner `0xace9c8`. This follow-up identifies the exposed producers of its command flags:

| Operation | Proxy slot and entry | Server entry | Evidence |
| --- | --- | --- | --- |
| `PerformAppPlatformChangeFileBackup` | 91, `0x904d6c` | `0xacf310` | Proxy diagnostic argument resolves to name string `0x145734f`. Server queues or merges `0x100`, then starts the queue runner at `0xacf3a4`. |
| `PerformAppPlatformChangeFileRestore` | 92, `0x904ecc` | `0xacf498` | Proxy diagnostic argument resolves to name string `0x1457372`. Server queues or merges `0x200`, then starts the queue runner at `0xacf52c`. |

The proxy vtable is `0x1744900`. The server vtable is `0x1762760`, whose entries at `0x1762a38` and `0x1762a40` point to the two server bodies. This independently ties the names to the slots; their position was not borrowed from another Steam build.

In the runner, command bit 8 launches job `0x615200` with command `0x100`, then inserts the app into the inhibit collection at `0xaceb90` through `0xaceb9c`. A request for an already inhibited app returns without scheduling unless command bit 9 is set. That branch removes the app at `0xacead4` through `0xaceaf8` before proceeding. Diagnostics explicitly identify platform-change backup/restore. These calls initiate real migration work. They are not a harmless lock/unlock pair and should not be used to manufacture Rust transfer ownership.

`SynchronizeApp` cannot set these two queue bits through its current option mapping. Server instructions `0xacfce4` through `0xacfd28` map direction to command bits 3 and 2, option bit 0 to command bit 7, option bits 1 and 2 to command bits 5 and 6, and option bit 3 conditionally to command bit 11. No option maps to command bit 8 or 9. Higher option bits are not consumed in that mapping. This rules out passing a guessed `0x100` or `0x200` option as the known inhibitor.

## Command-line and configuration search

A bounded scan of printable NUL-terminated ARM64 strings found no `-no-cloud`, `-disablecloud`, `-cloudsync`, or named local-only Cloud switch. That negative result does not prove that no indirect control exists.

The relevant distinct test control is `@test_nCloudForceSyncFailureAppID`, at string `0x15b3f3f`, registered by initializer `0xae354c`. Its help string at `0x15b3f61` describes a nonzero App ID causing a test synchronization failure. `@test_bCloudForceSyncConflict` changes the simulated failure kind. Static consumers are:

| Consumer | Observed effect |
| --- | --- |
| `SynchronizeApp`, `0xacfbf4` through `0xacfc5c` | Matching App ID bypasses normal queue admission and emits direction-specific completion with failure result 2 or 60. |
| State helper, `0xace138` through `0xace164` | Matching App ID produces state 6 or 7. |
| `GetRemoteStorageSyncState`, `0xad09f8` through `0xad0a20` | Matching App ID produces state 6 or 7 after the active-operation check. |

This is a failure-injection control, with a state-reporting side effect. The scan did not establish a supported setter, a command-line assignment syntax, or coverage of every autonomous work producer. The queue runner and cache-loading entry are distinct from explicit `SynchronizeApp`. Therefore this cvar is useful future test evidence, not a production exclusion mechanism.

Other matching controls set transfer parallelism, request counts, download throttling, dynamic-sync test App ID, or an out-of-date-game download policy. Their names and registrations do not establish a switch that forbids all remote mutations.

Cloud enablement remains a user preference. Valve's public documentation requires the app setter to follow an explicit user choice. Temporarily changing it to obtain ownership would also expose the changed value to games. Source: [ISteamRemoteStorage preference methods](https://partner.steamgames.com/doc/api/ISteamRemoteStorage#SetCloudEnabledForApp).

## Why a read-only native HOME does not settle ownership

Separating phases establishes that Rust is not transferring while the native host is alive. It does not establish that the native host cannot change remote Cloud state during that interval. Denying real save-root reads and all local writes does not filter the Cloud service multiplexed over the authenticated CM connection.

The pinned runtime contains a concrete CM delete route. Function `0x9d68b8` obtains a file record, builds an app/filename request, selects string `Cloud.ClientDeleteFile#1` at `0x159ea72`, and dispatches it at `0x9d69f8`. The success handler is called afterward at `0x9d6a20`. There is no checked filesystem write between request construction and CM dispatch in this function. Its direct callers are `0x9d796c` and `0x9da4b4`. This establishes a remote mutation route that cannot be excluded merely by observing no HTTP upload or no local write.

This analysis does not prove that the route is reachable from a fresh, metadata-free, read-only host. Conversely, that host's absence of appinfo and install registration is not a proved permanent barrier. Native authenticated metadata can exist in memory, and the existing host already supplies a valid identity and requests app ownership. A proof must follow metadata acquisition, SDK attachment/shutdown, reconnect, and queued work; a missing disk cache by itself is insufficient.

For a narrowly scoped Auto-Cloud deployment, a file sandbox can still protect real local saves. Its remaining ownership test must cover remote deletion and Cloud lifecycle messages through CM. Observing the server changelist before and after is a useful falsification check, but a bounded unchanged result alone does not prove that every later trigger is excluded.

## Next proof, without changing real saves

Use a dedicated owned Cloud-enabled test application and verify its real quota, preferences, remote manifest, and root rules first. App 480 is only a candidate; the offline synthetic App 480 quota is not evidence of real authorization or configuration. If the chosen app has existing remote data, stop this probe design and select an empty disposable app. A unique filename alone does not contain a native whole-app synchronization.

Use a fresh scratch HOME and private IPC endpoints, prohibit real Steam-installation/save access, and reuse Forge's authorized in-memory credentials through a private pipe. Do not persist credentials or spoof the disconnected fixture identity. Require native authenticated SteamID equality, real app metadata readiness and nonzero quota before any file operation. Keep the real user Cloud preferences unchanged and record their observed values.

Run these bounded stages, recording account, App ID, host generation, callback sequence and full verified CM snapshots:

1. With Rust CM writes disabled, initialize the native cache and establish an idle, empty baseline. Create one known small fixture with native `FileWrite`. Public SDK readback must match the bytes. Observe a short baseline window without explicit synchronization. If the remote file appears already, record autonomous transfer and its trigger rather than crediting the later explicit request.
2. In a runtime with no other operations, call the already verified slot 78 with direction 2 and options 0. Require callback 1250002 of size 12 for the expected app and success word 1. Then use the existing read-only Rust CM adapter to obtain a complete remote manifest and download/hash the fixture. Admission, native idle state, or a success callback alone cannot pass.
3. Shut down that runtime completely. Start a fresh scratch runtime for the same identity/app with no projected local fixture. Initialize metadata/cache, call slot 78 with direction 1 and options 0, require matching callback 1250001 and success word 1, then read the exact bytes through public RemoteStorage. Also record whether any file arrived before the explicit request. This tests the native remote baseline and download path without external live index replacement.
4. In this disposable app only, prove conflict admission using two controlled runtime generations. Seed baseline A, create divergent remote B while the other runtime is stopped, and preserve divergent local C in its established baseline. On resuming that runtime, record whether anything uploads/downloads before Forge receives actionable conflict state. A mutation before decision fails Forge-controlled conflict admission. The exact `ResolveSyncConflict` choice mapping must be established independently before calling it.
5. Prove active cancellation with a disposable fixture large enough to observe progress or with a controlled transfer pause. Require an actually active transfer before slot 84, then a terminal callback, no later commit, and complete remote readback. An immediate disconnected failure is not a cancellation test. Cancellation is runtime-wide, so this probe runtime must own only this app's operation.

Stages 1 through 3 are the immediate next executable proof. Stages 4 and 5 are release gates for adopting Valve as the transfer adapter. Cleanup must remove only this probe's known records and verify the final remote manifest; never clear an app that had preexisting data.

If those stages pass, a coherent design is available: Rust owns policy, archives, account fencing, retries and the detached lifecycle; Valve owns all Cloud mutations and its local RemoteStorage index. Rust CM requests observe and verify remote state. That is an architectural inference, not a validated product decision. Native autonomous activity still needs to fit the same policy boundaries before launch integration.

If Rust must remain the remote transfer implementation, the next missing proof is stronger: identify and validate a control that excludes all Valve Cloud mutations while leaving identity, SDK local storage, preferences and native index behavior intact. Neither the platform-migration methods, the failure-injection cvar, filesystem read-only mode, nor the current disconnected probes provides that proof.

Dynamic synchronization remains separate. Ordinary file mutation produced no local-file-change notification in the existing fixture, and public Steamworks behavior requires a real remote download and game-side processing of the resulting changes. Source: [Valve Dynamic Cloud Sync contract](https://partner.steamgames.com/doc/features/cloud#dynamic_cloud_sync).

## Follow-up: concrete producers in the embedded host

The initial recommendation above concerned unrestricted native RemoteStorage. The narrower question is whether the current embedded host automatically reconciles an arbitrary game's saves merely because the SDK attaches, disconnects, or the account logs in. The following static evidence narrows that question substantially. It does not rely on a quiet trace, a missing HTTP request, or the presence of a theoretical delete method.

### Queue and constructor census

A scan of every aligned ARM64 `B` and `BL` instruction in `__text` found these eight direct calls to queue runner `0xace9c8`:

| Call site | Enclosing function | Producer |
| --- | --- | --- |
| `0xace934` | `0xace868`–`0xace94c` | `InitializeUGCQuotaUsage`, server slot 22, command `0x400`. |
| `0xacf2fc` | `0xacf274`–`0xacf310` | `LoadLocalFileInfoCache`, slot 70, command `1`. |
| `0xacf3a4` | `0xacf310`–`0xacf3b8` | Platform backup, slot 91, command `0x100`. |
| `0xacf52c` | `0xacf498`–`0xacf540` | Platform restore, slot 92, command `0x200`. |
| `0xacf81c` | `0xacf654`–`0xacf834` | `EvaluateRemoteStorageSyncState`, slot 71, command `2`. |
| `0xacf92c` | `0xacf834`–`0xacfa38` | `ResolveSyncConflict`, slot 77, command `0x14` or `0x18`. |
| `0xacfd88` | `0xacfb48`–`0xacfde4` | `SynchronizeApp`, slot 78, directional and option flags. |
| `0xacff9c` | `0xacff0c`–`0xad0024` | Job completion: clear the active job and run an already queued request. It adds no command. |

`RunAutoCloudOnAppLaunch` (`0xacfa38`, slot 80) and `RunAutoCloudOnAppExit` (`0xacfac0`, slot 81) add or merge `0x20` and `0x40` respectively. Neither directly calls the runner. The primary insertion helper `0xace94c` has direct callers only in quota, cache initialization, evaluation, conflict resolution, these two AutoCloud entries, and synchronization; the platform methods use their own insertion path.

The sync-job construction chain is equally narrow: runner call `0xaceb00` is the only direct caller of allocator `0xacfec4`; allocator call `0xacfee4` is the only direct caller of constructor wrapper `0x614fa8`; that wrapper is the only direct caller of real constructor `0x614c6c`. Its diagnostic name is `CClientJobRemoteStorageSync`, string `0x156a33b`. App/command setter `0x615200` has exactly two direct callers, both in the runner (`0xaceb1c`, `0xaceb40`). This establishes the construction route for this job; it does not claim that every Cloud feature uses that job.

Requests merge by **App ID**, using bitwise OR. `LoadLocalFileInfoCache` adds only bit 0 (`0xacf2ec`–`0xacf2f4`). Consequently a preexisting explicit request for the same game can turn command `1` into a compound command, but a roaming request for App 7 cannot do so. A new runtime with an empty queue matters more than the absence of a disk cache.

### SDK attachment's command 1 does not run the transfer branch

`CUser::AddClientGame`, `0xa64a9c`–`0xa65418`, is identified by diagnostic string `AddClientGame` at `0x15acf09`. If the game's local cache has not been loaded, instructions `0xa65240`–`0xa65250` invoke the embedded storage object's slot 70 with that game's App ID. This is a concrete default per-game producer, but its command is cache initialization.

In job body `0x615574`, instructions `0x615cb4`–`0x615cc4` compare the full command word to **1** and jump directly to successful cleanup. They precede remote file-list work at `0x615d00`, reconciliation, download helper `0x6183b0` called at `0x6160cc`, and upload helper `0x618a9c` called at `0x6161c0`. The upload helper contains `Cloud.BeginAppUploadBatch#1`. Command 1 therefore cannot fall through to that app-level transfer path.

Cache initialization is not filesystem-inert. Before that early exit it loads `remotecache.vdf` through `0x61686c`/`0x619b0c`, validates local records through `0x61695c`/`0x61a4f4`, and calls AutoCloud local-state checking `0x5bb280`. The latter can swap local user files and, if it finds evidence of an incomplete prior run, invoke local exit scanning `0x5bb538`. Platform mismatch can also call local backup/restore helpers `0x5bc4b8` and `0x5bc6c8`. This is why denying native access to the real save roots remains part of the proposed AutoCloud-only contract.

The inspected local exit-scan deletion branch calls `0x5b78b0` → `0xacbb50`. It deletes/marks the local record and schedules `0xac85c0`; it does not dispatch `Cloud.ClientDeleteFile#1`. The cache-save queue at member `+0x5d8` is consumed by `CUserRemoteStorage::RunFrame`, `0xac641c`. Its allocator `0xac65b8` names **`CJobSaveCacheDirectory`** (`0x15b6dd1`), installs vtable `0x1764720`, and its body `0xadd738` calls `0xad17c8`. That method serializes the app's `remotecache.vdf` and starts a worker named `YldWriteCacheDirectoryToFile` (`0x15b6299`). The sync job's cleanup reaches this save queue via `0x619a88`; its other cleanup `0x619acc` completes the current job via `0xacff0c`. Neither adds a transfer command.

Thus the particular attach → command 1 → local scan → cleanup route does not establish a remote mutation. This corrects any inference that simply starting a class named “RemoteStorageSync” proves an upload or deletion. Queued explicit requests and separate API jobs still require separate treatment.

### Lifetime, roaming, and explicit system controls

`CUserRemoteStorage` constructor `0xac5278` registers an `AppLifetimeNotice_t` callback with handler `0xac560c`. The handler's flag-1 branch decodes the Game ID and calls `0xad4fe0`; that method clears app-specific request collections at members `+0x268` and `+0x2a8`. It does not add a sync request. The other flag branch returns. The `SteamServersDisconnected_t` handler `0xac55c0` clears each loaded cache's initialization byte; it does not queue a transfer. `AppInfoChanged_t` handler `0xac5604` calls `0xac6618` to invalidate local path/root caches, also without enqueueing transfer work. These are the object's actual callback registrations, not guessed public callback behavior.

There is automatic Cloud activity for **App 7**. Login initialization calls slot 70 with literal `7` at `0xa53d3c`; `CUser::SaveFavorites` does likewise at `0xab6920`. `CClientJobPostLogon`, `0x601354`, calls slot 78 with App 7, direction 1, at `0x601434`. `CUser::ScheduledSyncCloud` (`0xa50f48`, named at `0x15ad718`) calls `0xa6b4b0`; that method's state, evaluation, conflict, and synchronization calls all supply literal `7`, and its diagnostics say `[Roaming Config]`. App 7 must be observed separately from the game's manifest and from the user's Cloud preferences. An unchanged game manifest does not imply that the embedded host performed no Cloud work at all.

Other native slot-78 calls in `CVRManager` supply fixed App IDs `250820` and `330050`, for example `0xb1709c`/`0xb170bc`. They do not derive an arbitrary running game's App ID.

Arbitrary-AppID Dynamic Cloud synchronization does exist in `CClientJobPrepareForSuspend` and `CClientJobResumeSuspendedGames`. The former reaches slot 78 direction 2 through `0x6038a8`; the latter reaches direction 1/options 3 through `0x602d3c`. Their sole direct constructor chains originate in CUser methods `0xa73eec` and `0xa73f50`, exposed by **`IClientUser::PrepareForSystemSuspend`** and **`IClientUser::ResumeSuspendedGames`**. The names are independently present in proxy functions `0x8a8850` and `0x8a89bc`; adjusted CUser vtable entries `0x175b1b8`/`0x175b1c0` target thunks `0xa73f44`/`0xa73fcc`, with offsets `0x798`/`0x7a0` from the IClientUser subobject vtable. The inspected producers are explicit client-interface controls. No default SDK attach/detach producer of those jobs was found in the direct-branch and nearby indirect-call census. This is not a proposed ABI invocation and these slots must not be called experimentally on that basis alone.

These findings support the distinction between an embedded SDK host and a full client that actively sends lifecycle synchronization requests. They do not prove the complete behavior of the separate official Steam application, whose executable was not inspected.

### Public FileShare is a real, separate remote producer

An unrestricted public SDK contract cannot claim that only Forge can transfer game Cloud files. Public RemoteStorage016 `FileShare`, slot 7 at `0x7e2528`, derives the current game App ID and supplies root **0** to private slot 8. Server `0xad205c` allocates a separate job through `0xad7d98`, then schedules it through `0x5c64ac`. Its base diagnostic name is `CAPIBaseFileSynchronizingJob`.

The body `0xad8048` logs `Starting ShareFile`. It invokes `0xad8358` with the requested file key and a true force argument. The helper first requires an existing native local record; absence returns result **9** at `0xad8560`, before its CM request. Otherwise it requests `Cloud.GetSingleFileInfo#1`, compares the remote record, and can call `0xad8784` → `0x9da2fc` → `0x9d52b8`. That last body contains **`Cloud.ClientBeginFileUpload#1`** (`0x9d566c`) and **`Cloud.ClientCommitFileUpload#1`** (`0x9d6134`). After the upload succeeds, the FileShare body sends **`Cloud.ShareFile#1`** at `0xad81f8`–`0xad8210` and returns its sharing callback.

This is a single named base Cloud file upload, followed by sharing. It does not queue full-app reconciliation, but it can commit bytes in the same root-0 namespace that contains API saves. It is therefore more than a separate UGC-metadata notification. It is also an explicit game API request, not an automatic consequence of ordinary `FileWrite`, attachment, or exit. No preference-preserving interception/delay mechanism was established. Merely postponing its upload would postpone successful FileShare semantics; returning success before upload would misrepresent them.

### Minimal pinned contract to test next

The actionable first contract is **AutoCloud-only synchronization around a restricted embedded host**, not a binary-wide claim that Valve cannot transfer:

1. Pin the runtime hash, architecture and verified host ABI. Start an isolated native generation with no old queued commands. Rust completes prelaunch Cloud work before that generation starts and performs postexit work only after its verified termination.
2. Admit only the selected ordinary game's verified AutoCloud rules. API saves, Dynamic Cloud, and game-requested file sharing/workshop operations remain outside this first contract. App 7 roaming and the fixed VR apps are separate namespaces, never evidence of per-game transfer exclusion.
3. Keep the host call surface free of explicit evaluation, synchronization, conflict, AutoCloud launch/exit, platform-migration and system-suspend/resume controls. The current host implementation does not call them. Preserve the user's preference values.
4. Deny the native host real save-root reads and writes. Do not project API-save files or a native API-save index into that host, and do not claim working native API storage under this contract. The missing-record FileShare failure is a concrete barrier for an empty native API cache; an unrestricted successful FileWrite/FileShare pair would invalidate that assumption.
5. Validate the genuine separate-process SDK attachment path with the existing offline fixture, covering attachment, ordinary local reads/writes/deletion, callbacks and shutdown without explicit sync requests. Local API coherence is a separate result from the restricted production AutoCloud contract. Confirm that any recorded per-game sync-job command is exactly `1`, not merged flags.
6. For the bounded authenticated AutoCloud lifecycle, compare a complete game manifest before native startup and after verified native exit, including deletions and revision changes; verify real save-root denial independently. Repeat the same supported generation through reconnect and SDK disconnect. The static routes above explain the expected result; these observations can falsify the contract and must not be presented as universal proof from inactivity.

Within that restricted call surface, the identified automatic game producer is local command 1; no concrete default per-game remote-reconciliation route was found. For general API-save support, the concrete remaining ownership issue is successful **FileShare's individual Cloud commit**, plus any other explicitly requested API upload features. Their handling needs an explicit product/transport decision. It is not necessary to invent a nonexistent global “local-only” switch to run the narrower AutoCloud proof, and this static follow-up alone does not activate normal launch transfers.
