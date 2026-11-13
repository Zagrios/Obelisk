#!/bin/bash
# Decides Obelisk's automatic beta, which the Beta workflow builds every two hours: whether main changed since the
# highest version published, release or beta, beyond what a build never reads and the tables players read from main
# anyway; and the version it takes: the next beta of that beta's version, else of the next patch, or of main's
# MARKETING_VERSION when that is newer, such as 0.6.3.beta2 after the release 0.6.2 and its beta 1 (scripts/version.sh
# says how Sparkle orders them). A beta whose build failed is built again only once main changed, or when the workflow
# is run by hand ($GITHUB_EVENT_NAME workflow_dispatch). Prints `due`, `version` (without its v) and `since` (that
# highest version's tag) as KEY=VALUE lines for $GITHUB_OUTPUT.
#
# With --notes SINCE, prints the beta's notes instead: the subject of each commit since the tag SINCE.
#
# Needs gh, jq and git, and this checkout with its history and tags.
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPOSITORY="${GITHUB_REPOSITORY:-Zagrios/Obelisk}"
# shellcheck source=scripts/version.sh
source "$HERE/scripts/version.sh"
# What makes no beta: files a build never reads, the scripts that only publish or test it, and the game profiles and
# notes, which players read from main.
IGNORED=(
    ':(exclude)*.md'
    ':(exclude)docs'
    ':(exclude).github'
    ':(exclude)scripts/beta.sh'
    ':(exclude)scripts/write-appcast.sh'
    ':(exclude)scripts/test.sh'
    ':(exclude)scripts/run.sh'
    ':(exclude)crates/game-runtime/profiles.json'
    ':(exclude)crates/game-runtime/notes.json'
)

fail() {
    echo "$*" >&2
    exit 1
}

if [[ "${1:-}" == --notes ]]; then
    since="${2:?usage: beta.sh --notes SINCE}"
    echo "Automatic beta, built from main at $(git -C "$HERE" rev-parse --short HEAD). Changes since $since:"
    echo
    git -C "$HERE" log --no-merges --format='- %s (%h)' "$since..HEAD" -- . "${IGNORED[@]}"
    exit 0
fi

# Every release, drafts too, through GitHub's GraphQL API, which sends only these fields.
releases="$(gh release list -R "$REPOSITORY" --limit 10000 --json tagName,isDraft,publishedAt)"
# The highest version players can have: the beta's notes and changes start there.
since="" since_key=""
while IFS= read -r tag; do
    if key="$(version_key "$tag")" && [[ "$key" > "$since_key" ]]; then
        since="$tag" since_key="$key"
    fi
done <<<"$(jq -r '.[] | select(.isDraft | not) | .tagName' <<<"$releases")"
[[ -n "$since" ]] || fail "No release to start from."
git -C "$HERE" rev-parse --quiet --verify "refs/tags/$since^{commit}" > /dev/null ||
    fail "Tag $since is not in this checkout: fetch it with its history."

# The version whose betas come next: that of the newest beta, else the patch after the newest release, unless main's
# MARKETING_VERSION names a newer one, such as 0.7.0 to start its betas.
if [[ "$since" == *.beta* ]]; then
    next="${since#v}"
    next="${next%.beta*}"
else
    IFS=. read -r major minor patch <<<"${since#v}"
    next="$major.$minor.$(( patch + 1 ))"
fi
marketing="$(sed -n 's/^MARKETING_VERSION = //p' "$HERE/apps/macos/Config/App.xcconfig")"
if [[ "$marketing" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ && "$(version_key "$marketing")" > "$(version_key "$next")" ]]; then
    next="$marketing"
fi
# A deleted release leaves its tag: its number is never used again.
beta="$( { jq -r '.[].tagName' <<<"$releases"; git -C "$HERE" tag -l "v$next.beta*"; } |
    sed -n "s/^v${next//./\\.}\\.beta\\([0-9][0-9]*\\)$/\\1/p" | sort -n | tail -n 1)"
version="$next.beta$(( ${beta:-0} + 1 ))"

# The newest change in the history between $1 and HEAD that a build reads.
changed_since() {
    git -C "$HERE" log -1 --format=%H "$1..HEAD" -- . "${IGNORED[@]}"
}

# The newest Beta run on main after $1 whose build failed, or ran out of time, which GitHub calls cancelled, as JSON,
# if any. A run that failed before building, such as on a GitHub error, holds nothing back.
last_failed_build() {
    local runs id
    runs="$(gh run list -R "$REPOSITORY" --workflow beta.yml --branch main --limit 30 \
        --json databaseId,headSha,createdAt,conclusion \
        --jq ".[] | select((.conclusion == \"failure\" or .conclusion == \"cancelled\") and .createdAt > \"$1\")
            | @json")" || return 0
    while IFS= read -r run; do
        [[ -n "$run" ]] || continue
        id="$(jq -r .databaseId <<<"$run")"
        if [[ "$(gh api "repos/$REPOSITORY/actions/runs/$id/jobs" --jq '[.jobs[] | select(.name | startswith("build"))
            | .conclusion] | any(. == "failure" or . == "cancelled")' 2>/dev/null)" == true ]]; then
            echo "$run"
            return 0
        fi
    done <<<"$runs"
}

due=false
[[ -z "$(changed_since "$since")" ]] || due=true

# A beta that failed is not built again on the same code: only a change since that run started, or a run started by
# hand builds again.
if [[ "$due" == true && "${GITHUB_EVENT_NAME:-}" != workflow_dispatch ]]; then
    published="$(jq -r --arg tag "$since" '.[] | select(.tagName == $tag) | .publishedAt' <<<"$releases")"
    failed="$(last_failed_build "$published")"
    if [[ -n "$failed" ]]; then
        head="$(jq -r .headSha <<<"$failed")"
        if git -C "$HERE" cat-file -e "$head^{commit}" 2>/dev/null && [[ -z "$(changed_since "$head")" ]]; then
            echo "The beta built from $head failed, and nothing changed since: run Beta by hand to try again." >&2
            due=false
        fi
    fi
fi
printf 'due=%s\nversion=%s\nsince=%s\n' "$due" "$version" "$since"
