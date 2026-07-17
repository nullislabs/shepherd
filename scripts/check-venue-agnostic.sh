#!/usr/bin/env bash
# Zero-leak check for the host layer, scoped precisely: no host-layer
# crate graph (runtime, launcher, bare engine) reaches a
# videre/intent/venue/cow crate; the runtime Rust sources carry no
# charter symbol
# (videre:|videre_host|Venue[A-Z]|EgressGuard|synthesize_venue|value-flow)
# and no privileged router field; and nexum:host names no foreign WIT
# package and resolves as a leaf. The opaque-status envelope
# (intent-status-update, its venue id string) is ratified host surface,
# not a leak. Blocking in CI; run locally via `just check-venue-agnostic`.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.." || exit 2

pass() { printf '\033[1;32m[l1 PASS]\033[0m %s\n' "$*" >&2; }
fail() { printf '\033[1;31m[l1 FAIL]\033[0m %s\n' "$*" >&2; status=1; }

command -v rg >/dev/null || { echo "ripgrep (rg) is required" >&2; exit 2; }

status=0

# 1. Crate graph: nothing venue-shaped reachable from the host-layer
#    crates - the runtime, the generic launcher, and the bare engine
#    binary (normal + build edges; dev-deps stay local to the crate).
for crate in nexum-runtime nexum-launch nexum-cli; do
    if tree="$(cargo tree -p "$crate" -e normal,build --all-features --prefix none --locked)"; then
        reached="$(printf '%s\n' "$tree" |
            awk '{print $1}' | sort -u | rg -i 'videre|intent|venue|cow' || true)"
        if [[ -n $reached ]]; then
            fail "$crate crate graph reaches: $(tr '\n' ' ' <<<"$reached")"
        else
            pass "$crate crate graph clean"
        fi
    else
        fail "cargo tree failed for $crate"
    fi
done

# 2. Symbol scan: the charter set (the current venue vocabulary that would
#    signal a leak - videre WIT/crate refs, the Venue* types, the egress
#    guard). Section 1 guards dependency edges; this scan stays curated to
#    the live post-rename names so opaque extension payloads never false-flag.
charter='videre:|videre_host|Venue[A-Z]|EgressGuard|synthesize_venue|value-flow'
rg -n --no-heading -e "$charter" crates/nexum-runtime/src
case $? in
    0) fail "charter symbols leak into nexum-runtime" ;;
    1) pass "symbol scan empty" ;;
    *) fail "symbol scan errored (crates/nexum-runtime/src missing?)" ;;
esac

# 3. Privileged-field scan: the venue registry rides the extension
#    service map; no `VenueRegistry` router field may return to the
#    runtime. (The charter scan above also catches the type; this stays
#    as the named guard for that specific invariant.)
rg -n --no-heading -e 'VenueRegistry' crates/nexum-runtime/src
case $? in
    0) fail "a privileged router field returned to nexum-runtime" ;;
    1) pass "no privileged router field" ;;
    *) fail "field scan errored (crates/nexum-runtime/src missing?)" ;;
esac

# 4. WIT surface: nexum:host is a leaf. No foreign package named
#    anywhere in its sources, no cross-package use/import, and the
#    package resolves standalone. The opaque-status envelope stays.
wit_charter='nexum:intent|nexum:adapter|value-flow|videre:|shepherd:cow'
rg -n --no-heading -e "$wit_charter" wit/nexum-host
case $? in
    0) fail "a foreign WIT namespace leaks into wit/nexum-host" ;;
    1) pass "no foreign WIT namespace named" ;;
    *) fail "WIT namespace scan errored (wit/nexum-host missing?)" ;;
esac
rg -n --no-heading -e '^\s*(use|import)\s+[a-z0-9-]+:' wit/nexum-host
case $? in
    0) fail "nexum:host references another WIT package" ;;
    1) pass "nexum:host has no cross-package reference" ;;
    *) fail "WIT scan errored (wit/nexum-host missing?)" ;;
esac
if command -v wasm-tools >/dev/null; then
    if wasm-tools component wit wit/nexum-host >/dev/null; then
        pass "nexum:host resolves standalone"
    else
        fail "nexum:host does not resolve standalone"
    fi
else
    printf '\033[1;33m[l1 WARN]\033[0m wasm-tools not found; WIT resolve skipped\n' >&2
fi

exit "$status"
