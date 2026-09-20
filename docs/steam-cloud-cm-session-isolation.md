# Steam CM sessions during Cloud recovery

Validated on September 19, 2026, after removing save history.

## Failure and cause

After a controlled supervisor crash following Silksong's normal exit, Forge recovered the durable Cloud work and preserved all 18 current save files. Its main Steam session then required authentication: the profile disappeared, the friends menu displayed **Reconnexion requise**, and selecting a game no longer opened its detail. Restarting Forge with no Cloud work pending restored the same remembered account without changing credentials. This distinguishes the failure from a permanently invalid refresh token.

The main UI and Cloud maintenance create separate physical CM connections. Forge's login message omitted both session-disambiguation fields, leaving their default identifier in use. SteamKit documents that simultaneous connections for one account behind the same public IP need different login IDs; it writes both the legacy field and the modern address field. Node SteamUser documents the same replacement behavior and default zero. This identifies a protocol defect consistent with the observed failure. The actual server logoff result was not recorded, so this report does not claim an observed numeric `LogonSessionReplaced` reply.

Sources: [SteamKit login implementation](https://github.com/SteamRE/SteamKit/blob/master/SteamKit2/SteamKit2/Steam/Handlers/SteamUser/SteamUser.cs), [SteamUser logon options](https://github.com/DoctorMcKay/node-steam-user#logondetails), [Valve login protobuf mirror](https://github.com/SteamDatabase/Protobufs/blob/master/steam/steammessages_clientserver_login.proto), [address protobuf mirror](https://github.com/SteamDatabase/Protobufs/blob/master/steam/steammessages_base.proto).

## Change

Every authenticated `SteamConnection::log_on` generates a nonzero random 32-bit identifier from the operating system RNG. It writes that same value as legacy `CMsgClientLogon` uint32 field 2 and as `CMsgIPAddress.v4`, fixed32 field 1 inside message field 11. The identifier is generated independently across UI, supervisor and maintenance processes; it is neither a credential nor the user's actual address. An RNG failure rejects admission instead of falling back to the shared zero identifier. This does not guarantee mathematical uniqueness across the entire 32-bit space.

The wire regression invokes the real login method for two retained connections and independently decodes the outgoing protobuf fields. It failed before the implementation with a missing login identifier, then passed with two nonzero identifiers and consistent legacy/modern representations. Its synthetic replies prove serialization, not Steam server coexistence.

## Rebuilt-app check

With Forge and all game/host processes stopped, the replay first checked that the complete local set and baseline matched remote revision 55 and the user's preserved game-produced copy. It requeued an equal-content post-exit reconciliation in the existing context. It did not change a save file, remote object or upload transaction.

After rebuilding and opening Forge, normal remembered-account authentication started maintenance. The work completed and cleared `needs_work`, `desired` and `transaction`; no pending upload, replacement intent or game guard remained. Revision 55 and the existing verified transaction were unchanged. All 18 files still matched the user's copy. The main profile and online-friends status remained available, a library refresh was requested, and Silksong opened with **Lancer** available. This is the live coexistence check missing from the wire test.

`./scripts/test.sh` passed: 355 main-workspace Rust tests, four native tests, and 216 Swift tests executed with nine skipped and no failures. Formatting, strict Clippy, 538 localization keys, app signatures and the separate bundled-runtime isolation check passed. `./scripts/run.sh` then rebuilt and reopened Forge successfully. Logs are under `build/cm-session-isolation-tests.log` and `build/cm-session-isolation-run.log`; the initial failing regression is in `build/cm-login-id-red.log`.

The [redacted observations](evidence/steam-cloud-runtime-2026-09-19/modified-save-cycle-observations.json) include source fingerprints and distinguish the original crash recovery from this replay. Neither proves an interrupted file upload, another account, SDK persistence, FileShare or Dynamic Cloud. No commit or push was made.
