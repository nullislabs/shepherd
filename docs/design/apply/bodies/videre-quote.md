Add quoting to the videre:venue contract, which today exposes only submit, status, and cancel.

## Why
The vision is settlement plus quoting, but the contract has no quoting at all. It is free to add now and a wire break later, so it lands in this window. Firm and RFQ quotes and maker-side offers are out of scope and tracked in #355. Part of milestone M1: Videre contract reshape and host-intent decoupling. Blocked by: Pin the videre WIT surface. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Add quote to both faces of videre:venue.
- Define the quote record in videre:types, thin and value-flow-typed ({gives, wants, fee, valid-until-ms}).
- Add the SDK IntentClient.quote(&body)?.submit()? typestate.

## Done when
- videre:venue/client.quote(venue, body) and videre:venue/adapter.quote(body) exist and return a value-flow-typed quote.
- IntentClient.quote(&body)?.submit()? typestate compiles.
- echo-venue implements quote.
- The quote record is thin (gives, wants, fee, valid-until-ms) and EVM-only.
