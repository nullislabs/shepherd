Enforce the declared messaging scope on the `messaging.query` path, rejecting out-of-scope queries the same way publish scope is enforced. Land it with the Waku backend.

## Why
The `messaging.query` path is not scope-checked, so a module or adapter can query messaging outside its declared scope. This is latent only because the messaging backend is a stub today; the hole goes live the moment the Waku backend lands. Enforcement must be consistent with how publish scope is already enforced and must ship with, or ahead of, the Waku backend. Part of milestone M7: Egress guard. Blocked by: egress-guard-hardening-epic. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Enforce the declared messaging scope on `messaging.query`, denying out-of-scope queries.
- Match the enforcement model already used for publish scope.
- Wire the enforcement into the Waku backend as it lands.

## Done when
- Out-of-scope `messaging.query` is denied and in-scope queries succeed.
- Enforcement is wired into the Waku backend with tests for both cases.
