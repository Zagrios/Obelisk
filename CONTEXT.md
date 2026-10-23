# Obelisk

Obelisk manages games owned through storefront accounts, including their installation and launch.

## Language

**Transfer**:
One account-scoped attempt to install, verify or repair a game's files. Its lifetime includes the remaining disk work after cancellation; verified reusable data can survive for a later attempt.

**Installation receipt**:
A record identifying a game installation whose required files were successfully installed. Partial download data is not an installation receipt.

**Offer**:
What a storefront says Obelisk can install of one game, with Steam's language, before any folder is chosen. The storefront picks its build as the Transfer starts: the Mac build when its game runs natively on Apple silicon, as its program's first bytes tell, else the Windows build. Until a download found that a Mac build's game does not run natively, the game's page shows the Mac build. A Mac build without an arm64 slice, Intel or 32-bit, is never installed, nor launched. A new installation starts from an offer; an installed game launches, updates and uninstalls from its Installation receipt alone.
_Avoid_: download request, install target

**Platform**:
Which of a game's builds a Transfer installs, picked by the storefront as it starts, or an Installation receipt records: the Mac build, which runs natively, or the Windows build, which runs in Wine. Each storefront spells it its own way in its requests, receipts and folder names; Obelisk names it one way everywhere else.
_Avoid_: OS, os list (Steam's declared platforms, Linux included), macos/osx/Mac as values

**Games folder**:
Where Obelisk installs games: its own folder in its library until the player chooses another, such as one on an external disk, inside which Obelisk makes its own. Choosing one moves every installation there, one at a time in the download queue: a game cannot launch while it moves, the others can, and a move the player stops leaves its game where it was. Wine prefixes, launch settings and the Cloud archive stay in the library. An installation whose disk is not connected shows as not installed until the disk comes back.
_Avoid_: library folder (Obelisk's own data), install location

**Account session**:
One storefront account's signed-in connection to its storefront, from sign-in or restoration until sign-out. The account's library, Transfers, launches, friends and chat go through it. Steam's connects again by itself when its CM connection drops: three attempts after growing waits, then only the player's retry or the Mac's wake tries again, and never once Steam asked the account to sign in again. A Transfer whose turn comes meanwhile waits for the new connection, and the download queue waits with it. Each CM connection of a session is numbered, 0 for the first: its transport, in the code.
_Avoid_: login, CM connection (one connection of an account session)

**Library cache**:
The last library each storefront account loaded, with everything the storefront returned for each game and the game's last launch from Obelisk. The library shows from it while the account's session connects; the session keeps it as it loads, and signing the account out forgets it. It is not the catalog of games added by hand.
_Avoid_: library.json (the catalog's file), launch history file

**Account library**:
One storefront account's games as Obelisk shows them, with what orders and narrows each: its name, playtime, last played (on its storefront or from Obelisk, whichever is later), date added, achievements unlocked and size installed. It shows from the Library cache while the account's session connects, becomes live once the session lists the games, and signing the account out forgets it. Every storefront searches, filters and orders it the same six ways, and its featured game, its hero, is the last one played, else the most played, equal ones by name.
_Avoid_: catalog (the games added by hand), filtered games, Library cache (its saved copy)

**Cloud archive**:
Obelisk's own local store of each account's save versions, together with the record of which account currently owns each physical save location. One archive serves every storefront, so two storefront copies of a game that share a save location cannot silently adopt each other's saves.
_Avoid_: vault root, SteamCloud folder

**Last Cloud sync**:
The last moment Obelisk confirmed that an account's local saves for a game and its Cloud saves were identical, by uploading, downloading or finding them equal. Capturing or choosing a version without that confirmation does not count.
_Avoid_: last upload, last played

**Wine prefix**:
The Windows machine one Windows installation runs in, shared by every account that launches it. It holds the game's Windows saves, is readied before each launch of the game, and goes when the installation is uninstalled.
_Avoid_: bottle, Wine folder

**Engine**:
What runs a Windows game's Wine: Rosetta's Wine, Voile's Wine for macOS under Rosetta, for most games, or the Linux engine, Voile's Wine for Linux that voile-lkd runs natively, for games whose anti-cheat supports Linux. Each engine keeps its own Wine prefix of an installation and its own runtime in Voile, offers its own Graphics backends, and starts, quits and stops a game its own way; Prerequisites run only in Rosetta's Wine. Launch settings pick it, and a game moved to the other engine starts in that engine's Wine prefix.
_Avoid_: backend (a Graphics backend renders), runner, bottle, Wine flavour

**Launch settings**:
How one Windows installation runs, for every account that launches it: what Obelisk knows of the game, with the player's choices over it. The player picks the arguments added after the storefront's and the game's own on its command line, its engine, then the graphics backend that renders its Direct3D there and that backend's options, each as the engine allows, whether Metal's Performance HUD shows over the game, and, for a Steam game on Rosetta's Wine, its Steam client: Obelisk's, or Valve's Windows Steam in a Wine prefix of its own, which servers secured by VAC accept. Only the choices that differ from what Obelisk knows of the game are kept. A change applies from the game's next launch, a game moved to the other engine runs in that engine's Wine prefix, and the settings go when the installation is uninstalled.
_Avoid_: bottle, container, game settings, profile (what Obelisk itself knows of a game)

**Graphics backend**:
What renders a Windows game's Direct3D through Metal: D3DMetal, for Direct3D 11 and 12 in 64-bit programs, or DXMT, for Direct3D 10 and 11. A game has one, as Windows has one dxgi per process: on DXMT it has no Direct3D 12. On Rosetta's Wine, a 32-bit program of a game on D3DMetal renders through DXMT.
_Avoid_: renderer, translation layer

**Prerequisite**:
A program a storefront names to run once in a Wine prefix before its game first starts, such as a DirectX or Visual C++ installer. One that fails runs again on the next launch; the game starts anyway.
_Avoid_: redistributable (Steam's name for the package, not the run), dependency

**Runtime**:
What Obelisk downloads beside a game's own files for it to run: Voile for a Windows build, Valve's Steam components for every Steam game. A Transfer gets the missing ones before the game's files, and a launch that still misses one gets it after its Admission, before its game is prepared. A launch that finds a newer Voile release asks the player there, with what it brings: they take it before the game starts, or skip it for that launch, unless the game needs it. A Voile release a newer one replaced goes only once no game of any storefront may still run from it.
_Avoid_: dependency, prerequisite (a Prerequisite runs inside a Wine prefix)

**Reservation**:
The exclusive claim a Game session or Cloud maintenance holds on a game's installation. While it exists, no Transfer, uninstall or other launch of that game can start.
_Avoid_: lock, busy state

**Admission**:
The moment a launch claims its game's Reservation, before any verification, Voile download or authentication can wait. A launch cancelled before its handoff to a supervisor gives the Reservation back; once handed off, only the Game session's end does.
_Avoid_: launch start, pending launch

**Game session**:
One account-scoped launch of a game, holding a Reservation from admission until the game has really exited and its Cloud saves are settled. Obelisk keeps running in the background while it lasts; really quitting Obelisk stops its game first, and if Obelisk dies, its game stops with it.
_Avoid_: launch, supervisor session

**Diagnostics**:
The files Obelisk writes about how a game ran, in its storefront's folder under ~/Library/Logs/Obelisk: the log of the game's last launch, the one before it and its input service's, and a Steam launch's launch or failure report. A bug report attaches them. macOS's crash reports and Obelisk's unified log are not diagnostics.
_Avoid_: logs folder, launch logs

**Cloud maintenance**:
Synchronizing one game's Cloud saves without launching it, at the player's request. It runs inside Obelisk, holds the Reservation of every game of its storefront while it runs, and stops if Obelisk quits or the account signs out.
_Avoid_: Cloud resume, cloud-only session, background sync

**Cloud sync**:
One comparison of an account's local saves for a game with its Cloud saves, before a launch, after the game exits, or as Cloud maintenance. It ends with both sides equal, with the player's choice applied, or with work left pending for a later sync.
_Avoid_: Cloud check, sync cycle

**Cloud record**:
What the Cloud archive keeps of one game for one account besides its saves: the Cloud rules and the configuration that place its saves, the work a Cloud sync left pending, and whether an uninstall may have removed some of them. From an uninstall on, or once a Windows game's engine moves it to another Wine prefix, a save missing locally keeps its archived version and is never sent as deleted, until a Cloud sync finds both sides agreeing. An uninstalled game keeps its record, so its Cloud indicator and Cloud maintenance still find its pending work.
_Avoid_: Cloud context (the storefront's file that holds it), work journal

**Cloud view**:
The complete set of an account's Cloud saves for one game that a Cloud sync compares against: read from the storefront and trusted only while the storefront still lists the same saves afterwards, or what an upload just confirmed. An archived save stands in for a listed one when its size and the storefront's hash for it match, so an unchanged save is not downloaded again.
_Avoid_: remote snapshot, readback, remote listing

**Cloud question**:
A choice a Cloud sync asks the player: which saves to keep after a conflict, or whether to retry, cancel or launch anyway after a failed launch check. It takes one answer and closes with it; it closes unanswered when the sync is cancelled, the account is revoked, or nobody answers in time. An account asks at most one at a time, whichever sync asks it.
_Avoid_: prompt, Cloud decision

**Account revocation**:
Signing a storefront account out while its Game sessions may still run. It stops that account's network work and presence, keeps any exit save locally as pending for that account, and never ends a running game.
_Avoid_: sign-out cancel, logout kill

**Game action**:
What the player can do with one game of an account now, as its button shows it: download, resume or update it, launch it, wait for its queued Transfer, follow its Transfer, wait while it is uninstalled, launched or its session closes, follow a step outside any Transfer (a runtime its launch waits for, or a Cloud sync), or end its running game. A game's page and its library show the same action, except for an installed game whose storefront offers a newer build: the page offers the update alone, while the library launches it and the launch installs the update first. A Game session of another account is not this account's running game.
_Avoid_: primary action, card action

**Game note**:
What Obelisk tells players about one game on its page, whatever its storefront or build, such as what works of it and what does not yet: a few sentences in Obelisk's language, else in English. Obelisk writes it, not the storefront, and it changes nothing about how the game runs, which its profile decides.
_Avoid_: compatibility note, warning, badge
