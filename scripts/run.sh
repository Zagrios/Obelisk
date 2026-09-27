#!/bin/bash
set -euo pipefail
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
"$PROJECT_ROOT/scripts/build.sh"
APP="$PROJECT_ROOT/build/xcode/Build/Products/${CONFIGURATION:-Debug}/SailOver.app"
# Quitting SailOver only hides it, so a running build would stay in place:
# end it, and with it any game it runs, before opening the new one.
if pkill -f "$APP/Contents/MacOS/SailOver"; then
    while pgrep -f "$APP/Contents/MacOS/SailOver" >/dev/null; do sleep 0.2; done
fi
open "$APP"

