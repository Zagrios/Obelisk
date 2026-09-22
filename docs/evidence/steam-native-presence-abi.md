# Native presence ABI, September 22, 2026

Static verification of the ARM64 slice of the pinned `steamclient.dylib`, whose
universal SHA-256 is
`035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840`.
No account, game, or native method was invoked to establish this map.

| Method | Vtable | Slot | Code offset | Verified calling shape |
| --- | --- | --- | --- | --- |
| Engine005 Friends accessor | `0x1753a20` | 13 | `0xa02cb4` | `(this, user:i32, pipe:i32) -> pointer` |
| Friends GetPersonaState | `0x1741520` | 4 | `0x8b2234` | `(this) -> i32` |
| Friends SetPersonaState | `0x1741520` | 5 | `0x8b23a0` | `(this, state:i32) -> void` |

The accessor retains `x1` and `x2`, looks up their user/pipe pair, and returns the
session object's field `+0x20` at `0xa02d3c`. The constructor at `0x88f610`
installs the Friends proxy vtable `0x1741520` at `0x88f668`–`0x88f670`. Its caller
at `0xab5428` stores that proxy in the same session field `+0x20` at `0xab542c`.
The accessor therefore returns the existing user's Friends interface; it does
not create another authenticated connection.

The proxy at `0x8b23a0` preserves the state from `w1`, serializes exactly four
bytes at `0x8b2418`–`0x8b2428`, names `IClientFriends` and `SetPersonaState` at
`0x8b2450`–`0x8b2464`, and consumes only the IPC acknowledgement. It does not
decode a return value. Independently, the server dispatcher names the same
method at `0x819030`–`0x819064`, reads four bytes, then loads vtable offset `0x28`
at `0x8190f8` and calls it with `this` and the scalar state at `0x823844`.

The getter proxy names `GetPersonaState` at `0x8b22d4`, takes no scalar input,
and decodes four return bytes at `0x8b2304`–`0x8b2324`. The server object is
identified by the `CUserFriends` RTTI and vtable `0x175e868`; its slot 4 at
`0xa8e1a8` returns the desired persona field `+0x10` only when its user's logon
state is 4, and otherwise returns Offline. This readback verifies local state,
not acknowledgement by Steam's servers.

The corresponding server setter, slot 5 at `0xa8e1d8`, delegates to
`0xa8e21c`. Its behavior depends on the friends UI mode helper `0xb2eb4`:

- With the mode enabled, `0xa8e270`–`0xa8e298` posts the four-byte
  `SetPersonaStateInternal_t` callback `0xfde9b` and returns without changing
  the desired persona field. A host without the Steam friends UI cannot rely
  on that callback to apply the request.
- With the mode disabled, `0xa8e304` updates that field, triggers the native
  update job at `0xa8e318`, and writes `PersonaStateDesired` at `0xa8e34c`.

The helper reads the registered process flags `-friendsui` and `-nofriendsui`
(strings `0x1538bfd` and `0x1538c08`). `-friendsui` wins if present.
`-nofriendsui` forces the mode off at `0xb2f50`, provided the bootstrapper
version is zero or the process is in a debug session. A nonzero bootstrapper
version outside a debug session skips this override at `0xb2f28`. Forge's
standalone host provides no Steam bootstrapper. Specifically, the helper
looks up `GetBootstrapperVersion` through the `dlsym(RTLD_DEFAULT, ...)`
wrapper at `0x10857e4`. Its fallback calls `0xa15088`, which returns zero when
the function pointer at `0x1843e10` is null. That pointer is initialized by the
same symbol lookup at `0xa0ad08`–`0xa0ad1c`. Neither the Forge host/discovery
adapter nor the bundled Steam client, tier0, vstdlib, or audio images export
this symbol (`nm -arch arm64 -g`). With those images, both paths therefore
treat the bootstrapper as absent. An injected image exporting the symbol
could alter the result; none is required or installed by this implementation.
These conditions are part of the pinned binary evidence, not a general
guarantee for other Steam versions.

No command-line initialization call is required in the host. In the companion
ARM64 `libvstdlib_s.dylib`, the exported `SteamStdLib::CommandLine()` at `0x81d4`
calls the lazy initializer `0x82a0`. It obtains argc/argv through
`Plat_GetProcessArgv` at `0x82c0`, then invokes its parser with those values at
`0x82ec`. In the companion `libtier0_s.dylib`, `Plat_GetProcessArgv` at `0xad28`
calls macOS `_NSGetArgc` and `_NSGetArgv` at `0xad38` and `0xad44`. The option
therefore belongs in the native host process arguments, not a Steam config
file or the game's arguments.

These shapes also agree with the historical
[Open Steamworks declaration](https://github.com/SteamRE/open-steamworks/blob/master/Open%20Steamworks/IClientFriends.h).
That declaration was a cross-check only; its old Engine layout differs from
this binary. The public
[ISteamFriends interface](https://partner.steamgames.com/doc/api/ISteamFriends)
does not expose this setter. Online and Offline are the existing persona enum
values 1 and 0.

Reproduce with `lipo -thin arm64`, `llvm-objdump -d --start-address=…
--stop-address=…`, and the Mach-O `__DATA_CONST,__const` entries above. Production
still checks the entire binary hash and the image-relative method addresses
before calling them.

## Live verification

On September 22, 2026, the user relaunched Forge and Hollow Knight: Silksong
(App ID 1030300) between each build. The native setter without `-nofriendsui`
left the public Steam Community profile Offline. With that option, the same
profile showed `Dans un jeu Hollow Knight: Silksong` after refresh, observed
through a signed-out browser. The running native host's arguments contained
`-nofriendsui`.

This confirms server-visible game presence for that launch. The game retained
ownership of SDK initialization; no game-announcement ABI was added to the host.
The diagnostic getter and temporary traces were removed after this check.
Game exit, account revocation and reconnect behavior have fixture coverage;
they were not exercised against the live account in this trial. Games that do
not register through their own Steam SDK remain outside this observation.
