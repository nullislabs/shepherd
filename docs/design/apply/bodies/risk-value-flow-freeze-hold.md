Keep every `videre:*` WIT package additively extensible through the repo cut, and hold the `videre:value-flow` 1.0 freeze until a genuine second-protocol venue has compiled and passed videre-test against the split videre-sdk. This is a tracking issue that makes the freeze-hold mitigation explicit and checkable.

## Why
The three repos are cut before a real second venue exists, so a wrong `videre` abstraction is discovered only after the cut and becomes a cross-repo change rather than an in-monorepo fold. The accepted trade-off is to keep the correction cheap: every `videre:*` package (types, venue, value-flow) stays additively extensible with no field frozen or removed, so a later correction is a non-breaking additive cross-repo change; and the `videre:value-flow` 1.0 freeze is not applied until the post-cut second venue has proven the abstraction and fed back any shape corrections. The cross-repo WIT versioning has no teeth during the transition (a mispinned tag silently drifts), so the dependency-sync and semver CI check must stay enforcing through the correction window, and the operational ripple of any additive fix must have a documented runbook and a reserved rework budget. Part of milestone M6: Second-venue acceptance and vocabulary freeze. Blocked by: s2-three-carves. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Keep every `videre:*` package (types, venue, value-flow) additively extensible through the cut: no field is frozen or removed, so a post-cut correction is a non-breaking additive cross-repo change.
- Hold the `videre:value-flow` 1.0 freeze (#330) until the second-protocol venue (#140) has compiled and passed videre-test against the split videre-sdk and fed back any shape corrections; do not apply the freeze earlier.
- Own the operational ripple of any post-cut additive `videre:*` correction with a documented runbook: re-tag `videre`, re-pin the git-tag/registry version in nexum-runtime, shepherd, and the second-venue repo, regenerate goldens cross-repo, then re-run the byte-identical tip oracle per repo.
- Reserve an abstraction-rework budget for landing such a correction.
- Keep the dependency-sync and semver CI check enforcing through the correction window so an additive re-pin cannot silently drift.
- Confirm at the cut that `videre:*` carries no freeze markers, and sign off the freeze-gate on the second venue's acceptance.

## Done when
- A tracked checklist asserts `videre:*` is additively extensible with no freeze markers at the cut.
- The `videre:value-flow` 1.0 freeze (#330) is blocked until the post-cut second venue (#140) proves the abstraction.
- Any post-cut `videre:*` fix is landed as an additive, non-breaking cross-repo change.
- A documented re-tag then re-pin (nexum-runtime, shepherd, second-venue repo) then regenerate-goldens-cross-repo then re-run-tip-oracle-per-repo runbook exists.
- An abstraction-rework budget is reserved for the correction window.
- The dependency-sync and semver CI check is confirmed to stay enforcing through the correction window so an additive re-pin cannot silently drift.
- Signed off before the repo carve and before the vocabulary freeze.
