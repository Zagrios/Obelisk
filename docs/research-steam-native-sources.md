# Native Steam host source investigation

Checked on 2026-09-16. This note investigates a host for native macOS Steam games, without launching the complete Steam client. It supplements [the earlier GameNative note](research-gamenative-steam.md) and [the store services note](research-store-services.md). No Steam process or game was started during this source investigation. It does not report a working macOS login or game launch.

This is a historical source and protocol investigation. The later standalone
prototypes have been removed after their integration into Forge. See
[native Steam launches](steam-native-launch.md) for the current Rust implementation
and [archived evidence](evidence/steam-native-research/README.md) for the original
observations. Prototype names below identify past test artifacts.

## Fixed source snapshot

GameNative's remote HEAD resolved to [`a9b078ab75bcec7a042a0bb5203b1a07e57fd073`](https://github.com/utkarshdalal/GameNative/tree/a9b078ab75bcec7a042a0bb5203b1a07e57fd073), newer than the snapshot in the earlier note. All GameNative links below use this commit. Public files and one binary were downloaded into `/tmp/forge-steam-sources` for inspection; nothing from them was installed into Forge.

The decisive C source remains absent. The repository contains its [CMake file](https://github.com/utkarshdalal/GameNative/blob/a9b078ab75bcec7a042a0bb5203b1a07e57fd073/app/src/main/cpp/steambootstrap/CMakeLists.txt), [Kotlin controller](https://github.com/utkarshdalal/GameNative/blob/a9b078ab75bcec7a042a0bb5203b1a07e57fd073/app/src/main/java/app/gamenative/SteamBootstrap.kt), and [ARM64 binary](https://github.com/utkarshdalal/GameNative/blob/a9b078ab75bcec7a042a0bb5203b1a07e57fd073/app/src/main/jniLibs/arm64-v8a/libsteambootstrap.so). The [ignore rule](https://github.com/utkarshdalal/GameNative/blob/a9b078ab75bcec7a042a0bb5203b1a07e57fd073/.gitignore#L64-L69) explicitly excludes `steam_bootstrap.c`.

## What the current application actually does

The [third-party notice](https://github.com/utkarshdalal/GameNative/blob/a9b078ab75bcec7a042a0bb5203b1a07e57fd073/THIRD_PARTY_NOTICES#L105-L180) calls the bootstrap proprietary and describes an in-process JNI module. That mechanism is stale. CMake builds an executable, then names it `libsteambootstrap.so`. The Kotlin controller starts it with `ProcessBuilder`, passing App ID, client-library path, and a readiness-file path. It supplies an isolated home, IPC endpoint variables, account name, refresh token, and Steam ID. It waits up to 60 seconds for readiness. Shutdown sends SIGTERM and waits before using `destroy()`. This is a separate native host process. [Controller implementation](https://github.com/utkarshdalal/GameNative/blob/a9b078ab75bcec7a042a0bb5203b1a07e57fd073/app/src/main/java/app/gamenative/SteamBootstrap.kt#L92-L185).

Asset installation downloads `steam-androidarm64-20260709.tzst`, `steamclient-dlls-20260619.tzst`, Proton-specific `lsteamclient` packages and a Steam helper executable. The downloader uses GameNative's download domain with an R2 fallback. Thus, the notice's reference to the user's existing Steam installation does not fully describe the current dependency-installation path. This establishes the implementation's behavior, not the distribution rights for those files. [Asset versions and installation](https://github.com/utkarshdalal/GameNative/blob/a9b078ab75bcec7a042a0bb5203b1a07e57fd073/app/src/main/java/app/gamenative/utils/launchdependencies/BionicSteamAssetsDependency.kt#L19-L76), [archive extraction](https://github.com/utkarshdalal/GameNative/blob/a9b078ab75bcec7a042a0bb5203b1a07e57fd073/app/src/main/java/app/gamenative/utils/launchdependencies/BionicSteamAssetsDependency.kt#L219-L304), [download locations](https://github.com/utkarshdalal/GameNative/blob/a9b078ab75bcec7a042a0bb5203b1a07e57fd073/app/src/main/java/app/gamenative/service/SteamService.kt#L2147-L2169).

The Wine launcher supplies `Steam3Master=127.0.0.1:57343` and `SteamClientService=127.0.0.1:57344`, the game identity, library paths, and initialization variables. These are Android/Wine integration details, not a documented macOS discovery contract. Native macOS games already use Mach-O `libsteam_api.dylib`; a Windows-to-native `lsteamclient` bridge is outside this prototype's scope. [Environment construction](https://github.com/utkarshdalal/GameNative/blob/a9b078ab75bcec7a042a0bb5203b1a07e57fd073/app/src/main/java/com/winlator/xenvironment/components/BionicProgramLauncherComponent.java#L532-L614).

## Authentication evidence and native implementation clues

The account protocol and native-host authentication are separate operations. GameNative obtains `accessToken` and `refreshToken`, then passes the **refresh token** into JavaSteam's misleadingly named `LogOnDetails.accessToken` field. Its `getLoginUsersVdfOauth` accepts token arguments but does not serialize either token into the returned VDF. Reading comments or argument names alone would misidentify this flow. [Login implementation](https://github.com/utkarshdalal/GameNative/blob/a9b078ab75bcec7a042a0bb5203b1a07e57fd073/app/src/main/java/app/gamenative/service/SteamService.kt#L3332-L3469).

The separate token-preparation implementation writes a `ConnectCache` entry and an `MTBF` value into `config.vdf`. Its second path uses a Windows helper under Wine to prepare `local.vdf`. This is evidence of credential-cache preparation before native login; it does not establish the format or encryption required by the current macOS client. Forge should not transplant this cache-writing code into a macOS host without independently checking that format. [SteamTokenLogin](https://github.com/utkarshdalal/GameNative/blob/a9b078ab75bcec7a042a0bb5203b1a07e57fd073/app/src/main/java/app/gamenative/utils/SteamTokenLogin.kt#L58-L140), [cache selection and writes](https://github.com/utkarshdalal/GameNative/blob/a9b078ab75bcec7a042a0bb5203b1a07e57fd073/app/src/main/java/app/gamenative/utils/SteamTokenLogin.kt#L157-L282).

Static inspection of the public bootstrap binary found an AArch64 ELF PIE executable with `/system/bin/linker64`, not a macOS dylib. Its SHA-256 is `58a7473135e1960366146004ffe04f0e42d61e70e6a8122648318d87275f7f7e`. Strings identify `CLIENTENGINE_INTERFACE_VERSION005`, global-user creation, cached-account selection, login, ownership preparation, and callbacks. They also identify `SteamService_StartThread`, `SteamService_GetIPCServer`, `SteamService_Stop`, and `SteamService_Shutdown`. A diagnostic distinguishes cross-process IPC initialization from a thread-only fallback. Other messages mention `BHasCachedCredentials`, `SetAccountNameForCachedCredentialLogin`, PICS, `BUpdateAppOwnershipTicket`, and `LaunchApp`. These are leads for comparing the local macOS binary; strings do not establish call order, signatures, vtable slots, or successful execution. [Inspected binary](https://github.com/utkarshdalal/GameNative/blob/a9b078ab75bcec7a042a0bb5203b1a07e57fd073/app/src/main/jniLibs/arm64-v8a/libsteambootstrap.so).

Historical headers are publicly available in SteamRE's archived repository at commit [`f65c0439bf06981285da1e7639de82cd760755b7`](https://github.com/SteamRE/open-steamworks/tree/f65c0439bf06981285da1e7639de82cd760755b7). Its `IClientUser` includes cached-credential methods and `SetLoginInformation`, and its `IClientEngine` includes user, pipe and interface acquisition. Both are marked `UNSAFE_INTERFACE`. They provide names for investigation, not a current ABI specification. No slot in these headers was validated against the macOS library in this research. [IClientUser](https://github.com/SteamRE/open-steamworks/blob/f65c0439bf06981285da1e7639de82cd760755b7/Open%20Steamworks/IClientUser.h#L62-L134), [IClientEngine](https://github.com/SteamRE/open-steamworks/blob/f65c0439bf06981285da1e7639de82cd760755b7/Open%20Steamworks/IClientEngine.h#L67-L105).

## IPC and callback responsibilities

GameNative's UI talks to its host using an abstract Unix socket named `gamenative-steam-overlay`, with localhost TCP port 57349 as a fallback for its full-Steam mode. Commands cover availability, account information, friend listing, outgoing invites, accepted lobby/rich-presence invites and overlay requests. Accepted invitations ask the engine to deliver the game's callback. This separate control connection does not replace the Steamworks client IPC. Its server implementation remains in the closed binary. [SteamOverlayClient](https://github.com/utkarshdalal/GameNative/blob/a9b078ab75bcec7a042a0bb5203b1a07e57fd073/app/src/main/java/app/gamenative/service/SteamOverlayClient.kt#L11-L194).

Valve documents that `SteamAPI_RunCallbacks` dispatches the game's callbacks and call results. A host must also service its own client events and IPC, which is an engineering requirement inferred from GameNative's host architecture. A functioning launcher UI or account protocol session is insufficient evidence that another game process receives Steamworks callbacks. [Public callback contract](https://partner.steamgames.com/doc/api/steam_api#SteamAPI_RunCallbacks).

## Public Steamworks contract and what it leaves unanswered

Valve's initialization requirements include a running client implementation, the correct App ID, the same OS user context, and a valid license. This establishes prerequisites, not a documented requirement to display the desktop interface. The public documentation does not provide a recipe for a third-party macOS process to initialize an authenticated standalone client engine. [API overview](https://partner.steamgames.com/doc/sdk/api#SteamAPI_Init).

`ISteamClient` creates communication pipes and attaches to an existing global user. `ConnectToGlobalUser` fails when none exists; `CreateLocalUser` is documented for game servers. These functions should not be treated as a public consumer-account login flow. The native global-user bootstrap remains a distinct problem. [ISteamClient](https://partner.steamgames.com/doc/api/ISteamClient).

`SteamAPI_RestartAppIfNecessary` may start Steam and relaunch the game through `steam://run/<AppID>`. A development `steam_appid.txt` causes it to return false, but supplies neither authentication nor a Steamworks implementation. For a prototype explicitly excluding the full client, avoid invoking this function until discovery and the host are understood; launching an arbitrary game first can start Steam indirectly. [Restart behavior](https://partner.steamgames.com/doc/api/steam_api#SteamAPI_RestartAppIfNecessary).

The exact macOS client discovery mechanism, including its file paths, process registration, IPC naming and fallback behavior, was not resolved by these public documents. It must be established from the specific local `libsteam_api.dylib` and `steamclient.dylib` versions, then tested with a separate process. An in-process interface pointer alone does not establish discoverability by a game.

Steam session tickets, encrypted app tickets, refresh tokens and web access tokens are different credentials. Valve describes session-ticket and encrypted-app-ticket flows used by games and their servers. Those documents do not describe signing an independent client engine in with Forge's existing account session. Do not use possession of a ticket as proof of a completed client logon. [Authentication and ownership](https://partner.steamgames.com/doc/features/auth).

## Distribution facts to keep separate from feasibility

Valve's SDK documentation identifies `public/steam` headers and `redistributable_bin` Steamworks API binaries. Its macOS instructions describe shipping `libsteam_api.dylib` with a game's executable. These pages do not establish blanket redistribution rights for `steamclient.dylib`, `steamservice.dylib` or a modified desktop-client package. [SDK contents](https://partner.steamgames.com/doc/sdk), [macOS integration instructions](https://partner.steamgames.com/doc/sdk/api#Getting_Started).

The Steam Subscriber Agreement includes restrictions on copying, distribution and reverse engineering, with qualifications for other permissions and applicable law. The open-source distribution page also identifies license compatibility as the distributor's responsibility. GameNative's own proprietary-bootstrap notice is not a Valve license grant. No legal conclusion follows from this investigation; distribution permission remains unverified. [Subscriber agreement, section 2](https://store.steampowered.com/subscriber_agreement/#2), [open-source distribution guidance](https://partner.steamgames.com/doc/sdk/uploading/distributing_opensource).

## Consequence for the macOS experiment

The public-source evidence alone supports testing a separate native host that owns the client engine and services game IPC. The subsequent authenticated SDK experiment below supplies the missing dynamic proof: a separate consumer resolves the intended host, receives the expected Steam ID and completes a real asynchronous API request while the full Steam client remains stopped. The main [prototype report](research-steam-native-host.md) also records the later Silksong main-menu smoke test. Server-persisted game features still require their own observations.

## Silksong macOS SDK discovery, static inspection

The installed game supplied this additional local primary source:

`/Users/mathieu/Library/Application Support/com.zagrios.forge/Games/Steam/steam-1030300-macos/Hollow Knight Silksong.app/Contents/PlugIns/steam_api.bundle/Contents/MacOS/libsteam_api.dylib`

It is a universal x86_64/ARM64 Mach-O library. `lipo -thin arm64` produced `/tmp/forge-steam-sources/silksong-libsteam_api-arm64.dylib`. Inspection used `file`, `shasum`, `strings`, `nm`, and bounded `llvm-objdump --disassemble` calls. Neither copy was loaded or executed for this analysis. Addresses below are unslid virtual addresses in this ARM64 image, not portable offsets for another SDK version.

| Artifact | SHA-256 |
| --- | --- |
| Original universal library | `fef6bd7a0bb606e8f3556eee203bd04c5590525add592158b788c8af5c109ba6` |
| Extracted ARM64 library | `11ee013fe8c78d7f7af062b27e14601ae7b8453b9fb12004c4bbc9e1a67e6d52` |

The exported initialization functions differ from the older public example. `nm -gU` finds `SteamAPI_InitFlat` at `0x38a8`, `SteamInternal_SteamAPI_Init` at `0x38b4`, and `SteamAPI_InitSafe` at `0x3e44`; it does not find a `SteamAPI_Init` export. The underscore printed by `nm` is the Mach-O symbol prefix and is omitted in `dlsym` names.

The following calling shapes follow directly from the ARM64 wrappers:

- `SteamAPI_InitFlat` takes one pointer to an optional error buffer and returns an integer result. At `0x38a8` it passes that pointer as argument two to `SteamInternal_SteamAPI_Init`, with argument one null. The implementation copies at most 1,023 bytes and adds a terminator. A probe should provide a 1,024-byte buffer and treat result zero as initialization success.
- `SteamAPI_InitSafe` takes no argument and returns a Boolean. At `0x3e70` it invokes the shared initializer with mode zero and a private error buffer; at `0x3e80` it tests for integer result zero and also requires the cached client pointer to be nonnull.

For a dynamically resolved C probe, `int (*)(char *)` expresses the observed `InitFlat` ABI and `bool (*)(void)` expresses `InitSafe`. These are binary-derived descriptions of this specific library, not replacement SDK headers.

### Discovery path and environment limits

The ordinary `InitFlat` path reaches the loader at `0x3504`, which calls the path-discovery routine at `0x3088`. That routine initializes an IPC connection, requests a path and PID, checks the PID using `kill(pid, 0)`, and replaces the returned path's final component with `steamclient.dylib` at `0x3278`. The library contains the Mach service name `com.valvesoftware.steam.ipctool` at `0xb327` and imports `bootstrap_look_up`, `bootstrap_port`, `mach_msg`, Mach-port functions and legacy launchd calls. This is direct evidence of a macOS Mach/launchd discovery dependency, separate from the engine's TCP endpoints.

The loader only calls `dlopen` on the discovered path after discovery succeeds. On failure it writes the directory diagnostic at `0x36d4`; the string is at `0xb4d4`. A separate branch at `0x3590` attempts the bare name `steamclient.dylib`, but the branch depends on an initialization-mode flag. `InitFlat` and `InitSafe` select mode zero and do not enable that branch. The anonymous-user initializer selects the other mode. That is not an authenticated consumer-user workaround.

No strings for `SteamClientDll`, `STEAM_CLIENT_PATH`, `SteamClientLaunch`, `Steam3Master`, `SteamClientService`, or `registry.vdf` were found in this SDK image. No path override through those names was found in the inspected loader. This is a bounded negative result, not proof that all Steam libraries lack overrides. `DYLD_LIBRARY_PATH` cannot supply a missing discovery result before `dlopen` is reached in this path. Providing a TCP engine endpoint alone also does not supply the required macOS discovery response.

A Forge-owned discovery service remains a hypothesis requiring its own protocol and namespace investigation. The existing Steam Mach service must not be mistaken for an independent Forge host in a two-process test. This inspection did not register, replace, or contact that service.

### Restart behavior in this SDK

`SteamAPI_RestartAppIfNecessary` is exported at `0x3ed4`. At `0x3f20` it reads `SteamAppId`; the observed branch returns false when that variable parses to a nonzero integer within the accepted string-length bound. Otherwise it checks `steam_appid.txt`. Later branches build a `steam://run/` URL, format `/usr/bin/open '%s'` using the string at `0xacb1`, and call `system` at `0x42ac`. This confirms a concrete route for unintentionally launching the full Steam client.

Setting an App ID affects this restart check, but does not resolve the separate Mach discovery, engine authentication or license checks. The investigation therefore keeps the game-side probe limited to `InitFlat`, without calling the restart function or modifying the game's SDK library.

## Discovery responder and controlled-consumer adapter

A further bounded experiment mapped the SDK's discovery messages and implemented a separate responder. Its maintained Rust counterparts are now `crates/steam-native/src/host.rs`, `crates/steam-native/src/mach.rs` and `crates/steam-native/adapter/src/lib.rs`; the standalone discovery launcher was removed. The adapter redirects only the lookup of `com.valvesoftware.steam.ipctool` to a unique Forge-owned `com.zagrios.forge.discovery.*` Mach service. It leaves other lookups alone. The experiment used a process-local `DYLD_INSERT_LIBRARIES` adapter in a controlled diagnostic consumer; it did not replace, modify or stop the existing Steam service. The game-side Steamworks dylib and native client dylib remained byte-identical. The authenticated results in this report are from the original C++ implementation; the later Rust integration is documented separately.

The adapter changes discovery only. The responder verifies the declared host executable against `proc_pidpath(host_pid)` and checks that the process is alive. It returns the real host's PID and canonical executable path, from which the SDK constructs its adjacent `steamclient.dylib` path. It does not claim that initialization, login, ownership or any Steamworks call succeeded. Those results still come from Valve's libraries. When the host disappears it returns PID zero and an empty path, allowing the consumer to observe loss of the host.

### Wire format established for this SDK

| SDK ARM64 address | Observation |
| --- | --- |
| `0x332c` | Initializes discovery using name `com.valvesoftware.steam.ipctool` and protocol identifier 104. |
| `0x5484` | Calls `bootstrap_look_up` to obtain the discovery service's Mach send right. |
| `0x5528` | Sends a 32-byte message with opcode 100001 at byte 24 and version 104 at byte 28. Receive capacity is 40 bytes, including an 8-byte Mach trailer: the reply message is 32 bytes, with responder version at byte 28. |
| `0x336c` | Requires responder version at least 104. A lower version reaches the restart path at `0x5594`. |
| `0x5594` | Sends opcode 100000, and has a legacy launchd `StopJob` fallback. The prototype does not implement this opcode. |
| `0x33e4` | Sends opcode 14 in a 32-byte request. Receive capacity is 552 bytes, including an 8-byte Mach trailer: the reply message is 544 bytes, with PID at byte 28 and a 512-byte null-terminated path field at byte 32. |
| `0x5714` | Fills the Mach header with protocol identifier 104 as `msgh_id`, server destination, and a reply receive port supplied as a send right. |
| `0x5670` | Waits for replies and ignores messages whose `msgh_id` does not equal 104. |

The former `discovery-diagnostic` executable exercised those two supported messages without loading any Steam library. The launcher rejected unexpected message sizes, complex messages, protocol identifiers and opcodes. Replies announced protocol support, a verified process and a verified path only. The maintained message validation and its unit test now live in `crates/steam-native/src/mach.rs`.

### Why the experiment uses a unique named service

Two inheritance approaches were tested first, using disposable helper processes and diagnostic ports. `bootstrap_subset` returned 125 with no subset port on this machine. Assigning a private task bootstrap port did direct a fork/exec child's lookup request to a private receiver, but modern libxpc rejects an ordinary replacement bootstrap responder. Local disassembly of `_xpc_interface_routine` found the audit-token check at function offsets `+388` through `+540`, including the diagnostic that the bootstrap port must lead to PID 1. No audit token or system library was altered.

An attempted `mach_ports_register` handoff did not preserve the desired registered send right through this fork/exec sequence. Slot zero resolved to a bootstrap port and the tested extra slot was null. These attempts did not establish a usable private namespace. The working experiment consequently registers its own unique service with the real launchd and unregisters that same name when the consumer finishes. No registration uses Steam's service name.

Apple's published bootstrap implementation documents the distinction between subset creation, registration and lookup; those historical sources do not guarantee current availability. The installed SDK marks subset creation and registration deprecated. Chromium's own historical sandbox headers document the private XPC pipe declarations considered during the investigation. These sources explain the investigation, while the current-machine observations above determine which prototype path worked. [Apple bootstrap implementation](https://github.com/apple-oss-distributions/launchd/blob/main/liblaunch/libbootstrap.c), [Chromium XPC declarations](https://chromium.googlesource.com/chromium/src.git/+/62.0.3178.1/sandbox/mac/xpc.h).

### Historical test setup and measured scope

The original C++ sources were compiled with `clang++ -arch arm64 -std=c++17 -Wall -Wextra -Werror`, with `-dynamiclib` for the adapter. Those sources and their standalone Rust replacements have been removed. This records the former launcher's argument contract, not a current build instruction:

```text
discovery-launcher <host_pid> <host_executable_path> <adapter_dylib_path> <consumer_executable> [consumer_arguments...]
```

The launcher preserves the consumer's stdin and argument vector, directs only that child's dynamic loader to the adapter, and runs the responder until the child finishes or 90 seconds elapse. For the Steamworks probe, the consumer argument is the existing API dylib path. The discovery-only diagnostic requires no argument. The native host executable must sit next to the intended `steamclient.dylib`, because that is how this SDK constructs the client-library path.

The transport-only test used `/bin/sleep` as an explicitly unrelated, real host process. It returned `discovery_diagnostic_ok=1`, protocol version 104, that process's actual PID, and `/bin/sleep`. The launcher counted one version request, one path request, zero rejected messages, a completed consumer, and exit status zero. This proves private service selection and the mapped message format. It proves no Steam authentication or SDK initialization. The independent native-host document records any combined authenticated tests performed by the main investigation.

The first responder incorrectly sent 40- and 552-byte messages, confusing receive-buffer capacity with wire-message size. The first diagnostic also allocated spare trailer space, masking the mistake. A real SDK probe failed directory discovery. Correcting the replies to 32 and 544 bytes, and restricting the diagnostic's receive capacities to the SDK's exact 40 and 552 bytes, resolved this discrepancy. Both sources now use those corrected limits. Numeric request logs contain only protocol identifier, message size and opcode.

A separate scratch executable then loaded the unmodified ARM64 API library and called only `SteamAPI_InitFlat`, still announcing the actual `/bin/sleep` process. Its sandbox denied all network access, file writes other than `/dev/null`, and the global Steam Mach lookup. It returned `Failed to load module '/bin/steamclient.dylib'`, with two version requests, two path requests and zero rejected messages. That expected failure proves the real SDK passed discovery and constructed the adjacent client-library path; `/bin/sleep` does not have a client library next to it. No credential, Steam engine, full client or game was used in that diagnostic.

The adapter was initially an experiment-specific dependency and is now part of the integrated Rust host. Hardened-runtime settings, library validation, restricted process launch and per-game loader behavior may prevent injection. A successful controlled probe establishes a functioning service path for that probe, not universal support for unmodified shipped games or a production distribution mechanism. See the integration notes for the separate Silksong validation.

### Combined authenticated SDK result

The main investigation subsequently recorded `build/steam-native-research/run-20260916-194857-263084-auth-sdk`. The unmodified API library returned `SteamAPI_InitFlat=0`, and the loaded engine path matched the run's isolated `runtime/steamclient.dylib`. The SDK reported user and pipe handles of 1, a logged-in user, an identity matching the expected authenticated account, subscription to App ID 1030300, and the expected current App ID. Its read-only asynchronous `GetNumberOfCurrentPlayers` request returned a successful response with count 7,869. That count is a transient observation from this run.

After those checks, the runner sent SIGTERM to the native host alone. The consumer reported `sdk.host_loss_observed` and exited with status zero. Discovery counted four version requests, four path requests and zero rejected requests. This negative control ties the working SDK connection to the experimental host rather than a surviving full Steam client. It establishes authenticated, licensed SDK service and one real asynchronous response for this controlled consumer; it does not by itself establish game launch or game-specific behavior. The DYLD and hardened-runtime limitations above still apply.
