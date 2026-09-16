# Windows game runtimes on macOS

Research date: 2026-09-16. Scope: runtime architecture and performance for Forge. These are source-based findings and design recommendations, not results from running games on this checkout.

## What the runtime must translate

Wine supplies Windows API compatibility. Graphics translation is a separate choice. CrossOver exposes DXMT for Direct3D 11, D3DMetal for Direct3D 11 and 12, DXVK for Direct3D 10 and 11, and Wine's own wined3d. Its automatic setting chooses a backend using a game-specific database. MSync uses Mach semaphores; its benefit depends on the application. These are separate settings, so selecting Wine alone does not decide the rendering or synchronization strategy. [CodeWeavers settings documentation](https://support.codeweavers.com/en_US/advanced-settings-in-crossover-mac-26)

DXMT upstream now describes Direct3D 10 and 11 support. Its documentation requires Wine 8 or later and macOS 14 or later, recommends macOS 15, and supports both 32-bit and 64-bit Windows programs. Check the actual bundled version before assigning upstream capabilities to a shipped runtime. [DXMT repository](https://github.com/3Shain/dxmt), [DXMT runtime requirements](https://github.com/3Shain/dxmt/wiki/Device-System-Runtime-Specifications)

MoltenVK implements a subset of Vulkan over Metal. Consequently a macOS DXVK path adds a Direct3D-to-Vulkan step before the Vulkan-to-Metal step. This describes the translation chain; it does not prove that it loses every benchmark to a direct Metal backend. [MoltenVK repository](https://github.com/KhronosGroup/MoltenVK)

CPU instruction translation is another layer. On Apple silicon, current Intel CrossOver builds use Rosetta 2 for x86-64 instructions. Wine is not a substitute for that CPU translator. [CodeWeavers roadmap](https://www.codeweavers.com/blog/mjohnson/2026/06/11/whats-in-and-whats-out-for-crossover-27)

## CrossOver today and its next architecture

The stable changelog lists CrossOver 26.3.0, dated July 21, 2026. CrossOver 26 is based on Wine 11.0 and initially shipped D3DMetal 3.0 and DXMT 0.72. CrossOver 25 added its game-specific configuration database and listed GOG Galaxy and Epic Games Store support. The July 2026 release fixes both Epic Games Launcher downloads and a GOG Galaxy startup regression after their updates. Store clients therefore need continued compatibility maintenance, even after initial support works. [CrossOver changelog](https://www.codeweavers.com/crossover/changelog)

CrossOver 26 creates 64-bit bottles by default but can run 32-bit Windows applications inside them. Removing 32-bit bottles is not equivalent to dropping every 32-bit game. [CrossOver Mac guide](https://support.codeweavers.com/en_US/crossover-mac-user-guide)

CodeWeavers shipped an ARM64 Mac Preview on July 31, 2026. It combines native Wine and ARM64EC support with a customized FEX CPU translator and native ARM64 DXMT. The announcement describes that initial build as suitable for testing only: D3DMetal is absent, many game launchers fail, existing bottles cannot be converted, and the ARM64 path requires macOS 26.5 or later. The vendor targets CrossOver 27 in early 2027 for resolving these limitations. These are dated preview findings, not proof that every limitation remains in the latest preview. [CodeWeavers ARM64 announcement](https://www.codeweavers.com/blog/mjohnson/2026/7/31/crossover-preview-the-right-to-bear-arm64-on-mac)

Apple confirmed on September 1, 2026 that macOS 27 is the last release with general Rosetta support. The later exception is for certain older, unmaintained games relying on Intel frameworks. Do not assume that exception covers Forge's entire Windows runtime. [Apple developer announcement](https://developer.apple.com/news/?id=w5ngl9k2), [Apple Rosetta support article](https://support.apple.com/en-ca/102527)

## Which GameHub?

GameSir has a current GameHub for Mac at gamemac.com. Its official site explicitly describes Wine containers, selectable Wine versions, synchronization modes, per-game compatibility profiles, DLL overrides and a Metal performance HUD. It also advertises Steam login, cloud-save synchronization and account data. The linked GameSir GitHub repository is an issue tracker, with no runtime source in the inspected repository. [GameHub for Mac](https://www.gamemac.com/en), [official issue tracker](https://github.com/gamesir-labs/gamehub-for-mac)

Those primary sources establish Wine usage. They do not establish the exact shipped graphics backend versions, patches, current CPU translator, or parity with CrossOver. Treat claims that it uses a particular Proton, DXMT or D3DMetal build as unverified until its runtime package or maintainers' technical documentation is inspected. Its marketing claims are not independent measurements of compatibility or performance.

The older tkashkin/GameHub is a different Linux launcher. Its README lists Wine, Proton and several emulators, and GitHub marks it archived on April 19, 2025. It should not be used as evidence about GameSir's Mac product. [tkashkin/GameHub](https://github.com/tkashkin/gamehub)

## Proton and Apple's toolkit

Valve describes Proton as a Wine-based compatibility tool for running Windows games through Steam on Linux. A normal Proton Linux distribution is not a drop-in macOS runtime. Reusing suitable Wine patches is a different engineering task from shipping Proton unchanged. [Valve Proton repository](https://github.com/ValveSoftware/Proton)

Apple's current Game Porting Toolkit 4 includes an evaluation environment for running unmodified Windows executables on Apple silicon and evaluating performance and shader conversion. Apple also links CrossOver as one way to use that environment. This is not the same thing as obtaining the source and rights to bundle every proprietary component in another consumer launcher. The exact accompanying binary license was not retrieved in this research; redistribution and permitted use remain an explicit packaging question to resolve before a Forge release. Do not infer permission from Apple's open-source sample repository or from CrossOver's distribution. [Apple Game Porting Toolkit](https://developer.apple.com/games/game-porting-toolkit)

## Recommendation for Forge

This section is a design recommendation, not a measured result.

Use a Forge-managed macOS Wine runtime with versioned game profiles, and keep the CPU translator, graphics backend and synchronization mechanism replaceable. The user's requirement is one Forge installation with no separately installed CrossOver. Forge should install and update its own required runtime components. An installed CrossOver can be an optional engineering comparison for compatibility and performance, but is not a product dependency. Select the actual runtime distribution after verifying its maintenance and component distribution rules.

Prefer a working native Apple-silicon game build when it has the required store features and multiplayer compatibility. For Windows games, test direct Metal paths first: DXMT and D3DMetal for DX11, D3DMetal for DX12. Keep DXVK plus MoltenVK and wined3d as compatibility fallbacks. Do not promise that one backend or MSync is always fastest. Retain the result per game, Mac generation, macOS version and runtime version.

A launch profile should record the Windows prefix, store-client requirements, game executable and arguments, renderer, CPU translator, sync mode, runtime dependencies, DLL overrides and environment. Avoid one global bottle whose settings change whenever a different game launches. Prefix isolation must still allow the official store client and the game to share the Windows environment when platform integration requires it.

Measure average FPS, frame-time spikes, 1% lows, loading time, memory pressure, energy use and input latency. Test a repeatable gameplay scene both with cold and warm shader caches, with the required store client running. Pause avoidable Forge refresh work during play. Updating a launcher's SwiftUI view is not usually the largest cost; CPU translation, graphics translation, shaders, memory pressure and the game's own workload deserve measurement first.

Use Rosetta-backed builds where they are stable now, but plan for Wine ARM64EC plus a supported macOS CPU translator. Do not select the initial ARM64 Preview as the production default solely because it avoids Rosetta. Validate the actual current runtime and store clients before switching.

## GameNative and one-app installation

The user supplied GameNative as a model for Forge. Inspected source revision: `f17309da1add0bf6d74b9f96cd99579e5101cb13`, accessed 2026-09-16. This inspection did not build its APK or benchmark games.

GameNative's documented flow is installing its APK, signing in, installing a game, then playing. The project applies known configurations automatically and acknowledges game-specific compatibility limits. This supports the product model of one application managing the runtime for its users. [README lines 31-60](https://github.com/utkarshdalal/GameNative/blob/f17309da1add0bf6d74b9f96cd99579e5101cb13/README.md#L31-L60)

It manages several components behind that interface. Its Bionic launch path selects ARM64EC Wine with `libwow64fex.dll` or `wowbox64.dll`, or executes the guest through Box64 for the other Wine architecture. It extracts the selected emulator from bundled assets or installed content profiles. [Bionic launcher lines 420-490](https://github.com/utkarshdalal/GameNative/blob/f17309da1add0bf6d74b9f96cd99579e5101cb13/app/src/main/java/com/winlator/xenvironment/components/BionicProgramLauncherComponent.java#L420-L490)

The component manifest lists Bionic Proton x86-64 and ARM64EC packages, FEXCore, Box64, WOWBox64 and VKD3D downloads. Its graphics archive list includes DXVK. Some system components are bundled, while the modern Android path downloads others before extraction. One user-facing installation therefore does not require one immutable binary containing every possible runtime. [Runtime manifest lines 819-929](https://github.com/utkarshdalal/GameNative/blob/f17309da1add0bf6d74b9f96cd99579e5101cb13/manifest.json#L819-L929), [DXVK archive list](https://github.com/utkarshdalal/GameNative/blob/f17309da1add0bf6d74b9f96cd99579e5101cb13/app/src/main/assets/dxwrapper_download.json#L11-L58), [system component installer lines 213-242](https://github.com/utkarshdalal/GameNative/blob/f17309da1add0bf6d74b9f96cd99579e5101cb13/app/src/main/java/com/winlator/xenvironment/ImageFsInstaller.java#L213-L242)

The portable idea is app-managed components and game profiles. Android artifacts need replacement on Mac. Its Gradle build targets Android ABIs, including `arm64-v8a`; its Turnip packages target Adreno graphics, whereas a macOS runtime needs macOS Wine, a compatible CPU translator and Metal graphics integration. FEX upstream itself targets Arm64 Linux, and CodeWeavers explicitly had to adapt FEX for its Mac Preview. Shared ARM CPU architecture alone does not make Android libraries reusable on macOS. [Android build lines 123-155](https://github.com/utkarshdalal/GameNative/blob/f17309da1add0bf6d74b9f96cd99579e5101cb13/app/build.gradle.kts#L123-L155), [Mesa Turnip documentation](https://docs.mesa3d.org/drivers/freedreno.html#turnip), [FEX repository](https://github.com/FEX-Emu/FEX), [CodeWeavers Mac adaptation](https://www.codeweavers.com/blog/mjohnson/2026/7/31/crossover-preview-the-right-to-bear-arm64-on-mac)

The repository's application license is GPL-3.0, but its notices also identify proprietary or source-withheld components. In particular, `libredirect.so` is proprietary and handles Android-specific process compatibility. The maintainer's notices should not be treated as a blanket license for copying all packaged assets. This does not prevent an independently implemented Forge installer and runtime manager. [GameNative license](https://github.com/utkarshdalal/GameNative/blob/f17309da1add0bf6d74b9f96cd99579e5101cb13/LICENSE), [third-party notices lines 7-101](https://github.com/utkarshdalal/GameNative/blob/f17309da1add0bf6d74b9f96cd99579e5101cb13/THIRD_PARTY_NOTICES#L7-L101)
