# The linker extension seam

## What

The core host binds the `nexum:host/event-module` world: the six core
primitives (chain, identity, local-store, remote-store, messaging, logging)
plus the allowlisted `wasi:http` outgoing surface. A domain capability such
as a venue platform is not a core seam. It plugs into the host through an
extension assembled at the composition root, so the core runtime compiles
and runs with no domain backend at all (`Ext = ()`, no extensions
registered).

## The `Extension` trait

One trait, `Extension<T: RuntimeTypes>` (`host::extension`), is what a
domain contributes. Its members:

- `namespace()`: the namespace it owns; keys its service on `HostServices`.
- `capabilities() -> NamespaceCaps`: the `{ prefix, ifaces }` merged into
  enforcement so a module importing its interfaces still validates.
- `link(&mut Linker<HostState<T>>)`: adds its WIT imports to each worker
  linker, after the core interfaces and before instantiation. Takes only
  `&mut Linker`, never the wasmtime `Store` (not `Sync`), so the seam stays
  compatible with a future per-extension call router that serializes access
  to a `Store`.
- `service() -> Option<Arc<dyn HostService>>`: a type-erased service
  published under the namespace on the shared `HostServices` map and
  downcast at the call site.
- `provider() -> Option<Box<dyn ProviderKind<T>>>`: a provider component
  kind (e.g. the venue-adapter kind) the extension installs.
- `manifest_sections`, `admit_provider`, `admit_worker`: the non-core
  manifest sections it claims and its install-time predicates over them
  (an `Err` refuses the install fail-fast).
- `subscriptions`, `events`: the manifest subscription kinds it emits and
  the event sources it opens once the engine is booted.

An extension defines its own `bindgen!` for its world, generating a `Host`
trait local to the extension, and implements it for the foreign
`HostState<T>` (orphan-legal: the trait is local). It reaches its backend
either through the `HostServices` map
(`state.services.get::<S>(namespace)`, downcast) or, for a per-store
payload, through the `ExtState` accessor over the lattice `Ext` slot
(`RuntimeTypes::Ext`, held as `HostState.ext`). The shipped venue platform
uses the service map. The bindgen shares `nexum:host/types` with the core
bindings via `with`, so the extension's `fault` is the same type the core
host constructs.

## Registration and enforcement

`CapabilityRegistry` starts from the core namespace (`nexum:host/`) and
registers each extension's namespace; `enforce_capabilities` and manifest
name validation both consult it. The composition root assembles the
`Vec<Arc<dyn Extension<T>>>` once and threads it through the runtime
builder (`with_extensions`), which builds the linker and the registry from
it; the supervisor caches the list so the module-restart path rebuilds an
identical linker.

An extension lives in its own crate depending on the runtime for the seam
types (`HostState`, `Extension`, the `nexum:host/types` bindgen) and
depended on by the composition-root binary. The runtime carries no
dependency on any extension crate, so a domain cone stays out of the bare
engine. The `shepherd` binary registers one extension, the videre venue
platform (`videre_host::platform`), through its `Runtime::extensions` impl.

## Extension config

`engine.toml` stays domain-free. The engine deserializes every
`[extensions.<name>]` table into an opaque `toml::Value`
(`EngineConfig::extensions`) and never interprets it; the composition root
hands each extension its own entry to parse. Venue adapter components
install from the `[[adapters]]` table.

## Normative rule: import narrowing and boot ordering

Modules built through `#[nexum_sdk::module]` compile against a per-module
world derived from their manifest's `[capabilities]`, so a module that
never declares an extension capability has no such import and boots with a
core-only linker by construction. A module that DOES import an extension
interface instantiates only if, before instantiation:

- the extension's linker hook is registered (else an unsatisfied-import
  trap), AND
- the extension's capability namespace is registered (else the manifest's
  declaration of that capability is rejected as unknown).

Therefore the linker hook and the capability namespace of an extension MUST
be registered as a pair, from the same `Extension` value, before any module
is instantiated. Registering one without the other is a boot-time failure,
not a compile-time one.
