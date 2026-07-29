Close the seam gate: prove the flagship CoW keeper runs on the generic venue seam, then rewrite the reference docs to match.

## Goal
Get the CoW keeper submitting through the videre venue client with the legacy cow-api host retired, and rewrite the source-of-truth docs so they describe the shipped venue as the reference example rather than the deprecated host-extension model.

## Scope
This epic proves the generic L2 seam carries a real venue end to end, not just in principle. The keeper stops calling the cow-api host directly and instead goes through videre:venue/client, and the cow-api host extension is removed. Once the seam port is real, the architecture docs are rewritten so the venue persona is documented as shipped and venue adapters are named as the domain-extension mechanism, with cow-api reframed as a legacy read path. The two pieces move together: the code change makes the docs true, and the docs make the shipped design discoverable.

Milestone: M4: CoW on the generic seam (the shepherd bundle).
