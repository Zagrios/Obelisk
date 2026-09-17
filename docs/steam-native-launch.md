# Native Steam launches from Forge

Forge's existing Launch button starts a separate Rust supervisor, which starts
the Steam host before the native macOS game. It passes the session already held
by `SteamModel` through a one-time private stdin pipe. No extra screen, login, Keychain query or user action is involved.

The app bundles a minimal, hash-pinned Valve runtime archive. The host loads its
original libraries from a verified private cache prepared from that archive. It
requires authentication for the expected account and a nonempty ownership ticket
for the selected game. A separate consumer then checks the game's unmodified
Steamworks library, account, App ID and subscription before Forge starts the game.
A read-only player-count request is recorded when available; it is not required
for launching. The native engine provides the actual Steamworks results.

The Mach discovery adapter redirects only `com.valvesoftware.steam.ipctool` to a
unique Forge-owned service. Host, SDK checker and game share the same private IPC
endpoints. The first integrated test authenticated successfully but failed SDK
initialization because the checker did not inherit these endpoints. Sharing them
fixed the failure. A regression test covers all three consumers' environment.

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
- `SteamGameLauncher` observes supervisor snapshots. The shared, observable
  `SteamGameSessions` reservation spans preparation through teardown and account
  sign-out, and restores an existing session when a new Forge process starts.
  Authentication failure, cancellation before spawning or failed SDK validation
  cleans up the host and scratch home.
- `scripts/build-steam-native.sh` builds and signs both helpers in Forge's bundle.
  Only the helper has the entitlements needed to load Valve libraries and use
  DYLD discovery. Forge's Keychain identity remains unchanged.

Credential values never enter process arguments, environment variables, reports
or files. The native host discards Valve stdout/stderr before loading the engine;
only a finite typed protocol reaches Forge. Diagnostics contain boolean checks,
App ID, process IDs, ticket length and discovery counts. Failed starts record
finite stage names and error keys. Reports are under
`~/Library/Logs/Forge/Steam/` and libraries under
`~/Library/Caches/com.zagrios.forge/SteamRuntime/`.

## Game session tracking

Forge currently admits one Steam game session at a time. The model reserves it
synchronously before launch verification, and releases it after failure or actual
game exit and host teardown. The running game's action reads "Running". Selecting
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
Reservation IDs reject late events or cleanup from a previous session.

This tracks the directly launched process. Games that hand execution to another
process need separate compatibility work. Recovering Steamworks inside a game
after host loss is not implemented. Steam Cloud is unchanged.

Tests use disposable processes and injected session events for cancellation after
spawn, immediate/nonzero exit, failed spawn, duplicate requests, file-operation
guards, network recovery, host loss, account sign-out and stale session events.
These controls are separate from authenticated Silksong checks. The September 17
[session validation report](evidence/steam-session-lifecycle-2026-09-17.json) records
237 main-workspace Rust tests, one native test, 183 Swift tests with nine skips
and no failures, app signature verification and isolated runtime loading. No
real game or save was used to validate these session changes.

## Surviving Forge closure and crash

The supervisor receives its bounded launch request and credentials once over a
private Unix socket pair mapped to stdin and stderr. After publishing its initial
record and starting its control listener, it acknowledges the session over that
same channel. The client bounds the reply wait at 15 seconds; it no longer polls
`session.json` during startup. Failed acknowledgement triggers durable recovery,
and losing the parent never revokes an accepted launch. Neither endpoint is a log
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
These tests launch disposable processes, not Silksong. The user separately
reported successful Silksong quit/reopen and action-state checks before this
refinement; these latest changes have not been retested with a real game. Existing games
launched by the previous implementation have no recoverable supervisor record.

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

The SDK check currently requires Silksong's modern flat Steamworks interfaces.
Other SDK versions, games without `libsteam_api.dylib`, Intel games, hardened games
that reject the adapter need compatibility work. Concurrent Steam game sessions
are blocked until their compatibility is validated.
Private endpoints use ephemeral loopback ports; the host binds after allocation,
so another process taking the port can cause a failed launch. Valve libraries are
embedded in the local app bundle; they are not versioned in the repository.

Steam Cloud is not integrated. The game uses its normal local save location.
The supervisor preserves new sessions across Forge closure or crash. Authenticated
Silksong validation of this new ownership path, achievements, playtime and
online-game behavior remain separate checks.
