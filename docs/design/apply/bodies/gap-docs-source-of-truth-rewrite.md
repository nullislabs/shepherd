Rewrite docs/05 and docs/08 as source-of-truth: document the venue persona as shipped, name venue adapters as the domain-extension mechanism, and reframe cow-api as a legacy read path.

## Why
The current docs are actively misleading, which makes them a cheap, high-return fix: docs/05 says the venue persona is not shipped when it is, and docs/08 documents only the deprecated Layer-3 host-extension model. The design decision further requires deleting the shepherd:cow and cow-api-as-adapter-extension ambiguity from the docs, keeping cow-api only as the legacy event-module read path. No other issue owns this affirmative rewrite; the migration-cruft deletion is handled separately. Part of milestone M4: CoW on the generic seam (the shepherd bundle). Blocked by: cow-api-retire. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- docs/05: document the venue persona as shipped, with the crate layout and a step-by-step walkthrough of authoring a venue on videre.
- docs/08: name venue adapters (#[videre::venue]) as THE domain-extension mechanism.
- docs/08: mark shepherd:cow and cow-api as the legacy read path and delete the adapter-extension ambiguity.

## Done when
- docs/05 documents the shipped venue persona with an author-a-venue walkthrough.
- docs/08 names venue adapters as the extension mechanism and marks cow-api as the legacy read path with no adapter-extension ambiguity.
