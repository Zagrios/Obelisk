# Native Steam launches from Forge

Forge's existing Launch button now starts a Rust Steam host before the native
macOS game. It passes the session already held by `SteamModel` through a private
stdin pipe. No extra screen, login, Keychain query or user action is involved.

The host loads Valve's original libraries from a versioned private cache. It
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
- `crates/forge-bridge/src/steam_native.rs` exposes one launch attempt and its
  runtime. Swift cancellation explicitly reaches the blocking Rust operation.
- `SteamGameLauncher` retains the runtime until the game's process exits. Closing
  the private pipe stops the host if Forge exits. Authentication failure,
  cancellation or failed SDK validation cleans up the helper and scratch home.
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
It copies that build's libraries from the local Steam installation. Obtaining and
updating the runtime independently of an installed Steam client remains separate
work. A mismatched engine version fails before private ABI calls.

The SDK check currently requires Silksong's modern flat Steamworks interfaces.
Other SDK versions, games without `libsteam_api.dylib`, Intel games, hardened games
that reject the adapter and concurrent game launches need compatibility work.
Private endpoints use ephemeral loopback ports; the host binds after allocation,
so another process taking the port can cause a failed launch. No native libraries
are bundled in the repository or distribution by this change.

Steam Cloud is not integrated. The game uses its normal local save location.
Closing Forge stops its host, even if the game remains open. Broader game lifecycle
UX, achievements, playtime and online-game behavior are separate tasks.
