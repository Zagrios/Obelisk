#!/bin/bash
set -euo pipefail
# Writes Obelisk's Sparkle feed, which .github/workflows/appcast.yml attaches to
# the latest release: that release, and the newest published pre-release when
# it is newer, on the beta channel, which only players who take beta versions
# read. Each item holds its DMG's notes and the DMG's signature by the EdDSA
# key whose public half is SUPublicEDKey in apps/macos/Config/Info.plist. The
# key comes from $SPARKLE_ED_PRIVATE_KEY, as in CI, or else from the login
# Keychain, where generate_keys --account zagrios keeps it. Needs gh.
# scripts/version.sh says how versions and their betas order.
if [[ $# -ne 3 ]]; then
    echo "Usage: $0 REPOSITORY LATEST_TAG OUTPUT" >&2
    exit 2
fi
REPOSITORY="$1" LATEST="$2" OUTPUT="$3"
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=scripts/version.sh
source "$PROJECT_ROOT/scripts/version.sh"
# The tools of the Sparkle that apps/macos/project.yml pins.
SPARKLE_VERSION=2.10.0
SPARKLE_SHA256=c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c
TOOLS="$PROJECT_ROOT/build/sparkle-$SPARKLE_VERSION"
if [[ ! -x "$TOOLS/bin/sign_update" ]]; then
    mkdir -p "$TOOLS"
    curl -fsSL --retry 3 -o "$TOOLS.tar.xz.partial" \
        "https://github.com/sparkle-project/Sparkle/releases/download/$SPARKLE_VERSION/Sparkle-$SPARKLE_VERSION.tar.xz"
    if [[ "$(shasum -a 256 "$TOOLS.tar.xz.partial" | cut -d' ' -f1)" != "$SPARKLE_SHA256" ]]; then
        echo "Sparkle $SPARKLE_VERSION checksum mismatch." >&2
        exit 1
    fi
    tar -xJf "$TOOLS.tar.xz.partial" -C "$TOOLS" ./bin/sign_update
    rm "$TOOLS.tar.xz.partial"
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Prints the <item> of the release tagged $1, on channel $2 unless it is empty.
item() {
    local tag="$1" channel="$2" folder="$WORK/$1"
    mkdir -p "$folder"
    gh release download "$tag" -R "$REPOSITORY" --pattern 'Obelisk-*.dmg' --dir "$folder" >&2
    local dmgs=("$folder"/Obelisk-*.dmg)
    if [[ ${#dmgs[@]} -ne 1 ]]; then
        echo "$tag holds ${#dmgs[@]} Obelisk DMGs, not one." >&2
        exit 1
    fi
    local dmg="${dmgs[0]}"
    local url
    url="https://github.com/$REPOSITORY/releases/download/$tag/$(basename "$dmg")"
    local release published
    release="$(gh release view "$tag" -R "$REPOSITORY" --json body,publishedAt)"
    published="$(jq -r .publishedAt <<<"$release")"
    jq -r .body <<<"$release" > "$folder/notes.md"
    if [[ -n "$(tr -d '[:space:]' < "$folder/notes.md")" ]]; then
        gh api -X POST markdown -f mode=gfm -f context="$REPOSITORY" -F text=@"$folder/notes.md" > "$folder/notes.html"
    else
        : > "$folder/notes.html"
    fi

    # The versions the app inside the DMG declares, which Sparkle compares.
    local mount="$folder/mount"
    mkdir "$mount"
    /usr/bin/hdiutil attach -nobrowse -readonly -noautoopen -mountpoint "$mount" "$dmg" > /dev/null
    local info="$mount/Obelisk.app/Contents/Info.plist"
    local version short_version minimum_system
    version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$info")"
    short_version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$info")"
    minimum_system="$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$info")"
    /usr/bin/hdiutil detach -quiet "$mount"
    if [[ "$version" == *-* ]]; then
        echo "$tag's CFBundleVersion $version has a dash, which Sparkle reads as ${version%%-*}." >&2
        exit 1
    fi

    local signature
    if [[ -n "${SPARKLE_ED_PRIVATE_KEY:-}" ]]; then
        signature="$(printf '%s' "$SPARKLE_ED_PRIVATE_KEY" | "$TOOLS/bin/sign_update" --ed-key-file - -p "$dmg")"
    else
        signature="$("$TOOLS/bin/sign_update" --account zagrios -p "$dmg")"
    fi

    cat <<EOF
    <item>
      <title>Obelisk $short_version</title>
      <pubDate>$(LC_ALL=C date -j -u -f '%Y-%m-%dT%H:%M:%SZ' "$published" '+%a, %d %b %Y %H:%M:%S +0000')</pubDate>
      <sparkle:version>$version</sparkle:version>
      <sparkle:shortVersionString>$short_version</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>$minimum_system</sparkle:minimumSystemVersion>
EOF
    if [[ -n "$channel" ]]; then
        printf '      <sparkle:channel>%s</sparkle:channel>\n' "$channel"
    fi
    if [[ -s "$folder/notes.html" ]]; then
        # Sparkle shows the notes as HTML; a CDATA section cannot hold its own end.
        printf '      <description><![CDATA[%s]]></description>\n' "$(sed 's/]]>/]]]]><![CDATA[>/g' "$folder/notes.html")"
    fi
    cat <<EOF
      <enclosure url="$url" length="$(stat -f %z "$dmg")" type="application/octet-stream" sparkle:edSignature="$signature"/>
    </item>
EOF
}

# The latest release is what every player gets: never a beta, which a draft published as drafted would make it.
if [[ "$LATEST" == *.beta* ]] || ! NEWEST="$(version_key "$LATEST")"; then
    echo "The latest release $LATEST is no X.Y.Z version: make it a pre-release." >&2
    exit 1
fi
PRERELEASE=""
# Read first, so a list GitHub fails to give stops the script rather than writing a feed without the beta.
TAGS="$(gh release list -R "$REPOSITORY" --exclude-drafts --limit 100 --json tagName,isPrerelease \
    --jq '.[] | select(.isPrerelease) | .tagName')"
while IFS= read -r tag; do
    if key="$(version_key "$tag")" && [[ "$key" > "$NEWEST" ]]; then
        NEWEST="$key" PRERELEASE="$tag"
    fi
done <<<"$TAGS"

{
    cat <<EOF
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>Obelisk</title>
EOF
    item "$LATEST" ""
    if [[ -n "$PRERELEASE" ]]; then
        item "$PRERELEASE" beta
    fi
    cat <<EOF
  </channel>
</rss>
EOF
} > "$OUTPUT.partial"
mv "$OUTPUT.partial" "$OUTPUT"
echo "$OUTPUT lists $LATEST${PRERELEASE:+ and the pre-release $PRERELEASE}."
