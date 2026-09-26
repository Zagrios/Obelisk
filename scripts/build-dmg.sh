#!/bin/bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUTPUT_DIRECTORY="$PROJECT_ROOT/build/dist"
BUILD_APPLICATION=true

while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-build) BUILD_APPLICATION=false; shift ;;
        --output-dir)
            if [[ $# -lt 2 || -z "$2" || "$2" == --* ]]; then
                echo "--output-dir requires a directory." >&2
                exit 2
            fi
            OUTPUT_DIRECTORY="$2"
            shift 2
            ;;
        --help|-h)
            echo "Usage: $0 [--skip-build] [--output-dir DIRECTORY]"
            echo "Build the Release application and a drag-and-drop DMG for its architecture."
            exit 0
            ;;
        *) echo "Unknown argument: $1" >&2; exit 2 ;;
    esac
done

if [[ "$(uname -s)" != Darwin ]]; then
    echo "Create the DMG on macOS with Xcode installed." >&2
    exit 1
fi

if [[ "$BUILD_APPLICATION" == true ]]; then
    CONFIGURATION=Release "$PROJECT_ROOT/scripts/build.sh"
fi

APP="$PROJECT_ROOT/build/xcode/Build/Products/Release/SailOver.app"
INFO="$APP/Contents/Info.plist"
LAYOUT="$PROJECT_ROOT/packaging/macos/FinderLayout.dsstore"
if [[ ! -f "$INFO" || ! -f "$LAYOUT" ]]; then
    echo "Missing Release app or Finder layout. Run without --skip-build." >&2
    exit 1
fi

VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$INFO")"
EXECUTABLE="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$INFO")"
ARCHITECTURES="$(/usr/bin/lipo -archs "$APP/Contents/MacOS/$EXECUTABLE" | tr ' ' '-')"
case "$VERSION-$ARCHITECTURES" in
    *[!a-zA-Z0-9._+-]*) echo "Invalid version or architecture in DMG filename." >&2; exit 1 ;;
esac
"$PROJECT_ROOT/scripts/verify-app-signature.sh" "$APP"

mkdir -p "$OUTPUT_DIRECTORY" "$PROJECT_ROOT/build/dmg"
OUTPUT_DIRECTORY="$(cd "$OUTPUT_DIRECTORY" && pwd)"
STAGING="$(mktemp -d "$PROJECT_ROOT/build/dmg/staging.XXXXXX")"
# This directory is created by this invocation and contains only generated files.
trap 'rm -rf "$STAGING"' EXIT
mkdir "$STAGING/content"
/usr/bin/ditto "$APP" "$STAGING/content/SailOver.app"
ln -s /Applications "$STAGING/content/Applications"
cp "$LAYOUT" "$STAGING/content/.DS_Store"

NAME="SailOver-$VERSION-$ARCHITECTURES.dmg"
/usr/bin/hdiutil create -volname "Installer SailOver" -fs HFS+ \
    -srcfolder "$STAGING/content" -format UDZO \
    -imagekey zlib-level=9 "$STAGING/$NAME"
/usr/bin/hdiutil verify "$STAGING/$NAME"
mv -f "$STAGING/$NAME" "$OUTPUT_DIRECTORY/$NAME"
(
    cd "$OUTPUT_DIRECTORY"
    /usr/bin/shasum -a 256 "$NAME" > "$STAGING/$NAME.sha256"
)
mv -f "$STAGING/$NAME.sha256" "$OUTPUT_DIRECTORY/$NAME.sha256"

echo "DMG: $OUTPUT_DIRECTORY/$NAME"
echo "SHA-256: $OUTPUT_DIRECTORY/$NAME.sha256"
echo "This build keeps the application's current signature. Public distribution requires Developer ID signing and notarization."
