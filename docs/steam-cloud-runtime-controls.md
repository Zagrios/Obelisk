# Pinned Steam runtime: static Cloud control evidence

Research date: 2026-09-19. This note records static analysis of Forge's bundled runtime. It does not establish authenticated Cloud behavior, cache coherence with CM transfers, or Dynamic Cloud Sync support. No runtime was executed for this sub-investigation; the separate offline probe has its own report.

## Binary and method

Input: `build/steam-cloud-feasibility/steamclient.dylib`, universal Mach-O, SHA-256 `035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840`, bundled version `1788652215`. The ARM64 slice starts at fat-file offset `0x1980000` and has length `0x184ffa0`. All addresses below are ARM64 image-relative virtual addresses, **not offsets in the universal file**. Runtime addresses require the image slide.

Evidence comes from Mach-O sections, `LC_FUNCTION_STARTS`, constructors, vtable pointers, ARM64 argument movement, IPC serialization widths, reply decoding, public adapter calls, and enum-to-name tables. Method strings alone were not used to establish callable signatures. Symbols printed by objdump often name the nearest surviving export; those labels do not identify the stripped function.

For reproduction, extract the ARM64 slice with `lipo -thin arm64`, then use `xcrun llvm-objdump -d --start-address=0xADDRESS --stop-address=0xEND THIN_BINARY`. Omit `--macho`: in the installed tool it bypasses these range limits. Temporary disassembly and parsed function starts are under `/var/folders/bj/bjxppt2d0jg9ppj9_bkdsz580000gn/T/forge-cloud-static-w0ewwrzw/`.

## Accessor established by two independent call paths

`CLIENTENGINE_INTERFACE_VERSION005` vtable `0x1753a20`, slot **24**, points to `0xa03cac`. It accepts `this, user32, pipe32` and returns a nullable pointer. At `0xa03cec` it stores the two handles in that order as the lookup key; at `0xa03d34` it loads the matched record's pointer at `+0x68`. Record constructor `0xab53a4` calls proxy constructor `0x88ff5c` at `0xab5468` and stores its result at `+0x68`. The proxy constructor installs vtable **`0x1744900`** at `0x88ffbc`.

Independently, registration function `0x7e8cc8` associates `STEAMREMOTESTORAGE_INTERFACE_VERSION016` with constructor `0x7e1c50`. That constructor calls engine vtable `+0xc0` (slot 24), places the returned proxy at public-object `+8`, and installs public vtable `0x1738a28`. The public `SteamClient020` accessor is slot 17, function `0x7c9918`; it forwards the requested version and handles to the shared factory.

This establishes the private object and slot numbering for this exact image. It is not a version-independent interface contract. Hash, architecture, vtable, and function-address guards remain necessary before each probe.

## File ABI established for an offline probe

Every signature below has an explicit first `this` pointer. `i32` describes the 32-bit C/C++ ABI; enum and signed-size interpretation is distinguished from byte width where relevant. `CStr` is a NUL-terminated input. Returned strings are borrowed; their exact lifetime is unproven, so copy immediately. Pass valid storage for both output pointers in slot 19.

| Private slot | Function | Image address | ABI after `this` → result |
|---|---|---|---|
| 0 | `FileWrite` | `0x8fb410` | `(app:u32, root:i32, name:CStr, data:*const u8, size:i32) → i32 result code` |
| 1 | `GetFileSize` | `0x8fb5f8` | `(app:u32, root:i32, name:CStr) → i32` |
| 5 | `FileRead` | `0x8fbd48` | `(app:u32, root:i32, name:CStr, out:*mut u8, capacity:i32) → i32` |
| 18 | `GetFileCount` | `0x8fd370` | `(app:u32, option:byte) → i32` |
| 19 | `GetFileNameAndSize` | `0x8fd514` | `(app:u32, index:i32, out32a:*mut i32, out_size:*mut i32, option:byte) → C-string pointer` |
| 20 | `GetQuota` | `0x8fd710` | `(app:u32, out_total:*mut u64, out_available:*mut u64) → bool` |
| 67 | `ResolvePath` | `0x90278c` | `(app:u32, root:i32, name:CStr, out:*mut u8, capacity:i32) → bool` |

Evidence that prevents easy ABI mistakes:

- Private `FileWrite` returns **four bytes**, decoded at `0x8fb55c`, loaded at `0x8fb574`; its IPC failure fallback is 55 at `0x8fb5a8`. The public adapter at `0x7e22d0` calls private slot 0 and returns true only when that code equals **1** (`0x7e231c`). Reading it as a Rust/C++ boolean would discard useful failures and can produce invalid Boolean values.
- Public `FileWrite` and `FileRead` adapters derive the application argument from the current game ID and pass **root 0** at `0x7e2314` / `0x7e2368`. They move public name/data/length into private arguments 3/4/5. This establishes application-before-root ordering and the default SDK root.
- Private `FileRead` decodes a four-byte return followed by the caller-sized output buffer (`0x8fbe84`–`0x8fbe9c`). The public adapter tail-calls it without conversion, establishing the public SDK byte-count result, rather than a result code. `ResolvePath` instead decodes one-byte success followed by the caller-sized output (`0x9028c8`–`0x9028e0`); it does not return a path pointer.
- `GetFileCount` serializes its second argument as **one byte** (`0x8fd404`–`0x8fd414`), not as a root enum. Public adapter `0x7e2784` passes 1.
- Public `GetFileNameAndSize` adapter `0x7e27bc` passes a temporary first output pointer, the public size pointer as the second output, and final option 1. Private decoding writes four bytes to each pointer at `0x8fd658` and `0x8fd66c`. The first output's interpretation as root is strongly consistent with the private signature and root-aware enumeration, but its semantic use is not independently demonstrated by this adapter, which discards it. Keep it as a raw 32-bit value in initial probe output.
- `GetQuota` decodes one-byte Boolean followed by eight bytes into each output (`0x8fd810`, `0x8fd824`, `0x8fd838`). Public adapter `0x7e2818` preserves total-before-available order. When it returns false, do not interpret caller-initialized zero outputs as a valid zero quota; the server's failure path does not fill them.

Valve's [public `ISteamRemoteStorage` documentation](https://partner.steamgames.com/doc/api/ISteamRemoteStorage) defines the public filename/data/size behavior. It does not document these private extra arguments. In particular, a local write or successful local enumeration does not establish a completed remote upload.

### Root constants: independently established from the binary

Enum-to-name function `0x10d4d60` identifies `ERemoteStorageFileRoot`, compares the input against 22 entries at **`0x17a5708`**, stride 16, and returns the pointer at entry `+8`. The enum occupies the low 32 bits at entry `+0`.

| Value | Name | Value | Name |
|---|---|---|---|
| -1 | Invalid | 10 | WinProgramData |
| 0 | Default | 11 | SteamCloudDocuments |
| 1 | GameInstall | 12 | WinAppDataLocalLow |
| 2 | WinMyDocuments | 13 | MacCaches |
| 3 | WinAppDataLocal | 14 | LinuxHome |
| 4 | WinAppDataRoaming | 15 | LinuxXdgDataHome |
| 5 | SteamUserBaseStorage | 16 | LinuxXdgConfigHome |
| 6 | MacHome | 17 | AndroidSteamPackageRoot |
| 7 | MacAppSupport | 18 | WindowsHome |
| 8 | MacDocuments | 19 | AndroidExternalData |
| 9 | WinSavedGames | 20 | AndroidInternalData |

These are enum identities, not proof of their resolved filesystem locations. In particular, the exact default userdata root, the influence of HOME/runtime configuration, app-install roots, and root overrides require the isolated `ResolvePath` observations. Do not equate the name `MacHome` with a guaranteed process-HOME override without observing it.

## Cache, sync, and Dynamic-related control widths

These signatures establish IPC shape, not safe operational policy. Scalar meanings left unnamed below have not been recovered. No unverified flags should be passed to a live synchronization operation.

| Slot | Method | Address | ABI after `this` → result |
|---|---|---|---|
| 26 | `IsCloudSyncOnSuspendAvailableForApp` | `0x8fe0e4` | `(app:u32) → bool` |
| 27 | `IsCloudSyncOnSuspendEnabledForApp` | `0x8fe268` | `(app:u32) → bool` |
| 28 | `SetCloudSyncOnSuspendEnabledForApp` | `0x8fe3ec` | `(app:u32, enabled:byte) → void` |
| 70 | `LoadLocalFileInfoCache` | `0x902cb4` | `(app:u32) → void` |
| 71 | `EvaluateRemoteStorageSyncState` | `0x902e14` | `(app:u32, option:byte) → void` |
| 72 | `GetLastKnownSyncState` | `0x902f94` | `(app:u32) → i32` |
| 73 | `GetRemoteStorageSyncState` | `0x903118` | `(app:u32) → i32` |
| 77 | `ResolveSyncConflict` | `0x903798` | `(app:u32, choice:byte) → bool` |
| 78 | `SynchronizeApp` | `0x90393c` | `(app:u32, commands:u32, argument:u64) → bool` |
| 79 | `IsAppSyncInProgress` | `0x903af8` | `(app:u32) → bool` |
| 80 | `RunAutoCloudOnAppLaunch` | `0x903c7c` | `(app:u32) → void` |
| 81 | `RunAutoCloudOnAppExit` | `0x903ddc` | `(app:u32) → void` |
| 84 | `CancelCurrentAndPendingOperations` | `0x904220` | `() → void` |
| 85 | `GetLocalFileChangeCount` | `0x904368` | `(app:u32) → i32` |
| 86 | `GetLocalFileChange` | `0x9044ec` | `(app:u32, index:i32, out32a:*, out32b:*) → C-string pointer` |
| 87 | `BeginFileWriteBatch` | `0x9046d0` | `(app:u32) → bool` |
| 88 | `EndFileWriteBatch` | `0x904854` | `(app:u32) → bool` |

For `SynchronizeApp`, parameter stores at `0x9039c0`, `0x9039d4`, and `0x9039e8` serialize 4/4/8 bytes. The return is one-byte Boolean, normalized at `0x903ab8`. This does not identify its command flags, third-argument meaning, or completion semantics. `CancelCurrentAndPendingOperations` has **no application parameter**; its scope cannot be assumed to be one game. `LoadLocalFileInfoCache` has only an IPC acknowledgement, with no returned success or revision: the name does not prove cache invalidation/reload after external changes.

State labels are better established: enum-to-name function `0xac4a0c` identifies `ERemoteStorageSyncState` and scans nine entries at `0x17626a0`, stride 16: `0 disabled`, `1 unknown`, `2 synchronized`, `3 inprogress`, `4 changesincloud`, `5 changeslocally`, `6 changesincloudandlocally`, `7 conflictingchanges`, `8 notinitialized`. Their names alone do not specify transitions, durability, or whether an old cached state can be returned.

## Inhibition and callbacks remain incomplete

The `m_setAppCloudSyncInhibit` string at `0x15b4210` is referenced by diagnostic member traversal `0xac66fc`; it labels object member `+0x228`. It is not a setter. Function `0xace9c8` examines that collection, and its command-bit-8 branch logs inhibition for a pending platform change (`0xaceb84`) before inserting an application into the collection (`0xaceb90`–`0xaceb9c`). A command-bit-9 path removes an entry (`0xacead4`–`0xaceaf8`). This is evidence of a platform-change mechanism, **not a supported general Cloud ownership switch**. Do not write to object member offsets or appropriate these flags for Forge orchestration.

Private slots 24/25 are `IsCloudEnabledForApp` / `SetCloudEnabledForApp`, and 69 is `SetCloudEnabledForAccount`. These are preferences, not an established independent synchronizer-inhibition control.

RTTI establishes `RemoteStorageAppSyncStatusCheck_t`, `RemoteStorageAppSyncedClient_t`, `RemoteStorageAppSyncedServer_t`, and `RemoteStorageAppSyncProgress_t`, including consumers in suspend/resume jobs. The later control probe below establishes IDs and bounded layouts for ordinary sync completion. Progress, successful remote completion, conflict control and a launcher-side Dynamic notification route remain unproven.

The useful next proof is bounded: obtain the exact guarded proxy, query roots/states, perform a disposable offline write/read/list under the default root, then test the cache behavior with controlled files and a fresh runtime. Remote success, external CM/cache handoff, duplicate-sync suppression, Auto-Cloud lifecycle, and suspend/resume callbacks need separate evidence before they can support Forge's production coordinator.

## Initial disconnected fixture, before supplying metadata and a test identity

This section preserves the initial diagnosis. The later initialized fixture and cache handoff results are recorded in the follow-up below.

A bounded follow-up inspected the server side after the separate probe reported successful path resolution, `GetQuota=false`, write result 25, and a zero-byte private RemoteStorage read of a Forge-created file. The final direct `ClientUser::steam_id()` measurement reports **account ID 0, account type 1, instance 1, universe 1, a nonzero full SteamID, and `logged_on=false`**. The resolved `userdata/0` directory does not prove that the full SteamID or its account type is zero; the earlier inference to that effect was incorrect and is withdrawn. Relevant server vtable candidate `0x1762760` contains `FileWrite` at slot 0 (`0xac7e3c`), `FileRead` at slot 5 (`0xac91d8`), `GetQuota` at slot 20 (`0xacee88`), and `LoadLocalFileInfoCache` at slot 70 (`0xacf274`). Function bodies and their diagnostic strings corroborate those identities.

In both fresh probe processes, the test explicitly queued `LoadLocalFileInfoCache` and pumped native callbacks for two seconds, observing seven callbacks each time. Direct filesystem reads returned the matching 39-byte fixture; RemoteStorage file count and read result both remained zero. `GetQuota` returned false, so its output values are reported as unavailable; sync state was 0. These are bounded observations, not proof that the queued cache operation completed or that any of the seven callbacks represented its completion.

**The cache loader queues work; it does not synchronously reload the directory.** `0xacf274` inserts command 1 for the application, or ORs bit 0 into an existing queued command, then calls runner `0xace9c8` at `0xacf2fc`. Before scheduling work, that runner reads the owner pointer at remote-storage object `+0x190`, then reads an identity value at owner `+0x27a`. It checks that value's account-type bits: type 10 returns at `0xace9f4`; any type other than 1 returns at `0xacea58`, after an optional invalid-user-type diagnostic. Enum reflection function `0x10d3470`, table `0x17a28c8`, identifies 0 as Invalid, 1 as Individual, and 10 as Anonymous User. **This type check does not explain the observed run**, whose public getter reports Individual. Equivalence between that getter's value and this internal owner field has not yet been established. Nor does this gate establish that an active network session is required. Completion of the queued work and any later job prerequisites remain unobserved.

**A file on disk is insufficient for this read path.** Server `FileRead` first queries the application's in-memory file record through `0xac7088` (`0xac9268`), then resolves the path. A missing record branches at `0xac92a8` to the unknown-file path `0xac9340` and returns zero, before disk opening at `0xac9418`. `0xac7088` searches an application map and then its file map; it does not scan the directory. The zero read therefore cannot distinguish missing cache initialization from an incompatible persisted cache format. Whether a valid initialized session discovers unindexed files, requires `remotecache.vdf`, or consumes Forge-projected metadata remains unproven.

**Write result 25 has a concrete quota path.** `FileWrite` calls internal function `0xac7558`. Its available-byte variable starts at zero (`0xac7828`); it invokes quota helper `0xac8088` at `0xac784c`, then compares available bytes with the requested size at `0xac7854`. Insufficient bytes lead to result 25 at `0xac7a8c`; exceeding the file-count limit shares that result. Metadata helper `0xacf17c` reads `quota` and `maxnumfiles` with zero defaults, optionally applies `limiteduserquota`, and returns false when quota is zero (`0xacf254`). The quota caller returns early on false without filling outputs. These instructions explain how absent/zero quota metadata can reject a small fixture before writing; they do not establish why that particular runtime instance lacked usable metadata or the quota of an initialized account.

The next discriminating fixture needs known nonzero app quota metadata, observed completion of queued cache loading, and controlled cache/file contents. A process restart plus `logged_on=false` does not isolate which missing initialization or cache metadata matters. The current evidence establishes quota and file-index prerequisites; the specific reason the fixture's file was not indexed remains unknown. No identity forging, live authentication, real save access, or further runtime execution was performed in this static follow-up.


## Follow-up: initialized local storage and cache handoff

The [offline probe report](evidence/steam-cloud-runtime-2026-09-19/README.md) supersedes the initial fixture's unexplained prerequisites. Synthetic appinfo v40 supplies UFS quota 1,048,576 and maxnumfiles 16. The native metadata loader accepts it, `GetQuota` succeeds and private `FileWrite` returns 1. These are fixture values, not the real configuration of App 480. The appinfo layout was cross-checked with [SteamAppInfo's format description](https://github.com/ValveResourceFormat/SteamAppInfo/blob/master/README.md) and the pinned runtime loader.

A later identity check in job `0x615574` rejects Individual AccountID zero at `0x615698` and requires instance 1 at `0x61569c`. The [independent bootstrap investigation](steam-cloud-gamenative-bootstrap.md#the-account-id-zero-cache-failure-has-a-separate-guard) records the branch and log path. Calling the existing verified `ClientUser::log_on` with disposable identity `0x0110000100000001`, no credential, and external networking denied returns 5 and remains disconnected, but supplies the nonzero account component needed by this local job. `Init cache request complete` then appears. This test value must never replace an authenticated identity in production.

Ordering also matters. The probe initializes the file cache, writes the fixture, then pumps callbacks until the native `remotecache.vdf` contains its record, with a five-second deadline. A new process loads that record and reads the exact original bytes. This proves process-restart persistence, not power-loss durability or remote upload.

Rust can also create the known fixture and its index before initialization. The index uses the exact fields emitted by this macOS runtime, including root 0, syncstate 3 and the fixture SHA-1. Private enumeration and readback pass immediately and after restart. A bare file with identical bytes but no record remains invisible despite a completed initialization job.

The same technique does **not** provide a live reload. After an initial successful read, the probe renames both the fixture and its record, then queues `LoadLocalFileInfoCache` again. The runtime validates its old filename, logs that it is missing, removes that record and deletes the now-empty on-disk cache. The new filename returns zero bytes, including on restart because the replacement index was deleted. The renamed file itself survives with the original hash. Thus this method cannot be used as the Dynamic cache-update contract. Completion of an init job is not proof that it re-imported an externally changed index.

The next adapter work must establish a controlled update/invalidation route, authenticated native metadata readiness, and synchronization ownership. Android's bootstrap and the passing offline fixture do not establish those contracts.


## Follow-up: coherent live mutations and explicit synchronization

The [live control observations](evidence/steam-cloud-runtime-2026-09-19/live-controls-observations.json) record a second passing local route. Private `FileWrite` and `FileDelete` update the native index directly. Public RemoteStorage016 reads the changed contents in the same process; public writes are immediately readable through the private interface. Creation, a size-changing replacement from 39 to 56 bytes, deletion, enumeration and process-restart readback pass. No external `remotecache.vdf` write or reload is used in this route.

The final native index retains the deleted fixture with `persiststate=2` and the surviving fixture with `persiststate=0`, `syncstate=3`, size 56 and its matching SHA-1. Those are observed local states. These operations are not a way to mark downloaded remote data as already synchronized. The fixture's local-change count is zero; a real Dynamic game, download notification and state reload remain untested. Public interface calls in the host process also do not replace a separate game's `SteamAPI_Init` and IPC test.

### Public API app context

An initial public read returned zero while the private AppID-480 read succeeded. The fixture's native pipe had AppID **0**, independently observed through Utils. Unlike private storage calls, public calls derive their AppID from native context.

- Engine005 slot 14 at `0xa04d74` takes `(this, pipe:i32)` and returns the pipe's Utils object. The public interface factory at `0x7c5fa0` calls that accessor, then Utils slot 19. It copies a valid returned game identity into the public API's global context at `0x1841d28`.
- Utils slot 19, `0x8cc76c`, takes only `this` and decodes a four-byte AppID. Slot 18, `0x8cc5c8`, serializes `(app:u32, option:byte)` and decodes four bytes. The server dispatch at `0x8282dc` names the latter `SetAppIDForCurrentPipe`; at `0x8283b8` it calls vtable offset `0x90`, slot 18. GetAppID's dispatch name is at `0x82703c`.
- Before creating the public storage interface, the disposable probe sets AppID 480 with option 0 and re-queries it. Both the setter return and subsequent getter are 480; before was 0. The option's wider production meaning is not established. No private field is patched. This test setup does not authenticate a user or establish ownership.
- Private FileDelete slot 7 at `0x8fc0f0` takes `(this, app:u32, root:i32, name:CStr) -> bool`. It serializes two four-byte scalars and a string, then decodes a one-byte result at `0x8fc204`–`0x8fc224`. Public FileDelete slot 6 at `0x7e24ec` forwards AppID, root 0 and filename to it.

### Request directions and completion callback layouts

Server `SynchronizeApp` at `0xacfb48` distinguishes the second scalar as a direction mask: 1 selects the client/download route and 2 the server/upload route. At `0xacfce4`–`0xacfd08`, these become queued command bits 3 and 2, respectively. The third argument supplies auxiliary options; zero selects none of those bits. The exact names and supported combinations of nonzero options remain unproven and are not used.

The direction's meaning has two independent checks. Failure emitters at `0xacfde4` and `0xacfe54` identify the server and client callbacks. Offline runs of direction 1 and 2 log `Starting sync (down,)` and `Starting sync (up,)`, respectively.

| Callback | ID | Observed/established payload |
| --- | --- | --- |
| `RemoteStorageAppSyncedClient_t` | 1250001 | 12 bytes: AppID, result word, a final raw u32. |
| `RemoteStorageAppSyncedServer_t` | 1250002 | 12 bytes: AppID, result word, a final raw u32. |
| `RemoteStorageAppSyncStatusCheck_t` | 1250005 | 8 bytes, statically established; not exercised here. |
| `RemoteStorageConflictResolution_t` | 1250006 | 8 bytes, statically established; not exercised here. |

RTTI name addresses `0x1424d38`, `0x147669c`, `0x1424d14` and `0x1424cf0` are paired with those IDs by registration helpers `0x61d210`, `0x61d24c`, `0x61d1d4` and `0x61d198`. Emitters pass sizes 12 or 8 explicitly. The cancellation emitter at `0xad4c8c` supplies the matching shapes. The probe copies only the whitelisted ID/size pairs before freeing a callback; other events retain metadata only.

Both offline synchronization requests return accepted=true, followed by the appropriate callback `[480, 21, 0]`. Native logs report `login=false`. `IsAppSyncInProgress` is already false at the first sample, before the completion callback is consumed. Thus neither admission nor an idle state is a completion or upload acknowledgement. The fixture requires a matching callback and bounded completion, and rejects a reported success result in this disconnected case.

The cancellation call returns, but the observed outcome is again `[480, 21, 0]`. The disconnected job may already have failed before cancellation. This does **not** establish that cancellation interrupts active transfers. Cancellation has no AppID argument and affects the runtime's queued/current operations; its scope must not be advertised as per-game. Callback identity is only account/runtime plus app and direction here, not a unique request ID. A production adapter needs serialized operations and stale-event handling.

`RunAutoCloudOnAppLaunch` and `RunAutoCloudOnAppExit` server methods at `0xacfa38` and `0xacfac0` queue flags `0x20` and `0x40`. Their bodies do not call the scheduler directly. They cannot be treated as synchronous completed transfers. The fixture does not call them or establish Auto-Cloud file selection.

The new evidence makes a Forge-driven Valve transfer adapter plausible, while Rust retains policy and lifecycle ownership. Before selecting it, prove authenticated results, remote content readback, conflict interception, active cancellation, autonomous-transfer behavior and the Dynamic download notification. The Rust transfer route still needs a way to keep native and remote baselines consistent and prevent competing transfers.
