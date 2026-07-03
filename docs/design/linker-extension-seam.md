# The linker extension seam

## Why

The core host binds the `nexum:host/event-module` world: the six core
primitives (chain, identity, local-store, remote-store, messaging, logging)
plus the ambient clock and http services. A domain capability such as
cow-api is not a core seam. It plugs into the host through an extension seam
that is assembled at the composition root, so the core runtime compiles and
runs with no domain backend at all (`Ext = ()`, no hooks registered).

An extension contributes three things that travel together:

1. an `Ext` payload carrying its backend, held in the runtime `HostState`;
2. a linker hook that adds its WIT interfaces to each module linker;
3. a capability namespace so enforcement recognises its imports.

## The seam

### `Ext` slot and the `ExtState` accessor

`RuntimeTypes` names an associated `Ext: Clone + Send + Sync + 'static`. The
per-module `HostState<T>` holds one `ext: T::Ext`. The generic accessor
trait is the load-bearing piece:

```rust
pub trait ExtState {
    type Ext;
    fn ext(&self) -> &Self::Ext;
}
impl<T: RuntimeTypes> ExtState for HostState<T> {
    type Ext = T::Ext;
    fn ext(&self) -> &Self::Ext { &self.ext }
}
```

An extension defines its own `bindgen!` for its world. That generates a
`Host` trait local to the extension. The extension implements it for the
foreign `HostState<T>`, which is orphan-legal because the trait is local.
To reach its own backend without knowing the concrete lattice `T`, the impl
goes through `ExtState::ext`, then bounds the payload on an
extension-defined trait:

```rust
pub trait CowBackend { type Cow: CowApi; fn cow(&self) -> &Self::Cow; }

impl<T> cow_bindings::...::Host for HostState<T>
where T: RuntimeTypes, T::Ext: CowBackend {
    async fn request(&mut self, ...) { self.ext().cow().request(...).await }
}
```

Two traits, two owners: `ExtState` is the runtime's generic reach into the
slot; `CowBackend` is the extension's own payload shape. The bindgen shares
`nexum:host/types` with the core bindings via `with`, so the extension's
`HostError` is the same type the core host constructs.

### Linker hook and capability registry

An extension is one value:

```rust
pub struct Extension<T: RuntimeTypes> {
    pub link: LinkerHook<T>,        // Arc<dyn Fn(&mut Linker) -> Result<()>>
    pub capabilities: NamespaceCaps, // { prefix, ifaces }
}
```

`build_linker` binds the core world then runs each hook. `CapabilityRegistry`
starts from the core namespace (`nexum:host/`) and registers each extension's
namespace; `enforce_capabilities` and manifest name validation both consult
it. The composition root (`nexum-cli`'s `launch::run_from_config`) assembles
the `Extension` list once and threads it into the generic
`nexum_runtime::bootstrap::run`, which builds the linker and the registry
from it. The supervisor caches the list so the module-restart path rebuilds
an identical linker.

An extension such as cow-api lives in its own crate (`shepherd-cow-host`)
that depends on the runtime for the seam types (`HostState`, `Extension`,
the `nexum:host/types` bindgen) and is depended on by `nexum-cli` at the
composition root. The runtime carries no dependency on any extension crate,
so the cow cone stays out of the bare engine.

The hook takes only `&mut Linker`, never the wasmtime `Store` (which is not
`Sync`). This keeps the seam compatible with a future per-extension call
router that serialises access to a `Store`.

## Normative rule: elision and boot ordering

Modules are compiled against the supertype world. The `wasm-tools` pipeline
elides any WIT import the produced component does not exercise, so a module
that never touches cow-api boots with a core-only linker. A module that DOES
import an extension interface instantiates only if, before instantiation:

- the extension's linker hook is registered (else an unsatisfied-import trap), AND
- the extension's capability namespace is registered (else the manifest's
  declaration of that capability is rejected as unknown, or the imported
  interface is not recognised as a declared capability).

Therefore the linker hook and the capability namespace of an extension MUST
be registered as a pair, from the same `Extension` value, before any module
is instantiated. Registering one without the other is a boot-time failure,
not a compile-time one. This is exercised in both directions: the runtime's
supervisor tests pin the negative (a cow-importing module fails to boot with
the extension absent), and `shepherd-cow-host`'s own boot tests pin the
positive (the same module boots and dispatches with the extension present).
