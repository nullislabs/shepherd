#!/usr/bin/env bash
# Dep-sync gate for the transitional three-grouping workspace (M5, #403).
#
# The physical layout is the source of truth: every workspace crate lives under
# exactly one group dir (nexum/ = L1, videre/ = L2, shepherd/ = L3). A crate may
# depend only within its own tier or a lower one (nexum <- videre <- shepherd).
# An upward edge (e.g. nexum depending on videre) would become a circular repo
# dependency the moment the groups are carved into separate repos, so it is
# rejected here rather than discovered at the cut.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

meta="$(cargo metadata --format-version 1 --no-deps)"

# Emit "<offender>|<reason>" lines for every violation, then gate on the count.
violations="$(printf '%s' "$meta" | jq -r '
  .workspace_root as $root
  | (["nexum","videre","shepherd"]) as $tiers
  | def grp($p): ($p | ltrimstr($root + "/") | split("/")[0]);
    def tier($p): ($tiers | index(grp($p)));
    .packages[]
  | select(.manifest_path | startswith($root + "/"))
  | .name as $n
  | (.manifest_path | rtrimstr("/Cargo.toml")) as $self
  | if (tier($self) == null)
    then "\($n)|not under a group dir (nexum/videre/shepherd): \(grp($self))"
    else
      (tier($self)) as $st
      | .dependencies[]
      | select(.path != null and (.path | startswith($root + "/")))
      | select((tier(.path)) != null and (tier(.path)) > $st)
      | "\($n)|upward dep on \(.name) (\(grp($self)) -> \(grp(.path)))"
    end
')"

if [ -n "$violations" ]; then
  echo "carve-groups: FAIL — grouping invariant violated:" >&2
  printf '%s\n' "$violations" | sed 's/^/  /; s/|/: /' >&2
  exit 1
fi

echo "carve-groups: OK — every workspace crate is grouped and depends only within or below its tier"
