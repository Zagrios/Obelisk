# Platform logos

The Steam, Epic Games, and GOG.com sidebar logos come from
[Simple Icons](https://github.com/simple-icons/simple-icons/tree/4ba19240849175ab4b855a732ab98c0f87cfb714),
revision `4ba19240849175ab4b855a732ab98c0f87cfb714`.
The SVG paths are preserved. Epic Games path parameters are separated with
spaces so CoreSVG can read them correctly on macOS.

| Asset | Brand source referenced by Simple Icons |
| --- | --- |
| `SteamLogo` | [Steam](https://partner.steamgames.com/doc/marketing/branding) |
| `EpicGamesLogo` | [Epic Games](https://dev.epicgames.com/docs/services/en-US/EpicAccountServices/DesignGuidelines/index.html#epicgamesbrandguidelines) |
| `GOGLogo` | [CD PROJEKT](https://www.cdprojekt.com/en/media/logotypes/) |

Simple Icons distributes its files under [CC0 1.0](https://github.com/simple-icons/simple-icons/blob/4ba19240849175ab4b855a732ab98c0f87cfb714/LICENSE.md).
The trademarks remain the property of their respective owners.

The game-card compatibility logos also come from Simple Icons, at revision
[`ce334b5bda8d8d054cfde7ce35caf40651078a28`](https://github.com/simple-icons/simple-icons/tree/ce334b5bda8d8d054cfde7ce35caf40651078a28),
the last revision before Microsoft brands were removed. Both SVG files retain
their upstream paths and are covered by that revision's
[CC0 1.0 license](https://github.com/simple-icons/simple-icons/blob/ce334b5bda8d8d054cfde7ce35caf40651078a28/LICENSE.md).

| Asset | SVG source | Brand source referenced by Simple Icons |
| --- | --- | --- |
| `AppleLogo` | [apple.svg](https://github.com/simple-icons/simple-icons/blob/ce334b5bda8d8d054cfde7ce35caf40651078a28/icons/apple.svg) | [Apple](https://www.apple.com) |
| `WindowsLogo` | [windows.svg](https://github.com/simple-icons/simple-icons/blob/ce334b5bda8d8d054cfde7ce35caf40651078a28/icons/windows.svg) | [Windows 2021 logo](https://commons.wikimedia.org/wiki/File:Windows_logo_-_2021_(Black).svg) |

The image sets in `apps/macos/Forge/Resources/Assets.xcassets` preserve vector
data and use template rendering. SwiftUI applies the foreground color on a
transparent background, using the menu's accent color for storefront logos
and white for game-card compatibility logos.
