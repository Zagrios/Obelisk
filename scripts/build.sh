#!/bin/bash
set -euo pipefail
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFIGURATION="${CONFIGURATION:-Debug}"
xcodebuild -project "$PROJECT_ROOT/apps/macos/SailOver.xcodeproj" -scheme SailOver \
    -configuration "$CONFIGURATION" -derivedDataPath "$PROJECT_ROOT/build/xcode" \
    -destination 'platform=macOS' build
"$PROJECT_ROOT/scripts/verify-app-signature.sh" "$PROJECT_ROOT/build/xcode/Build/Products/$CONFIGURATION/SailOver.app"
echo "Application: $PROJECT_ROOT/build/xcode/Build/Products/$CONFIGURATION/SailOver.app"
