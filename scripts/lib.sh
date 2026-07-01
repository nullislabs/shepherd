# scripts/lib.sh — shared bash helpers for the E2E automation.
# Source this from each e2e-*.sh; do not run it directly.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENV_FILE="$SCRIPT_DIR/.env"
STATE_FILE="$SCRIPT_DIR/.state"
REPORTS_DIR="$REPO_ROOT/docs/operations/e2e-reports"

# Pinned identities — match docs/operations/e2e-prep.md
# section 0. If you change one, change them in lock-step and re-run
# `cargo test -p stop-loss --lib e2e_settings_yield_expected_uid`.
TEST_EOA="0x7bF140727D27ea64b607E042f1225680B40ECa6A"
TEST_SAFE="0x14995a1118Caf95833e923faf8Dd155721cd53c2"
COMPOSABLE_COW="0xfdaFc9d1902f4e0b84f65F49f244b32b31013b74"
TWAP_HANDLER="0x6cF1e9cA41f7611dEf408122793c358a3d11E5a5"
ETHFLOW="0xbA3cB449bD2B4ADddBc894D8697F5170800EAdeC"
GPV2_SETTLEMENT="0x9008D19f58AAbD9eD0D60971565AA8510560ab41"
GPV2_VAULT_RELAYER="0xc92e8bdf79f0507f65a392b0ab4667716bfe0110"
WETH_SEPOLIA="0xfFf9976782d46CC05630D1f6eBAb18b2324d6B14"
COW_SEPOLIA="0x0625aFB445C3B6B7B929342a04A22599fd5dBB59"
EXPECTED_ORDER_UID="0xc2b9cb4ea1ee5a86d8049ac09d8f494bf04cca0a68407285f31e2e6379800be87bf140727d27ea64b607e042f1225680b40eca6affffffff"

log()  { printf "\033[1;34m[e2e]\033[0m %s\n" "$*" >&2; }
warn() { printf "\033[1;33m[e2e WARN]\033[0m %s\n" "$*" >&2; }
die()  { printf "\033[1;31m[e2e FAIL]\033[0m %s\n" "$*" >&2; exit 1; }

require_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "missing dependency: $1 — install before running"
}

load_env() {
    [[ -f "$ENV_FILE" ]] || die "scripts/.env not found. Run: cp scripts/env-template scripts/.env && \$EDITOR scripts/.env"
    set -a
    # shellcheck disable=SC1090
    source "$ENV_FILE"
    set +a
    [[ -n "${RPC_URL_SEPOLIA:-}"      ]] || die "RPC_URL_SEPOLIA unset in scripts/.env"
    [[ -n "${RPC_URL_SEPOLIA_HTTP:-}" ]] || die "RPC_URL_SEPOLIA_HTTP unset in scripts/.env"
    [[ "${RPC_URL_SEPOLIA}"      == wss*  ]] || die "RPC_URL_SEPOLIA must be wss:// (engine uses eth_subscribe)"
    [[ "${RPC_URL_SEPOLIA_HTTP}" == http* ]] || die "RPC_URL_SEPOLIA_HTTP must be http(s)://"
}

# Render engine.e2e.toml -> engine.e2e.local.toml with the rpc_url
# substituted in. engine.e2e.local.toml is gitignored (via *.local.toml)
# so the URL with embedded key never leaks into git history.
render_engine_config() {
    local src="$REPO_ROOT/engine.e2e.toml"
    local dst="$REPO_ROOT/engine.e2e.local.toml"
    [[ -f "$src" ]] || die "engine.e2e.toml not found at $src"

    # We do the substitution via python -c to avoid any sed escape
    # issues with the URL.
    RPC_URL_SEPOLIA="$RPC_URL_SEPOLIA" python3 - "$src" "$dst" <<'PY'
import os, re, sys
src, dst = sys.argv[1], sys.argv[2]
rpc = os.environ["RPC_URL_SEPOLIA"]
with open(src) as f:
    content = f.read()
# Match the rpc_url line inside [chains.11155111] block. The toml is
# small + we control its shape — a regex is safe here.
new = re.sub(
    r'(\[chains\.11155111\]\nrpc_url\s*=\s*)"[^"]*"',
    lambda m: m.group(1) + f'"{rpc}"',
    content,
    count=1,
)
if new == content:
    sys.exit("could not substitute rpc_url in engine.e2e.toml")
with open(dst, "w") as f:
    f.write(new)
PY
    log "rendered $dst"
}

write_state() { printf '%s\n' "$@" >> "$STATE_FILE"; }
read_state()  { [[ -f "$STATE_FILE" ]] && cat "$STATE_FILE" || true; }
clear_state() { rm -f "$STATE_FILE"; }

state_value() {
    local key="$1"
    [[ -f "$STATE_FILE" ]] || return 1
    grep -E "^${key}=" "$STATE_FILE" | tail -1 | cut -d= -f2-
}
