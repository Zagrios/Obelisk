# Steam Cloud preference read contract

Research date: September 19, 2026. The initial investigation used public schemas and static analysis of Forge's already bundled Valve runtime. The subsequent authenticated read described below verified the identified route with Forge's existing session. No official Steam installation was read and no preference was changed.

## Authenticated read result

The implemented Rust reader successfully downloaded and parsed app 7's `sharedconfig.vdf` through the normal logged-in Forge session. Full listings before and after the checksum-verified download matched at revision 241. For app 1030300, both account and game Cloud preferences resolved to true. The complete roaming contents were held in memory and were not printed or retained. Only derived booleans and the source revision were written to the private result.

`cloud/preferences.rs` accepts only a unique `UserRoamingConfigStore` object and unambiguous paths. Explicit values must be `0` or `1`; absent individual keys inside that verified tree use the native default. An absent remote file remains unknown. The result does not distinguish explicit values from defaults, and it is not a comparison against a simultaneously synchronized native getter. See the [authenticated upload report](steam-cloud-authenticated-upload.md) for the follow-up use of these preferences.

## Finding

The pinned Valve runtime stores both the account preference and per-app preferences in its user roaming configuration. Its roaming-config loader reads **app 7, root 0, `sharedconfig.vdf`** through RemoteStorage. This identifies a concrete file to request with Forge's authenticated CM connection. The service name `CloudConfigStore` alone did not establish this route.

The implemented read uses `Cloud.GetAppFileChangelist#1` for app 7, followed by `Cloud.ClientFileDownload#1` for the returned `sharedconfig.vdf` entry. Neither request changes preferences. The later authenticated check established authorization and file availability for Forge's connected account; other accounts still require the same bounded read. [CM schema](https://github.com/SteamDatabase/Protobufs/blob/d35adb23d7d48ad9673261f9572b60d1bfe06c12/steam/steammessages_cloud.steamclient.proto#L269), [current Forge CM reader](../crates/storefronts/steam-kit/src/cm/cloud.rs)

## Preference keys and defaults

The extracted client-settings schema explicitly assigns `CMsgClientSettings.cloud_enabled`, field 10000, to `k_EClientSettingStore_ConfigStore_UserRoaming`, with key `Software\Valve\Steam\CloudEnabled` and default `true`. Store enum value 2 is user roaming. This is source evidence for the account setting and default, not a standalone CM settings-read RPC. [Pinned schema](https://github.com/SteamDatabase/Protobufs/blob/d35adb23d7d48ad9673261f9572b60d1bfe06c12/steam/steammessages_clientsettings.proto#L184)

Static binary evidence establishes these paths within the roaming KeyValues tree:

| Preference | Key path | Default used by native reader |
| --- | --- | --- |
| Account | `Software/Valve/Steam/CloudEnabled` | `1` |
| Application | `Software/Valve/Steam/apps/<appid>/cloudenabled` | `1` |

The parser resource/root name passed by the native roaming loader is `UserRoamingConfigStore`. The authenticated reader subsequently confirmed this root in the downloaded bytes. Paths above describe tree traversal; they are not proposed filesystem paths.

Defaults apply only after establishing a usable preference snapshot. An unauthenticated native runtime with an empty home can return the same `true` without having obtained the account's remote setting. A failed CM request, unsupported encoding, malformed VDF, missing expected root, or unverified file absence must remain an unknown preference state in Forge.

Valve documents separate account and per-game getters and requires checking both. It restricts `SetCloudEnabledForApp` to an explicit user request. Forge must not use that setter to suppress a competing transfer engine. [Public preference contract](https://partner.steamgames.com/doc/api/ISteamRemoteStorage#IsCloudEnabledForAccount), [setter restriction](https://partner.steamgames.com/doc/api/ISteamRemoteStorage#SetCloudEnabledForApp)

## Reproducible static evidence

Input: `build/steam-cloud-feasibility/steamclient.dylib`, version `1788652215`, SHA-256 `035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840`. Its SHA-256 was checked during this investigation. Addresses below are ARM64 image-relative virtual addresses. The already extracted ARM64 slice and function boundaries came from the earlier [runtime-control investigation](steam-cloud-runtime-controls.md).

Disassembly excerpts were written under `/tmp/forge-cloud-preference-research/`: `preferences.asm`, `roaming-load.asm`, and `xrefs.py`. No binary execution was needed. Reproduce excerpts with `lipo -thin arm64` and `xcrun llvm-objdump -d --start-address=... --stop-address=...`; do not interpret objdump's nearest surviving exported symbol as the stripped function's identity.

### Account preference

The established server RemoteStorage vtable at `0x1762760` has slot 23 pointing to `0xacdc28`. This getter reads cached member `+0x56c`, calls `0xacdc60` when that value is `-1`, and returns whether the value is positive. The corresponding established proxy slot 23 points to `0x8fdc74`.

At `0xacdc80` the loader selects the string `Software\Valve\Steam\CloudEnabled` at `0x15b5d9b`. It calls the config getter with store 2 at `0xacdc8c` and default 1 at `0xacdc90`. It stores the result in the same cached member at `0xacdc98`. The account setter, server slot 69 at `0xacdcd8`, writes that same store and key. No setter was invoked.

### Application preference

Function `0xacdd54` selects config store 2 at `0xacdd7c`, loads `Software\Valve\Steam\apps` through pointer `0x17e8b88`, and iterates its child entries. It parses each child's name as an app ID, reads `cloudenabled` at `0xacddbc`, passes default 1 at `0xacddc0`, and fills the app-to-boolean map at object member `+0x2e8`.

The app setter is independently anchored by established RemoteStorage server slot 25 at `0xace3b4`. Pointer `0x17e8b90` resolves to the format string `Software\Valve\Steam\apps\%u\cloudenabled` at `0x15b5dd7`. The setter formats the requested app ID and writes config store 2 at `0xace428`. This corroborates the reader's path. No setter was invoked.

The established server slot 24, `IsCloudEnabledForApp`, points to `0xace254`. It rejects an explicit false in that map. It also checks app metadata keys `ignoreexternalfiles` and `hidecloudui` before returning true. Therefore the raw roaming preference is not the whole app-capability decision. The coordinator should keep user preference and supported Cloud configuration as separate inputs.

### Roaming configuration backing file

The config loader at `0x6a73cc` branches on its store argument. The store-2 path begins at `0x6a7464`. Its following RemoteStorage call sequence is:

| Address | Evidence |
| --- | --- |
| `0x6a77ec` to `0x6a7800` | Calls storage vtable `+0x248`, slot 73, with app ID 7. The previous runtime investigation identifies slot 73 as `GetRemoteStorageSyncState`. |
| `0x6a7840` to `0x6a785c` | Calls storage slot 1 with arguments `app=7`, `root=0`, `name=sharedconfig.vdf`. Slot 1 is `GetFileSize`. |
| `0x6a78dc` to `0x6a7900` | Calls storage slot 5 with those same first three arguments, a destination buffer, and the returned size. Slot 5 is `FileRead`. |
| `0x6a7904` | Compares read count against expected size before parsing. |
| `0x6a7948` to `0x6a7974` | Loads pointer `0x17e4e10`, which resolves to `UserRoamingConfigStore`, and parses the RemoteStorage file buffer. |

This loader also has a branch whose diagnostic explicitly describes a potentially out-of-sync local copy, near `0x6a7afc`. A local native getter without proof of completed roaming synchronization is therefore insufficient for an authoritative read. The native storage-slot identities and file argument order are independently documented in [runtime controls](steam-cloud-runtime-controls.md#file-abi-established-for-an-offline-probe).

## Bounded read-only verification

1. Use the existing Forge CM session and record its authenticated SteamID. Fetch a complete app-7 changelist through `Cloud.GetAppFileChangelist#1`, validating the response header. If it is a delta, merge it against a verified baseline or request a complete view; absence from a delta proves nothing.
2. Locate the returned entry for `sharedconfig.vdf`. Preserve its returned remote name. Reject duplicates, unsupported persistence states, and unsupported transfer formats. Download only that entry with the existing checksum-verified reader.
3. Parse the actual KeyValues encoding and confirm the expected roaming root. Extract only the account boolean and the requested game's boolean. Preserve whether each key was explicit or defaulted. An unsupported scalar value should fail parsing rather than silently enable synchronization.
4. Fetch app 7 again and compare change number plus the selected file's name, size, and hash. If they changed, discard and retry within a bound. Associate the result with SteamID, source app 7, change number, hash, and observation time.
5. Expose only those derived preferences and provenance to diagnostics. `sharedconfig.vdf` can hold unrelated user settings. Do not print or keep its complete contents in ordinary logs.

For a first production integration, keep an absent `sharedconfig.vdf` as unknown until the native post-sync empty-store behavior is separately verified. Absence of an individual preference key inside a verified valid roaming tree can use the native default 1 recorded above. A downloaded snapshot represents the latest state Steam returned; it cannot include another device's unsent local changes.

Use `SteamConnection.cloud_listing(7)` and `cloud_download` directly for this check. `SteamClient.cloud_backup` currently starts with app metadata and quota calls, then downloads every persisted file. Those extra prerequisites and transfers are unnecessary for preference reads. [Backup implementation](../crates/storefronts/steam-kit/src/client/cloud.rs)

## Limits and integration decision

`CloudConfigStore.Download` remains unproven for these settings. Its schema defines versioned namespaces, but no reviewed source assigns the two preference paths above to one of those namespaces. [Namespace schema](https://github.com/SteamDatabase/Protobufs/blob/d35adb23d7d48ad9673261f9572b60d1bfe06c12/webui/service_cloudconfigstore.proto)

The app-7 file route has pinned binary evidence and a successful authenticated read with stable listings. Comparison with a simultaneously synchronized native getter remains untested. Failed, ambiguous or missing-file reads still produce an unknown preference, and launch permission remains separate from transfer permission. A user choosing to launch despite a failed check does not implicitly turn on Cloud.
