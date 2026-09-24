# Intel games and Rosetta

The existing Launch action retains its file-verification setting, reconciles Cloud, and then checks the selected native architecture. On Apple silicon, an Intel launch requires a successful execution of Forge's signed Intel probe. If Rosetta is unavailable, the supervisor keeps the launch reservation and publishes a request for the current session and account. Swift presents the approved Rosetta sheet and opens `Contents/Helpers/Forge Rosetta.app` through `NSWorkspace` when the user selects Install. The system owns installation and any authentication prompts. Swift checks readiness through the Rust supervisor once per second, independently of the system opening callback, for at most ten minutes. The supervisor requires successful Intel execution before resuming.

Cancellation and account revocation wake the waiting supervisor. Installation callbacks belonging to an old session, account or decision cannot resume a game. An opening error does not stop readiness checks unless it reports cancellation. The sheet offers retry if readiness times out or the supervisor reports another failure. The file verification, Cloud coordinator, environment, save ownership and single-session guards retain their existing responsibilities.

## Shared Steam and Epic requirement

Both storefronts use the same bounded Mach-O parser and signed Rosetta probe,
the same asynchronous Rust wait in `supervisor::rosetta::Gate::wait`, and the shared `RosettaModel`
and `RosettaPromptView`. The wait uses `Notify` for resume and cancellation;
only the bounded execution probe runs on a blocking worker. Steam selects a slice
compatible with its original Steam API library; Epic selects from the game
executable alone, preferring ARM64 and otherwise accepting x86_64. Epic does not
load the Steam runtime. Its native exec helper lets macOS launch the selected
Intel executable after Rosetta availability has been confirmed.

Each provider retains its own account identity and session directory; the
supervisor core owns the journal, the Rosetta wait and the admission for both.
The common Rosetta request carries a decision ID. A resume must match session,
account and decision, and the supervisor probes again before admitting the game.
Cancelling or signing out before admission prevents execution; closing the UI
after an admitted launch never terminates the game. `supervisor::child` shares exec
confirmation and event-driven child exit for both providers.

The shared path is covered by disposable process and Mach-O fixtures and Swift
session tests. Those checks do not prove a real Epic Intel game or a Rosetta
installation on a Mac where Rosetta is absent.

## Installation completion

A user trial on September 20, 2026 exposed a false failure sheet while installation continued. At 17:00:59.597 local time, CoreServicesUIAgent logged an initial LaunchServices opening error, `NSOSStatusErrorDomain -10669`. Rosetta installation followed, and the signed Intel helper later launched as PID 86997 and exited with status 0 at 17:01:33.364. The original Swift callback did not log its error, so the CSUI entry does not establish which error Swift received. It does establish that this opening error was followed by successful Intel execution.

The original model made readiness checks depend on a successful `NSWorkspace` return. The corrected model starts the system opening request and bounded Rust readiness checks independently. A late opening error cannot turn a confirmed, current launch into an installation failure. Explicit cancellation still cancels the waiting launch, and stale results cannot resume it. Diagnostics record the opening error's domain and code and the Rust readiness confirmation.

Apple documents [`openApplication(at:configuration:completionHandler:)`](https://developer.apple.com/documentation/appkit/nsworkspace/openapplication(at:configuration:completionhandler:)) as reporting whether the app launched. [`promptsUserIfNeeded`](https://developer.apple.com/documentation/appkit/nsworkspace/openconfiguration/promptsuserifneeded) delays relevant callbacks until the system UI is dismissed. Neither API documents a Rosetta installation result, so Forge uses its execution probe as the readiness signal.

## Architecture and SDK compatibility

The runtime reads bounded Mach-O headers from the game and its original Steam API library. It selects a shared ARM64 slice when available, otherwise a shared x86_64 slice. Unsupported, malformed or incompatible binaries fail before SDK preparation. The Valve package already includes both architectures and is unchanged.

The private Steam engine and supervisor remain ARM64. `forge-steam-sdk-x86_64` only performs the Intel exec handoff described below. Epic games use the arm64 `game-exec` helper, and macOS runs a selected Intel executable under Rosetta. The public SDK diagnostics, their legacy profile and the ARM64 SDK helper were removed. The discovery adapter is universal. Host and game use the same private endpoints and verified runtime generation. Build and signature checks verify each helper's exact architecture set and reject the obsolete ARM64 SDK helper.

## Intel process handoff

The signed Intel exec helper replaces itself with the game using `POSIX_SPAWN_SETEXEC` and an explicit binary preference. This retains the process identity and the game environment, including the discovery adapter, which the supervisor set when it spawned the helper. It avoids passing that environment through a protected system launcher that could strip DYLD variables. `POSIX_SPAWN_SETSIGDEF` gives the game the default SIGPIPE disposition instead of the helper's inherited ignore.

The helper initially waits for an admission message on stdin, shared with the arm64 `game-exec` helper. It names the executable, the literal arguments and the working directory. The supervisor records the helper's identity and expected game executable before sending it. Losing the supervisor before a complete message closes the pipe and prevents execution. After admission, recovery recognizes the helper's process, before or after it becomes the game, by PID, UID and kernel birth timestamp. The executable does not decide, so a game that execs another binary stays reserved. Unknown identity retains the reservation.

A separate close-on-exec control pipe reports an explicit `exec-failed` marker if replacement fails. After reading its admission, the helper replaces its stdin with null, so the game receives null stdin/stdout rather than the admission or control channels. The supervisor reaps a failed helper before aborting preparation. Observation errors after a child starts cannot release its reservation while it may still be alive.

## Validation scope

Automated tests use temporary Mach-O fixtures, disposable processes and injected installation results. They cover architecture selection, process admission and replacement, Rosetta absence/presence, cancellation, retry, stale callbacks, account changes, recovery and Cloud ordering. Separate rendered fixtures check the French prompt and retry state.

The implementation baseline on September 20, 2026, before the installation-completion fix above, passed `./scripts/test.sh` on macOS 27.0 ARM64. The workspace passed 420 Rust tests, the native SDK workspace passed 15, and the separately enabled isolated runtime-loading test passed. Five opt-in Steam tests requiring network access, reference fixtures or installed files remained ignored. XCTest reported 241 tests with 10 skipped and no failures. Formatting, strict Clippy, all 546 English/French localization keys, the app build, Apple certificate signatures and exact helper architecture checks passed. The baseline log is `build/rosetta-tests.log`; rendered prompt and retry fixtures are in `build/rosetta-previews/`. These results do not validate the later fixes, whose validation is recorded separately.

The installation-completion regression first failed with three assertions: an opening error prevented the readiness check, kept the sheet visible and displayed a false installation failure. After correction, the complete XCTest suite passed with 248 tests, 10 skipped and no failures. The 19 Rosetta tests cover early, failed, pending and late opening callbacks, readiness becoming available, deadline expiry, retry, cancellation and stale account/session results. The rebuilt app passed signature and helper-architecture verification. Logs are `build/rosetta-callback-red.log` and `build/rosetta-fix-tests.log`.

At the initial September 20, 2026 check, before the user installed Rosetta, executing the packaged `ForgeRosetta --probe` directly returned OS error 86, `Bad CPU type in executable`. That historical check verified the unavailable result without opening LaunchServices or starting installation. The user subsequently installed Rosetta through the system UI; the same packaged probe then returned status 0.

The user also launched Dead Cells, and observed sessions reached `Finished` with exit status 0. Those trials establish that the game ran and the supervisor recorded its exit. They do not establish full Steamworks compatibility, every legacy SDK interface or a complete Cloud upload and recovery cycle. Apple cancellation behavior and the corrected installation flow still require their own evidence. Cross-compilation and fixture tests do not establish those live results. macOS 28 compatibility is not claimed; see the [Apple research](rosetta-intel-research-2026-09-20.md).
