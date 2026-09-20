# Intel Steam games and Rosetta

The existing Launch action retains its file-verification setting, reconciles Cloud, and then checks the selected native architecture. On Apple silicon, an Intel launch requires a successful execution of Forge's signed Intel probe. If Rosetta is unavailable, the supervisor keeps the launch reservation and publishes a request for the current session and account. Swift presents the approved Rosetta sheet and opens `Contents/Helpers/Forge Rosetta.app` through `NSWorkspace` when the user selects Install. The system owns installation and any authentication prompts. Swift checks readiness through the Rust supervisor once per second, independently of the system opening callback, for at most ten minutes. The supervisor requires successful Intel execution before resuming.

Cancellation and account revocation wake the waiting supervisor. Installation callbacks belonging to an old session, account or decision cannot resume a game. An opening error does not stop readiness checks unless it reports cancellation. The sheet offers retry if readiness times out or the supervisor reports another failure. The file verification, Cloud coordinator, environment, save ownership and single-session guards retain their existing responsibilities.

## Installation completion

A user trial on September 20, 2026 exposed a false failure sheet while installation continued. At 17:00:59.597 local time, CoreServicesUIAgent logged an initial LaunchServices opening error, `NSOSStatusErrorDomain -10669`. Rosetta installation followed, and the signed Intel helper later launched as PID 86997 and exited with status 0 at 17:01:33.364. The original Swift callback did not log its error, so the CSUI entry does not establish which error Swift received. It does establish that this opening error was followed by successful Intel execution.

The original model made readiness checks depend on a successful `NSWorkspace` return. The corrected model starts the system opening request and bounded Rust readiness checks independently. A late opening error cannot turn a confirmed, current launch into an installation failure. Explicit cancellation still cancels the waiting launch, and stale results cannot resume it. Diagnostics record the opening error's domain and code and the Rust readiness confirmation.

Apple documents [`openApplication(at:configuration:completionHandler:)`](https://developer.apple.com/documentation/appkit/nsworkspace/openapplication(at:configuration:completionhandler:)) as reporting whether the app launched. [`promptsUserIfNeeded`](https://developer.apple.com/documentation/appkit/nsworkspace/openconfiguration/promptsuserifneeded) delays relevant callbacks until the system UI is dismissed. Neither API documents a Rosetta installation result, so Forge uses its execution probe as the readiness signal.

## Architecture and SDK compatibility

The runtime reads bounded Mach-O headers from the game and its original Steam API library. It selects a shared ARM64 slice when available, otherwise a shared x86_64 slice. Unsupported, malformed or incompatible binaries fail before SDK preparation. The Valve package already includes both architectures and is unchanged.

The private Steam engine and supervisor remain ARM64. `forge-steam-sdk-arm64` and `forge-steam-sdk-x86_64` perform the public SDK checks. The discovery adapter is universal. Each consumer uses the same private endpoints and verified runtime generation as the game. Build and signature checks verify each helper's exact architecture set.

Modern SDK initialization and interface access remain supported. The additional legacy profile recognizes the original 609,216-byte Steam API library with SHA-256 `41dc3afbe16368e0e54624d561ed45ab85186e7e8641176d81dfdf4b414068f9`, observed in the Forge-installed Dead Cells. Recognition is by SDK bytes, with no AppID exception. Unknown legacy SDKs are refused before initialization.

That profile uses `SteamAPI_Init`, `SteamClient`, the existing user/pipe handles, and the matching User019, Apps008, Utils009, Stats011 and Storage014 interfaces. It does not apply newer interface layouts to old wrappers. Account identity, AppID, ownership and projected storage bytes remain real SDK checks. Player-count callbacks remain optional, as before. This profile does not claim compatibility with every historical Steamworks release.

## Intel process handoff

The signed Intel SDK helper replaces itself with the game using `POSIX_SPAWN_SETEXEC` and an explicit binary preference. This retains the process identity, working directory, literal arguments and game environment, including the discovery adapter. It avoids passing that environment through a protected system launcher that could strip DYLD variables.

The helper initially waits for an admission byte on stdin. The supervisor records its identity and expected game executable before sending that byte. Losing the supervisor before admission closes the pipe and prevents execution. After admission, recovery recognizes either the same helper or its replacement game using UID, PID, kernel birth timestamp and the expected executable. Unknown identity retains the reservation.

A separate close-on-exec control pipe reports an explicit `exec-failed` marker if replacement fails. The game receives null stdin/stdout rather than the control channels. The supervisor reaps a failed helper before aborting preparation. Observation errors after a child starts cannot release its reservation while it may still be alive.

## Validation scope

Automated tests use temporary Mach-O fixtures, native SDK fixture libraries, disposable processes and injected installation results. They cover architecture selection, legacy refusal and access, process admission and replacement, Rosetta absence/presence, cancellation, retry, stale callbacks, account changes, recovery and Cloud ordering. Separate rendered fixtures check the French prompt and retry state.

The implementation baseline on September 20, 2026, before the installation-completion fix above, passed `./scripts/test.sh` on macOS 27.0 ARM64. The workspace passed 420 Rust tests, the native SDK workspace passed 15, and the separately enabled isolated runtime-loading test passed. Five opt-in Steam tests requiring network access, reference fixtures or installed files remained ignored. XCTest reported 241 tests with 10 skipped and no failures. Formatting, strict Clippy, all 546 English/French localization keys, the app build, Apple certificate signatures and exact helper architecture checks passed. The baseline log is `build/rosetta-tests.log`; rendered prompt and retry fixtures are in `build/rosetta-previews/`. These results do not validate the later fixes, whose validation is recorded separately.

The installation-completion regression first failed with three assertions: an opening error prevented the readiness check, kept the sheet visible and displayed a false installation failure. After correction, the complete XCTest suite passed with 248 tests, 10 skipped and no failures. The 19 Rosetta tests cover early, failed, pending and late opening callbacks, readiness becoming available, deadline expiry, retry, cancellation and stale account/session results. The rebuilt app passed signature and helper-architecture verification. Logs are `build/rosetta-callback-red.log` and `build/rosetta-fix-tests.log`.

At the initial September 20, 2026 check, before the user installed Rosetta, executing the packaged `ForgeRosetta --probe` directly returned OS error 86, `Bad CPU type in executable`. That historical check verified the unavailable result without opening LaunchServices or starting installation. The user subsequently installed Rosetta through the system UI; the same packaged probe then returned status 0.

The user also launched Dead Cells, and observed sessions reached `Finished` with exit status 0. Those trials establish that the game ran and the supervisor recorded its exit. They do not establish full Steamworks compatibility, every legacy SDK interface or a complete Cloud upload and recovery cycle. Apple cancellation behavior and the corrected installation flow still require their own evidence. Cross-compilation and fixture tests do not establish those live results. macOS 28 compatibility is not claimed; see the [Apple research](rosetta-intel-research-2026-09-20.md).
