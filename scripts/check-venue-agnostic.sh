#!/usr/bin/env bash
# Venue-agnosticism check for nexum-runtime: the crate graph reaches no
# videre/intent/venue/cow crate, the sources carry no venue symbol, and
# nexum:host resolves as a leaf WIT package. Advisory in CI until the
# physical cut lands; run locally via `just check-venue-agnostic`.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.." || exit 2

pass() { printf '\033[1;32m[l1 PASS]\033[0m %s\n' "$*" >&2; }
fail() { printf '\033[1;31m[l1 FAIL]\033[0m %s\n' "$*" >&2; status=1; }

command -v rg >/dev/null || { echo "ripgrep (rg) is required" >&2; exit 2; }

status=0

# 1. Crate graph: nothing venue-shaped reachable from nexum-runtime
#    (normal + build edges; dev-deps stay local to the crate).
if tree="$(cargo tree -p nexum-runtime -e normal,build --prefix none --locked)"; then
    reached="$(printf '%s\n' "$tree" |
        awk '{print $1}' | sort -u | rg -i 'videre|intent|venue|cow' || true)"
    if [[ -n $reached ]]; then
        fail "crate graph reaches: $(tr '\n' ' ' <<<"$reached")"
    else
        pass "crate graph clean"
    fi
else
    fail "cargo tree failed"
fi

# 2. Symbol scan: no venue vocabulary anywhere in the crate. Word shapes
#    skip std::borrow::Cow, ProviderError, and "intentional".
symbols='\b[Vv]idere|\b[Ii]ntent([_A-Z-]|s?\b)|\b[Vv]enue|\bcow|CoW|\bCow[A-Z]'
rg -n --no-heading -e "$symbols" crates/nexum-runtime
case $? in
    0) fail "venue symbols leak into nexum-runtime" ;;
    1) pass "symbol scan empty" ;;
    *) fail "symbol scan errored (crates/nexum-runtime missing?)" ;;
esac

# 3. WIT DAG: nexum:host is a leaf. No cross-package use/import, and the
#    package resolves standalone.
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
