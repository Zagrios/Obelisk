# Native Steam macOS host feasibility

Investigation date: 2026-09-16. Scope: native macOS Steam engine, ARM64, without
running the complete Steam client. This is a historical research report. The
prototype code has been removed; the maintained implementation and validation
workflow are documented in [native Steam launches](steam-native-launch.md).
The original reports are retained in the
[research evidence archive](evidence/steam-native-research/README.md).
Primary-source research and SDK discovery analysis:
[source notes](research-steam-native-sources.md).

## Result

### Later Forge integration

The Rust host is now integrated into Forge's Launch button and has been validated
with the in-memory session and Silksong's actual Steam online subsystem. See
[native Steam launches](steam-native-launch.md) for the current implementation,
evidence and limits. The migration and original C++ sections below preserve their
historical validation status.

### Historical Rust migration status

The experiment was ported entirely to Rust, including the host,
private ABI calls, discovery adapter/responder, Steamworks and AppKit probes,
credential transport, sandbox runner, inventory and static analysis. At that
stage, the production Forge workspace and launcher were not changed. The later
integration reused the native ABI and Mach modules before removing the standalone
research tools.

Migration validation covered native ARM64 loading, global-user creation, callbacks,
separate-process pipe creation, actual Mach discovery with the Rust dylib, the
expected offline SDK failures and both AppKit sandbox controls. The Rust analyzer
reproduced the historical 259-slot ABI map exactly. Authenticated Rust Steamworks
and the game smoke test were not validated at that stage: the corrected
noninteractive Keychain read returned -25293, before starting an authenticated
engine. The later Forge integration passed its existing in-memory session,
without another Keychain request, and validated authentication and the game.

The first migration credential check used only `kSecUseAuthenticationUIFail`; the
legacy login Keychain still permitted interaction. The corrected research helper
also called `SecKeychainSetUserInteractionAllowed(false)`, offered no interactive
fallback and never changed the item's ACL. That helper has since been removed.
Credential-read success during that first check is not proof of a native Steam
login.

See [Rust migration evidence](evidence/steam-native-research/rust-port-validation.json).
All authenticated and game observations below describe the **original C++ tests**;
their retained evidence must not be attributed to the Rust binaries.

### Original feasibility result

**The isolated native ARM64 prototype works, including a normal Steamworks consumer.**
A Forge-controlled ARM64 executable loads the genuine Valve libraries, authenticates
the existing Forge account with its refresh token, receives Steam's connection
callback, and verifies the expected identity. The engine confirms ownership of
Silksong and obtains a 178-byte ownership ticket. A second process then attaches
to that authenticated engine user over IPC. The unmodified Silksong Steamworks
library initializes, confirms the account and license, and completes a real
asynchronous player-count request. Stopping the host makes that consumer detect
Steam's disappearance. Silksong itself subsequently reaches its main menu.

macOS discovery uses a narrow DYLD adapter for one bootstrap-service lookup and a
Forge-owned Mach responder announcing the real host PID/path. Authentication,
ownership and Steamworks calls still run in Valve's original binaries. This proves
the mechanism for this build and machine; it is not a production integration or
a claim that every hardened game accepts an injected discovery adapter.

## Repository state and preservation

The checkout was on unborn `main`, with no commits and all project files untracked.
There was no applicable `AGENTS.md` in the repository or its checked ancestors.
The initial inventory hashed 166 existing source/configuration/documentation files
into `build/steam-native-research/worktree-before.json`.

Existing code confirms that QR/credential authentication, CM logon, owned-library
retrieval, depot downloads, native launch plans and friend observation are already
implemented. This investigation did not revalidate all of those services live.
Relevant files are `crates/storefronts/steam-kit/src/{auth,cm,client,friends}.rs`,
`crates/forge-bridge/src/steam.rs`, and `apps/macos/Forge/Models/SteamModel.swift`.

All edits from this investigation are new experiment files and two new research
documents. Other work changed production files concurrently, including launcher
and test files; the snapshot is not a claim that the checkout remained unchanged.
Those files were not overwritten. The current launcher still preserves the
validated bundle/Mach-O check and adds only owner execution with `mode | 0o100`.
Silksong's earlier successful opening is the user's observation. Its earlier
Steamworks restart behavior remains unproven. The new, separately traced game
smoke test is described below and does not rewrite that earlier observation.

## Binary inventory

Reference installation:
`~/Library/Application Support/Steam/Steam.AppBundle/Steam/Contents/MacOS/`.
The local package manifest reports version `1788652215`; bundle version is `6.1`.
Mach-O compatibility/current dylib versions generally remain `1.0.0`, which does
not identify the Steam client release. Hashes and Mach-O UUID/load commands are
recorded by the inventory command. The client signature metadata names Valve
Corporation, team `MXGJJ98X76`, with a 2026-09-03 signing timestamp.

`steamclient.dylib` SHA-256:
`035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840`.
Every private ABI offset below is specific to this hash and its ARM64 slice.

The initial three-library hypothesis was incomplete. The tested loader closure is:

| Component | Architectures observed | Role in this experiment |
| --- | --- | --- |
| `steamclient.dylib` | ARM64, x86_64 | Engine and exported factory/lifecycle calls |
| `libtier0_s.dylib` | ARM64, x86_64 | Direct load dependency |
| `libvstdlib_s.dylib` | ARM64, x86_64 | Direct load dependency |
| `libaudio.dylib` | ARM64, x86_64 | Direct load dependency |
| `crashhandler.dylib` | ARM64, x86_64 | Transitive dependency through tier0 |
| `Breakpad.framework/.../Breakpad` | ARM64, x86_64, i386 | Transitive dependency through crashhandler |
| `Breakpad.framework/.../breakpadUtilities.dylib` | ARM64, x86_64, i386 | Framework dependency |

macOS system frameworks and libraries satisfy the remaining static dependencies.
The global-user path also tries `libSDL3.dylib` and `libvideo.dylib`; missing-load
assertions are logged, but the tested user and callback operations still return.
They are not evidence that those libraries are optional for a working game runtime.
Their dependency closure is inventoried separately, including FFmpeg and Vorbis
libraries; every inspected member has an ARM64 slice.

`steamservice.dylib`, `ipcserver` and `steam_osx` are universal too. They were
inventoried as possible service/reference components, not executed by the probe.
The helper copies the complete local Breakpad framework to retain its layout;
its extra executable resources are not launched. Per-run file hashes record this
larger copied set. No Steam UI, account config, license cache, game data or saves
are copied.

Signature metadata and successful verification are separate observations.
`codesign --verify --strict --arch arm64` succeeds for the inspected client slice.
The all-architecture check reports an internal error for that universal file.
Other all-architecture/bundle checks also report failures, including unbound or
missing resources and codec signing differences. The evidence retains both
verification outputs; the mere presence of a Valve authority string does not
establish a valid, distributable runtime package.

## Experiments and observations

Each run records PID/PPID, `ps` executable paths before/during/after, loaded images,
exit status and timeout. The helper is compiled with `-arch arm64` and reports
`sysctl.proc_translated=0`. Native process execution is restricted to the experiment
executables and, for the game smoke test only, the declared Silksong executable.
No `steam_osx` or Steam web helper appeared in the recorded process snapshots.
The existing `ipcserver`, PID 3550, predated the investigation
and remained untouched. Process sampling is not a complete kernel exec trace;
the sandbox provides the additional execution restriction.

| Test | Observed result | What it proves |
| --- | --- | --- |
| Five libraries, missing Breakpad | `dlopen` fails with the exact missing framework path | Three named dylibs are not the load closure |
| Complete loader closure | `dlopen` succeeds | ARM64 loadability only |
| Factories | `SteamClient023`, `SteamClient020`, `CLIENTENGINE_INTERFACE_VERSION005`, return code 0 | Real interface pointers from this build |
| Pipe without host | `Steam_CreateSteamPipe()` returns 0 | Negative control for the standalone consumer |
| Global user, all sockets denied | Native assertion then crash | Confinement blocked required local IPC; not an engine feasibility verdict |
| Global user, invalid IPC variable values | Invalid name/address assertion then crash | Arbitrary labels are not valid endpoint values |
| Global user with loopback endpoints | `user=1`, `pipe=1`, connected/logged-on false | Engine creates an unauthenticated local user |
| Host event loop | Multiple internal callback IDs and payload sizes, then clean observation completion | Genuine engine callbacks are retrieved and released |
| Second library consumer | Pipe 1, global-user attachment 0, pipe release true | Communication pipe works; authenticated user attachment does not |
| Initial unmodified SDK tests, before the adapter | `SteamAPI_InitFlat=1`, cannot determine client install directory | TCP endpoints alone do not provide macOS discovery |
| Initial Forge credential helper | 8/15-second timeout before user finished the macOS prompt | The short deadline interrupted approval; not evidence of inaccessible credentials |
| Interactive credential helper, 180-second deadline | Exit 0, expected item fields received over private pipe | Existing Forge credentials can be handed to the isolated process |
| Native token logon | Callback 101, state 4, connected/logged-on true, expected identity matches | Real native account authentication succeeds |
| Immediate authenticated consumer | User attachment 0 before app data is ready | Authentication alone is insufficient for this App ID |
| Ownership request | Request accepted, subscribed true, ticket length 178 | Engine obtained the account's real Silksong ownership data |
| Consumer after ownership readiness | User 1, pipe 1, connected/logged-on true | Authenticated engine works across separate ARM64 processes |
| SDK with discovery adapter | `InitFlat=0`, isolated engine loaded, identity/license/App ID match | Unmodified Steamworks API attaches to the real host |
| Read-only SDK service request | Player-count asynchronous result received, success true | Genuine Steamworks request/response works through the host |
| Host-loss control | Host terminated, SDK detects absence and exits 0 | The successful consumer depended on the experimental host |
| Initial game sandbox | AppKit aborts before Steam initialization | Headless restrictions cannot be copied unchanged to a GUI bundle |
| Corrected game sandbox | Silksong main menu visually verified; two discovery handshakes and path requests | The real game opens under the isolated host arrangement |

Final observations and PID records are in the
[historical evidence archive](evidence/steam-native-research/README.md).
The callback loop logs callback ID and size once per distinct pair, plus the total
count, never payload bytes. Authenticated runs include callback 101,
`SteamServersConnected`. Other internal callbacks do not demonstrate game
achievements or cloud synchronization.

## Startup, IPC and private ABI

The exported `CreateInterface` returns an engine vtable at image offset `0x1753a20`.
Export-wrapper disassembly establishes pipe creation, global-user creation,
release and internal user retrieval without borrowing historical vtable layouts.
Engine slot 8, offset `0xa02be8`, takes user and pipe handles and returns the user
proxy. Its observed vtable is `0x173fe80`.

The engine consults `steam_master_ipc_name_override`, falling back to the name
`Steam3Master`. The process-pipe implementation resolves endpoint names using the
environment. Supplying `Steam3Master=127.0.0.1:<unused-port>` allowed global-user
creation. The runner also supplies a separate `SteamClientService` endpoint as an
explicit experiment setting; that alone is not proof a client-service host exists.
Ports are chosen per run. The released-port allocation has a small race and is
research code, not production port reservation.

`Steam_BGetCallback(pipe, CallbackMsg_t*)` and `Steam_FreeLastCallback(pipe)` drive
the host observation loop. The native implementation starts its own threads;
the prototype does not claim to have mapped every engine-frame responsibility.
Normal game `SteamAPI_RunCallbacks` is a separate responsibility and was not
reached because initialization failed.

The unmodified game API has a separate macOS discovery path through the Mach
service `com.valvesoftware.steam.ipctool`. Static analysis found PID/path discovery
before `dlopen`; normal `InitFlat` does not take the anonymous path that falls back
to a bare library name. See the source notes for offsets and the API binary hash.
The prototype denies lookup of the existing service to avoid attributing an
installed Steam instance to Forge. An adapter loaded only into the controlled
consumer redirects that exact service lookup to
`com.zagrios.forge.discovery.<launcher_pid>`. The launcher registers and removes
only its own service. It verifies the host's PID and canonical executable path
before returning them. All other bootstrap lookups retain their normal behavior.

The responder implements protocol version 104 and the PID/path request. Mach
messages are 32 bytes for the version reply and 544 bytes for the path reply;
the SDK's 40/552-byte receive buffers also accommodate the trailer. An initial
implementation incorrectly treated receive capacity as message length; the real
SDK exposed that error even though the first diagnostic's larger buffer accepted
it. Corrected exact-size diagnostics and the real SDK both pass.

This is a discovery adapter, not a Steamworks implementation. Valve's SDK is
copied byte-for-byte, loads the isolated `steamclient.dylib`, obtains user/pipe 1,
checks the expected Steam ID and subscription, and handles the request itself.
The consumer uses the documented read-only
[GetNumberOfCurrentPlayers](https://partner.steamgames.com/doc/api/ISteamUserStats#GetNumberOfCurrentPlayers)
and retrieves its asynchronous result through
[GetAPICallResult](https://partner.steamgames.com/doc/api/ISteamUtils#GetAPICallResult).
The 19:48 run returned success with 7,869 players, a time-specific observation.
The runner then deliberately terminates only the host. The SDK reports its loss
and exits 0; host exit -15 is the planned negative control, not a crash.

The adapter uses DYLD interposing and a deprecated bootstrap registration API.
Hardened runtime/library validation, other Steamworks versions, service cleanup
after forced termination and a production replacement need separate validation.

The SDK probe never invokes `SteamAPI_RestartAppIfNecessary`. The game's internal
calls to that function were not instrumented. Static analysis finds the
`steam://run` and `/usr/bin/open` route in the game API. No game binary was patched,
no success return was replaced, and no anonymous account was substituted for the
user. The discovery adapter reports the actual experimental host and leaves
license/authentication checks in the genuine engine.

## Authentication and Forge coexistence

Forge retains an account name, Steam ID and Steam-client refresh token in Keychain.
Its authentication request uses the SteamClient platform. The Rust CM logon sends
that refresh token in the protocol field named `access_token`, then validates the
Steam ID returned by the server. A web access token, app ticket and this refresh
token are not interchangeable. No token value was displayed or put in arguments.

For this exact native build, static proxy strings and server dispatch identify
`SetLoginToken` at user slot 56, `0x896610`, and `LogOn` at slot 1, `0x890f10`.
The latter serializes one 64-bit Steam ID, unlike the old header's boolean-plus-ID
signature. The `SetLoginToken` dispatcher at `0x7ff564` redacts its first argument
as `XXXX`. The CUser implementation reached through `0xa88f44` / `0xa88d5c` parses
that first argument as a token and treats the second as an account string. Thus
the candidate call order is `SetLoginToken(token, account)` followed by `LogOn(id)`.
Live calls now confirm this argument order and compatibility with Forge's current
refresh token. Server connection callback 101 and connected/logged-on state are
required in addition to the local identity comparison.

The original short Keychain timeout interrupted the user's password entry. The
user clarified that cause, and the original helper permitted macOS interaction for up to
180 seconds. It requests the exact Forge item and writes only to a private pipe.
It neither changes ACLs nor impersonates the signed Forge application. The Python
runner forwards a binary packet through stdin, never process arguments or
environment variables. No credentials are retained in experiment files.

Authenticated runs allow outbound network access and prohibit native file writes
except `/dev/null`. Native output is discarded unless it matches an allowlist of
the probe's numeric diagnostics. This also avoids logging newly issued tokens
unknown to the runner. Host, SDK probe and Python runner disable core dumps.
Callback payloads and ownership-ticket contents are never logged.

App attachment initially returned user 0 even after successful authentication.
The engine's `ConnectToGlobalUser` at `0xa014bc` calls its application-binding
check and can reject the requested App ID. Calling `BUpdateAppOwnershipTicket`
through user slot 69, offset `0x897944`, starts the genuine ownership request.
Slot 181, `0x8a2a10`, checks subscription; slot 103, `0x89b004`, reports ticket
length. Their proxies establish `(uint32, bool) -> bool`, `uint32 -> bool`, and
`uint32 -> uint32` signatures. After subscribed became true and ticket length
became 178, a separate consumer successfully attached with App ID 1030300.

Forge friends currently share the existing CM reader/session with downloads and
the library. A native-engine login adds another CM session. The source does not
configure an explicit distinct `client_instance_id`/`logon_id` in Forge's Logon
record. Collision, replacement, presence and reconnect behavior must be tested;
coexistence was not exercised: Forge was absent from the live authentication runs'
process snapshots. A local handle or Steam ID
parsed from a token would not prove server authentication. Require `BLoggedOn`,
the expected account identity, a successful callback/request and licensed app
access before allowing game launch.

## Silksong crash diagnosis and smoke test

The first `auth-game` run created game PID 34592, then AppKit aborted during
`_RegisterApplication`. The crash report and unified log explicitly identify
denied lookup of `com.apple.coreservices.launchservicesd` and the resulting failure
to obtain an application serial number. This was caused by the experimental
sandbox, before Unity/Steam initialization. It does not invalidate the earlier
successful Steamworks consumer test.

The original `verify_appkit_sandbox.py` reproduced the failure using a real minimal
GUI bundle. Its later Rust replacement used the `probe verify-appkit` command.
Both research tools have been removed; their
[control results](evidence/steam-native-research/appkit-registration.json) remain.
The restricted profile reaches SIGABRT, intercepted as exit 86 to avoid another
crash dialog. Removing only the LaunchServices lookup restriction produces
`appkit.registered=1`, exit 0. An initial command-line executable control did not
reproduce it; a GUI `.app` bundle is the necessary regression-test seam.

The corrected game profile permits application registration. It still denies
reading real home data except the installed game bundle and this run's files,
and permits file writes only in the run's private home/temp directories and
`/dev/null`. `HOME` and `CFFIXED_USER_HOME` point there. Host writes remain denied.
The game receives its real App ID, keeping the normal SDK restart decision in
the verified environment.

Run `run-20260916-195642-740892-auth-game` started host PID 36166, discovery launcher
36253 and Silksong PID 36256. Unity initialized and the game made two successful
version/path exchanges. The main menu was visually verified through computer use.
No gameplay was started. The runner ended the game after its 45-second observation
window, then stopped the host. The game's signal termination and wrapper exit 9
reflect this planned stop. The game's own log did not print an explicit SteamAPI
success value, so SDK return-value/identity proof belongs to the separate consumer
test. A later `lsof` attempt ran after the game ended and provides no loaded-image
evidence. No new Steam UI process appeared in the sampled process tree. The game
binary is universal ARM64/x86_64; its first crash report explicitly recorded
`translated=false`. The helper and SDK consumer independently enforce ARM64.

After the final code changes, run `run-20260916-195820-253790-auth-sdk` repeated the
complete SDK proof: host 36957, discovery launcher 37031, consumer 37034,
`InitFlat=0`, expected identity/license/App ID, successful asynchronous result,
isolated engine path, and host-loss detection. SDK exit 0, no timeout.

## Follow-up identified by the original investigation

This list records the original research conclusions. See
[current integration limits](steam-native-launch.md#current-limits) for the
maintained implementation's status.

1. Replace or harden the experimental discovery injection/registration, including
   compatibility with hardened games, SDK versions, cleanup and host recovery.
2. Preserve the verified credential handoff, native log filtering and ownership
   readiness gate. Separately check coexistence with Forge's CM/friends connection.
3. Build reproducible runtime installation/update/rollback from an appropriate
   distribution source. The local Steam installation remains a research reference.
4. Test actual gameplay and each Steam feature independently, with an explicit
   save/cloud strategy. The game and engine architectures remain separate questions.

No cloud, achievements, playtime, presence publishing, multiplayer or invitation
feature was added. Future cloud work must distinguish Auto-Cloud from Steam Remote
Storage and choose one synchronization owner, with conflict handling, backups,
offline behavior and crash recovery. Runtime installation and redistribution
also remain unresolved. Downloadability and technical loadability are not a
distribution permission; see the primary-source notes for the checked documents.
