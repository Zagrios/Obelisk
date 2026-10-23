# The contract between Obelisk and Voile

Obelisk runs Windows games in a [Voile](https://github.com/Zagrios/Voile) runtime it downloads on its own: as a Windows game launches, it reads Voile's signed `voile-feed.json` and offers the player the newest release of its contract. The contract is everything Obelisk relies on inside a runtime. One number counts its changes, on both sides:

- Voile's `CONTRACT` file, which every release built from that commit carries in its `runtime.json`;
- `contract` in Obelisk's `crates/game-runtime/voile-release.json`.

Obelisk runs only the releases whose contract equals its own. A release older than `CONTRACT` counts as contract 1.

## When to raise it

Raise both numbers in the change that alters anything below, whichever repository it starts in. A fix inside the runtime that keeps all of it, such as a Wine, Linux engine, DXMT or Neptune fix, keeps the contract: players get it with the next published release.

A release of a new contract reaches only the Obelisk builds of that contract. Older builds stay on the last release of theirs until Sparkle updates them.

## What it covers

### Paths inside a release

- `bin/wine`, `bin/wineserver`
- `runtime.json`, and its `version` field
- `lib/d3dmetal`, `lib/dxmt`: each graphics backend's DLLs, which `WINEDLLPATH_PREPEND` names and `wineboot` lays into prefixes
- `lib/external/D3DMetal.framework/D3DMetal`, `lib/external/libd3dshared.dylib`
- `lib/gstreamer-1.0`
- `share/wine/mono/wine-mono-*`: its folder name is recorded in each prefix, and a new one updates the prefix
- `steam/steamclient.dll`, `steam/steamclient64.dll`, copied into each prefix
- `linux/engine/fexdarwin-run`, `linux/engine/wsd`, `linux/engine/wsdctl`, `linux/engine/obelisk-input`, `linux/engine/run-with-timeout.sh`
- `linux/root`, the Linux guest's root, and `linux/wine`, bound at `/opt/wine`
- `linux/windows/{d3d11,dxgi,d3d12}.dll`, copied into the Linux engine's prefix on each launch
- `linux/steam/linux64`, Valve's Linux steamclient, bound at `/opt/steam`
- `linux/guest/steam-hostpid.so`, preloaded in the guest
- `linux/graphics/libexec/virgl_render_server`, `linux/graphics/dxmt-native.framework`
- `linux/graphics/x86_64/{bin/virgl_test_server, libexec/virgl_render_server, lib/libd3dmetal-native.dylib}`

### What Obelisk's Wine reads

On the macOS engine, the variables Obelisk sets and the Wine features they need:

- `WINEDLLPATH_PREPEND`, the backend folders searched before Wine's own
- `CX_ACTIVE_GRAPHICS_BACKEND`, `CX_APPLEGPTK_LIBD3DSHARED_PATH`, `CX_HIDE_BUILTIN_VERSION_RESOURCES`
- `D3DMETAL_FRAMEWORK_PATH`, `D3DM_SUPPORT_DXR`
- `GST_PLUGIN_SYSTEM_PATH`, `GST_REGISTRY`
- `WINEMSYNC`, `WINE_LARGE_ADDRESS_AWARE`, `ROSETTA_ADVERTISE_AVX`, `DOTNET_EnableWriteXorExecute`
- `WINE_OBELISK_INPUT_SOCKET`, which Voile's Obelisk input bus reads
- `PROTON_DISABLE_LSTEAMCLIENT`, which keeps Wine's loader from handing steamclient's exports to lsteamclient, in the prefix where Valve's Windows Steam starts a game
- the DLL overrides Obelisk sets: `mscoree`, `mshtml`, `winemenubuilder.exe`, and `gameoverlayrenderer`, `gameoverlayrenderer64` where Valve's Windows Steam starts a game, with `steam.exe` disabled for that prefix's `wineboot`

In the Linux engine's guest:

- `WINEARCH`, `WINEDLLOVERRIDES` with `d3d11,dxgi,d3d12=n`
- `VTEST_SOCKET_NAME`, `NPT_DEBUG`, `LD_PRELOAD`

### The Linux engine's launch

`crates/game-runtime/src/wine/linux_engine/launch.sh`, built into Obelisk, assembles a launch from the runtime. Its contract covers:

- the `FEXDARWIN_LKD`, `FEXDARWIN_ROOT`, `FEXDARWIN_ROOT_RO`, `FEXDARWIN_SUPERVISOR_PID` and `FEXDARWIN_BIND` variables, and `fexdarwin-run --cwd`
- `wsd --dir`, and the bundle `wsd` is copied into
- `virgl_test_server --no-virgl --no-fork --socket-path`, with `RENDER_SERVER_EXEC_PATH`, `NPT_BACKEND`, `NPT_{D3D11,DXGI,D3D12}_LIBRARY_PATH` and `DMN_RESIDENCY_SET`
- the render server's `--obelisk-translate`
- `obelisk-input --dir`, and the `wsd.sock`, `vtest.sock` and `input.sock` sockets it waits for
- `wineserver -p0` and `wineserver -w`

### The Steam bridge

- Voile's ntdll sends the exports of `steamclient(64).dll` to lsteamclient.
- lsteamclient loads the Intel slice of Obelisk's `steamclient.dylib` from `STEAM_COMPAT_CLIENT_INSTALL_PATH`.
- `steam.exe` starts the game, which gets `SteamAppId`, `SteamGameId` and `SteamOS`.
- In the Linux engine, `~/.steam/sdk64` leads to `/opt/steam/linux64`.

## Releasing across a change

1. In Voile, raise `CONTRACT`, then run the Release workflow and publish the release. The Feed workflow offers it, and Obelisk builds of the old contract ignore it.
2. In Obelisk, raise `contract` in `voile-release.json` with the change that needs it, then tag a release. Its workflow pins the newest Voile release of the new contract with `scripts/pin-voile.sh`, and refuses to build when there is none.
3. Sparkle brings players the new Obelisk, which moves them to the new release.

## The feed

`voile-feed.json` lives on Voile's `main`, newest release first:

```json
{
  "format": 1,
  "releases": [
    {
      "version": "v26.3.0-16",
      "contract": 1,
      "published": "2026-10-21",
      "url": "https://github.com/Zagrios/Voile/releases/download/v26.3.0-16/voile-v26.3.0-16.tar.xz",
      "size": 431204816,
      "sha256": "…",
      "notes": "### Fixes\n\n- …"
    }
  ]
}
```

Voile's Feed workflow writes a release's entry when the release is published, from its assets, its notes and its commit's `CONTRACT`. Obelisk shows the notes, Markdown as GitHub shows them, of each release newer than the player's before they take one; run the workflow again with the version after editing a release's notes. Entries written before notes were added have none. It then signs the file with Ed25519 into `voile-feed.json.sig`, the base64 of the signature. Obelisk checks it with the public key in `crates/game-runtime/src/voile/feed.rs`, which Voile's `scripts/feed-key.sh` made; it ignores a feed whose signature does not hold, or of another `format`, and goes on with the last one it checked.

To withdraw a release, run the Feed workflow from Voile's Actions tab with its version and a reason. The entry stays, with `withdrawn`: Obelisk no longer runs that release, downloads the newest one left on the next launch, and removes the withdrawn one once no game runs. Running the workflow again with an empty reason puts the release back.

## A game that needs a release

A game profile in `crates/game-runtime/profiles.json` can name the oldest release its game runs in, with `minimum_voile`. It is a floor, not a pin: the game runs in the newest release installed, as every game does. Every Windows game's launch that finds a newer release asks the player first: they take it, and it downloads before the game starts, or skip it, and the game starts with the release in place; the next launch asks again. Only when the release in place is older than the game's `minimum_voile` can it not be skipped: the player takes it, or closes the question, which stops the launch.
