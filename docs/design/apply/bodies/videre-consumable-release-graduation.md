Cut the first externally consumable videre-sdk and videre:* WIT release, graduate videre off the umbrella path-deps for external consumers, and add a fresh-clone smoke test that builds a trivial venue against only published deps.

## Why
The second venue in this milestone is the first consumer of videre from outside the transitional umbrella superproject, via published and tagged deps only with no path-dep fallback. shepherd proves the videre WIT edges earlier but does so inside the umbrella (path-deps during stabilization) and may not publish, since it is an app-level bundle. The design describes videre's convergence to a genuinely consumable artifact (git-tag to crates.io plus wkg/OCI) only as "once stable", with no owner and no milestone, so nothing guarantees a fresh external repo can cargo add videre-sdk plus wit-deps videre:* and build a venue before this milestone needs it to. Part of milestone M5: The gated three-repo split. Blocked by: Three history-preserving git-filter-repo carves: nexum-runtime / videre / shepherd. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Tag and publish videre-sdk, videre-test, and the videre:* WIT packages (git-tag, wkg/OCI, per-package semver).
- Graduate videre off the umbrella path-deps for external consumers.
- Add a fresh-clone external-consumer smoke test that builds a trivial venue against only published videre deps (no path-dep, no umbrella).

## Done when
- The first consumable videre-sdk, videre-test, and videre:* WIT release is cut (git-tag plus wkg/OCI, per-package semver).
- videre is graduated off the umbrella path-deps for external consumers.
- A fresh-clone external-consumer smoke test builds a trivial venue against only published videre deps (no path-dep, no umbrella) and passes in CI.
- This is documented as the precondition for the second-venue acceptance in this milestone.
