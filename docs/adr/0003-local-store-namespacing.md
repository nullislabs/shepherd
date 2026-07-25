---
status: accepted
---

# Per-module namespacing in `local-store` via 32-byte deterministic hash prefix

## Context

`nexum:host/local-store` is a key-value store shared across every module the runtime runs. Two modules using the same key string must see disjoint values, and one module must never read or overwrite another's data. The runtime knows each module's identity at instantiation, so namespacing is a host-side concern. The prefix must be deterministic and unspoofable: an operator-supplied `module_name` string would let two modules collide by name, so the prefix derives from the module's canonical identity as a fixed-size hash.

## Decision

Single redb database file at `EngineConfig.engine.state_dir`, single shared table. Every key handed to redb is composed host-side as `[32-byte namespace prefix][raw key bytes]`.

The prefix is `keccak256(module_name)`, where `module_name` is `module.toml`'s `[module].name`. keccak256 shares the domain of the ENS namehash, so a module loaded locally and later published under an ENS name (see `docs/03-module-discovery.md`) can keep its state via an alias registered during migration; the alias mechanism is out of scope here.

Modules see plain key strings on both paths; the prefix is invisible to the WIT API.

## Consequences

- The prefix is fixed-size and independent of key length. A module's `list-keys` iterates the 32-byte prefix range; the host strips the prefix before returning to the guest.
- Changing the prefix derivation would orphan every module's persisted state, so the derivation stays stable through 0.x; ENS-mode namespacing is introduced additively via the alias mechanism, not by changing existing prefixes.
- The store does not version values. Modules that need schema migration embed their own version marker in stored payloads and migrate on `init`.
- An operator cannot make module A read module B's state by renaming: matching names produces a boot-time conflict, not a silent state takeover.
