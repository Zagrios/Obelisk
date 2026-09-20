# Disposable Steam Cloud runtime probe

September 19, 2026. The Rust probe exercises Forge's pinned Valve runtime in a disposable HOME. It does not launch a game, authenticate, read an installed Steam client or transfer Cloud data.

## Current result

With synthetic app metadata and a disconnected test identity, local RemoteStorage writes, reads, replacement and deletion now work while the runtime remains open. Private and public interfaces observe each other's changes, and the final state survives process restart. Rust can also prepare a file and matching index before initialization. Rewriting an index already loaded by Valve still fails; the passing live route uses native file operations instead.

Explicit download/upload requests are also exercised offline. Valve accepts them and later reports failure in a callback for the correct AppID. No remote transfer is claimed. The cancellation call is exercised, but its effect on an active transfer remains unproven.

| Test | Observed result |
| --- | --- |
| Native write, initialized local cache | Write result 1, count 1, exact 39-byte read in the writer and a second process. |
| Rust projection without an index | Direct disk read succeeds; native count and read remain zero in both processes. |
| Rust projection with an index before cache initialization | Native count 1 and exact 39-byte read in both processes. |
| Rename file and index after initialization | Initial read succeeds. A second cache-load request removes the old record and deletes the replacement index. New-name read returns zero in the same process and after restart. |
| Live native mutations | Private creation, public size-changing replacement, another private creation and private deletion are visible through both interfaces. Final 56-byte file and deletion persist after restart. |
| Explicit offline download/upload | Requests are accepted; callbacks 1250001/1250002 each contain `[480, 21, 0]`. The runtime logs that it is not logged in. |
| Cancellation attempt | Call returns and the offline failure callback is observed. The job can fail before cancellation, so interruption is not proven. |

The file hashes match their respective expected fixture contents in every case. An exit code 1 for bare projection or live cache refresh is the deliberately retained failed hypothesis, not a successful feature test. The [initialized observations](initialized-observations.json) preserve both passing and failing cases, selected native cache messages and source fingerprints.

The later [live control observations](live-controls-observations.json) include public/private coherence, persisted deletion metadata, exact callback payloads and a regression run of the earlier private write/read path. Their source fingerprints are separate from earlier reports.

## Reproduce

Run from the repository root on ARM64 macOS with the existing local signing identity and built runtime archive:

```sh
# Live private/public creation, replacement, deletion and restart.
python3 scripts/probe-steam-cloud.py --mode local-live-mutation --fixture-appinfo --fixture-identity

# Explicit native sync requests, disconnected failures and a cancellation attempt.
python3 scripts/probe-steam-cloud.py --mode sync-controls --fixture-appinfo --fixture-identity

# Passing private write/read/list/restart.
python3 scripts/probe-steam-cloud.py --mode local-storage --fixture-appinfo --fixture-identity

# Negative control: bytes on disk without an index remain invisible.
python3 scripts/probe-steam-cloud.py --mode local-projection --fixture-appinfo --fixture-identity

# Passing Rust file/index projection before initialization, then restart.
python3 scripts/probe-steam-cloud.py --mode local-projection --fixture-appinfo --fixture-identity --fixture-cache

# Failed live cache refresh, with a third process checking the final state.
python3 scripts/probe-steam-cloud.py --mode local-cache-refresh --fixture-appinfo --fixture-identity --fixture-cache
```

`--keep-scratch` retains disposable files for inspection. By default the runner records the result and removes its own temporary directory. Omitting the fixture flags reproduces the initial missing-metadata/AccountID-zero conditions. Interface-only modes are `inventory` and `storage-inventory`.

The runner builds `crates/steam-native/examples/cloud_storage_probe.rs` with locked, offline dependencies, verifies the runtime payload manifest, signs the probe and uses `sandbox-exec`. The normal application build excludes this example. Each run gets a fresh HOME and private IPC endpoints. The sandbox denies external networking, reads beneath the real user's home, writes outside scratch, Steam Mach discovery and launching other processes. Credentials are neither requested nor supplied. Rust captures only its own typed JSON and selected cache diagnostics from the disposable runtime.

## Fixture and causal checks

The file is `forge-offline-probe.dat`, 39 bytes, SHA-256 `c49cc578c123fe8274b04f62dfa436d666208dfd2fe29d36de9952396eb38f3c`. The live-cache case renames it to `forge-offline-probe-renamed.dat` without changing its bytes. App ID 480 is an offline input, with synthetic quota 1,048,576 and file limit 16. Those values are not claims about the real App 480 configuration.

1. Without app metadata, `GetQuota` returns false and `FileWrite` returns 25. The generated appinfo v40 file supplies the missing UFS values. Valve then reports the fixture quota and accepts the write.
2. The default disconnected identity has Individual type, instance 1 and AccountID zero. Its full SteamID is not zero. A later cache job rejects that account component, matching `Sync Failed, no user set` in intermediate runs.
3. The fixture requests identity `0x0110000100000001` using the already verified logon method, without credentials and with external network denied. Result 5 and `logged_on=false` are retained. The local cache job now reports completion. This is a disposable local test input, not authentication or a production identity mechanism.
4. The native-write case requests cache initialization before writing, then observes a persisted fixture record before exiting. The deadline is five seconds. The second process validates actual indexed readback.
5. Rust projection uses a minimal `remotecache.vdf` record based on output produced by this exact macOS runtime, not a copied GameNative writer. It supplies root 0, size, local timestamps, remote timestamp 0, SHA-1 `b9d228874d30ac5d864bebd5252d232a3858c866`, syncstate 3, persiststate 0 and platform mask -1. Their complete remote semantics remain unverified.

Every cache request pumps callbacks for a bounded two-second observation window. Callback counts are not interpreted as completion. Native cache messages and subsequent file assertions provide the recorded observations; no production readiness protocol is inferred from the fixed wait.

During live refresh, the second init job validates the old in-memory filename after Rust has renamed it. The runtime logs that the old file is missing and removes it, then deletes the empty index. Thus the failure is more specific than a short wait or lost bytes: an external index replacement is not imported and is overwritten by native state.

See [static ABI and server evidence](../../steam-cloud-runtime-controls.md) and the [GameNative bootstrap investigation](../../steam-cloud-gamenative-bootstrap.md) for the native prerequisites and the independently verified metadata request.

## Live mutation and request-control details

The public interface needs its native pipe to be bound to the fixture AppID before interface creation. The initial pipe AppID is 0 even with the environment values set. The pinned Utils methods set and verify AppID 480 for this disposable probe. No internal field is patched. This is not an authenticated SDK initialization or ownership check.

The live sequence starts with 39 bytes, replaces them through the public API with 56 different bytes, creates `forge-offline-live.dat`, then deletes the original through the private API. Both read interfaces see the current bytes. The final SHA-256 is `f6d0f3dc8a43444532cb5c7c837cf9b9dec4092a3af9c3af108cdebe089ce736`. The checkpoint requires the surviving record's size and SHA-1 plus the deleted record's persiststate 2. The next process verifies that the deletion and updated bytes remain visible.

`GetLocalFileChangeCount` stays zero in this fixture. Ordinary local mutations do not establish remote-download notification or a Dynamic game reload. The native index still represents local changes, not an acknowledged remote baseline.

For sync controls, callbacks are copied only when their ID and size match statically established pairs. Unknown callback payloads are never read. The caller waits for the matching AppID and direction, with a five-second bound; request acceptance or an idle state alone cannot pass. The disconnected result is intentionally an error word, and the probe would reject a success word. The third completion word remains uninterpreted. Cancellation is global to this runtime and is not validated against an active transfer.

See the [ABI, direction mapping and callback evidence](../../steam-cloud-runtime-controls.md#follow-up-coherent-live-mutations-and-explicit-synchronization).

## Historical evidence and limits

A later [authenticated upload validation](../../steam-cloud-authenticated-upload.md) passed the Rust CM disposable upload/readback/delete sequence. Its result is separate from these native offline probes.

### Separate genuine SDK consumer

The new `sdk-pair` diagnostic runs a native fixture host and an original game-supplied Steamworks SDK in separate processes. The intended assertions are host-to-consumer read, consumer-to-host write, host reply and a complete three-file enumeration. Only the copied SDK is re-signed inside the disposable runtime; its source hash is recorded. The game installation is unchanged.

The current result is a retained failure. The discovery adapter loads, the host answers two path requests, and `SteamAPI_InitFlat` returns 1 with `ConnectToGlobalUser failed.` The SDK never reaches the RemoteStorage assertions. Removing AppID/GameID from the host's environment to match production does not change that result. The [observation record](sdk-consumer-observations.json) contains the typed events and source fingerprints.

Pinned static analysis identifies an application-admission predicate in `ConnectToGlobalUser`: its Utils call uses the requested AppID with option 1, and denial can return user 0. The earlier same-process fixture used option 0. The disconnected synthetic identity supplies no real subscription or ownership ticket. This is the leading explanation, not proof of the exact native branch executed. No SDK return value, license or admission result was overridden to make the fixture pass.

Reproduce with a library from a Forge-installed game, never an official Steam installation:

```sh
python3 scripts/probe-steam-cloud.py --mode sdk-pair \
  --fixture-appinfo --fixture-identity --steam-api /absolute/path/to/game/libsteam_api.dylib
```

The runner sets `DYLD_INSERT_LIBRARIES` with `/usr/bin/env` after entering the sandbox, because `sandbox-exec` strips that variable on entry. The profile allows only the fixture and this wrapper to execute, still denies external networking and real-home reads, and permits only the two private loopback endpoints. The host stops when its private stdin closes or its bounded lifetime ends. Both processes are reaped even on failure. This result does not validate a separate SDK consumer or Dynamic synchronization.

The initial [observations](observations.json) and [interface inventory](interfaces.json) are preserved with their original source fingerprints. They document the earlier missing-quota and missing-index failures. The initialized report records the later probe version separately.

Rust formatting, targeted Clippy with `-D warnings`, Python syntax and diff checks pass. No product source changed, so no full Swift build, UI test or real-game launch was performed.

Still unproven: an actual game/SDK consumer, authenticated metadata and remote transfers, preference-preserving exclusion of Valve's own synchronizer, remote-baseline coherence during live updates, account switching, Cloud finalization by the detached supervisor, conflict/history behavior and Dynamic callbacks plus game reload. Process restart is not power-loss durability. These local probes do not make any of the three Cloud modes production-ready.
