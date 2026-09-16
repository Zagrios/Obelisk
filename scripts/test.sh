#!/bin/bash
set -euo pipefail
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"
python3 "$PROJECT_ROOT/scripts/verify-localization.py"
cargo fmt --manifest-path "$PROJECT_ROOT/Cargo.toml" --all -- --check
cargo clippy --locked --manifest-path "$PROJECT_ROOT/Cargo.toml" --workspace --all-targets --all-features -- -D warnings
cargo test --locked --manifest-path "$PROJECT_ROOT/Cargo.toml" --workspace
cargo fmt --manifest-path "$PROJECT_ROOT/crates/steam-native/Cargo.toml" --all -- --check
cargo clippy --locked --manifest-path "$PROJECT_ROOT/crates/steam-native/Cargo.toml" --workspace --all-targets -- -D warnings
cargo test --locked --manifest-path "$PROJECT_ROOT/crates/steam-native/Cargo.toml" --workspace
xcodebuild -project "$PROJECT_ROOT/apps/macos/Forge.xcodeproj" -scheme Forge \
    -configuration Debug -derivedDataPath "$PROJECT_ROOT/build/xcode" \
    -destination 'platform=macOS' test
"$PROJECT_ROOT/scripts/verify-app-signature.sh" "$PROJECT_ROOT/build/xcode/Build/Products/Debug/Forge.app"
