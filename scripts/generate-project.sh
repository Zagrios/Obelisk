#!/bin/bash
set -euo pipefail
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
"$PROJECT_ROOT/scripts/build-rust.sh"
XCODEGEN="${XCODEGEN:-xcodegen}"
if ! command -v "$XCODEGEN" >/dev/null 2>&1; then
    echo "XcodeGen is only needed to regenerate the project. Install it with: brew install xcodegen" >&2
    exit 1
fi
"$XCODEGEN" generate --spec "$PROJECT_ROOT/apps/macos/project.yml"

