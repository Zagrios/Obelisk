# Native Steam launches from Forge

Forge's existing Launch button starts a separate Rust supervisor, which starts
the Steam host before the native macOS game. It passes the session already held
by `SteamModel` through a one-time private stdin pipe. No interactive sign-in or additional Keychain query is involved. After Cloud reconciliation, Intel games can present the [Rosetta requirement](steam-rosetta-intel.md) if translation is unavailable.

The app bundles a minimal, hash-pinned Valve runtime archive. The host loads its
original libraries from a verified private cache prepared from that archive. It
requires authentication for the expected account, subscription to the selected
game and a nonempty ownership ticket. Forge then launches the game, which
initializes its own unmodified Steamworks library and requests its own interface
versions from Valve. Normal launch does not load the game's SDK in a preflight
consumer or require `SteamAPI_InitFlat`, a particular `SteamUser` version, or an
SDK library fingerprint.

The Intel `forge-steam-sdk-x86_64` helper only implements `exec-game`, which
selects the game's Intel slice without initializing Steamworks. The SDK
diagnostics, their Debug entry points and the ARM64 SDK helper were removed.
Normal launch reports host account/license
verification and `sdk_checked: false`; it makes no claim that game-side SDK
initialization, a player-count request, or native cache consumption succeeded.

The Mach discovery adapter redirects only `com.valvesoftware.steam.ipctool` to a
unique Forge-owned service. Host and game share the same private IPC endpoints.
The game's discovery environment is covered by the runtime tests.

## Validation of game-owned SDK initialization, September 21

Four new runtime regressions exercise the production readiness path with
disposable hosts. A fixture whose SDK library has unknown exports is admitted
and reported as unchecked. Other cases cover prepared storage surviving host
shutdown, invalid/missing/wrong-scope/unindexed/symlinked cache rejection, and
authentication or ownership failure. The unknown-SDK regression failed on the
former mandatory preflight and passed after its removal. Admission no longer
selects a helper for SDK verification.

`./scripts/test.sh` passed: 426 main-workspace Rust tests with six opt-in tests
ignored, 15 native-workspace tests, and XCTest reported 260 tests with ten skipped
and no failures. Formatting, strict Clippy, localization, the macOS build,
Apple signatures, helper architecture checks and the separately enabled isolated
bundled-runtime load passed. The local log is `build/native-launch-validation.log`.
These checks used disposable fixtures, not authenticated games or real saves.
The user subsequently confirmed that Thronefall launched successfully. Game-side
SDK storage consumption remains a separate live check.

### SDK diagnostic removal

The SDK verifier, authenticated storage probes, Swift/UniFFI entry points and
offline `sdk-pair` experiment were removed. No maintained source calls the game's
`SteamAPI_*` exports. The Intel helper retains its exec handshake; the obsolete
ARM64 SDK helper is removed from incremental builds and rejected by bundle checks.
AutoCloud reconciliation and native RemoteStorage cache preparation retain their
existing production paths.

`./scripts/test.sh` passed after this removal: 426 main-workspace Rust tests,
seven native-workspace tests, 260 XCTest tests with ten skipped, and the separately
enabled isolated runtime-loading test. Formatting, strict Clippy, localization,
the macOS build, regenerated Swift bindings and signature/architecture checks
passed. An initial run hit `Busy` in the partial-uninstall Cloud test; that test
then passed alone and in the complete rerun without changes to its locking code.
Logs: `build/sdk-diagnostic-removal-tests.log` and
`build/sdk-diagnostic-removal-validation.log`. No real game or authenticated Cloud
experiment was run for this removal.

## Ownership and lifetime

- `crates/steam-runtime` contains safe process supervision, cache preparation,
  bounded credential transport, cancellation and sanitized diagnostics.
- `crates/steam-native` is a separate workspace for the Rust executable, private
  ABI calls, Mach transport and discovery adapter. The main workspace retains
  `unsafe_code = "forbid"`.
- `crates/forge-bridge/src/steam_native.rs` exposes a supervised launch and a
  reconnectable game-session client. Swift cancellation explicitly cancels
  preparation; dropping the client only detaches the interface.
- `forge-steam-host supervise` runs outside the Valve host sandbox, in a separate
  process session. It owns the host's private pipe and the game child process.
  The `engine` child retains the existing sandbox and ABI boundary. Forge can
  exit or crash without closing the host's pipe.
- `SteamGameLauncher` prepares the native launch and returns the supervisor client.
  The shared, observable `SteamGameSessions` owns admission, snapshot observation,
  recovery and release. Its reservation spans preparation through teardown and
  account sign-out, and it restores an existing session when Forge starts.
  Authentication failure, cancellation before spawning or invalid storage preparation
  cleans up the host and scratch home.
- `scripts/build-steam-native.sh` builds and signs the ARM host, Intel exec helper,
  the universal adapter and the Intel Rosetta application in Forge's bundle.
  Only the Steam helpers have the entitlements needed to load Valve libraries and use
  DYLD discovery. Forge's Keychain identity remains unchanged.

Credential values never enter process arguments, environment variables, reports
or files. The native host discards Valve stdout/stderr before loading the engine;
only a finite typed protocol reaches Forge. Diagnostics contain boolean checks,
App ID, process IDs, ticket length and discovery counts. Failed starts record
finite stage names and error keys. Reports are under
`~/Library/Logs/Forge/Steam/` and libraries under
`~/Library/Caches/com.zagrios.forge/SteamRuntime/`.

## Game session tracking

Forge currently admits one Steam game session at a time. `SteamGameSessions.launch`
reserves it synchronously before invoking the model's launch verification and plan
preparation. The module releases it after a definite preparation failure, confirmed
absence during recovery, or actual game exit and host teardown. The running game's
action reads "Running". Selecting
another installed Steam game does not enable another launch. Download, resume,
verification and uninstall entry points reject operations on the reserved App ID,
including through a replacement account session. Other games can still download
while a game is running.

The independent Rust supervisor consumes the host's typed event pipe. The UI
observes snapshots through a private Unix socket and an asynchronous UniFFI wait.
UniFFI exposes the supervisor's `Preparing`, `Running`, `Finished` and `Failed`
phases directly. Control reconnection is separate from both game phase and Steam
host state: an IPC error or failed identity query does not declare the host dead.
The Rust client retains its last observed snapshot and paces retries at 500 ms;
Swift presents those states without its own transport retry timer.
Network disconnection, reconnection and host loss are distinct states.
Disconnection and loss appear in the game details without releasing the session
or terminating the game. Diagnostics retain the last `host_state` separately from
`game_started`/`game_exited`, so the final exit record does not erase host failure.
No credentials or native log payloads are added to those records.

Once the supervisor spawns the game, Swift task cancellation does not terminate
or abandon it. The supervisor waits for process exit even if the account signs out. A failed
post-spawn diagnostic write or host death likewise cannot trigger termination.
A nonzero game exit reports an error and releases the session after cleanup.
Callers cannot release reservations or forward session events. Each operation owns
its client observation through completion; cancelling an old completed operation
cannot change the next session.

This tracks the directly launched process. Games that hand execution to another
process need separate compatibility work. Recovering Steamworks inside a game
after host loss is not implemented. Steam Cloud is unchanged.

Tests use disposable processes and scripted supervisor snapshots for cancellation after
spawn, immediate/nonzero exit, failed spawn, duplicate requests, file-operation
guards, network recovery, host loss, account sign-out and old operation handles.
`SteamGameSessionsTests` covers lost acknowledgement followed by failed/delayed
recovery, cancellation before preparation, during handoff and after spawn, control
loss, terminal errors and old operation handles. `SteamPresentationTests` exercises
that same module through launch, verification/download/uninstall guards and account
replacement during recovery, using scripted supervisor snapshots.
These controls are separate from authenticated Silksong checks. The September 17
[session validation report](evidence/steam-session-lifecycle-2026-09-17.json) records
237 main-workspace Rust tests, one native test, 183 Swift tests with nine skips
and no failures, app signature verification and isolated runtime loading. That
automated validation did not use a real game or save.

## Surviving Forge closure and crash

Steam and Epic share `game_session` for exec confirmation, process identity
resolution and asynchronous child reaping. Each supervisor runs one Tokio
executor through preparation, gameplay and finalization. Steam Cloud uses that
executor. Steam presence uses the native host's existing connection. Native
Steam host preparation stays in an awaited blocking task, while host events
retain their independent pipe observer. Account and Cloud protocols remain in
their storefront adapters.

After confirmed execution, including the Intel exec handshake, the supervisor
requests Online persona on the native host's existing Steam user. The game's
own SDK registers its activity with that same host. Account revocation requests
Offline permanently without killing the game; normal exit withdraws presence
before closing the host and finalizing Cloud. Closing Forge leaves the supervisor
and host alive. Reopening observes the existing session. A lost host can lose
Steam presence while the game continues; process observation does not replace
that connection. See the [presence lifecycle](steam-cm-notes.md#publishing-the-supervised-game)
and [pinned native ABI evidence](evidence/steam-native-presence-abi.md).

Both supervisor entry points clear the inherited signal mask before creating
Tokio or worker threads. Rust preserves the launching thread's mask across
`Command::spawn`; a blocked `SIGCHLD` prevents Tokio's child-exit notification
from waking `Child::wait`. The former blocking Steam wait and Epic `try_wait`
loop did not depend on that notification, so switching to the shared async wait
exposed this startup requirement. The native regression fixture starts with
`SIGCHLD` and `SIGTERM` blocked and verifies that `detach_supervisor` enables
signals inside the new process. No polling fallback is added to game tracking.
An isolated Swift probe on September 22, 2026 found both signals unblocked on
the main thread and blocked on a `DispatchQueue.global()` worker. A live stuck
supervisor retained its exited Silksong child as a zombie; neither a new
`SIGCHLD` nor `SIGTERM` woke it. The corrected initialization passed the native
fixture, but a new real launch/exit check was blocked by the locked Mac.

The supervisor receives its bounded launch request and credentials once over a
private Unix socket pair mapped to stdin and stderr. After publishing its initial
record and starting its control listener, it acknowledges the session over that
same channel. The client bounds the reply wait at 15 seconds; it no longer polls
`session.json` during startup. Failed acknowledgement returns an uncertain-handoff
error to `SteamGameSessions`,
which starts durable recovery on the same injected instance without releasing its
reservation. While recovery has not identified the live game, all Steam installation
mutations stay blocked. Losing the parent never revokes an accepted launch.
Neither endpoint is a log
or a source of credentials after handoff. No further input from Forge is needed.
A new process session prevents a terminal/process-group shutdown of Forge from taking
it down. The supervisor remains only for preparation and the game lifetime; no
login item or persistent system service is installed.

A private `SteamSessions` directory under
`~/Library/Application Support/com.zagrios.forge/` holds launch/owner locks and
atomic session metadata. Each session has a UUID, App ID, account ID, supervisor
and game identity, socket address and final status. Credentials and ownership
tickets are never written there. Directories use mode 0700 and records mode 0600.
Session metadata and diagnostics share a unique-temporary-file/rename helper;
session writes retain file `fsync`, while diagnostic writes remain best effort.
This preserves the existing file durability policy, not a new directory-fsync
or power-loss recovery guarantee.
A separate short, random 0700 directory under `/tmp` holds the control socket to
respect macOS socket-path limits even with long home-directory names.

The supervisor holds an exclusive file lock for its lifetime. Launch admission
also uses a startup lock. A blocking lock waiter allows one second for the old
owner's terminal teardown; after timeout it retains the startup lock until the
owner releases, preventing additional waiters or competing launches.
A client validates UID, executable path and libproc's
kernel birth timestamp, including microseconds, before connecting to a recorded
supervisor. Responses must match the session UUID, account, App ID and supervisor
identity. Stale PID reuse does not establish a session. Normal observation waits
for state changes over the socket, with a bounded timeout for lost connections.
The control listener blocks in `accept` and receives an explicit shutdown wakeup.
The host pipe reader signals a generation-based condition variable on events and
EOF, replacing the runtime's periodic 250 ms wakeup. Game exit also wakes it.
If the control socket is removed externally, process teardown does not wait on
an unreachable listener.

At startup, Swift blocks Steam installation operations and launches until
recovery completes. A recovered running game restores the Running action and
its App ID's installation guards, independently of account authentication. The
supervisor reaps its game child, stops the host, publishes the final status and
removes the socket/scratch directory even if Forge never reopens. Private final
metadata remains available to a late observer, including after another session
starts. Automatic metadata/cache retention cleanup is separate work.

A supervisor crash is different from an interface crash. If the supervisor is
lost, the game identity is checked before retaining the reservation; the UI
reports host loss and keeps protecting the game until that exact process exits.
This exceptional path rechecks process identity, without restarting Steamworks.
Unreadable metadata, failed identity queries or an ambiguous running record keep
operations blocked. An orphan's exit code cannot be recovered from libproc.

[Supervisor validation](evidence/steam-supervisor-lifecycle-2026-09-17.json) covers
normal detach, a killed disposable UI parent, reattachment, duplicate rejection,
preparation cancellation, failed spawn, immediate/nonzero exit, host loss,
supervisor loss, PID identity mismatch, and recovered Swift actions/guards.
[Refinement validation](evidence/steam-supervisor-refinement-2026-09-17.json)
adds lost acknowledgements, control reconnection, lock deadlines, unreadable
metadata, socket removal, atomic-write rollback and equal-size/equal-mtime tampering.
These automated tests launch disposable processes. On September 18, 2026, the
user reported a successful Silksong retest after the refinement: Forge showed
"En cours" after launch, still showed "En cours" after closing and reopening
Forge while the game remained open, then showed "Lancé" after the game closed.
See the [manual lifecycle check](#manual-lifecycle-check-september-18-2026) below.
Existing games launched by the previous implementation have no recoverable
supervisor record.

## Bundled runtime preparation

`crates/steam-runtime/runtime-manifest.json` pins Valve client version `1788652215`,
three HTTPS archive URLs, their byte lengths and SHA-256 hashes, and the exact
eight files retained from them. The acquisition source is Valve's
[macOS update manifest](https://client-update.akamai.steamstatic.com/steam_client_osx),
observed on September 17, 2026 with SHA-256
`8b7c2b2159efc9949abd88883aec99a5295e6177ee2f0715308684f8ef0225f8`.
The seven library hashes match the September 16 binary inventory. The build does not
follow the latest manifest or accept a different engine automatically.

The retained files are `steamclient.dylib`, `libtier0_s.dylib`,
`libvstdlib_s.dylib`, `libaudio.dylib`, `crashhandler.dylib`, and Breakpad's
`Versions/A/Breakpad` and `Versions/A/Resources/breakpadUtilities.dylib` at their
original relative paths, plus Breakpad's original `Versions/A/Resources/Info.plist`.
The plist is required by the library's code signature: removing it reproduces a
dyld rejection, and restoring those exact bytes makes the isolated load pass.
Framework headers, crash-report applications, UI files,
web helpers and the Steam executable are excluded. The game's own Steamworks
API library remains the SDK consumer. Minimal loading and authenticated gameplay
are separate validation steps.

The Rust `forge-steam-runtime-package` build tool, behind the `packaging` feature,
downloads the fixed Valve archives into ignored `build/steam-runtime/downloads/`,
checks their complete size/hash, and writes only the allowlisted files into
`build/steam-runtime/runtime.zip`. It verifies every payload file before publishing
the ZIP atomically. A valid existing package supports subsequent offline builds.
The source repository contains the manifest and tool, not Valve binaries.

`scripts/build-steam-native.sh` includes this ZIP as
`Forge.app/Contents/Resources/SteamRuntime/runtime.zip`. Keeping the original
libraries inside a resource archive preserves their exact bytes and avoids
re-signing the pinned engine as nested application code. The app's signature
seals the archive. This is not a claim of notarization or distribution approval.

At game launch, the Rust supervisor verifies a cache generation identified by the
payload and bundled-helper hashes. If necessary, it extracts the embedded ZIP
into a private staging directory, checking exact paths, entry types, byte lengths
and SHA-256. It copies Forge's signed helpers alongside the engine, verifies the
whole tree, and publishes a unique generation by rename. Validation descends only
through checked directories, rejects symlinked ancestors and files, and hashes
every expected regular file in one traversal. Full hashing remains mandatory:
a stamp, matching size or matching modification time cannot prove unchanged
contents. No runtime download,
Steam installation lookup, Keychain read or additional UI is involved.

An invalid cache is repaired from the app bundle into a new generation. Existing
generations are never overwritten or deleted while a game may be using them.
Concurrent preparations use separate staging directories. Cancellation and normal
errors remove their own staging data; leftovers from an abrupt process exit are
ignored, never treated as a runtime. Automatic reclamation of old generations is
not implemented. Purging the cache only requires local extraction from Forge.
The existing host sandbox continues to deny access to Steam's application-support
directory. This change does not add offline account authentication or Steam Cloud.

Tests cover first preparation without a Steam directory, reuse without the source
ZIP, missing/corrupt payloads, unexpected paths and symlinks, cache corruption,
helper updates, concurrent preparations, interruption and cancellation. An opt-in
check, also invoked by `scripts/test.sh` after the app build, prepares the actual
bundled package in a temporary directory and runs the
signed helper's internal `verify-runtime` mode under a sandbox denying network,
Steam installation reads, and file writes. It loads the engine and resolves its
factories without creating a global user, authenticating or launching a game:

```sh
FORGE_STEAM_BUNDLE="$PWD/build/xcode/Build/Products/Debug/Forge.app" \
  cargo test --locked -p steam-runtime \
  bundled_runtime_loads_without_steam_installation_access -- --ignored
```

The September 17 [packaging validation report](evidence/steam-runtime-bundle-2026-09-17.json)
records successful build/signature checks, both Rust workspaces, 177 Swift tests
with nine skips and no failures, the isolated load, and a byte-identical offline
rebuild of the 19,323,267-byte archive. Local detailed logs remain under `build/`.

The September 16 authenticated Silksong run below used the former imported
runtime. The reduced bundled payload requires a separate authenticated game
check; the offline loader test alone does not establish game or Cloud behavior.

## Silksong validation

The integrated Rust path was exercised from Forge's existing Launch button on
September 16, 2026 with Silksong, App ID 1030300. It authenticated the existing
session, obtained a 178-byte ownership ticket, initialized the original SDK,
verified account/license/App ID, and completed the asynchronous player-count
request. No additional Keychain prompt or full Steam client process appeared.

Silksong itself loaded the cached `steamclient.dylib` and wrote these messages in
its own `Player.log`:

```text
Steam logged in as <connected account>
Selected online subsystem SteamOnlineSubsystem
Steam stats received.
```

The game window was also observed in its video settings. These observations prove
more than opening a window: the game selected its Steam online subsystem and
received stats. They do not establish an unlocked achievement, cloud sync,
playtime accounting, multiplayer or a complete gameplay session.

The sanitized [launch report](evidence/steam-native-silksong-2026-09-16.json) records
this run. Silksong was left open while the user was in its settings, then exited
with code 0. The final report records four discovery requests, and both the game
and native host processes were gone. Supervisor teardown and cancellation are
also covered by process tests.

`scripts/test.sh` covers the Rust workspaces and Swift integration. The supervisor
tests reject wrong App IDs/PIDs, missing authentication and empty tickets, detect
dead or malformed helpers, and interrupt a live wait on cancellation. Swift launch
fixtures assert the original credentials arrive and the credential store was
read only once during initial session restoration.

The integration's automated validation, before cleanup, passed 223 main-workspace Rust tests, one native ABI test
and three research-tool tests. Five opt-in Rust tests were ignored. Swift executed
177 tests: 168 passed and nine opt-in tests were skipped. Its app-hosted tests ran
from a separately signed validation copy to keep the user's game host alive.
Clippy with warnings denied, formatting, 514 localization keys, the macOS build
and the Apple signature checks also passed. Detailed outputs are retained locally
under `build/steam-native-integration-baseline/`.

### Manual lifecycle check, September 18, 2026

The user retested Silksong with the refined supervisor and reported this sequence.
The action labels below preserve the wording of the report:

1. Start Silksong from Forge: "En cours".
2. Close Forge, then reopen it while Silksong remains open: "En cours".
3. Close Silksong: "Lancé".

This confirms the user-observed running state, its recovery after normal Forge
closure and reopening, and the action-state change after game exit. The report
does not include new diagnostic logs or process traces. Crash recovery, blocked
relaunch/install operations and host teardown retain their separate automated
coverage; this manual check does not validate Steam Cloud, achievements, playtime
or multiplayer.

## Research cleanup

The standalone `experiments/` directory was removed on September 16, 2026 after
the native host was integrated and validated through Forge. Its prototypes,
command-line runner, optional Keychain reader, manifests and build output are no
longer maintained. Forge's build and launch path did not depend on that workspace.
The production implementations and their tests remain in `crates/steam-native`,
`crates/steam-runtime` and the Swift/UniFFI integration.

The eleven research reports were moved unchanged to the
[historical evidence archive](evidence/steam-native-research/README.md). They retain
the binary inventory and pinned ABI map, discovery protocol observations, AppKit
sandbox diagnosis, authenticated SDK controls and the separate Rust migration
results. Paths and hashes inside those JSON files describe the original test
artifacts, including files that have since been removed. They are not current
dependencies or runnable instructions.

Use `./scripts/build.sh` to build the app and signed native helpers, and
`./scripts/test.sh` for the maintained tests. For a live check, use Forge's existing
Launch button and inspect its sanitized report together with the game's own log.
The removed prototype commands and direct Keychain reader are not part of this
workflow.

## Current limits

This integration currently targets native Apple silicon and the pinned Valve
client SHA-256
`035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840`.
The build obtains the exact libraries from pinned Valve CDN packages and embeds
them in Forge. No installed Steam client is read, including on first launch.
Updating the pinned engine still requires ABI and compatibility validation; a
mismatched version fails before private ABI calls.

Architecture selection, Rosetta readiness and the Intel exec handoff
remain required. Games without `libsteam_api.dylib` and hardened games that
reject the adapter still need compatibility work. Host readiness is not proof
of successful SDK initialization inside a game. Concurrent Steam game sessions
are blocked until their compatibility is validated.
Private endpoints use ephemeral loopback ports; the host binds after allocation,
so another process taking the port can cause a failed launch. Valve libraries are
embedded in the local app bundle; they are not versioned in the repository.

Steam Cloud is integrated; current AutoCloud and ordinary SDK storage evidence
is recorded in [the September 20 compatibility report](steam-cloud-compatibility-2026-09-20.md).
The supervisor preserves new sessions across Forge closure or crash. The
September 18 manual Silksong check confirms action-state recovery after normal
Forge closure and reopening, followed by the action-state change after game exit.
Crash recovery is covered by automated disposable-process tests. Achievements,
playtime and online-game behavior remain separate checks.
