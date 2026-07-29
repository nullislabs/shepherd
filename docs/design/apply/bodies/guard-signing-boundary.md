Extend the guard checkpoint to cover the requires-signing class by placing it at the signed unsigned-tx / identity boundary, wired to the real keystore identity backend.

## Why
The guard does not cover the requires-signing class at all. For that class the real value movement is the unsigned-tx calldata returned by submit and signed on the identity path, which is a stub today (`accounts()` returns `Ok(vec![])`). Identity signing lands together with the guard, so the checkpoint must sit at the signed unsigned-tx / identity boundary and gate against a real identity backend. The work must coordinate with the identity-boundary checkpoint under the guard epic so there is exactly one checkpoint, not two. Part of milestone M7: Egress guard. Blocked by: egress-guard-hardening-epic, #52, #139. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Add the guard checkpoint at the decoded unsigned-tx / identity boundary so requires-signing submits are gated.
- Wire the checkpoint to the real identity backend (#52) rather than the empty stub.
- Coordinate with the identity-boundary checkpoint child under #139 so a single shared checkpoint exists.

## Done when
- Requires-signing submits are gated at the decoded unsigned-tx boundary against a real identity backend.
- A single checkpoint is shared with #139, not duplicated.
