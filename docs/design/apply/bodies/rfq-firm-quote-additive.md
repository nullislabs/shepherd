Add an optional firm quote to the videre quote record so a market-maker or RFQ venue can return a signed, time-limited firm price the taker accepts. Today videre 0.1 quoting returns only a plain indicative quote record.

## Why
An RFQ venue does not return an indicative price: it returns a signed, time-limited firm price that the taker accepts and settles against. This is the smaller taker-side counterpart to the maker-side offer work, and it slots into the existing quote record additively as a firm: option<firm-quote> field rather than a new interface. It stays deferred until a real RFQ venue exists so the firm-quote shape is not guessed. Part of milestone M8: Post-v1 hardening and debt. Blocked by: #355. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Add firm: option<firm-quote> to the videre:types quote record, present only for RFQ venues.
- Add the accept and settle path on the client and adapter faces.
- Keep the change additive and EVM-only.
- Gate the work on a real RFQ venue appearing.

## Done when
- quote.firm is added additively and is present only for RFQ venues.
- A real RFQ venue exercises the firm quote against goldens.
- The indicative quote path is unchanged, with no breaking change.
