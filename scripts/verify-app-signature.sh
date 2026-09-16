#!/bin/bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "Usage: $0 APP" >&2
    exit 2
fi

# An ad hoc signature can be valid, but its identity changes with the binary.
# Require an Apple-issued certificate as well as a valid bundle signature.
if ! /usr/bin/codesign --verify --deep --strict -R='anchor apple generic' "$1"; then
    echo "Forge needs a valid Apple certificate signature to preserve Keychain access across builds." >&2
    echo "Configure apps/macos/Config/Signing.local.xcconfig using the example beside it." >&2
    exit 1
fi
