#!/bin/bash
# Writes into crates/game-runtime/voile-release.json the newest release Voile's feed offers for the contract that file
# names: the release a build installs until it reads the feed itself, such as without the network. The Release
# workflow runs it before it builds, so the pin needs no commit of its own. The feed's signature is checked with the key
# Obelisk checks it with, in crates/game-runtime/src/voile/feed.rs. FEED_URL and KEY replace those, to try it.
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
PIN="$HERE/crates/game-runtime/voile-release.json"
FEED_URL="${FEED_URL:-https://raw.githubusercontent.com/Zagrios/Voile/main/voile-feed.json}"
KEY="${KEY:-$(sed -n 's/^const KEY: &str = "\(.*\)";$/\1/p' "$HERE/crates/game-runtime/src/voile/feed.rs")}"
OPENSSL="${OPENSSL:-$(brew --prefix openssl@3)/bin/openssl}"

fail() {
    echo "$*" >&2
    exit 1
}

[[ -x "$OPENSSL" ]] || brew install --quiet openssl@3
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
curl -fsSL "$FEED_URL" -o "$work/feed.json"
curl -fsSL "$FEED_URL.sig" -o "$work/feed.json.sig"
# An Ed25519 key's SubjectPublicKeyInfo is a fixed prefix, then the key's 32 bytes.
{
    printf '\x30\x2a\x30\x05\x06\x03\x2b\x65\x70\x03\x21\x00'
    base64 --decode <<<"$KEY"
} > "$work/key.der"
base64 --decode < "$work/feed.json.sig" > "$work/feed.sig"
"$OPENSSL" pkeyutl -verify -pubin -keyform DER -inkey "$work/key.der" -rawin -in "$work/feed.json" \
    -sigfile "$work/feed.sig" > /dev/null || fail "The feed's signature does not hold with Obelisk's key."

[[ "$(jq .format "$work/feed.json")" == 1 ]] || fail "The feed is of a format Obelisk does not read."
contract="$(jq .contract "$PIN")"
# Obelisk is built with a release, never a beta.
entry="$(jq -c --argjson contract "$contract" '
    [.releases[] | select(.contract == $contract and (.withdrawn | not) and (.version | test("beta") | not))]
    | sort_by(.version | ltrimstr("v") | [splits("[.-]")] | map(tonumber)) | last // empty' "$work/feed.json")"
[[ -n "$entry" ]] || fail "The feed offers no release of contract $contract."
jq --argjson entry "$entry" '{contract} + ($entry | {version, url, size, sha256})' "$PIN" > "$work/pin.json"
cp "$work/pin.json" "$PIN"
cat "$PIN"
