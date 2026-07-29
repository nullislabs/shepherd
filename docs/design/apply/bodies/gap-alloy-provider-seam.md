Put an alloy Provider seam over the raw `ChainHost::request` path so guest strategies call typed provider methods instead of hand-building JSON-RPC, and carry the typed chain method surface through to the guest.

## Why
Chain access today is a stringly `request(u64, &str, &str) -> String`, so authors hand-build JSON-RPC params and parse strings; this is the largest single DX gap from the alloy target. The promised `HostTransport: alloy Transport` shim is not in the SDK, and the closed chain method RPC enum exists host-side but is never carried to the guest. No other issue covers this. Part of milestone M3: Videre SDK, macros and DX. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Add a `HostTransport` implementing the alloy `Transport` over `ChainHost::request`.
- Expose a guest `host.provider(Chain)` returning an alloy `Provider`.
- Carry the typed `ChainMethod` surface through to the guest.
- Add zero-cost `Chain` and `ChainId` newtypes.

## Done when
- A guest strategy can call `host.provider(chain).get_block_number()` and `.call(&tx)` through an alloy `Provider` backed by `ChainHost`.
- The typed `ChainMethod` surface reaches the guest.
- No hand-rolled JSON-RPC remains at the call sites.
