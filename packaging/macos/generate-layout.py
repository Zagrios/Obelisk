#!/usr/bin/env python3
"""Regenerate the Finder layout template, without opening Finder.

This is an optional maintenance tool. Normal DMG builds copy the checked-in
FinderLayout.dsstore and require neither Python nor third-party packages.

From the repository root:
    python3 -m venv build/dmg-layout-tools
    build/dmg-layout-tools/bin/pip install ds-store==1.3.1 mac-alias==2.2.2
    build/dmg-layout-tools/bin/python packaging/macos/generate-layout.py

Format references:
    https://github.com/dmgbuild/ds_store
    https://github.com/dmgbuild/dmgbuild/blob/main/src/dmgbuild/core.py
    https://metacpan.org/dist/Mac-Finder-DSStore/view/DSStoreFormat.pod

All records are created from scratch. The template contains only relative item
names and Finder presentation settings, with no volume aliases or user paths.
"""

from pathlib import Path

from ds_store import DSStore


def generate_layout(destination: Path) -> None:
    with DSStore.open(str(destination), "w+") as store:
        root = store["."]
        root["vSrn"] = ("long", 1)
        root["bwsp"] = {
            "WindowBounds": "{{100, 100}, {640, 380}}",
            "ShowToolbar": False,
            "ShowSidebar": False,
            "ContainerShowSidebar": False,
            "SidebarWidth": 0,
            "ShowPathbar": False,
            "ShowStatusBar": False,
            "ShowTabView": False,
            "PreviewPaneVisibility": False,
        }
        root["icvp"] = {
            "viewOptionsVersion": 1,
            "backgroundType": 0,  # Finder's system background, no image alias.
            "backgroundColorRed": 1.0,
            "backgroundColorGreen": 1.0,
            "backgroundColorBlue": 1.0,
            "iconSize": 128.0,
            "textSize": 14.0,
            "labelOnBottom": True,
            "showItemInfo": False,
            "showIconPreview": True,
            "arrangeBy": "none",  # Preserve the two explicit icon positions.
            "gridOffsetX": 0.0,
            "gridOffsetY": 0.0,
            "gridSpacing": 54.0,
            "scrollPositionX": 0.0,
            "scrollPositionY": 0.0,
        }
        root["icvl"] = ("type", b"icnv")
        root["vstl"] = ("type", b"icnv")  # Always open in icon view.
        store["SailOver.app"]["Iloc"] = (165, 180)
        store["Applications"]["Iloc"] = (475, 180)


if __name__ == "__main__":
    generate_layout(Path(__file__).with_name("FinderLayout.dsstore"))
