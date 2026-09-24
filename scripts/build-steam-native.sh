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
# Optimized: the no-op path still inflates and hashes the whole payload, which
# costs seconds per incremental build when this tool is built unoptimized.
CARGO_TARGET_DIR="$PROJECT_ROOT/target" cargo run --locked --release \
    --manifest-path "$PROJECT_ROOT/Cargo.toml" -p game-runtime \
    --features packaging --bin forge-steam-runtime-package -- \
    "$PROJECT_ROOT/build/steam-runtime/downloads" "$RUNTIME_PACKAGE"
RUNTIME_DESTINATION="$TARGET_BUILD_DIR/$UNLOCALIZED_RESOURCES_FOLDER_PATH/SteamRuntime"
mkdir -p "$RUNTIME_DESTINATION"
if ! cmp -s "$RUNTIME_PACKAGE" "$RUNTIME_DESTINATION/runtime.zip"; then
    cp "$RUNTIME_PACKAGE" "$RUNTIME_DESTINATION/runtime.zip"
fi
export CARGO_TARGET_DIR="$PROJECT_ROOT/build/steam-native-target"
cargo build --locked --manifest-path "$PROJECT_ROOT/crates/steam-native/Cargo.toml" \
    --workspace --bin forge-steam-host --lib --profile "$PROFILE_NAME" --target aarch64-apple-darwin
cargo build --locked --manifest-path "$PROJECT_ROOT/crates/steam-native/Cargo.toml" \
    -p forge-steam-native --bin forge-steam-sdk --profile "$PROFILE_NAME" --target x86_64-apple-darwin
cargo build --locked --manifest-path "$PROJECT_ROOT/crates/steam-native/Cargo.toml" \
    -p forge-steam-discovery-adapter --lib --profile "$PROFILE_NAME" --target x86_64-apple-darwin
ARM_BUILD="$CARGO_TARGET_DIR/aarch64-apple-darwin/$PROFILE_DIRECTORY"
INTEL_BUILD="$CARGO_TARGET_DIR/x86_64-apple-darwin/$PROFILE_DIRECTORY"
DESTINATION="$TARGET_BUILD_DIR/$CONTENTS_FOLDER_PATH/Helpers/SteamNative"
mkdir -p "$DESTINATION"
cp "$ARM_BUILD/forge-steam-host" "$DESTINATION/forge-steam-host"
# Remove the former diagnostic helper from incremental app builds.
rm -f "$DESTINATION/forge-steam-sdk-arm64"
cp "$INTEL_BUILD/forge-steam-sdk" "$DESTINATION/forge-steam-sdk-x86_64"
ADAPTER="$DESTINATION/libforge_steam_discovery_adapter.dylib"
/usr/bin/lipo -create "$ARM_BUILD/libforge_steam_discovery_adapter.dylib" \
    "$INTEL_BUILD/libforge_steam_discovery_adapter.dylib" -output "$ADAPTER"
IDENTITY="${EXPANDED_CODE_SIGN_IDENTITY:?A stable Apple signing identity is required}"
/usr/bin/codesign --force --sign "$IDENTITY" --options runtime \
    --identifier com.zagrios.forge.steam-host \
    --entitlements "$PROJECT_ROOT/packaging/steam-host.entitlements" "$DESTINATION/forge-steam-host"
/usr/bin/codesign --force --sign "$IDENTITY" \
    --identifier com.zagrios.forge.steam-discovery "$ADAPTER"
/usr/bin/codesign --force --sign "$IDENTITY" --options runtime \
    --identifier com.zagrios.forge.steam-sdk-x86_64 \
    --entitlements "$PROJECT_ROOT/packaging/steam-host.entitlements" \
    "$DESTINATION/forge-steam-sdk-x86_64"

# Opening this Intel-only app through LaunchServices lets macOS present its
# Rosetta installer. A direct --probe invocation never starts the app run loop.
ROSETTA_SOURCE="$PROJECT_ROOT/packaging/rosetta"
ROSETTA_BUILD="$PROJECT_ROOT/build/rosetta"
ROSETTA_APP="$TARGET_BUILD_DIR/$CONTENTS_FOLDER_PATH/Helpers/Forge Rosetta.app"
mkdir -p "$ROSETTA_BUILD" "$ROSETTA_APP/Contents/MacOS"
ROSETTA_EXECUTABLE="$ROSETTA_BUILD/ForgeRosetta"
SDK_PATH="$(xcrun --sdk macosx --show-sdk-path)"
MINIMUM_MACOS="${MACOSX_DEPLOYMENT_TARGET:-14.0}"
BUILD_SETTINGS="$SDK_PATH|$MINIMUM_MACOS"
PREVIOUS_SETTINGS="$(cat "$ROSETTA_BUILD/settings" 2>/dev/null || true)"
if [[ ! -f "$ROSETTA_EXECUTABLE" \
    || "$ROSETTA_SOURCE/main.m" -nt "$ROSETTA_EXECUTABLE" \
    || "$0" -nt "$ROSETTA_EXECUTABLE" \
    || "$BUILD_SETTINGS" != "$PREVIOUS_SETTINGS" ]]; then
    xcrun --sdk macosx clang -arch x86_64 -isysroot "$SDK_PATH" \
        "-mmacosx-version-min=$MINIMUM_MACOS" -fobjc-arc -O2 -Wall -Wextra -Werror \
        -framework AppKit "$ROSETTA_SOURCE/main.m" -o "$ROSETTA_EXECUTABLE"
    printf '%s\n' "$BUILD_SETTINGS" > "$ROSETTA_BUILD/settings"
fi
cp "$ROSETTA_EXECUTABLE" "$ROSETTA_APP/Contents/MacOS/ForgeRosetta"
if ! cmp -s "$ROSETTA_SOURCE/Info.plist" "$ROSETTA_APP/Contents/Info.plist"; then
    cp "$ROSETTA_SOURCE/Info.plist" "$ROSETTA_APP/Contents/Info.plist"
fi
/usr/bin/codesign --force --sign "$IDENTITY" --options runtime "$ROSETTA_APP"
