Settle how the CoW keeper obtains a deterministic identifier for its idempotency check before order assembly moves into the adapter. Today shepherd-sdk/src/cow/run.rs derives the client-side order UID and checks the submitted Journal before the network call.

## Why
Once OrderCreation and UID assembly move into the adapter's submit, the keeper can no longer derive the UID pre-submit, which opens a double-post risk: on restart the keeper cannot tell whether it already posted an order. This must be settled before assembly moves, otherwise the idempotency check silently stops working across the keeper-to-adapter boundary. Part of milestone M4: CoW on the generic seam (the shepherd bundle). Blocked by: cow-onvidere-epic; Cleave cow-venue: orderbook-only venue vs composable-cow keeper. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Pick one mechanism: adapter.derive-header returns a deterministic intent-id the keeper journals, or SubmitOutcome carries a receipt equal to the UID.
- Re-route the Journal idempotency check onto that identifier.
- Add a regression test covering resubmit after restart.

## Done when
- The keeper can derive or obtain a deterministic intent-id pre-submit without assembling OrderCreation itself.
- The chosen mechanism is implemented: either adapter.derive-header returns a deterministic intent-id the keeper journals, or SubmitOutcome carries a receipt equal to the UID.
- The submitted Journal check remains effective across the keeper-to-adapter boundary, with no double-post window.
- A regression test exercises resubmit-after-restart and asserts a single orderbook POST.
