# GameNative bootstrap and native metadata preparation

Research date: 2026-09-19. This investigation used public source and static disassembly. No downloaded program was executed, no Steam account was authenticated, and no real save or local Steam installation was read. The findings describe behavior and ABI facts; no GameNative implementation was copied into Forge.

## Provenance

GameNative source is pinned to [`88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52`](https://github.com/utkarshdalal/GameNative/tree/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52). The repository itself contains [`app/src/main/jniLibs/arm64-v8a/libsteambootstrap.so`](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/jniLibs/arm64-v8a/libsteambootstrap.so), so an APK was unnecessary. That file is a stripped ARM64 ELF executable, 32,776 bytes, SHA-256 `58a7473135e1960366146004ffe04f0e42d61e70e6a8122648318d87275f7f7e`. Addresses in the Android analysis below are ELF virtual addresses in that file.

The [third-party notice](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/THIRD_PARTY_NOTICES#L105-L189) declares the bootstrap proprietary and its source withheld. The surrounding Kotlin/Java application is GPL-3.0. The notice's JNI description is stale relative to the current Kotlin process launcher and ELF executable; source and binary take precedence for behavior.

The native artifact, complete `llvm-objdump -d` output, section map, and string cross-references are in the temporary directory `/var/folders/bj/bjxppt2d0jg9ppj9_bkdsz580000gn/T/forge-gamenative-bootstrap-static-lsce5etf/`. Public source snapshots from the earlier investigation remain under `build/steam-cloud-feasibility/gamenative/` and `/var/folders/bj/bjxppt2d0jg9ppj9_bkdsz580000gn/T/forge-gamenative-cloud-ne_nxn4v/`.

## Identity and filesystem inputs

[`SteamBootstrap.kt`](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/app/gamenative/SteamBootstrap.kt#L56-L157) starts a child with three arguments: app ID, Valve library path, and readiness-file path. It passes `HOME`, `Steam3Master`, `SteamClientService`, optional `SB_ACCOUNT`, `SB_REFRESH_TOKEN`, and `SB_STEAMID64`, plus selected environment values. `prepareApp` stops the previous host first. It watches `sb_host_ready` for up to 60 seconds and redirects output to `sb_host.log`. Its status parser accepts `INIT`, `READY`, `FAILED:...`, and `CLOSED`; readiness is not a Cloud completion result.

The [Bionic launcher](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/com/winlator/xenvironment/components/BionicProgramLauncherComponent.java#L530-L681) sets native `HOME` to the parent of the Wine prefix's `Steam` directory and `STEAM_BASE_FOLDER` to that directory itself. It supplies loopback IPC endpoints and the application's persisted account name, refresh token, and full SteamID. Windows bridge paths and `SteamAppId`/`SteamGameId` are supplied separately. This is initialized account material, not the account-ID-zero disconnected state observed in Forge's initial fixture. These Android/Wine environment choices are not assumed portable to macOS.

Static ELF references at `0x34ac`–`0x36e4` and `0x5804`–`0x5ad4` show per-account session preservation under `/data/data/<package>/files/imagefs/.steambootstrap/<SteamID64>`, with `config.vdf` and `local.vdf` restoration/saving. Actual symlink, rename, stat, and write imports are called around those paths. The binary also has a cached-credential login path at `0x5b2c`–`0x5c60`, including cached-credential checks, account-name selection, and logon. None of these files is an app metadata or Cloud file index.

The public [`SteamTokenLogin.kt`](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/app/gamenative/utils/SteamTokenLogin.kt#L78-L155) can write a text `config.vdf` using `InstallConfigStore/Software/Valve/Steam`, `MTBF`, `ConnectCache`, and `Accounts/<login>/SteamID`. A separate local configuration uses `MachineUserConfigStore` and `ConnectCache`. Its token transformation is not needed to study Cloud and was not reproduced. The [loginusers writer](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/app/gamenative/service/SteamService.kt#L3341-L3371) emits identity and remembered-session flags; despite accepting token parameters, that shown writer does not serialize their contents. Exact caller coverage across launch modes remains unverified.

## Native PICS preparation is the useful precedent

The bootstrap does not rely solely on GameNative's Kotlin app database. In its normal preparation path, it obtains Valve's internal interfaces and asks the native runtime to update app information. The [launcher comments](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/com/winlator/xenvironment/components/BionicProgramLauncherComponent.java#L661-L680) describe this preparation; disassembly establishes the following operations independently:

| Android ELF address | Observed operation |
|---|---|
| `0x4350`–`0x43f0` | Creates Engine005 and obtains Apps, AppManager, User, and Friends through engine slots 17, 43, 8, and 13. |
| `0x4528`–`0x4544` | Calls Apps slot 7 with an app-ID array and count. The adjacent result log identifies `RequestAppInfoUpdate`. |
| `0x4598`–`0x4658` | Pumps `Steam_BGetCallback` / `Steam_FreeLastCallback` until callback ID `0x1360f3`, decimal 1,270,003. The adjacent message identifies `AppInfoUpdateComplete_t`. |
| `0x490c`–`0x4a30` | Requests ownership-ticket updates through User slot 69, with up to ten immediate attempts per app. |
| `0x4b40`–`0x4b88` | Calls AppManager slot 2, identified as `LaunchApp`, and logs the returned job ID. |

The PICS wait checks the callback ID, not its result payload or requested app IDs. It has no visible time bound in that loop. Missing callback exports can bypass the wait and still reach the primed flag/log at `0x465c`. Thus the sequence is a useful implementation precedent, while its `PICS primed` and `READY` messages alone are insufficient proof of usable UFS data.

The binary also matches three native library builds by the image-relative `CreateInterface` address, then uses build-specific offsets for DLC preparation and OS identity changes. Those Android offsets are not proposed for Forge. The [asset loader](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/app/gamenative/utils/launchdependencies/BionicSteamAssetsDependency.kt#L30-L55) requests `steam-androidarm64-20260709.tzst` and Proton-version-specific bridges. That runtime archive was not downloaded or analyzed here.

The inspected bootstrap contains no `appinfo.vdf`, `remotecache.vdf`, UFS-quota, or explicit Cloud-inhibition string. Its preparation path establishes native PICS requests, not direct injection of Kotlin UFS records. Absence of these strings does not prove the Valve library performs no Cloud work. In particular, `LaunchApp` may have lifecycle effects beyond ticket preparation; those effects were not followed in the Android runtime.

## What the public Cloud writer supplies

[`PathType.SteamUserData`](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/app/gamenative/enums/PathType.kt#L32-L39) resolves to the prefix's `Steam/userdata/<accountID>/<appID>/remote`. After a reported successful or up-to-date Cloud sync, [`SteamAutoCloud`](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/app/gamenative/service/SteamAutoCloud.kt#L1134-L1148) writes `remotecache.vdf` in the parent directory.

The [writer](https://github.com/utkarshdalal/GameNative/blob/88a60ef5fa3f25b9d0dfe0a0d041ba70d7873e52/app/src/main/java/app/gamenative/service/SteamAutoCloud.kt#L1377-L1452) emits text KeyValues keyed by app ID, with `ChangeNumber`, `OSType`, and one entry per Cloud filename. Entries contain root, size, timestamps, SHA, sync/persistence states, and platform mask. This implementation writes root 0 for every entry and its caller supplies syncstate 1. These are observed producer values, not validated meanings for Forge's macOS runtime. Errors are logged and swallowed. The native bootstrap does not visibly invoke a RemoteStorage cache-loader method after this write, and this investigation did not prove which Cloud lifecycle calls run in Bionic mode. Therefore shared directory names do not establish a validated runtime cache handoff or exclusion of duplicate synchronizers.

## Independently verified macOS equivalents

The following evidence comes from Forge's pinned ARM64 `steamclient.dylib`, SHA-256 `035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840`, not from Android slot assumptions. Addresses are macOS image-relative. The matching thin image and disassembly are in `/var/folders/bj/bjxppt2d0jg9ppj9_bkdsz580000gn/T/forge-cloud-static-w0ewwrzw/`.

- Engine005 vtable `0x1753a20`, slot 17, points to `0xa02e4c`. ABI is `(this, user:i32, pipe:i32) -> nullable pointer`. It returns the matched record's member `+0x50` at `0xa02ed4`.
- Record constructor `0xab53a4` calls Apps proxy constructor `0x88f8cc` at `0xab5638` and stores its result at `+0x50`. That constructor installs vtable `0x17431f0` at `0x88f92c`.
- Apps slot 7 is `RequestAppInfoUpdate`, function `0x8dd498`. ABI is `(this, app_ids:*const u32, count:i32) -> bool`. It serializes count as four bytes at `0x8dd518`–`0x8dd528`, copies `count << 2` bytes from the array at `0x8dd52c`–`0x8dd538`, and decodes one-byte success at `0x8dd5a0`, normalized at `0x8dd5f8`. The Boolean result is not update completion.
- Callback emitter `0xa156cc` independently dispatches ID `0x1360f3` with **12 bytes** at `0xa15700` and `0xa15730`. Caller `0xa155e0` constructs three consecutive 32-bit words at `0xa15698` and `0xa1569c`. The first word follows a result-code convention, with value 1 taking the success branch at `0xa15640`. The last two words' meanings were not established; retain them as raw values. No request-specific correlation is proven by the callback ID alone.

### The account-ID-zero cache failure has a separate guard

The later macOS probe supplied app metadata and observed a successful local write/read, but cache initialization after restart logged `Sync Failed, no user set`. Static function `0x615574` explains a prerequisite matching that observation. It reads the job's owner pointer at `+0x1c0`, then the full identity at owner `+0x27a`. After account-type/universe validation, the Individual branch at `0x615690` rejects a zero low-32-bit account ID at **`0x615698`**. That branch reaches the no-user log at `0x615644`. It also checks that the Individual instance is 1 at `0x61569c`–`0x6156a8`.

This is distinct from the earlier, withdrawn hypothesis that the account type was Invalid. A full SteamID can be nonzero, with type 1 and instance 1, while its account ID is zero. It can pass the queue runner's type check and fail this later job validity check. This evidence identifies the relevant local prerequisite; it does not authorize or require borrowing a real Steam identity, and it does not establish remote Cloud behavior.

The parent probe subsequently completed private local write/read/list and process-restart readback with synthetic app metadata and the disconnected test identity. Rust-created file/index projection also passes before cache initialization. Replacing the index after it has been loaded fails and the runtime deletes that replacement. See the [reproducible offline observations](evidence/steam-cloud-runtime-2026-09-19/README.md) for the final sequence and its limits. This does not establish authenticated Cloud persistence or Dynamic synchronization.

For Forge, the concrete lesson is to prepare and verify native app metadata and identity before judging cache handoff. The Android implementation supplies a PICS preparation precedent. It does not establish that Forge can silently suppress Valve synchronization, interpret Android cache states as macOS states, or claim Dynamic Cloud Sync completion from host readiness.
