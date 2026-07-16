//! Capability enforcement: cross-checks the component's WIT imports
//! against the `[capabilities]` block declared in `module.toml`.
//!
//! The set of recognised capabilities is not fixed: the core namespace is
//! built in, and each runtime extension contributes its own namespace at
//! the composition root via [`CapabilityRegistry::register`]. An extension
//! interface is enforceable only once its namespace is registered.
//!
//! Components built through `#[nexum_sdk::module]` compile against a
//! per-module world derived from the same manifest, so their imports
//! equal their declarations by construction and this check is a pure
//! backstop for them; it retains its teeth for components built against
//! a wider world by hand, where nothing upstream narrows the imports.
//!
//! The WASI surface is gated the same way: io/clocks/random and all of
//! `wasi:cli` are ambient, `wasi:sockets` and `wasi:filesystem` are opt-in
//! via the `wasi-*` capabilities, and any other `wasi:` interface is
//! refused fail-closed.

use std::collections::HashSet;

use super::error::{CapabilityError, CapabilityViolation};
use super::types::{CORE_CAPABILITIES, LoadedManifest};

/// One WIT namespace prefix plus the interface names under it that count as
/// capabilities. Core registers `nexum:host/`; an extension registers its
/// own (e.g. `shepherd:cow/`).
#[derive(Clone, Copy)]
pub struct NamespaceCaps {
    /// Interface-name prefix, e.g. `"nexum:host/"`.
    pub prefix: &'static str,
    /// Interface names under `prefix` that are capabilities.
    pub ifaces: &'static [&'static str],
}

/// The core namespace: the interfaces the `event-module` world links.
pub const CORE_NAMESPACE: NamespaceCaps = NamespaceCaps {
    prefix: "nexum:host/",
    ifaces: CORE_CAPABILITIES,
};

/// Capability names under the `nexum:intent/` package a module may import.
/// Only the strategy-facing `pool` interface is a capability; the `types`
/// package is type-only and needs no declaration.
pub const INTENT_CAPABILITIES: &[&str] = &["pool"];

/// The intent namespace: the `nexum:intent/pool` import is linked into every
/// module linker, so a module that submits intents declares the `pool`
/// capability the same way it declares a `nexum:host/` one.
pub const INTENT_NAMESPACE: NamespaceCaps = NamespaceCaps {
    prefix: "nexum:intent/",
    ifaces: INTENT_CAPABILITIES,
};

/// The interfaces a `venue-adapter` world links: the scoped transport
/// only. An adapter has no local-store, remote-store, identity, or
/// logging - it moves bytes to and from its venue and nothing else. `http`
/// is not listed here for the same reason it is not in the core set: it
/// gates `wasi:http/*` and is handled by the registry directly.
pub const ADAPTER_CAPABILITIES: &[&str] = &["chain", "messaging"];

/// The adapter namespace: the same `nexum:host/` prefix as core but only
/// the scoped-transport interfaces. Validating an adapter manifest against
/// a registry built from this namespace rejects a declaration of any core
/// interface an adapter must not reach (e.g. `local-store`) as unknown.
pub const ADAPTER_NAMESPACE: NamespaceCaps = NamespaceCaps {
    prefix: "nexum:host/",
    ifaces: ADAPTER_CAPABILITIES,
};

/// Import prefix of the wasi:http package. Every interface under it
/// (outgoing-handler, types, ...) is gated by the single
/// [`HTTP_CAPABILITY`] declaration.
const WASI_HTTP_PREFIX: &str = "wasi:http/";

/// Capability name a module declares to import any `wasi:http/*`
/// interface; the per-module `[capabilities.http].allow` list scopes it.
const HTTP_CAPABILITY: &str = "http";

/// Gated WASI capability names. Declaring one grants the matching `wasi:`
/// interface group; see [`classify_wasi`]. `wasi:io`, `wasi:clocks`,
/// `wasi:random` and all of `wasi:cli` (environment included; the host
/// populates it empty) are ambient and need no declaration.
const WASI_CAPABILITIES: &[&str] = &["wasi-sockets", "wasi-filesystem"];

/// A `wasi:` import (other than `wasi:http`) classified against the gate.
enum WasiGate {
    /// Always linked, never declared: io, clocks, random, stdio/exit/terminal.
    Ambient,
    /// Usable only when the named capability is declared.
    Gated(&'static str),
    /// Unrecognised `wasi:` interface: refused fail-closed.
    Unknown,
}

/// Classify a non-http `wasi:` interface id, ignoring any `@version` suffix.
fn classify_wasi(import_name: &str) -> WasiGate {
    let iface = import_name.split('@').next().unwrap_or(import_name);
    if iface.starts_with("wasi:io/")
        || iface.starts_with("wasi:clocks/")
        || iface.starts_with("wasi:random/")
    {
        WasiGate::Ambient
    } else if iface.starts_with("wasi:filesystem/") {
        WasiGate::Gated("wasi-filesystem")
    } else if iface.starts_with("wasi:sockets/") {
        WasiGate::Gated("wasi-sockets")
    } else if iface.starts_with("wasi:cli/") {
        WasiGate::Ambient
    } else {
        WasiGate::Unknown
    }
}

/// Registry of capability namespaces recognised by enforcement. Built from
/// the core namespace plus every registered extension.
#[derive(Clone)]
pub struct CapabilityRegistry {
    namespaces: Vec<NamespaceCaps>,
}

impl Default for CapabilityRegistry {
    fn default() -> Self {
        Self::core()
    }
}

impl CapabilityRegistry {
    /// The registry with the core `nexum:host/` namespace plus the
    /// strategy-facing `nexum:intent/pool` import every module linker carries.
    pub fn core() -> Self {
        Self {
            namespaces: vec![CORE_NAMESPACE, INTENT_NAMESPACE],
        }
    }

    /// The registry a venue adapter validates against: only the scoped
    /// transport interfaces plus `http`. An adapter manifest that declares
    /// a core-only capability (e.g. `local-store`) fails as unknown here,
    /// and the adapter linker withholds the same interfaces so the
    /// component cannot instantiate against them either.
    pub fn adapter() -> Self {
        Self {
            namespaces: vec![ADAPTER_NAMESPACE],
        }
    }

    /// Add an extension's namespace.
    pub fn register(&mut self, ns: NamespaceCaps) {
        self.namespaces.push(ns);
    }

    /// Whether `name` is a capability under any registered namespace.
    /// Used to validate declared capability names in a manifest.
    pub fn is_known(&self, name: &str) -> bool {
        name == HTTP_CAPABILITY
            || WASI_CAPABILITIES.contains(&name)
            || self.namespaces.iter().any(|ns| ns.ifaces.contains(&name))
    }

    /// Comma-joined recognised capability names, for error messages.
    pub fn known_names(&self) -> String {
        self.namespaces
            .iter()
            .flat_map(|ns| ns.ifaces.iter().copied())
            .chain(std::iter::once(HTTP_CAPABILITY))
            .chain(WASI_CAPABILITIES.iter().copied())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Map a WIT import name to a capability name, or `None` for
    /// non-capability imports.
    ///
    /// Returns `Some(iface)` only for interfaces under a registered
    /// namespace, plus `Some("http")` for anything under `wasi:http/`;
    /// type-only packages like `nexum:host/types` and the remaining
    /// `wasi:*` namespaces fall through to `None` so they do not need a
    /// manifest declaration.
    ///
    /// Examples:
    /// - `"nexum:host/chain@0.2.0"`     -> `Some("chain")`
    /// - `"shepherd:cow/cow-api@0.2.0"` -> `Some("cow-api")` once the cow
    ///   namespace is registered
    /// - `"wasi:http/outgoing-handler@0.2.12"` -> `Some("http")`
    /// - `"nexum:host/types@0.2.0"`     -> `None` (type-only, not a capability)
    /// - `"wasi:io/streams@0.2.0"`      -> `None`
    pub fn wit_import_to_cap<'a>(&self, import_name: &'a str) -> Option<&'a str> {
        let without_version = import_name.split('@').next().unwrap_or(import_name);
        if without_version.starts_with(WASI_HTTP_PREFIX) {
            return Some(HTTP_CAPABILITY);
        }
        for ns in &self.namespaces {
            if let Some(iface) = without_version.strip_prefix(ns.prefix)
                && ns.ifaces.contains(&iface)
            {
                return Some(iface);
            }
        }
        None
    }
}

/// Check that every capability-bearing WIT import of the component is covered
/// by the module's manifest declarations. Call after loading the component,
/// before instantiation.
///
/// The WASI surface is gated fail-closed. With `[capabilities]` absent
/// (0.1-fallback) the registry surface stays permissive and load warns.
///
/// `component_imports` is the name part of each import from
/// `component.component_type().imports(&engine)`. `registry` carries the
/// core namespace plus any extension namespaces.
pub fn enforce_capabilities<'a>(
    loaded: &LoadedManifest,
    component_imports: impl Iterator<Item = &'a str>,
    registry: &CapabilityRegistry,
) -> Result<(), CapabilityError> {
    let caps = loaded.manifest.capabilities.as_ref();
    let fallback = caps.is_none();
    let declared: HashSet<&str> = caps
        .into_iter()
        .flat_map(|c| c.required.iter().chain(c.optional.iter()))
        .map(String::as_str)
        .collect();

    for import_name in component_imports {
        let without_version = import_name.split('@').next().unwrap_or(import_name);
        // `wasi:http` is gated by the registry below; the rest of the WASI
        // surface is gated here, fail-closed even in 0.1-fallback.
        if without_version.starts_with("wasi:") && !without_version.starts_with(WASI_HTTP_PREFIX) {
            match classify_wasi(import_name) {
                WasiGate::Ambient => {}
                WasiGate::Gated(cap) if declared.contains(cap) => {}
                WasiGate::Gated(cap) => {
                    return Err(CapabilityViolation {
                        capability: cap.to_owned(),
                        wit_import: import_name.to_owned(),
                    }
                    .into());
                }
                WasiGate::Unknown => {
                    return Err(CapabilityError::UnknownWasi {
                        wit_import: import_name.to_owned(),
                    });
                }
            }
            continue;
        }
        // Registry surface stays permissive in 0.1-fallback.
        if fallback {
            continue;
        }
        if let Some(cap) = registry.wit_import_to_cap(import_name)
            && !declared.contains(cap)
        {
            return Err(CapabilityViolation {
                capability: cap.to_owned(),
                wit_import: import_name.to_owned(),
            }
            .into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::types::{CapabilitiesSection, Manifest};

    /// A registry with the cow extension namespace registered, mirroring
    /// what the composition root assembles.
    fn registry_with_cow() -> CapabilityRegistry {
        let mut r = CapabilityRegistry::core();
        r.register(NamespaceCaps {
            prefix: "shepherd:cow/",
            ifaces: &["cow-api"],
        });
        r
    }

    #[test]
    fn wit_import_to_cap_nexum_host() {
        let r = CapabilityRegistry::core();
        assert_eq!(r.wit_import_to_cap("nexum:host/chain@0.2.0"), Some("chain"));
        assert_eq!(
            r.wit_import_to_cap("nexum:host/local-store@0.2.0"),
            Some("local-store")
        );
    }

    #[test]
    fn wit_import_to_cap_wasi_http_maps_to_http() {
        let r = CapabilityRegistry::core();
        assert_eq!(
            r.wit_import_to_cap("wasi:http/outgoing-handler@0.2.12"),
            Some("http")
        );
        assert_eq!(r.wit_import_to_cap("wasi:http/types@0.2.12"), Some("http"));
        // Version-agnostic: the prefix decides, not the pinned version.
        assert_eq!(
            r.wit_import_to_cap("wasi:http/outgoing-handler@0.2.0"),
            Some("http")
        );
        assert_eq!(r.wit_import_to_cap("wasi:http/types"), Some("http"));
    }

    #[test]
    fn http_is_a_known_capability_name() {
        let r = CapabilityRegistry::core();
        assert!(r.is_known("http"));
        assert!(r.known_names().split(", ").any(|n| n == "http"));
    }

    #[test]
    fn wit_import_to_cap_shepherd_cow_needs_registration() {
        // Core registry does not recognise the cow namespace.
        let core = CapabilityRegistry::core();
        assert_eq!(core.wit_import_to_cap("shepherd:cow/cow-api@0.2.0"), None);
        // Once registered, it resolves.
        let r = registry_with_cow();
        assert_eq!(
            r.wit_import_to_cap("shepherd:cow/cow-api@0.2.0"),
            Some("cow-api")
        );
    }

    #[test]
    fn intent_pool_is_a_core_capability_but_intent_types_is_not() {
        let r = CapabilityRegistry::core();
        assert_eq!(r.wit_import_to_cap("nexum:intent/pool@0.1.0"), Some("pool"));
        assert!(r.is_known("pool"));
        // The type-only interface is not a capability and needs no declaration.
        assert_eq!(r.wit_import_to_cap("nexum:intent/types@0.1.0"), None);
    }

    #[test]
    fn wit_import_to_cap_non_http_wasi_is_none() {
        let r = registry_with_cow();
        assert_eq!(r.wit_import_to_cap("wasi:io/streams@0.2.0"), None);
        assert_eq!(r.wit_import_to_cap("wasi:cli/stdin@0.2.0"), None);
        assert_eq!(r.wit_import_to_cap("wasi:sockets/tcp@0.2.0"), None);
    }

    fn manifest_with_caps(required: &[&str], optional: &[&str]) -> LoadedManifest {
        LoadedManifest {
            manifest: Manifest {
                capabilities: Some(CapabilitiesSection {
                    required: required.iter().map(|s| s.to_string()).collect(),
                    optional: optional.iter().map(|s| s.to_string()).collect(),
                    http: None,
                }),
                ..Default::default()
            },
            http_allowlist: vec![],
            config: vec![],
        }
    }

    fn manifest_no_caps() -> LoadedManifest {
        LoadedManifest {
            manifest: Manifest::default(),
            http_allowlist: vec![],
            config: vec![],
        }
    }

    #[test]
    fn enforce_passes_when_caps_absent() {
        // 0.1-fallback: no capabilities section -> all imports allowed
        let loaded = manifest_no_caps();
        let imports = ["nexum:host/chain@0.2.0", "nexum:host/remote-store@0.2.0"];
        let r = registry_with_cow();
        assert!(enforce_capabilities(&loaded, imports.into_iter(), &r).is_ok());
    }

    #[test]
    fn enforce_passes_when_all_imports_declared() {
        let loaded = manifest_with_caps(&["chain", "cow-api"], &["http"]);
        let imports = [
            "nexum:host/chain@0.2.0",
            "shepherd:cow/cow-api@0.2.0",
            "wasi:http/outgoing-handler@0.2.12",
            "wasi:io/streams@0.2.0", // non-http wasi is always skipped
        ];
        let r = registry_with_cow();
        assert!(enforce_capabilities(&loaded, imports.into_iter(), &r).is_ok());
    }

    #[test]
    fn enforce_rejects_wasi_http_import_without_declaration() {
        let loaded = manifest_with_caps(&["chain"], &[]);
        let imports = [
            "nexum:host/chain@0.2.0",
            "wasi:http/outgoing-handler@0.2.12",
        ];
        let r = registry_with_cow();
        let err = enforce_capabilities(&loaded, imports.into_iter(), &r).unwrap_err();
        let CapabilityError::Undeclared(v) = err else {
            panic!("expected undeclared: {err:?}")
        };
        assert_eq!(v.capability, "http");
        assert_eq!(v.wit_import, "wasi:http/outgoing-handler@0.2.12");
    }

    #[test]
    fn enforce_accepts_wasi_http_when_http_declared() {
        // Required and optional declarations both cover the import.
        for (required, optional) in [(&["http"][..], &[][..]), (&[][..], &["http"][..])] {
            let loaded = manifest_with_caps(required, optional);
            let imports = [
                "wasi:http/outgoing-handler@0.2.12",
                "wasi:http/types@0.2.12",
            ];
            let r = registry_with_cow();
            assert!(enforce_capabilities(&loaded, imports.into_iter(), &r).is_ok());
        }
    }

    #[test]
    fn enforce_rejects_undeclared_import() {
        let loaded = manifest_with_caps(&["chain"], &[]);
        // module imports remote-store but didn't declare it
        let imports = ["nexum:host/chain@0.2.0", "nexum:host/remote-store@0.2.0"];
        let r = registry_with_cow();
        let err = enforce_capabilities(&loaded, imports.into_iter(), &r).unwrap_err();
        let CapabilityError::Undeclared(v) = err else {
            panic!("expected undeclared: {err:?}")
        };
        assert_eq!(v.capability, "remote-store");
    }

    #[test]
    fn enforce_optional_caps_are_also_allowed() {
        let loaded = manifest_with_caps(&["chain"], &["remote-store"]);
        let imports = ["nexum:host/chain@0.2.0", "nexum:host/remote-store@0.2.0"];
        let r = registry_with_cow();
        assert!(enforce_capabilities(&loaded, imports.into_iter(), &r).is_ok());
    }

    #[test]
    fn adapter_registry_knows_only_scoped_transport() {
        // The scoped transport plus http are known; the core-only
        // interfaces an adapter must not reach are not, so a manifest
        // declaring them fails validation as unknown.
        let r = CapabilityRegistry::adapter();
        assert!(r.is_known("chain"));
        assert!(r.is_known("messaging"));
        assert!(r.is_known("http"));
        assert!(!r.is_known("local-store"));
        assert!(!r.is_known("remote-store"));
        assert!(!r.is_known("identity"));
        assert!(!r.is_known("logging"));
    }

    #[test]
    fn adapter_registry_maps_transport_imports_but_not_core_only() {
        let r = CapabilityRegistry::adapter();
        assert_eq!(r.wit_import_to_cap("nexum:host/chain@0.2.0"), Some("chain"));
        assert_eq!(
            r.wit_import_to_cap("nexum:host/messaging@0.2.0"),
            Some("messaging")
        );
        assert_eq!(
            r.wit_import_to_cap("wasi:http/outgoing-handler@0.2.12"),
            Some("http")
        );
        // A core-only interface is not a recognised adapter capability.
        assert_eq!(r.wit_import_to_cap("nexum:host/local-store@0.2.0"), None);
    }

    #[test]
    fn adapter_manifest_declaring_a_core_only_cap_is_unknown() {
        // The load path validates declared names against the registry; an
        // adapter declaring `local-store` must surface as unknown.
        let r = CapabilityRegistry::adapter();
        assert!(!r.is_known("local-store"));
        assert!(r.known_names().split(", ").all(|n| n != "local-store"));
    }

    #[test]
    fn ambient_wasi_needs_no_declaration() {
        let loaded = manifest_with_caps(&["logging"], &[]);
        let imports = [
            "wasi:io/streams@0.2.6",
            "wasi:io/poll@0.2.6",
            "wasi:clocks/monotonic-clock@0.2.6",
            "wasi:clocks/wall-clock@0.2.6",
            "wasi:random/random@0.2.6",
            "wasi:cli/stdout@0.2.6",
            "wasi:cli/stdin@0.2.6",
            "wasi:cli/stderr@0.2.6",
            "wasi:cli/exit@0.2.6",
            "wasi:cli/terminal-stdout@0.2.6",
            "wasi:cli/environment@0.2.6",
        ];
        let r = registry_with_cow();
        assert!(enforce_capabilities(&loaded, imports.into_iter(), &r).is_ok());
    }

    #[test]
    fn undeclared_gated_wasi_is_refused() {
        let loaded = manifest_with_caps(&["logging"], &[]);
        let r = registry_with_cow();
        for (import, cap) in [
            ("wasi:sockets/tcp@0.2.6", "wasi-sockets"),
            ("wasi:filesystem/types@0.2.6", "wasi-filesystem"),
        ] {
            let err = enforce_capabilities(&loaded, [import].into_iter(), &r).unwrap_err();
            let CapabilityError::Undeclared(v) = err else {
                panic!("expected undeclared for {import}: {err:?}")
            };
            assert_eq!(v.capability, cap);
            assert_eq!(v.wit_import, import);
        }
    }

    #[test]
    fn declared_gated_wasi_is_permitted() {
        let loaded = manifest_with_caps(&["wasi-sockets", "wasi-filesystem"], &[]);
        let imports = [
            "wasi:sockets/tcp@0.2.6",
            "wasi:sockets/udp@0.2.6",
            "wasi:filesystem/types@0.2.6",
            "wasi:filesystem/preopens@0.2.6",
        ];
        let r = registry_with_cow();
        assert!(enforce_capabilities(&loaded, imports.into_iter(), &r).is_ok());
    }

    #[test]
    fn declaring_one_gated_cap_does_not_grant_another() {
        let loaded = manifest_with_caps(&["wasi-filesystem"], &[]);
        let r = registry_with_cow();
        assert!(
            enforce_capabilities(&loaded, ["wasi:filesystem/types@0.2.6"].into_iter(), &r).is_ok()
        );
        assert!(enforce_capabilities(&loaded, ["wasi:sockets/tcp@0.2.6"].into_iter(), &r).is_err());
    }

    #[test]
    fn unknown_wasi_interface_is_refused_fail_closed() {
        // Even with an unrelated gated cap declared, an unrecognised wasi:
        // namespace is denied outright.
        let loaded = manifest_with_caps(&["wasi-sockets"], &[]);
        let r = registry_with_cow();
        let err =
            enforce_capabilities(&loaded, ["wasi:nn/tensor@0.2.0"].into_iter(), &r).unwrap_err();
        assert!(matches!(err, CapabilityError::UnknownWasi { .. }));
    }

    #[test]
    fn wasi_gate_ignores_version_suffix() {
        let declared = manifest_with_caps(&["wasi-sockets"], &[]);
        let none = manifest_with_caps(&["logging"], &[]);
        let r = registry_with_cow();
        assert!(enforce_capabilities(&declared, ["wasi:sockets/tcp"].into_iter(), &r).is_ok());
        assert!(
            enforce_capabilities(&declared, ["wasi:sockets/tcp@0.2.6"].into_iter(), &r).is_ok()
        );
        assert!(enforce_capabilities(&none, ["wasi:filesystem/types"].into_iter(), &r).is_err());
    }

    #[test]
    fn fallback_gates_wasi_but_stays_permissive_on_registry_surface() {
        // No [capabilities] section -> 0.1-fallback: registry imports pass,
        // but the WASI surface is still gated fail-closed.
        let loaded = manifest_no_caps();
        let r = registry_with_cow();
        assert!(
            enforce_capabilities(&loaded, ["nexum:host/remote-store@0.2.0"].into_iter(), &r)
                .is_ok()
        );
        assert!(enforce_capabilities(&loaded, ["wasi:io/streams@0.2.6"].into_iter(), &r).is_ok());
        assert!(enforce_capabilities(&loaded, ["wasi:sockets/tcp@0.2.6"].into_iter(), &r).is_err());
        assert!(matches!(
            enforce_capabilities(&loaded, ["wasi:nn/tensor@0.2.0"].into_iter(), &r).unwrap_err(),
            CapabilityError::UnknownWasi { .. }
        ));
    }

    #[test]
    fn wasi_capability_names_are_known() {
        let r = registry_with_cow();
        for cap in ["wasi-sockets", "wasi-filesystem"] {
            assert!(r.is_known(cap), "{cap} missing from known set");
            assert!(r.known_names().split(", ").any(|n| n == cap));
        }
    }
}
