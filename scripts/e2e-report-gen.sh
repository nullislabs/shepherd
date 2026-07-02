#!/usr/bin/env bash
# scripts/e2e-report-gen.sh — auto-fill the e2e-report template from
# the engine log + metrics-start/end snapshots + tx hashes captured
# during the run.
#
# Called by scripts/e2e-finish.sh, or stand-alone if the operator
# wants to regenerate the report after editing scripts/.state by
# hand.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/lib.sh"

require_cmd jq
require_cmd python3

[[ -f "$STATE_FILE" ]] || die "scripts/.state not found"
log_file="$(state_value LOG_FILE)"            || die "LOG_FILE missing"
metrics_start="$(state_value METRICS_START)" || die "METRICS_START missing"
metrics_end="$(state_value METRICS_END)"     || die "METRICS_END missing"
start_iso="$(state_value START_ISO)"         || start_iso="(unknown)"
end_iso="$(state_value END_ISO)"             || end_iso="(unknown)"

date_tag="$(date -u +%Y-%m-%d)"
report="$REPORTS_DIR/e2e-report-$date_tag.md"
template="$REPORTS_DIR/e2e-report.template.md"
[[ -f "$template" ]] || die "report template not found at $template"

log "report → $report"
log "deriving chain-coverage + per-module markers from $log_file"

python3 - "$log_file" "$metrics_start" "$metrics_end" "$start_iso" "$end_iso" "$template" "$report" "$STATE_FILE" <<'PY'
import json, os, re, sys
from pathlib import Path
from datetime import datetime, timezone

LOG, M_START, M_END, START_ISO, END_ISO, TEMPLATE, OUT, STATE = sys.argv[1:9]

# ── Parse engine log ─────────────────────────────────────────────────

blocks   = []   # list of dispatched block_numbers (per module, but we just want range)
markers  = {m: [] for m in ("twap-monitor","ethflow-watcher","price-alert","balance-tracker","stop-loss")}
errors   = []
trapped  = []
poisoned = []

# Per-module terminal-state log fingerprints. Derived from the
# host.log() call sites inside modules/*/src/strategy.rs. Any one
# match against `message` (when the event carries that module's
# name) counts as an acceptance marker.
MARKER_PATTERNS = {
    "twap-monitor":    ["watch:", "indexed watch:", "poll watch:"],
    "ethflow-watcher": ["ethflow submitted", "ethflow backoff", "ethflow dropped", "already submitted"],
    "price-alert":     ["TRIGGERED"],
    # balance-tracker logs each per-block diff as
    # "0x<addr> changed +N wei (prior=..., current=...)".
    "balance-tracker": ["changed +", "changed -"],
    "stop-loss":       ["TRIGGERED", "retry on next block", "stop-loss submitted",
                        "stop-loss dropped", "already submitted", "submitted:"],
}

def event_field(ev, key, default=None):
    """tracing-subscriber's JSON formatter puts message / module /
    block_number / target either at the top level (default) or
    under a nested `fields` object (older versions / different
    flatten modes). Look in both places."""
    if not isinstance(ev, dict):
        return default
    if key in ev:
        return ev[key]
    fields = ev.get("fields")
    if isinstance(fields, dict) and key in fields:
        return fields[key]
    return default

# Engine emits JSON to stdout by default (no --pretty-logs). Each
# line is one event.
with open(LOG) as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        msg    = event_field(ev, "message", "") or ""
        module = event_field(ev, "module")
        bn     = event_field(ev, "block_number")
        target = event_field(ev, "target", "") or ""
        if bn is not None:
            try:
                blocks.append(int(bn))
            except (TypeError, ValueError):
                pass
        if isinstance(msg, str) and module in markers:
            for needle in MARKER_PATTERNS.get(module, []):
                if needle in msg:
                    markers[module].append({"ts": ev.get("timestamp",""), "level": ev.get("level",""), "msg": msg})
                    break
        if ev.get("level") == "ERROR" and target.startswith("nexum_runtime"):
            errors.append({"ts": ev.get("timestamp",""), "msg": msg})
        if "trapped" in msg and module:
            trapped.append({"module": module, "msg": msg})
        if "poisoned" in msg and module:
            poisoned.append({"module": module, "msg": msg})

first_block = min(blocks) if blocks else None
last_block  = max(blocks) if blocks else None
block_delta = (last_block - first_block + 1) if blocks else 0

# ── Parse metrics ────────────────────────────────────────────────────

def parse_metrics(path):
    """Return dict of {name+label_set: float}."""
    out = {}
    if not os.path.isfile(path):
        return out
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            # name{labels} value  OR  name value
            m = re.match(r"^(\w+)(?:\{([^}]*)\})?\s+(.+)$", line)
            if not m:
                continue
            name, labels, val = m.groups()
            try:
                v = float(val)
            except ValueError:
                continue
            key = name + "{" + (labels or "") + "}"
            out[key] = v
    return out

ms = parse_metrics(M_START)
me = parse_metrics(M_END)

def delta(name_prefix):
    rows = []
    keys = sorted(set(k for k in {**ms, **me} if k.startswith(name_prefix)))
    for k in keys:
        s = ms.get(k, 0.0)
        e = me.get(k, 0.0)
        if e == 0.0 and s == 0.0:
            continue
        rows.append((k, s, e, e - s))
    return rows

shepherd_keys = [
    "shepherd_event_latency_seconds",  # histogram, will surface _sum/_count/_bucket
    "shepherd_module_errors_total",
    "shepherd_module_restarts_total",
    "shepherd_module_poisoned",
    "shepherd_chain_request_total",
    "shepherd_cow_api_submit_total",
    "shepherd_stream_reconnects_total",
]

# ── Tx hashes (from .state) ──────────────────────────────────────────

state_kv = {}
with open(STATE) as f:
    for line in f:
        line = line.strip()
        if "=" in line:
            k, v = line.split("=", 1)
            state_kv[k] = v

# ── Compose the report ───────────────────────────────────────────────

git_commit = os.popen("git -C $(dirname $0)/.. rev-parse HEAD 2>/dev/null").read().strip() or "(unknown)"
# Re-run cwd is /tmp/m3-base when invoked from finish.sh, but be safe:
git_commit = os.popen("git rev-parse HEAD 2>/dev/null").read().strip() or git_commit

lines = []
lines.append(f"# E2E testnet integration report — {datetime.now(timezone.utc).strftime('%Y-%m-%d')}")
lines.append("")
lines.append("> Auto-generated by `scripts/e2e-report-gen.sh`. Operator")
lines.append("> review each section + flesh out anomalies + sign off in")
lines.append("> section 8 before committing.")
lines.append("")
lines.append("## 1. Run metadata")
lines.append("")
lines.append("| Field | Value |")
lines.append("|---|---|")
lines.append("| Start (UTC) | " + START_ISO + " |")
lines.append("| End   (UTC) | " + END_ISO   + " |")
try:
    sdt = datetime.fromisoformat(START_ISO.replace("Z","+00:00"))
    edt = datetime.fromisoformat(END_ISO.replace("Z","+00:00"))
    dur = edt - sdt
    h, rem = divmod(int(dur.total_seconds()), 3600)
    m, _   = divmod(rem, 60)
    lines.append(f"| Wall clock  | {h}h {m}m |")
except Exception:
    lines.append("| Wall clock  | (parse error) |")
lines.append(f"| Engine commit | `{git_commit}` |")
lines.append("| Engine config | `engine.e2e.local.toml` (rendered from `engine.e2e.toml`) |")
lines.append("| RPC provider  | (filled by operator) |")
lines.append("")

lines.append("## 2. Chain coverage")
lines.append("")
lines.append("| Chain | First block | Last block | Block delta |")
lines.append("|---|---|---|---|")
lines.append(f"| Sepolia (11155111) | {first_block if first_block is not None else 'n/a'} | {last_block if last_block is not None else 'n/a'} | {block_delta} |")
lines.append("")
bar = 1500
lines.append(f"Acceptance: block delta ≥ {bar} → " + ("**PASS**" if block_delta >= bar else "**FAIL**"))
lines.append("")

lines.append("## 3. On-chain actions submitted")
lines.append("")
def tx_row(kind, label):
    h = state_kv.get(f"TX_{kind}")
    if not h:
        return f"| {label} | _(not run)_ |"
    return f"| {label} | [{h}](https://sepolia.etherscan.io/tx/{h}) |"
lines.append("| Action | Tx |")
lines.append("|---|---|")
lines.append(tx_row("TWAP",    "TWAP ComposableCoW.create()"))
lines.append(tx_row("ETHFLOW", "EthFlow.createOrder()"))
lines.append(tx_row("WRAP",    "WETH9.deposit() (optional)"))
lines.append(tx_row("PRESIGN", "setPreSignature (optional)"))
lines.append(tx_row("APPROVE", "WETH approve to GPv2VaultRelayer (optional)"))
lines.append("")

lines.append("## 4. Per-module terminal-state markers")
lines.append("")
lines.append("| Module | First marker | Sample line |")
lines.append("|---|---|---|")
for m in ("twap-monitor","ethflow-watcher","price-alert","balance-tracker","stop-loss"):
    if markers[m]:
        first = markers[m][0]
        # Truncate the marker line for the table
        sample = first["msg"]
        if len(sample) > 100:
            sample = sample[:97] + "..."
        sample = sample.replace("|", "\\|")
        lines.append(f"| {m} | {first['ts']} | `{sample}` |")
    else:
        lines.append(f"| {m} | _(none observed)_ | |")
lines.append("")

lines.append("## 5. Error counts (Prometheus delta)")
lines.append("")
lines.append("| Metric | Start | End | Delta |")
lines.append("|---|---|---|---|")
any_delta = False
for prefix in shepherd_keys:
    for k, s, e, d in delta(prefix):
        any_delta = True
        lines.append(f"| `{k}` | {s:g} | {e:g} | {d:g} |")
if not any_delta:
    lines.append("| _(no non-zero counters surfaced — check metrics files exist + endpoint was reachable)_ | | | |")
lines.append("")

lines.append("## 6. Anomalies + defects")
lines.append("")
if errors:
    lines.append(f"- `ERROR` lines from `nexum_runtime::*`: **{len(errors)}** (first: `{errors[0]['msg'][:80]}`)")
if trapped:
    lines.append(f"- `trapped` events: **{len(trapped)}** ({set(t['module'] for t in trapped)})")
if poisoned:
    lines.append(f"- `poisoned` events: **{len(poisoned)}** ({set(p['module'] for p in poisoned)})")
if not (errors or trapped or poisoned):
    lines.append("- _(no automatic anomalies surfaced. Operator: do a final spot-check of the engine log and add any human-noticed weirdness here, then file an issue for each.)_")
lines.append("")

lines.append("## 7. Acceptance checklist")
lines.append("")
def check(ok, label):
    return f"- [{'x' if ok else ' '}] {label}"
lines.append(check(block_delta >= bar, f"block delta ≥ {bar} (got {block_delta})"))
five_markers = all(bool(markers[m]) for m in markers)
lines.append(check(five_markers, "all 5 modules emitted ≥ 1 terminal-state marker"))
zero_trap_modules = []
for k, s, e, d in delta("shepherd_module_errors_total"):
    if 'error_kind="trap"' in k and d > 0:
        zero_trap_modules.append(k)
lines.append(check(not zero_trap_modules, f"shepherd_module_errors_total{{error_kind=\"trap\"}} == 0 (offenders: {zero_trap_modules or 'none'})"))
poisoned_keys = [k for k,v in me.items() if k.startswith("shepherd_module_poisoned") and v != 0.0]
lines.append(check(not poisoned_keys, f"no module poisoned at end (offenders: {poisoned_keys or 'none'})"))
lines.append(check(not errors, f"0 ERROR lines from nexum_runtime::* (got {len(errors)})"))
lines.append(check(state_kv.get("TX_TWAP") and state_kv.get("TX_ETHFLOW"), "TWAP + EthFlow on-chain txs submitted"))
lines.append("")

lines.append("## 8. Sign-off (operator)")
lines.append("")
lines.append("> Auto-generated report. Operator: in 1-2 sentences confirm whether this run is clean enough to proceed to the 7-day soak. If any acceptance row above is `[ ]`, file the defect before signing off.")
lines.append("")
lines.append("…")
lines.append("")

lines.append("## 9. Attachments")
lines.append("")
lines.append(f"- Engine log: `{os.path.relpath(LOG, os.path.dirname(OUT))}`")
lines.append(f"- Metrics start: `{os.path.relpath(M_START, os.path.dirname(OUT))}`")
lines.append(f"- Metrics end:   `{os.path.relpath(M_END,   os.path.dirname(OUT))}`")
lines.append("")

Path(OUT).write_text("\n".join(lines))
print(f"wrote {OUT}", file=sys.stderr)
PY

log "report written. Next: review + add anomalies + sign off + commit:"
log "  \$EDITOR $report"
log "  git add -f $report"
log "  git commit -m 'ops(e2e): run report ${date_tag}'"
