# AutoCloud launch and detached completion

September 19, 2026. The macOS launcher, detached supervisor and save interface now use `game-runtime::steam::cloud`. The coordinator uses the ordinary authenticated Forge session; no credential is copied to an argument, environment variable, log or archive.

## Initial Silksong cycle, before save-history removal

The installed native ARM64 game passed its 8.26 GB verification before launch. AutoCloud compared 18 files totaling 3,806,866 bytes against the complete remote view at revision 54. The native host then reported authenticated account/license checks and an initialized SDK, and the game ran.

While playing, Forge displayed one history version with 18 files and disabled restoration and uninstall. Closing Forge left both the game and supervisor alive. The game subsequently exited normally through its macOS Quit command. With Forge still closed, the supervisor reached `Finished`, host `stopped`, exit code 0 and Cloud `Synced`; both game and supervisor had exited. The archive contained no pending upload or game guard. Reopening Forge showed **Launch** again.

All 18 local file hashes still matched the pre-test backup. The remote revision remained 54 and history remained at one version. This run proves authenticated prelaunch comparison, native compatibility with the restricted host, UI guards and detached finalization for unchanged saves. It does **not** claim a new automatic upload or a real account-switch/network-loss test. A separate [authenticated CM upload/readback/deletion test](steam-cloud-authenticated-upload.md) verifies actual transfer completion; isolated tests cover divergent content, revocation and retry behavior.

Machine-readable observations: [lifecycle-observations.json](evidence/steam-cloud-runtime-2026-09-19/lifecycle-observations.json). The recoverable local backup stays in Forge's private `SteamCloudValidation` directory; no save bytes are committed to the repository.

## Native isolation regression

The first integrated run reached Cloud Ready but failed before native authentication. Denying the real home also denied ancestor metadata required by `current_exe().canonicalize()` in the host. A sandbox regression test reproduced that failure before the fix and passed after exact ancestor metadata allowances were added. Runtime resolution works while save content and official Steam content remain unreadable. The successful authenticated game cycle above used that fix.

## Automated verification

The final `./scripts/test.sh` run passed with 349 main-workspace Rust tests, four native-workspace tests and the separately enabled bundled-runtime isolation test. The Swift suite executed 219 tests with nine skipped and no failures. Formatting, strict Clippy, all 547 English/French localization keys and the application signature passed. Source fingerprints are recorded in the observation file above.

The local suites cover revision-bound decisions, sign-out cancellation, ownership projection recovery, immutable pending uploads, local-only restoration, uninstall history preservation and reinstallation fences. Supervisor tests run disposable processes to verify preflight order, cancellation before spawn, finalization reservation, UI detachment and authenticated recovery. Swift tests cover prompt/history visibility and account isolation.

SDK RemoteStorage persistence, FileShare and Dynamic Cloud remain unsupported. These results apply to supported metadata-driven macOS AutoCloud rules.

The later [follow-up validation](steam-cloud-next-validation.md) records save-history removal, a successful automatic upload of two game-modified files with Forge closed, controlled conflict resolution, post-exit crash recovery and a tested, restricted settlement improvement. Recovery exposed a [CM session identifier collision](steam-cloud-cm-session-isolation.md), which was fixed and checked in the rebuilt app. The source fingerprints above describe the earlier completed cycle; they do not include those later changes.
