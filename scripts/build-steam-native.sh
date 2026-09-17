#!/bin/bash
set -euo pipefail
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
if [[ "$(uname -m)" != arm64 ]]; then
    echo "The native Steam engine integration currently requires Apple silicon." >&2
    exit 1
fi
case "${CONFIGURATION:-Debug}" in
    Debug) PROFILE_NAME=dev; PROFILE_DIRECTORY=debug ;;
    Release) PROFILE_NAME=release; PROFILE_DIRECTORY=release ;;
    *) exit 1 ;;
esac
# Acquisition happens at build time from hash-pinned Valve CDN archives, never
# from an installed Steam client. The app ships the verified minimal payload.
RUNTIME_PACKAGE="$PROJECT_ROOT/build/steam-runtime/runtime.zip"
CARGO_TARGET_DIR="$PROJECT_ROOT/target" cargo run --locked \
    --manifest-path "$PROJECT_ROOT/Cargo.toml" -p steam-runtime \
    --features packaging --bin forge-steam-runtime-package -- \
    "$PROJECT_ROOT/build/steam-runtime/downloads" "$RUNTIME_PACKAGE"
RUNTIME_DESTINATION="$TARGET_BUILD_DIR/$UNLOCALIZED_RESOURCES_FOLDER_PATH/SteamRuntime"
mkdir -p "$RUNTIME_DESTINATION"
if ! cmp -s "$RUNTIME_PACKAGE" "$RUNTIME_DESTINATION/runtime.zip"; then
    cp "$RUNTIME_PACKAGE" "$RUNTIME_DESTINATION/runtime.zip"
fi
export CARGO_TARGET_DIR="$PROJECT_ROOT/build/steam-native-target"
cargo build --locked --manifest-path "$PROJECT_ROOT/crates/steam-native/Cargo.toml" \
    --workspace --bins --lib --profile "$PROFILE_NAME"
DESTINATION="$TARGET_BUILD_DIR/$CONTENTS_FOLDER_PATH/Helpers/SteamNative"
mkdir -p "$DESTINATION"
cp "$CARGO_TARGET_DIR/$PROFILE_DIRECTORY/forge-steam-host" "$DESTINATION/"
cp "$CARGO_TARGET_DIR/$PROFILE_DIRECTORY/libforge_steam_discovery_adapter.dylib" "$DESTINATION/"
IDENTITY="${EXPANDED_CODE_SIGN_IDENTITY:?A stable Apple signing identity is required}"
/usr/bin/codesign --force --sign "$IDENTITY" --options runtime \
    --identifier com.zagrios.forge.steam-host \
    --entitlements "$PROJECT_ROOT/packaging/steam-host.entitlements" "$DESTINATION/forge-steam-host"
/usr/bin/codesign --force --sign "$IDENTITY" \
    --identifier com.zagrios.forge.steam-discovery "$DESTINATION/libforge_steam_discovery_adapter.dylib"
