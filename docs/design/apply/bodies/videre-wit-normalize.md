Reset every WIT package version string and every use reference to a single @0.1.0.

## Why
The current version strings (nexum:host@0.2.0, nexum:intent@0.1.0, shepherd:cow@0.2.0) are pre-release cruft, not compatibility boundaries; no external consumer pins any. After the cut, each package adopts independent per-package semver, so the shared baseline must be a clean @0.1.0. Part of milestone M1: Videre contract reshape and host-intent decoupling. Blocked by: Rename the nexum:intent WIT packages and symbols to videre. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Reset every package and every use reference across nexum:host, videre:* (post-rename), and shepherd:cow to @0.1.0 as one fold with the tip oracle.
- Delete the migration cruft: docs/migration/0.1-to-0.2.md and the "Migration from 0.1" prose in docs/08-platform-generalisation.md.

## Done when
- Every WIT package and every `use ...@<ver>` reference reads @0.1.0.
- docs/migration/0.1-to-0.2.md and the "Migration from 0.1" prose in docs/08 are deleted.
- The tree builds and the byte-identical tip oracle holds.
