# SailOver

SailOver manages games owned through storefront accounts, including their installation and launch.

## Language

**Transfer**:
One account-scoped attempt to install, verify or repair a game's files. Its lifetime includes the remaining disk work after cancellation; verified reusable data can survive for a later attempt.

**Installation receipt**:
A record identifying a game installation whose required files were successfully installed. Partial download data is not an installation receipt.

**Cloud archive**:
SailOver's own local store of each account's save versions, together with the record of which account currently owns each physical save location. One archive serves every storefront, so two storefront copies of a game that share a save location cannot silently adopt each other's saves.
_Avoid_: vault root, SteamCloud folder

**Last Cloud sync**:
The last moment SailOver confirmed that an account's local saves for a game and its Cloud saves were identical, by uploading, downloading or finding them equal. Capturing or choosing a version without that confirmation does not count.
_Avoid_: last upload, last played

**Wine prefix**:
The Windows machine one Windows installation runs in, shared by every account that launches it. It holds the game's Windows saves, is readied before each launch of the game, and goes when the installation is uninstalled.
_Avoid_: bottle, Wine folder

**Prerequisite**:
A program a storefront names to run once in a Wine prefix before its game first starts, such as a DirectX or Visual C++ installer. One that fails runs again on the next launch; the game starts anyway.
_Avoid_: redistributable (Steam's name for the package, not the run), dependency

**Reservation**:
The exclusive claim a Game session or Cloud maintenance holds on a game's installation. While it exists, no Transfer, uninstall or other launch of that game can start.
_Avoid_: lock, busy state

**Admission**:
The moment a launch claims its game's Reservation, before any verification, Voile download or authentication can wait. A launch cancelled before its handoff to a supervisor gives the Reservation back; once handed off, only the Game session's end does.
_Avoid_: launch start, pending launch

**Game session**:
One account-scoped launch of a game, holding a Reservation from admission until the game has really exited and its Cloud saves are settled. SailOver keeps running in the background while it lasts; really quitting SailOver stops its game first, and if SailOver dies, its game stops with it.
_Avoid_: launch, supervisor session

**Cloud maintenance**:
Synchronizing one game's Cloud saves without launching it, at the player's request. It runs inside SailOver, holds the Reservation of every game of its storefront while it runs, and stops if SailOver quits or the account signs out.
_Avoid_: Cloud resume, cloud-only session, background sync

**Cloud sync**:
One comparison of an account's local saves for a game with its Cloud saves, before a launch, after the game exits, or as Cloud maintenance. It ends with both sides equal, with the player's choice applied, or with work left pending for a later sync.
_Avoid_: Cloud check, sync cycle

**Cloud question**:
A choice a Cloud sync asks the player: which saves to keep after a conflict, or whether to retry, cancel or launch anyway after a failed launch check. It takes one answer, and closes unanswered when the sync is cancelled, the account is revoked, or nobody answers in time. An account asks at most one at a time, whichever sync asks it.
_Avoid_: prompt, Cloud decision

**Account revocation**:
Signing a storefront account out while its Game sessions may still run. It stops that account's network work and presence, keeps any exit save locally as pending for that account, and never ends a running game.
_Avoid_: sign-out cancel, logout kill
