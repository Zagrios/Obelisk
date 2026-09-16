# Native Steam research evidence

These eleven JSON reports were moved unchanged from the retired standalone
experiment on September 16, 2026. The prototype sources, runner, optional Keychain
reader and build output were removed once Forge used the production Rust host.
No game-launch or build dependency points to these reports.

Use [native Steam launches](../../steam-native-launch.md) for the maintained
implementation, commands and limitations. Its
[Silksong launch report](../steam-native-silksong-2026-09-16.json) is the later
end-to-end Rust integration result. The
[feasibility report](../../research-steam-native-host.md) and
[source investigation](../../research-steam-native-sources.md) explain the earlier
experiments.

| Report | Recorded evidence |
| --- | --- |
| [inventory.json](inventory.json) | Reference Valve binaries, architectures, versions and hashes |
| [user-abi-map.json](user-abi-map.json) | The 259-slot map for the pinned ARM64 engine; method names alone do not establish signatures |
| [runs.json](runs.json) | Initial loading, callbacks and unauthenticated IPC controls |
| [sandbox-check.json](sandbox-check.json) | Original isolation control |
| [credential-check.json](credential-check.json) | Credential handoff only, without network authentication or stored credential values |
| [authentication-runs.json](authentication-runs.json) | Original native authentication and ownership readiness |
| [steamworks-discovery-runs.json](steamworks-discovery-runs.json) | Original discovery, authenticated SDK request and host-loss controls |
| [appkit-registration.json](appkit-registration.json) | GUI registration failure and corrected sandbox control |
| [game-runs.json](game-runs.json) | Original Silksong crash diagnosis and main-menu smoke test |
| [validation.json](validation.json) | Original C++ prototype checks and source hashes |
| [rust-port-validation.json](rust-port-validation.json) | Standalone Rust migration checks before integration with Forge |

The original authenticated/game runs used C++. The standalone Rust migration
validated offline loading, IPC, discovery and AppKit, but its corrected
noninteractive Keychain read returned -25293 before authentication. The later
Forge integration reused the session in memory and validated the actual Rust
game-launch path. Do not merge those distinct validation stages.

Absolute paths, process IDs, old source paths and source hashes inside the JSON
files describe the original runs. Paths under the deleted experiment and local
`build/` directories are historical identifiers, not links or required files.
Flags such as `production_launch_integration: false` describe that report's date,
not the current application. Keeping these records unchanged preserves their
provenance.

None of these reports proves Steam Cloud synchronization, a new server-persisted
achievement, playtime accounting, multiplayer, compatibility with every game or
independent runtime distribution.
