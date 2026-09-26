#!/bin/bash
set -euo pipefail
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
"$PROJECT_ROOT/scripts/build.sh"
open "$PROJECT_ROOT/build/xcode/Build/Products/${CONFIGURATION:-Debug}/SailOver.app"

