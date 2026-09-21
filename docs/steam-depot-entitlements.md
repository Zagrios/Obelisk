# Steam depot entitlements

September 20, 2026. Dead Cells installation failed with Steam result 15 before downloading any files. The main Mac depot, 588652, returned a valid key. The additional Mac depot, 1046443, returned access denied. Its publisher metadata has a public manifest but no `dlcappid`. The account's base-game package grants app 588650 and depots 588651 through 588654.

Forge previously retained only `appids` from active PICS packages. Depot selection treated an absent `dlcappid` as sufficient permission, so the unauthorized depot blocked the entire installation and inflated the download size.

The content catalog now retains `appids` and `depotids` separately. Selection checks depot membership before choosing language or architecture, while retaining explicit DLC ownership checks. Publisher-declared `FreeToDownload` depots can still be selected without package membership. Steam continues to authorize each key, manifest and CDN request; a server refusal is never silently ignored.

The regression using the observed depot IDs failed before the selection fix, returning `[588652, 1046443]` instead of `[588652]`. All 126 `steam-kit` library tests passed afterward, with five ignored. Additional tests cover distinct app/depot namespaces, explicit DLC ownership, free-download selection and language/architecture fallback. Strict Clippy passed. Temporary diagnostics recorded no keys or credentials and were removed from the implementation.

The package-depot check is also used by [SteamRE's DepotDownloader](https://github.com/SteamRE/DepotDownloader/blob/master/DepotDownloader/ContentDownloader.cs), in `AccountHasAccess`. The reproduction and regression above use Forge's own authenticated session and selection code.

The rebuilt, signed Forge app completed the real Dead Cells macOS installation: 2,106,994,403 verified bytes from depot 588652, manifest 620007880138288496. The UI showed `Lancer` and confirmed that downloading and file verification had finished. The additional depot was excluded; no access-denied response was suppressed. The game was not launched during this installation check.

## Standalone Mac executable follow-up

The first launch then exposed a separate permission issue: the verified `deadcells` Mach-O executable had mode 0644. The launcher repaired missing owner-execute permissions only for bundle executables. It now also recognizes standalone Mach-O binaries through `CFBundleCopyExecutableArchitecturesForURL`, then adds only owner execute. Non-executable scripts and ordinary files remain rejected. The new regression failed with the actual launch error before the fix; all 14 launcher tests passed afterward, including permission preservation and repeated repair after verification. The signed app was reopened and the real binary reached mode 0744 through the normal launch path.

The real retry passed executable resolution and Cloud restoration, then failed at SDK library loading before game launch. Dead Cells ships an i386/x86_64 Steam API library; Forge currently ships an ARM64-only verifier and discovery adapter. Its library also predates `SteamAPI_InitFlat` and the versioned interface getters used by the verifier. Intel consumer support and older SDK interface access need separate compatibility work. This retry does not validate a real Dead Cells gameplay cycle.

The subsequent [Intel and Rosetta implementation](steam-rosetta-intel.md) added architecture-specific helpers and a universal adapter. It preserves the depot selection above. Normal launch now leaves SDK initialization to the game, and the SDK diagnostics and their legacy profile have been removed. The original failure describes the earlier binary; live launch evidence is recorded in the linked implementation report.
