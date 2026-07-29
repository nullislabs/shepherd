#!/usr/bin/env bash
set -euo pipefail

# ------------------------------------------------------------------ 0. config + guard
REPO="nullislabs/shepherd"
OWNER="nullislabs"
PROJECT="1"
PROJECT_ID="PVT_kwDODm2Wqs4BcJ3a"

COMPONENT_FIELD="PVTSSF_lADODm2Wqs4BcJ3azhW0t-M"
STATUS_FIELD="PVTSSF_lADODm2Wqs4BcJ3azhW0t9I"
STATUS_TODO="f75ad846"

# per-issue body files live in bodies/<key>.md next to this script
BODIES="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/bodies"

DRY_RUN="${DRY_RUN:-1}"
run(){ if [ "$DRY_RUN" = 1 ]; then echo "+ $*"; else echo ">> $*"; eval "$*"; fi; }

# per-key state captured at create/ensure time (declared early: the sub-issue helpers read it)
declare -A NUM ID ITEM

# --- native sub-issue helpers (used from step 2 onward). KEY refs resolve via NUM/ID; #NNN via cache or REST.
resolve_num(){ local r="$1"; if [[ "$r" == \#* ]]; then echo "${r#\#}"; else echo "${NUM[$r]}"; fi; }
resolve_id(){
  local r="$1"
  if [[ "$r" == \#* ]]; then
    if [ -n "${ID[$r]:-}" ]; then echo "${ID[$r]}"
    elif [ "$DRY_RUN" = 1 ]; then echo "<id:$r>"
    else gh api "repos/$REPO/issues/${r#\#}" -q .id; fi
  else echo "${ID[$r]}"; fi
}
# link_sub PARENT_REF CHILD_REF  (native sub-issue; detach any existing DIFFERENT parent first, then attach)
link_sub(){
  local p c cn; p="$(resolve_num "$1")"; c="$(resolve_id "$2")"; cn="$(resolve_num "$2")"
  if [ "$DRY_RUN" != 1 ] && [[ "$cn" =~ ^[0-9]+$ ]]; then
    local pp
    pp="$(gh api graphql -f query="query{repository(owner:\"$OWNER\",name:\"shepherd\"){issue(number:$cn){parent{number}}}}" -q '.data.repository.issue.parent.number' 2>/dev/null || echo "")"
    if [ -n "$pp" ] && [ "$pp" != "null" ] && [ "$pp" != "$p" ]; then
      echo ">> detach #$cn from current parent #$pp before re-parent to #$p"
      gh api -X DELETE "repos/$REPO/issues/$pp/sub_issue" -F sub_issue_id="$c" >/dev/null 2>&1 || true
    fi
  fi
  run "gh api repos/$REPO/issues/$p/sub_issues -F sub_issue_id=$c"
}
# unlink_sub OLD_PARENT_REF CHILD_REF  (remove a stale parent link before re-parenting)
unlink_sub(){
  local p c; p="$(resolve_num "$1")"; c="$(resolve_id "$2")"
  run "gh api -X DELETE repos/$REPO/issues/$p/sub_issue -F sub_issue_id=$c"
}

# --- Project Component field: the 12 options (name -> single-select option id)
declare -A COMP_OPT=(
  [Engine]=6f70e61b [Runtime/Lifecycle]=b73f2d05 [Host Backends]=443041f1 [Chain]=c11b797e
  [Storage]=a2eb71e6 [WIT/ABI]=e077d895 [CoW]=63aaa607 [SDK/DX]=74e5d4be [Modules]=6ef6a455
  [Observability]=7e739f27 [Tooling/Packaging]=461d450e [Docs]=526e258a
)
# --- matching component/* label per Component (Docs carries no component label; the docs kind suffices)
declare -A COMP_LABEL=(
  [Engine]=component/engine-runtime [Runtime/Lifecycle]=component/lifecycle [Host Backends]=component/engine-host
  [Chain]=component/chain [Storage]=component/local-store [WIT/ABI]=component/wit-abi [CoW]=component/cow-integration
  [SDK/DX]=component/sdk [Modules]=component/modules [Observability]=component/observability
  [Tooling/Packaging]=component/tools [Docs]=""
)

# --- clean milestone titles (nine-milestone scheme; every issue/epic uses these)
M0="M0: Runtime architecture and lifecycle"
M1="M1: Videre contract reshape and host-intent decoupling"
M2="M2: Generic venue-agnostic host"
M3="M3: Videre SDK, macros and DX"
M4="M4: CoW on the generic seam (the shepherd bundle)"
M5="M5: The gated three-repo split"
M6="M6: Second-venue acceptance and vocabulary freeze"
M7="M7: Egress guard"
M8="M8: Post-v1 hardening and debt"
# create_issue KEY TITLE SUMMARY MILESTONE KIND_LABELS_CSV COMPONENT [BLOCKED_BY_TEXT]
#   creates the issue, captures NUM+REST id, adds to Project #1, sets Component + Status=Todo.
#   The component/* label is appended automatically from COMPONENT (Docs adds none).
create_issue(){
  local key="$1" title="$2" summary="$3" ms="$4" kinds="$5" comp="$6" blk="${7:-}"
  local clabel="${COMP_LABEL[$comp]:-}"
  local bodyfile="$BODIES/$key.md"
  if [ ! -f "$bodyfile" ]; then echo "FATAL: missing body file $bodyfile (issue key '$key')" >&2; exit 1; fi
  local largs=()
  local IFS=','; local l
  for l in $kinds; do [ -n "$l" ] && largs+=(--label "$l"); done
  unset IFS
  [ -n "$clabel" ] && largs+=(--label "$clabel")

  if [ "$DRY_RUN" = 1 ]; then
    echo "+ gh issue create --repo $REPO --title '$title' --milestone '$ms' ${largs[*]} --body-file bodies/$key.md [key=$key]"
    echo "+   project item-add + Component='$comp'(${COMP_OPT[$comp]}) + Status=Todo   [key=$key]"
    NUM[$key]="<$key>"; ID[$key]="<id:$key>"; ITEM[$key]="<item:$key>"
    return
  fi
  local url num id item
  url="$(gh issue create --repo "$REPO" --title "$title" --milestone "$ms" "${largs[@]}" --body-file "$bodyfile")"
  num="${url##*/}"
  id="$(gh api "repos/$REPO/issues/$num" -q .id)"
  NUM[$key]="$num"; ID[$key]="$id"
  echo ">> created #$num  [$key]  $url"
  item="$(gh project item-add "$PROJECT" --owner "$OWNER" --url "$url" --format json -q .id)"
  ITEM[$key]="$item"
  gh project item-edit --project-id "$PROJECT_ID" --id "$item" --field-id "$COMPONENT_FIELD" --single-select-option-id "${COMP_OPT[$comp]}"
  gh project item-edit --project-id "$PROJECT_ID" --id "$item" --field-id "$STATUS_FIELD"    --single-select-option-id "$STATUS_TODO"
}

# ensure_existing #NUM MILESTONE(""=leave) COMPONENT [ADD_LABEL]
#   moved-in existing issue: set milestone, add component label, ensure Project #1 membership + Component field.
ensure_existing(){
  local ref="$1" ms="$2" comp="$3" addlbl="${4:-}"
  local num="${ref#\#}" clabel="${COMP_LABEL[$comp]:-}"
  local eargs="gh issue edit $num --repo $REPO"
  [ -n "$ms" ]     && eargs="$eargs --milestone '$ms'"
  [ -n "$clabel" ] && eargs="$eargs --add-label $clabel"
  [ -n "$addlbl" ] && eargs="$eargs --add-label $addlbl"
  run "$eargs"
  if [ "$DRY_RUN" = 1 ]; then
    echo "+   project item-add #$num + Component='$comp'(${COMP_OPT[$comp]})"
    ID["#$num"]="<id:#$num>"
    return
  fi
  local id item
  id="$(gh api "repos/$REPO/issues/$num" -q .id)"; ID["#$num"]="$id"
  item="$(gh project item-add "$PROJECT" --owner "$OWNER" --url "https://github.com/$REPO/issues/$num" --format json -q .id)"
  gh project item-edit --project-id "$PROJECT_ID" --id "$item" --field-id "$COMPONENT_FIELD" --single-select-option-id "${COMP_OPT[$comp]}"
}

# ensure_epic #NUM MILESTONE COMPONENT [TITLE]
#   reused existing epic: add `epic` label + milestone + component + project; optional retitle.
ensure_epic(){
  local ref="$1" ms="$2" comp="$3" title="${4:-}"
  [ -n "$title" ] && run "gh issue edit ${ref#\#} --repo $REPO --title '$title'"
  ensure_existing "$ref" "$ms" "$comp" epic
}

# ---- resume preload: the M1..M5 issues (#359..#409) were already created on the first run.
# Only epic-m4-operator-delivery (#409) is referenced as a parent by the remaining ops, so preload it.
NUM[epic-m4-operator-delivery]=409
ID[epic-m4-operator-delivery]="$(gh api repos/$REPO/issues/409 -q .id)"
echo ">> resume: preloaded epic-m4-operator-delivery = #409 (id ${ID[epic-m4-operator-delivery]})"
echo; echo "## completing from M5 #124 through M8 (first run aborted here on the #124->#127 parent clash)"
ensure_existing "#124" "$M5" "Docs";              link_sub epic-m4-operator-delivery "#124"

# ---- M6 : reused #140 (second-venue acceptance + freeze)
echo; echo "### M6 -- existing #140 (second-venue acceptance and vocabulary freeze)"
ensure_epic "#140" "$M6" "SDK/DX" "sdk: prove venue-neutrality with a second venue and freeze the vocabulary"
create_issue risk-value-flow-freeze-hold \
  "wit: hold the value-flow freeze until the second venue proves the abstraction" \
  "Keep videre additively extensible through the cut and hold the value-flow freeze until the post-cut second venue proves the abstraction; owns the cross-repo re-pin ripple runbook." \
  "$M6" "debt,needs-design" "WIT/ABI"
# child order: risk-hold, #141 (curated registry, demoted), #330 (freeze, re-parented off #137, lands last)
link_sub "#140" risk-value-flow-freeze-hold
ensure_existing "#141" "$M6" "SDK/DX"; link_sub "#140" "#141"
unlink_sub "#137" "#330"; ensure_existing "#330" "$M6" "WIT/ABI"; link_sub "#140" "#330"

# ---- M7 : reused #139 + new capability-teeth epic
echo; echo "### M7 -- existing #139 (the real egress guard)"
ensure_epic "#139" "$M7" "Runtime/Lifecycle"
create_issue guard-policy-async \
  "runtime: make the guard policy check async for live-state I/O" \
  "Convert the guard policy check to async so the real guard can simulate over live state and call remote analyzers without blocking the loop." \
  "$M7" "breaking" "Runtime/Lifecycle"
create_issue guard-derive-before-guard \
  "runtime: close the derive-before-guard escape and single-decode the body" \
  "Prevent derivation side-effects before the guard check and decode the body once so submit consumes the guard-vetted header." \
  "$M7" "security" "Runtime/Lifecycle"
create_issue guard-signing-boundary \
  "runtime: move the guard checkpoint to the signed transaction boundary" \
  "Add the guard checkpoint at the signed unsigned-tx boundary against the real identity backend, sharing one checkpoint with the guard engine." \
  "$M7" "security" "Runtime/Lifecycle"
# child order: policy-async, derive-before-guard, #52 (identity backend), signing-boundary
link_sub "#139" guard-policy-async
link_sub "#139" guard-derive-before-guard
ensure_existing "#52" "$M7" "Host Backends"; link_sub "#139" "#52"
link_sub "#139" guard-signing-boundary

echo; echo "### M7 -- epic: capability and egress enforcement teeth"
create_issue epic-m6-egress-capability-teeth \
  "guard: capability and egress enforcement teeth" \
  "Bring http and messaging egress under real enforcement and align mock-grant fidelity so no capability escapes the compile-time world guarantee." \
  "$M7" "security,epic" "Runtime/Lifecycle"
create_issue guard-egress-cap-world-guarantee \
  "runtime: bring http egress under the compile-time world guarantee" \
  "Bring http egress under the synthesised-world guarantee and canonicalize a single adapter import-narrowing contract." \
  "$M7" "security" "Runtime/Lifecycle"
create_issue messaging-query-scope \
  "host: enforce the messaging query scope with the waku backend" \
  "Enforce the declared messaging scope on the query path, landed with the waku backend." \
  "$M7" "bug" "Host Backends"
create_issue mock-grant-fidelity \
  "host: align mock capability-grant fidelity to the real host grant" \
  "Reconcile the mock capability grant with the real host grant so a capability the host would deny is denied under mock." \
  "$M7" "debt" "Host Backends"
link_sub epic-m6-egress-capability-teeth guard-egress-cap-world-guarantee
link_sub epic-m6-egress-capability-teeth messaging-query-scope
link_sub epic-m6-egress-capability-teeth mock-grant-fidelity

# ---- M8 : four new debt epics + the slim grant tracker #127
echo; echo "### M8 -- epic: deferred videre abstraction concepts"
create_issue epic-m7-videre-deferred-concepts \
  "sdk: deferred videre abstraction concepts" \
  "The intentionally-parked videre abstractions (maker-side offer, taker-side RFQ firm-quote, and the venue-neutral materialiser) held until a real driving venue exists to shape them." \
  "$M8" "feature,epic" "SDK/DX"
create_issue rfq-firm-quote-additive \
  "wit: add an additive firm-quote field for RFQ venues" \
  "Add an additive firm-quote field to the quote record plus the accept/settle path, gated on a real RFQ venue." \
  "$M8" "feature,needs-design" "WIT/ABI"
create_issue materialiser-source-venue \
  "sdk: generalize the keeper into a venue-neutral materialiser" \
  "Generalize the keeper sweep assembler into a source-and-venue-neutral materialiser proven against two dissimilar pairs." \
  "$M8" "dx,needs-design" "SDK/DX"
ensure_existing "#355" "$M8" "WIT/ABI"; link_sub epic-m7-videre-deferred-concepts "#355"
link_sub epic-m7-videre-deferred-concepts rfq-firm-quote-additive
link_sub epic-m7-videre-deferred-concepts materialiser-source-venue

echo; echo "### M8 -- epic: chain robustness and typed-fault debt"
create_issue epic-m7-chain-typed-fault-debt \
  "chain: robustness and typed-fault debt" \
  "Finish the typed-fault story across chain and the stub backends and clear the chain request-batch and backfill debt." \
  "$M8" "debt,epic" "Chain"
ensure_existing "#269" "$M8" "Chain";         link_sub epic-m7-chain-typed-fault-debt "#269"
ensure_existing "#288" "$M8" "Chain";         link_sub epic-m7-chain-typed-fault-debt "#288"
ensure_existing "#286" "$M8" "SDK/DX";        link_sub epic-m7-chain-typed-fault-debt "#286"
ensure_existing "#285" "$M8" "Observability"; link_sub epic-m7-chain-typed-fault-debt "#285"
ensure_existing "#289" "$M8" "Docs";          link_sub epic-m7-chain-typed-fault-debt "#289"
ensure_existing "#302" "$M8" "Chain";         link_sub epic-m7-chain-typed-fault-debt "#302"

echo; echo "### M8 -- epic: runtime test-harness and performance debt"
create_issue epic-m7-runtime-test-perf-debt \
  "runtime: test-harness and performance debt" \
  "Host-internal debt: a multi-module test harness, a supervisor clock seam, lock performance, and state-seam batching." \
  "$M8" "debt,epic" "Runtime/Lifecycle"
ensure_existing "#283" "$M8" "Runtime/Lifecycle"; link_sub epic-m7-runtime-test-perf-debt "#283"
ensure_existing "#284" "$M8" "Runtime/Lifecycle"; link_sub epic-m7-runtime-test-perf-debt "#284"
ensure_existing "#280" "$M8" "Runtime/Lifecycle"; link_sub epic-m7-runtime-test-perf-debt "#280"
ensure_existing "#105" "$M8" "Storage";           link_sub epic-m7-runtime-test-perf-debt "#105"

echo; echo "### M8 -- epic: messaging backend, docs and soak"
create_issue epic-m7-messaging-docs-soak \
  "host: messaging backend, docs and soak evidence" \
  "The deferred waku messaging backend and payload codec, the doc-consistency passes, and the unattended seven-day soak evidence." \
  "$M8" "feature,epic" "Host Backends"
ensure_existing "#152" "$M8" "Host Backends";     link_sub epic-m7-messaging-docs-soak "#152"
ensure_existing "#212" "$M8" "Host Backends";     link_sub epic-m7-messaging-docs-soak "#212"
ensure_existing "#341" "$M8" "Docs";              link_sub epic-m7-messaging-docs-soak "#341"
ensure_existing "#65"  "$M8" "Tooling/Packaging"; link_sub epic-m7-messaging-docs-soak "#65"

echo; echo "### M8 -- existing #127 (slim grant tracker; keeps epic label, no children)"
ensure_epic "#127" "$M8" "Docs" "docs: grant delivery plan and evidence tracker"
# children #121/#125 re-parented out (to M4 and M5); #127 references them in its body only.

# ------------------------------------------------------------------ 6. re-parents summary (executed inline above)
echo; echo "## 6. native re-parents executed inline: #321->generic-host, #322->sdk-authoring, #330->#140, #121->cow-bugfixes, #125->operator-delivery"

echo; echo "== done (DRY_RUN=$DRY_RUN). Review, then run: DRY_RUN=0 ./apply-plan.sh =="
