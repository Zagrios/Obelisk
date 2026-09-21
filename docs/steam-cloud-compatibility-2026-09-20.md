# Steam Cloud compatibility, September 20, 2026

## Launch change, September 21

Normal launch now leaves Steamworks initialization to the game. Rust still
reconciles Cloud, prepares the account/app-specific native index and validates
its local records and file paths before starting the host. The native host still
checks account identity, subscription and an ownership ticket, requests app
metadata, and retains its existing filesystem restrictions. The supervisor still
waits for native shutdown before post-game capture and transfer.

The former SDK quota and byte-readback diagnostics have been removed, including
their authenticated Debug commands and the offline `sdk-pair` experiment. A
prepared local cache does not prove that the game's SDK has consumed it; that
step occurs when the game attaches. The historical authenticated evidence below
includes a separate SDK consumer and must not be presented as a live validation
of the new launch path. No Cloud transfer ownership has moved to Valve.

## Scope

This work follows the September 20 handoff: broaden AutoCloud interpretation, then add ordinary SDK RemoteStorage persistence and synchronization before launch and after native shutdown. FileShare and Dynamic Cloud are outside this scope. Save history and internal-format migrations remain removed.

## AutoCloud evidence

The normal Forge session inspected publisher metadata for all 126 owned apps. There were 100 UFS configurations, including 53 explicitly declaring macOS and 37 of those with AutoCloud rules. The private report contains remote manifests but no save contents or credentials. Checked-in fixtures contain publisher UFS metadata only; SteamID substitutions remain placeholders.

The parser now accepts omitted `recursive` as false, serialized platform lists, `addpath`, one root-relative leading slash, and lowercase `gameinstall`. Remote manifests independently establish that the latter uses the canonical `%GameInstall%` namespace. Mac-only new files retain platform mask 2. Explicit Windows/Linux rules exclude their namespaces even when older remote records carry the all-platform mask.

The changes cover Hades, Muse Dash, Nine Sols, Celeste, Melatonin, Poly Bridge 2, Brütal Legend and Thronefall without per-game branches. Unknown platforms, ambiguous transforms, traversal, symlinks, aliases and unresolved AppID redirects remain errors. A foreign-only configuration is not an empty local save set. Ruined Kingdom publishes a replacement for another game's source path and remains rejected. No new OS comparison form was observed, so unsupported comparisons remain rejected.

Vampire Survivors, Frostpunk 2 and Unturned have plain SDK filenames alongside AutoCloud configuration. The runtime routes prefixed AutoCloud names and plain SDK names separately. Unknown prefixed names remain errors.

## RemoteStorage ownership

`SdkCloudPaths` reads root-0 native cache records, maps exact relative filenames and prepares the native index before a new host starts. Unknown/unindexed local files, duplicate records, unsafe paths and unsupported persistence states fail closed. An account/app cache cannot become an empty save set merely because its index is unreadable.

The native HOME is outside the archive directory, at a sibling `<cloud-root>-sdk/<steam-id>/<app-id>`. The vault deliberately rejects save bindings inside its own archive. Its existing account ownership, conflict, capture, upload and recovery paths also manage SDK bindings.

A host may write only its app's userdata cache and appinfo cache in that HOME. Login configuration, other accounts/apps and real save roots remain denied. Native app metadata is requested using the pinned, verified Apps interface. The host checks identity, subscription and an ownership ticket, and Rust validates the local projection and its index, before the game is admitted; the game's own SDK then consumes that projection. No synthetic production identity or quota is supplied.

Rust finishes prelaunch reconciliation before native startup. The supervisor waits for game exit and native termination before capturing and transferring SDK files. Native sync, conflict and suspend/resume controls are not called. This contract covers ordinary file read/write/enumeration/deletion; it does not claim semantics for out-of-scope SDK features.

## Validation

The authenticated diagnostic uses an owned SDK-only app whose Cloud namespace is checked empty immediately before each run. Project Zomboid, AppID 108600, was used with an original Valve SDK library from a Forge-installed game. No game executable was launched.

The first probe passed native write/read, complete process restart, separate SDK readback, enumeration and deletion. Both native generations authenticated; native quota was 1,000,000 bytes. The app's remote manifest was unchanged. Rust index projection was then introduced between generations. One early run failed in the second consumer; a later run passed. Readiness then required bounded SDK byte/name checks rather than assuming quota readiness implies cache readiness. Those checks were removed with the SDK diagnostic; Rust now validates the projection and its index before the host starts.

At the time of this validation, `--steam-cloud-storage-probe APP_ID DIRECTORY SDK_LIBRARY` exercised the local persistence/index handoff. `--steam-cloud-storage-transfer-probe` additionally used two separate coordinator stores: sender SDK write and Rust upload, receiver Rust download and SDK read/delete, then Rust deletion. Both commands have since been removed. The evidence remains historical; failed transfer attempts retained their private store directory for recovery.

The authenticated transfer probe passed with two independent coordinator stores. The first SDK consumer wrote 44 bytes, Rust uploaded them, a fresh receiver downloaded them, and the second SDK consumer enumerated/read the exact bytes and deleted the file. Rust then completed remote deletion. Both comparisons made after native shutdown and before Rust finalization found the remote manifest unchanged. Two Rust synchronization cycles completed; no active remote fixture remained. These observations cover ordinary write/delete at the measured boundaries, not every native API. See [redacted roundtrip evidence](evidence/steam-cloud-runtime-2026-09-20/sdk-cloud-roundtrip.json).

The complete `./scripts/test.sh` passed: 400 Rust tests across its runs (six ignored), 225 Swift tests executed (nine skipped, zero failures), bundled-runtime isolation, formatting, localization and signature checks. Subsequent account-scope and conflict assertions passed in the main workspace: 397 tests, six ignored. Strict workspace Clippy passed. SDK cache byte verification was bounded to ten seconds and 100 MiB per file, with larger files failing admission instead of starting with unverified cache contents; that verification no longer runs on the launch path.

The normal launch path is not proven by an isolated SDK test alone. A real game cycle and a Mac/Steam Deck confirmation remain separate checks. The previous Silksong device cycle remains valid for the earlier AutoCloud implementation; it does not establish this new SDK behavior.

Primary reference: [Valve Steam Cloud configuration](https://partner.steamgames.com/doc/features/cloud). Pinned native ABI evidence: [bootstrap investigation](steam-cloud-gamenative-bootstrap.md) and [transfer ownership](steam-cloud-transfer-ownership.md).
