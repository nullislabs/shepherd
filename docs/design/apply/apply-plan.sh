#!/usr/bin/env bash
#
# apply-plan.sh : nullislabs/shepherd issue/milestone reorganization (nine-milestone videre spine)
#
# REVIEW THIS SCRIPT, THEN RUN:  DRY_RUN=0 ./apply-plan.sh
#
# By default (DRY_RUN=1) every mutating GitHub call is ECHOED, not executed, so you can read the
# full plan of record before anything touches the remote. Nothing here reads or writes local git.
#
# Source of truth : docs/design/issue-milestone-plan.json  (reconcile + epics arrays)
# Human context   : docs/design/issue-milestone-plan.md
# House style      : terse What / Why / Done-when; no phase jargon; no em dashes; Oxford -ize.
#
# Prerequisites: `gh auth status` green with repo + project + write:org scopes for nullislabs.
#   The Project #1 Component/Status field + option IDs below are pinned from the apply facts.
#
# What it does, in order:
#   1. Rename 7 milestones in place + CREATE the new M2 milestone (milestone #7 is KEPT AS-IS as M0).
#   2. Close 7 delivered/obsolete issues; resolve 2 merges (#325/#326 -> #324, #329 -> #293).
#   3. Apply the 6 modifies (retitle / rescope / re-milestone) + demote #136/#141 out of epic-hood.
#   4. Define helpers (create_issue / ensure_existing / ensure_epic / link_sub / unlink_sub).
#   5. Walk every milestone M0..M8: reuse-or-create each epic, create/attach each child in order.
#   6. Execute the 5 native re-parents (#321,#322,#330 off #137; #121,#125 off #127).
#
# NOTE ON BODIES: new-issue bodies are terse house-style stubs (one-sentence What + Why linking the
# milestone/plan + Done-when pointing at the plan JSON key). Enrich before running if you want the
# full acceptance prose inline; the authoritative acceptance lives in the plan JSON per key.

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
# link_sub PARENT_REF CHILD_REF   (native GitHub sub-issue; POST attaches/reassigns)
link_sub(){
  local p c; p="$(resolve_num "$1")"; c="$(resolve_id "$2")"
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

echo "== DRY_RUN=$DRY_RUN (1 echoes, 0 executes). Repo=$REPO Project #$PROJECT =="

# ------------------------------------------------------------------ 1. milestones
echo; echo "## 1. milestones (rename 7 in place; #7 kept as M0; create M2)"
# #7 "M0: Runtime architecture and lifecycle" -> KEEP AS-IS (no rename, no content move)
run "gh api repos/$REPO/milestones/8  -X PATCH -f title='$M1'"
run "gh api repos/$REPO/milestones/5  -X PATCH -f title='$M3'"
run "gh api repos/$REPO/milestones/3  -X PATCH -f title='$M4'"
run "gh api repos/$REPO/milestones/4  -X PATCH -f title='$M5'"
run "gh api repos/$REPO/milestones/10 -X PATCH -f title='$M6'"
run "gh api repos/$REPO/milestones/9  -X PATCH -f title='$M7'"
run "gh api repos/$REPO/milestones/6  -X PATCH -f title='$M8'"
# #1 "(dissolved) Host backends real" -> leave dissolved/closed (no action)
# create the one brand-new milestone (M2)
run "gh api repos/$REPO/milestones -f title='$M2' -f description='Make nexum-runtime a generic, venue-agnostic component host: grow the Extension seam, extract VenueRegistry, delete HostState.pool_router, zero-leak CI gate.'"

# ------------------------------------------------------------------ 2. closes + merges
echo; echo "## 2. close delivered/obsolete + resolve merges"
close_issue(){ # $1 num  $2 comment  $3 reason(completed|"not planned")
  run "gh issue comment $1 --repo $REPO --body \"$2\""
  run "gh issue close $1 --repo $REPO --reason \"${3:-completed}\""
}
close_issue 339 "Obsolete: this fixes a bullet inside docs/migration/0.1-to-0.2.md, which is deleted as reshape migration cruft. Closing as no longer applicable." "not planned"
close_issue 287 "Superseded: the legacy cow-api extension it targets is removed by #293; the timeout/429 typed-fault requirement is carried by the cow adapter errorType-to-venue-error projection and the host-chain equivalent #269." "not planned"
close_issue 222 "Delivered by the M1 train: ConditionalSource/Retrier/RetryAction landed in nexum-sdk. Forward keeper-sweep rework is tracked by the videre-sdk work." "completed"
close_issue 137 "Delivered by the M1 train. Successor is the videre contract reshape epic (rename, quote, normalize, host-intent decouple), not a reopen." "completed"
close_issue 135 "Delivered by the M1 train: keeper primitives and the single-venue loop landed in nexum-sdk. Deferred generalization is the videre-sdk sweep assembler plus the M8 materialiser." "completed"
close_issue 131 "Docs-only, delivered by the design-docs PR. Go-forward doc work is the source-of-truth rewrite." "completed"
close_issue 7   "Stale pre-restructure roadmap epic. Its goal is largely delivered and its workstreams are decomposed into the M0..M8 milestones and individual issues. Nothing tracks against it." "not planned"
# merges: comment the target, close the folded issues as merged
run "gh issue comment 324 --repo $REPO --body \"Absorbing #325 (golden vectors + conformance wiring) and #326 (bundle the adapter into the distribution): all three are the single cow-adapter cdylib deliverable of the shepherd bundle.\""
close_issue 325 "Merged into #324 (the single cow adapter cdylib deliverable)." "not planned"
close_issue 326 "Merged into #324 (the single cow adapter cdylib deliverable)." "not planned"
run "gh issue comment 293 --repo $REPO --body \"Absorbing #329: per the shepherd-bundle decision, shepherd-sdk is folded INTO the bundle at the carve rather than retired as a standalone crate. Tracked here with the legacy cow-api cone retirement.\""
unlink_sub "#136" "#329"   # remove #329 from #136 before close (#136 demotes to a leaf)
close_issue 329 "Merged into #293 (shepherd-sdk absorbed into the shepherd bundle at the carve)." "not planned"

# ------------------------------------------------------------------ 3. modifies (retitle / rescope / demote)
echo; echo "## 3. modifies + epic-label demotions (milestone + component are set in the epic walk below)"
# #274 rescope 2-repo -> 3-repo cut (retitled here; milestone/component set as reused epic in step 5)
run "gh issue edit 274 --repo $REPO --title 'packaging: carve nexum-runtime, videre and shepherd into three repos'"
run "gh issue comment 274 --repo $REPO --body \"Rescoped from a two-repo split to the three-repo cut (nexum-runtime, videre, shepherd); post-split preset items fold into the seam generalization. Re-milestoned to the gated three-repo split.\""
# #136 rescope + DEMOTE from epic to normal issue (its child #329 merged into #293, so it is childless)
run "gh issue edit 136 --repo $REPO --title 'sdk: consolidate the nexum-sdk macros and rename to videre-sdk' --remove-label epic"
run "gh issue comment 136 --repo $REPO --body \"Rescoped: shepherd-sdk is absorbed into the shepherd bundle (not kept standalone). Residual is the nexum-sdk/macro consolidation plus the nexum-venue-sdk to videre-sdk rename. Demoted from epic to a normal issue.\""
# #141 DEMOTE from epic to leaf under the reused second-venue epic (#140); owns no native sub-issues
run "gh issue edit 141 --repo $REPO --remove-label epic"
run "gh issue comment 141 --repo $REPO --body \"Demoted from epic to a leaf issue under the second-venue acceptance epic (curated adapter registry and consent surface). No native sub-issues to re-parent.\""
# #330 retitle under the videre rename; freeze held to the second-venue milestone
run "gh issue edit 330 --repo $REPO --title 'wit: freeze-gate decisions for value-flow'"
run "gh issue comment 330 --repo $REPO --body \"Retitled under the videre:value-flow rename. Keeps the two freeze-gate ontology decisions (minimal-length canonical amount encoding; native-token representable-but-invalid). The 1.0 freeze is HELD until the post-cut second venue proves the abstraction.\""
# #273 rescope: preset launch-surface subsumed by the seam generalization
run "gh issue edit 273 --repo $REPO --title 'runtime: fold the preset launch-surface into the seam generalization'"
run "gh issue comment 273 --repo $REPO --body \"Rescoped: the preset/Runtime-trait launch-surface ask is subsumed by growing the Extension seam plus the bare launcher. Residual is the MockRuntime preset path. Re-milestoned to the generic venue-agnostic host.\""
# #289 drop the deleted-migration-file bullet; keep the still-valid doc debt; stays M8
run "gh issue comment 289 --repo $REPO --body \"Drop the docs/migration/0.1-to-0.2.md bullet (that file is deleted as reshape cruft). Keep the ADR-0011 error-record restoration, the docs/07 rpc payload callout, the production.md house-style pass and the stale diagram regen. Distinct from the source-of-truth rewrite.\""
# #139 rescope: fold the router/capability/lifecycle hardening as children (done structurally in step 5)
run "gh issue comment 139 --repo $REPO --body \"Rescoped to fold the router/capability/lifecycle hardening the guard engine did not enumerate (single-decode/derive-before-guard, signed-tx boundary, async policy, http-under-world guarantee, messaging query scope, adapter sweeps, mock fidelity) as children. Advisory-only for the reshape milestone; depends on the identity backend #52.\""

# ------------------------------------------------------------------ 4. helpers
echo; echo "## 4. (helpers defined)"

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

# ------------------------------------------------------------------ 5. epics + children, milestone order M0..M8
echo; echo "## 5. epics + children (single parent per issue; children in execution order)"

# ---- M0 : reused lifecycle epic #294 only (milestone #7 KEPT; do NOT re-milestone #294 or its children)
echo; echo "### M0 -- existing #294 (no milestone change)"
ensure_epic "#294" "" "Runtime/Lifecycle"
ensure_existing "#51"  "" "Runtime/Lifecycle"     # already parented under #294
ensure_existing "#53"  "" "Runtime/Lifecycle"
ensure_existing "#107" "" "Runtime/Lifecycle"
ensure_existing "#244" "" "Runtime/Lifecycle"
ensure_existing "#265" "" "Observability"
ensure_existing "#266" "" "Runtime/Lifecycle"
# (children already native sub-issues of #294 -> no link needed)

# ---- M1 : two new WIT epics
echo; echo "### M1 -- epic: master-gate fold"
create_issue epic-m0-p0-master-gate-fold \
  "wit: land the host-intent decouple master gate for an acyclic split" \
  "Land the host-intent WIT decouple (host event carries opaque status bytes) as the master gate, plus the single oracle-validated fold to a green tip, so the acyclic split is CI-verifiable while nothing is pinned." \
  "$M1" "feature,epic" "WIT/ABI"
create_issue gap-opaque-status-contract-spec \
  "wit: spec the opaque-status destructuring contract with a version discriminator" \
  "Pin the wire form (version discriminator plus destructuring rule) and schema ownership for the opaque status bytes the host event will carry." \
  "$M1" "docs,needs-design" "WIT/ABI"
create_issue host-r6-decouple \
  "wit: carry opaque status bytes so the host stops importing intent" \
  "Drop the host-to-intent WIT use so the host world becomes a leaf carrying opaque status bytes; the master gate for the whole split." \
  "$M1" "breaking,needs-design" "WIT/ABI"
create_issue p0-acyclicity-scaffold \
  "engine: land the acyclicity and zero-leak CI check (advisory first)" \
  "Add the CI and local command that assert the host reaches no intent/venue/cow crate; advisory now, flipped to blocking later." \
  "$M1" "debt" "Engine"
create_issue guard-advisory-m1 \
  "runtime: ship an advisory-only guard posture and document the non-enforcing checkpoint" \
  "Keep the allow-all guard as default, feature-gate the intent import, and document the checkpoint as advisory-only for this milestone." \
  "$M1" "security" "Runtime/Lifecycle"
create_issue guard-deny-quota \
  "runtime: charge quota on guard-deny to close the busy-loop denial-of-service" \
  "Charge the caller quota on a guard-deny verdict so a denied submission cannot be retried in a free tight loop." \
  "$M1" "bug" "Runtime/Lifecycle"
create_issue gap-p0-fold-tail-hygiene \
  "packaging: land the fold-tail codec discriminator, migration-cruft deletion and retry doc" \
  "Fold-tail hygiene riding the reshape: codec version discriminator plus reject-unknown, delete migration cruft, and the must-not-retry doc caveat." \
  "$M1" "debt" "Tooling/Packaging"
create_issue gap-p0-wit-fold-execution \
  "packaging: execute the contract reshape as one oracle-validated fold across the train" \
  "Run the whole reshape as a single range-limited fold across the train with goldens regenerated and the byte-identical tip oracle re-asserted." \
  "$M1" "debt" "Tooling/Packaging"
create_issue gap-m1-green-tip-gate \
  "packaging: finish the train to a single green linear tip before any carve" \
  "Track the remaining train cars to a single green linear tip; the signed-off precondition for beginning the carve." \
  "$M1" "debt" "Tooling/Packaging"
for c in gap-opaque-status-contract-spec host-r6-decouple p0-acyclicity-scaffold guard-advisory-m1 guard-deny-quota gap-p0-fold-tail-hygiene gap-p0-wit-fold-execution gap-m1-green-tip-gate; do
  link_sub epic-m0-p0-master-gate-fold "$c"
done

echo; echo "### M1 -- epic: videre L2 contract"
create_issue epic-m0-videre-l2-contract \
  "wit: reshape the intent contract into the videre venue abstraction" \
  "Reshape the pre-release intent WIT into videre (rename, pin the surface, normalize to 0.1.0, add quote): the venue-neutral settlement and quoting contract every later venue and keeper compiles against." \
  "$M1" "feature,epic" "WIT/ABI"
create_issue videre-wit-rename \
  "wit: rename the intent packages to videre" \
  "One mechanical rename of the intent/value-flow/adapter packages to videre, folded into the oracle-validated pass." \
  "$M1" "breaking" "WIT/ABI"
create_issue videre-wit-surface \
  "wit: pin the videre contract surface (types, venue, value-flow)" \
  "Pin the shapes of videre types, venue (mirrored worker/provider faces) and value-flow (named records); EVM-only." \
  "$M1" "breaking" "WIT/ABI"
create_issue videre-wit-normalize \
  "wit: normalize every package to a single 0.1.0" \
  "Reset every WIT package and use reference to 0.1.0 as one fold, deleting the migration cruft." \
  "$M1" "debt" "WIT/ABI"
create_issue videre-quote \
  "wit: add quote to the videre venue faces and the client typestate" \
  "Add quote to both venue faces and a thin value-flow-typed quote record with a client quote-then-submit typestate." \
  "$M1" "breaking" "WIT/ABI"
create_issue gap-handshake-manifest-key-decision \
  "wit: decide the install-time body-versions manifest key and match semantics" \
  "Pin the manifest key name and the supported-set match semantics for the install-time body-versions handshake." \
  "$M1" "docs,needs-design" "WIT/ABI"
for c in videre-wit-rename videre-wit-surface videre-wit-normalize videre-quote gap-handshake-manifest-key-decision; do
  link_sub epic-m0-videre-l2-contract "$c"
done

# ---- M2 : two new host-generalization epics (the brand-new milestone)
echo; echo "### M2 -- epic: generic venue-agnostic host"
create_issue epic-m1-generic-venue-agnostic-host \
  "runtime: make the host generic and venue-agnostic" \
  "Grow the Extension seam to worker and provider roles, extract the venue registry and the generic supervised-component primitive, de-hardcode the known table, land the bare launcher and the videre-host platform registration, so nothing venue/intent/cow shaped lives in the host layer." \
  "$M2" "feature,epic" "Runtime/Lifecycle"
create_issue host-extension-seam-roles \
  "runtime: grow the extension seam to carry worker and provider roles" \
  "Grow the extension seam to contribute namespace/capabilities/link/service/provider, adding a type-erased host service and a provider kind; the long pole." \
  "$M2" "feature,needs-design" "Runtime/Lifecycle"
create_issue host-venue-registry-extract \
  "runtime: extract the venue registry service and delete the privileged router field" \
  "Move the router into the generic services map as an extension-owned venue registry and delete the privileged pool_router field." \
  "$M2" "debt" "Runtime/Lifecycle"
create_issue host-generic-component-kind \
  "runtime: extract a generic supervised-component primitive from the adapter actor" \
  "Extract the generic supervised-component primitive (fuel, trap projection, serialization, sweeps) and collapse the hardcoded kind match into a generic role loop." \
  "$M2" "feature" "Runtime/Lifecycle"
create_issue adapter-supervision-sweeps \
  "runtime: fold venue adapters into the restart and poison sweeps" \
  "Fold provider components into the restart and poison-recovery sweeps and expose a liveness signal distinguishing unknown-venue from temporarily-dead." \
  "$M2" "debt" "Runtime/Lifecycle"
create_issue host-nexum-world-registry \
  "engine: de-hardcode the known table and extract world synthesis into nexum-world" \
  "Delete the baked capability rows, source rows from registered extensions, and extract world synthesis plus the table into a plain nexum-world lib." \
  "$M2" "debt" "Engine"
create_issue host-generic-launcher-bin \
  "runtime: extract a generic launcher and a bare engine binary" \
  "Extract a generic launcher lib and a bare no-extension engine binary, retiring the backwards cli-to-cow dependency." \
  "$M2" "debt" "Runtime/Lifecycle"
create_issue gap-videre-host-platform-crate \
  "runtime: build the videre-host crate and platform registration" \
  "Build the videre-host L2 crate and a platform() entrypoint registering the provider kind, venue registry, guard seam, venue client and install predicate through the generic seam." \
  "$M2" "feature" "Runtime/Lifecycle"
create_issue videre-body-versions-handshake \
  "wit: add an install-time body-versions schema handshake" \
  "Add a body-versions query plus a manifest field, with the supervisor refusing to boot a keeper/adapter pair whose versions do not intersect." \
  "$M2" "feature" "WIT/ABI"
# child order: seam-roles, registry-extract, #321, component-kind, sweeps, world-registry, launcher, #273, videre-host, handshake
link_sub epic-m1-generic-venue-agnostic-host host-extension-seam-roles
link_sub epic-m1-generic-venue-agnostic-host host-venue-registry-extract
unlink_sub "#137" "#321"; ensure_existing "#321" "$M2" "Runtime/Lifecycle"; link_sub epic-m1-generic-venue-agnostic-host "#321"
link_sub epic-m1-generic-venue-agnostic-host host-generic-component-kind
link_sub epic-m1-generic-venue-agnostic-host adapter-supervision-sweeps
link_sub epic-m1-generic-venue-agnostic-host host-nexum-world-registry
link_sub epic-m1-generic-venue-agnostic-host host-generic-launcher-bin
ensure_existing "#273" "$M2" "Runtime/Lifecycle"; link_sub epic-m1-generic-venue-agnostic-host "#273"
link_sub epic-m1-generic-venue-agnostic-host gap-videre-host-platform-crate
link_sub epic-m1-generic-venue-agnostic-host videre-body-versions-handshake

echo; echo "### M2 -- epic: prove venue-agnostic (zero-leak gate)"
create_issue epic-m1-s1-venue-agnostic-gate \
  "runtime: prove the host is venue-agnostic (zero-leak gate)" \
  "Land the permanent zero-leak and acyclicity CI check and flip it to blocking, proving the host is venue-agnostic once the router field is deleted and the echo venue boots." \
  "$M2" "feature,epic" "Engine"
create_issue host-zero-leak-ci-gate \
  "engine: add the zero-leak CI check for the host layer" \
  "Add a required check that fails if the host regains intent/venue/cow symbols or crate edges, green at the generalized tip." \
  "$M2" "debt" "Engine"
create_issue s1-gate-runtime-venue-agnostic \
  "runtime: prove the host is venue-agnostic (zero-leak CI blocking)" \
  "Delete the router field and flip the zero-leak check to blocking, with an echo-venue boot integration test as the oracle." \
  "$M2" "feature" "Engine"
link_sub epic-m1-s1-venue-agnostic-gate host-zero-leak-ci-gate
link_sub epic-m1-s1-venue-agnostic-gate s1-gate-runtime-venue-agnostic

# ---- M3 : two new SDK/DX epics
echo; echo "### M3 -- epic: videre-sdk and the blessed authoring path"
create_issue epic-m2-videre-sdk-authoring \
  "sdk: videre-sdk and the blessed venue and keeper authoring path" \
  "Land the venue and keeper author front door: videre-sdk with the keeper sweep assembler, the single blessed venue and keeper macros with a typed venue client, and the videre conformance kit; additively extensible for the post-cut second venue." \
  "$M3" "feature,epic" "SDK/DX"
create_issue videre-sdk-crate \
  "sdk: rename to videre-sdk and add the keeper sweep assembler and venue client" \
  "Rename the venue SDK to videre-sdk and add the generic keeper sweep assembler and the typed intent client." \
  "$M3" "feature" "SDK/DX"
create_issue videre-venue-macro \
  "sdk: make the venue macro the single blessed authoring path" \
  "Fix the venue macro to emit the typed venue adapter, demote the raw export path to internal codegen, and narrow imports by construction." \
  "$M3" "dx" "SDK/DX"
create_issue videre-conformance-kit \
  "sdk: ship the videre conformance kit with a wire-drift test gate" \
  "Rename and harden the conformance kit so a venue test fails on any wire-shape drift, with hardened goldens." \
  "$M3" "dx" "SDK/DX"
create_issue videre-keeper-macro \
  "sdk: add the keeper macro and a typed venue client" \
  "Add the keeper macro that drives a venue through a typed venue client, wiring the event subs with zero boxing on the hot path." \
  "$M3" "dx" "SDK/DX"
# child order: #136, videre-sdk-crate, #322, #264, videre-venue-macro, videre-conformance-kit, videre-keeper-macro
ensure_existing "#136" "$M3" "SDK/DX"; link_sub epic-m2-videre-sdk-authoring "#136"
link_sub epic-m2-videre-sdk-authoring videre-sdk-crate
unlink_sub "#137" "#322"; ensure_existing "#322" "$M3" "SDK/DX"; link_sub epic-m2-videre-sdk-authoring "#322"
ensure_existing "#264" "$M3" "SDK/DX"; link_sub epic-m2-videre-sdk-authoring "#264"
link_sub epic-m2-videre-sdk-authoring videre-venue-macro
link_sub epic-m2-videre-sdk-authoring videre-conformance-kit
link_sub epic-m2-videre-sdk-authoring videre-keeper-macro

echo; echo "### M3 -- epic: guest seams, alloy provider and DX polish"
create_issue epic-m2-reth-alloy-dx-seams \
  "sdk: guest seams, alloy provider and the dx polish cluster" \
  "Complete the guest-facing developer surface: identity/messaging/remote-store guest traits with mocks, richer local-store queries, an alloy provider seam over the chain host, and the alloy-grade DX polish cluster." \
  "$M3" "dx,epic" "SDK/DX"
create_issue host-backend-guest-seams \
  "sdk: add guest seams and mocks for identity, messaging and remote-store" \
  "Add guest traits and mocks for the three backend interfaces missing a seam and wire them to the stub backends." \
  "$M3" "dx" "SDK/DX"
create_issue gap-alloy-provider-seam \
  "sdk: add an alloy provider seam over the chain host" \
  "Add an alloy transport over the chain host and a guest provider, carrying the typed chain-method surface to the guest." \
  "$M3" "dx" "SDK/DX"
create_issue gap-dx-polish-cluster \
  "sdk: land the alloy-grade DX polish cluster" \
  "Mirror the venue fault type, add an order builder, uniform non-exhaustive, sealed traits, single-source consts, and remove the golden-bridge boilerplate." \
  "$M3" "dx" "SDK/DX"
link_sub epic-m2-reth-alloy-dx-seams host-backend-guest-seams
ensure_existing "#291" "$M3" "Storage"; link_sub epic-m2-reth-alloy-dx-seams "#291"
link_sub epic-m2-reth-alloy-dx-seams gap-alloy-provider-seam
link_sub epic-m2-reth-alloy-dx-seams gap-dx-polish-cluster

# ---- M4 : reused #138 + two new CoW epics
echo; echo "### M4 -- existing #138 (cow adapter + flagship ports)"
ensure_epic "#138" "$M4" "CoW"
create_issue cleave-cow-venue \
  "cow: cleave the cow venue from the composable-cow keeper" \
  "Split the mixed crate so the venue holds only the orderbook body while composable machinery moves to a separate keeper, gated by a CI symbol check." \
  "$M4" "feature" "CoW"
create_issue cow-idempotency-seam \
  "cow: settle the idempotency seam before order assembly moves into the adapter" \
  "Pick and wire a deterministic pre-submit identifier so the journal idempotency check survives order assembly moving into the adapter." \
  "$M4" "feature" "CoW"
create_issue shepherd-cow-event-abi-wits \
  "wit: own the shepherd-cow event-ABI packages at the bundle layer" \
  "Consolidate the cow on-chain event-ABI surfaces under the bundle-owned WIT package, consumed only by bundle crates." \
  "$M4" "feature" "WIT/ABI"
create_issue composable-poll-wire-swap \
  "cow: swap the composable-cow poll wire and delete the legacy adapter" \
  "Fork-gated: swap the poll onto the structured non-reverting path, fully populate the post verdict, and delete the legacy revert adapter." \
  "$M4" "debt,blocked,needs-design" "CoW" "the fork deployment"
# child order: cleave, idempotency, #324, event-abi, #323, #327, #328, #293, poll-wire-swap
link_sub "#138" cleave-cow-venue
link_sub "#138" cow-idempotency-seam
ensure_existing "#324" "$M4" "CoW"    # already parented under #138
link_sub "#138" shepherd-cow-event-abi-wits
ensure_existing "#323" "$M4" "CoW"    # already parented under #138
ensure_existing "#327" "$M4" "CoW"    # already parented under #138
ensure_existing "#328" "$M4" "CoW"    # already parented under #138
ensure_existing "#293" "$M4" "CoW"    # already parented under #138
link_sub "#138" composable-poll-wire-swap

echo; echo "### M4 -- epic: seam gate + source-of-truth docs"
create_issue epic-m3-s1b-seam-gate \
  "cow: run the cow keeper on the generic seam and rewrite the docs" \
  "Close the seam gate: the keeper submits through the videre venue client with the cow-api host retired, and the source-of-truth docs are rewritten as the shipped-venue reference." \
  "$M4" "feature,epic" "CoW"
create_issue s1b-gate-cow-on-generic-seam \
  "cow: run the cow keeper on the generic venue client" \
  "Flip the keeper submit onto the venue client and retire the cow-api host extension, keeping the seam port decoupled from the fork-gated poll swap." \
  "$M4" "feature" "CoW"
create_issue gap-docs-source-of-truth-rewrite \
  "docs: rewrite the platform docs as the shipped-venue source of truth" \
  "Rewrite the platform docs so the venue persona is documented as shipped and venue adapters are the extension mechanism, marking cow-api the legacy read path." \
  "$M4" "docs" "Docs"
link_sub epic-m3-s1b-seam-gate s1b-gate-cow-on-generic-seam
link_sub epic-m3-s1b-seam-gate gap-docs-source-of-truth-rewrite

echo; echo "### M4 -- epic: carry the live keeper fixes into the port"
create_issue epic-m3-cow-keeper-bugfixes \
  "cow: carry the live twap and composable keeper fixes into the port" \
  "Land the still-live twap and composable keeper correctness fixes on the ported keeper, plus the grant deliverable-divergence reconcile." \
  "$M4" "bug,epic" "CoW"
# children all existing (#121 re-parented off #127; rest freshly parented)
unlink_sub "#127" "#121"; ensure_existing "#121" "$M4" "CoW"; link_sub epic-m3-cow-keeper-bugfixes "#121"
ensure_existing "#48"  "$M4" "CoW"; link_sub epic-m3-cow-keeper-bugfixes "#48"
ensure_existing "#75"  "$M4" "CoW"; link_sub epic-m3-cow-keeper-bugfixes "#75"
ensure_existing "#320" "$M4" "CoW"; link_sub epic-m3-cow-keeper-bugfixes "#320"
ensure_existing "#54"  "$M4" "CoW"; link_sub epic-m3-cow-keeper-bugfixes "#54"
ensure_existing "#64"  "$M4" "CoW"; link_sub epic-m3-cow-keeper-bugfixes "#64"

# ---- M5 : reused #274 + new operator-delivery epic
echo; echo "### M5 -- existing #274 (the three-repo cut)"
ensure_epic "#274" "$M5" "Tooling/Packaging"   # title already set in step 3
create_issue s2-transitional-workspace \
  "packaging: build the transitional path-dep workspace in three groupings" \
  "Reorganize the crates into the three prospective groupings as path-dep workspace members and add a dep-sync CI check." \
  "$M5" "debt" "Tooling/Packaging"
create_issue host-wit-deps-flip-carve \
  "packaging: flip the host WIT to crate-local wit-deps and carve the L1 repo" \
  "Flip host WIT resolution to crate-local wit-deps and carve the host as a standalone repo under the tip oracle." \
  "$M5" "debt,blocked" "Tooling/Packaging" "the zero-leak gate landing"
create_issue s2-wit-cross-repo-consumption \
  "packaging: source cross-repo WIT from wit-deps and git tags" \
  "Make every WIT resolution crate-local and source cross-repo packages from pinned git tags with lockfiles." \
  "$M5" "debt" "Tooling/Packaging"
create_issue s2-cut-gate-checklist \
  "packaging: assert the go/no-go cut gate before any carve" \
  "A single checklist that must close before the carves start: host venue-agnostic and cow on the generic seam, with the second venue de-gated to post-cut." \
  "$M5" "debt" "Tooling/Packaging"
create_issue s2-three-carves \
  "packaging: carve nexum-runtime, videre and shepherd into three repos" \
  "Three history-preserving carves executed as one coordinated operation under the byte-identical tip oracle." \
  "$M5" "breaking" "Tooling/Packaging"
create_issue videre-consumable-release-graduation \
  "packaging: cut the first consumable videre release and graduate off the umbrella" \
  "Cut the first consumable videre-sdk and WIT release and add a fresh-clone external-consumer smoke test against published deps only." \
  "$M5" "dx" "Tooling/Packaging"
# child order: transitional, wit-deps-flip, wit-cross-repo, checklist, three-carves, release-graduation
link_sub "#274" s2-transitional-workspace
link_sub "#274" host-wit-deps-flip-carve
link_sub "#274" s2-wit-cross-repo-consumption
link_sub "#274" s2-cut-gate-checklist
link_sub "#274" s2-three-carves
link_sub "#274" videre-consumable-release-graduation

echo; echo "### M5 -- epic: operator delivery"
create_issue epic-m4-operator-delivery \
  "packaging: operator delivery, multi-chain and the swarm remote-store" \
  "Operator-facing delivery landed alongside the cut: green CI and CD, the multi-chain provider map and deployment docs, ghcr image packaging, and the real Swarm remote-store backend (an implementation item riding this epic for delivery convenience)." \
  "$M5" "feature,epic" "Tooling/Packaging"
ensure_existing "#337" "$M5" "Tooling/Packaging"; link_sub epic-m4-operator-delivery "#337"
ensure_existing "#151" "$M5" "Storage";           link_sub epic-m4-operator-delivery "#151"
unlink_sub "#127" "#125"; ensure_existing "#125" "$M5" "Tooling/Packaging"; link_sub epic-m4-operator-delivery "#125"
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
