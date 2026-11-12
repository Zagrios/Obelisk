#!/bin/bash
# Obelisk's version order, as Sparkle compares CFBundleVersion: X.Y.Z, after its betas X.Y.Z.beta<n>, such as
# 0.6.2 < 0.6.3.beta1 < 0.6.3.beta2 < 0.6.3.beta10 < 0.6.3. Sparkle ignores what follows a dash: 0.6.3-beta.1 would
# read as 0.6.3. Sourced by beta.sh and write-appcast.sh.

# A key that orders version or tag $1 as Sparkle orders it, compared as text; fails for any other name.
version_key() {
    [[ "${1#v}" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)(\.beta([0-9]+))?$ ]] || return 1
    local stage=1 beta=0
    if [[ -n "${BASH_REMATCH[4]}" ]]; then
        stage=0 beta="${BASH_REMATCH[5]}"
    fi
    printf '%010d%010d%010d%d%010d' "$((10#${BASH_REMATCH[1]}))" "$((10#${BASH_REMATCH[2]}))" \
        "$((10#${BASH_REMATCH[3]}))" "$stage" "$((10#$beta))"
}
