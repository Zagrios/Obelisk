#!/bin/bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "Usage: $0 APP" >&2
    exit 2
fi

# An ad hoc signature can be valid, but its identity changes with the binary.
# Require an Apple-issued certificate as well as a valid bundle signature.
if ! /usr/bin/codesign --verify --deep --strict -R='anchor apple generic' "$1"; then
    echo "SailOver needs a valid Apple certificate signature to preserve Keychain access across builds." >&2
    echo "Configure apps/macos/Config/Signing.local.xcconfig using the example beside it." >&2
    exit 1
fi

require_architectures() {
    local binary="$1"
    local expected="$2"
    local actual
    actual="$(/usr/bin/lipo -archs "$binary" | tr ' ' '\n' | LC_ALL=C sort | paste -sd ' ' -)"
    if [[ "$actual" != "$expected" ]]; then
        echo "Unexpected architectures for $binary: $actual; expected $expected." >&2
        exit 1
    fi
    /usr/bin/codesign --verify --strict -R='anchor apple generic' "$binary"
}

STEAM_HELPERS="$1/Contents/Helpers/SteamNative"
require_architectures "$STEAM_HELPERS/sailover-steam-host" "arm64"
if [[ -e "$STEAM_HELPERS/sailover-steam-sdk-arm64" ]]; then
    echo "SailOver contains the removed ARM64 SDK diagnostic helper." >&2
    exit 1
fi
require_architectures "$STEAM_HELPERS/sailover-steam-sdk-x86_64" "x86_64"
require_architectures "$STEAM_HELPERS/libsailover_steam_discovery_adapter.dylib" "arm64 x86_64"
require_architectures "$1/Contents/Helpers/SailOver Rosetta.app/Contents/MacOS/SailOverRosetta" "x86_64"
