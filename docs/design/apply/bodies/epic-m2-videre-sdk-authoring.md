Deliver the blessed front door for venue and keeper authors: one SDK crate, one venue macro, one keeper macro, a typed venue client, and a conformance kit.

## Goal
Give venue and keeper authors a single, clear authoring path. Ship videre-sdk with the generic keeper sweep assembler, one blessed venue macro and one blessed keeper macro backed by a typed venue client, and a conformance kit that holds every venue to portable wire vectors and header goldens. Leave room to add the second venue after the split without reworking the surface.

## Scope
This epic renames and reshapes the existing venue-author SDK into videre-sdk and adds the generic Keeper::sweep assembler that wires the watch set, gates, source poll, retrier and journal together. On top of the crate it lands the two macros that authors actually write against: one that emits a typed venue adapter and one that drives a venue through a typed client, so authors never hand-write byte marshalling. The conformance kit rounds it out by turning wire-shape drift into a failing cargo test, with hardened goldens under the videre namespace. The pieces compose so a keeper compiles against videre-sdk alone, with no cow or host dependency.

Milestone: M3: Videre SDK, macros and DX.
