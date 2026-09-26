#!/bin/bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
export CARGO_TARGET_DIR="$PROJECT_ROOT/target"
CONFIGURATION="${CONFIGURATION:-Debug}"
case "$CONFIGURATION" in
    Debug) CARGO_PROFILE=debug; PROFILE_NAME=dev ;;
    Release) CARGO_PROFILE=release; PROFILE_NAME=release ;;
    *) echo "Unsupported configuration: $CONFIGURATION" >&2; exit 1 ;;
esac

HOST_ARCH="$(uname -m)"
case "$HOST_ARCH" in
    arm64) HOST_TARGET=aarch64-apple-darwin ;;
    x86_64) HOST_TARGET=x86_64-apple-darwin ;;
    *) echo "Build this application on macOS." >&2; exit 1 ;;
esac

RUST_TARGET="${RUST_TARGET:-$HOST_TARGET}"
if [[ "$RUST_TARGET" != "$HOST_TARGET" ]]; then
    echo "This starter builds for the host architecture ($HOST_TARGET)." >&2
    echo "A universal distribution needs a separate multi-architecture build." >&2
    exit 1
fi

GENERATED="$PROJECT_ROOT/apps/macos/SailOver/Bridge/Generated"
STAGING="$PROJECT_ROOT/build/bindings/$CONFIGURATION"
LIBRARY_DIR="$PROJECT_ROOT/build/rust/$CONFIGURATION"
mkdir -p "$GENERATED" "$STAGING" "$LIBRARY_DIR"

cargo build --locked --manifest-path "$PROJECT_ROOT/Cargo.toml" -p sailover-bridge --lib \
    --target "$RUST_TARGET" --profile "$PROFILE_NAME"

cargo run --locked --manifest-path "$PROJECT_ROOT/Cargo.toml" -p sailover-bridge \
    --bin uniffi-bindgen --features cli -- generate \
    --library "$PROJECT_ROOT/target/$RUST_TARGET/$CARGO_PROFILE/libsailover_bridge.dylib" \
    --language swift --config "$PROJECT_ROOT/uniffi.toml" --out-dir "$STAGING" --no-format

# Preserve timestamps when generated contents have not changed.
for generated_file in "$STAGING"/*; do
    destination="$GENERATED/$(basename "$generated_file")"
    if ! cmp -s "$generated_file" "$destination"; then
        cp "$generated_file" "$destination"
    fi
done
MODULEMAP="$STAGING/SailOverBridgeFFI.modulemap"
if ! cmp -s "$MODULEMAP" "$GENERATED/module.modulemap"; then
    cp "$MODULEMAP" "$GENERATED/module.modulemap"
fi
cp "$PROJECT_ROOT/target/$RUST_TARGET/$CARGO_PROFILE/libsailover_bridge.a" "$LIBRARY_DIR/"
