//! Capability enforcement: cross-checks the component's WIT imports
//! against the `[capabilities]` block declared in `module.toml`.
//!
//! The set of recognised capabilities is not fixed: the core namespace is
//! built in, and each runtime extension contributes its own namespace at
//! the composition root via [`CapabilityRegistry::register`]. An extension
//! interface is enforceable only once its namespace is registered.

use std::collections::HashSet;

use super::error::CapabilityViolation;
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
    /// The registry with only the core namespace.
    pub fn core() -> Self {
        Self {
            namespaces: vec![CORE_NAMESPACE],
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
        name == HTTP_CAPABILITY || self.namespaces.iter().any(|ns| ns.ifaces.contains(&name))
    }

    /// Comma-joined recognised capability names, for error messages.
    pub fn known_names(&self) -> String {
        self.namespaces
            .iter()
            .flat_map(|ns| ns.ifaces.iter().copied())
            .chain(std::iter::once(HTTP_CAPABILITY))
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
/// by the module's manifest declarations. Call this after loading the
/// component but before instantiation.
///
/// When `[capabilities]` is absent the manifest is in 0.1-fallback mode and
/// all imports are allowed; the caller is expected to have already emitted
/// a deprecation warning.
///
/// `component_imports` should be the iterator returned by
/// `component.component_type().imports(&engine)` - pass the **name** part
/// (`&str`) of each `(&str, ComponentItem)` tuple. `registry` carries the
/// core namespace plus any extension namespaces wired at the composition
/// root.
pub fn enforce_capabilities<'a>(
    loaded: &LoadedManifest,
    component_imports: impl Iterator<Item = &'a str>,
    registry: &CapabilityRegistry,
) -> Result<(), CapabilityViolation> {
    let caps = match loaded.manifest.capabilities.as_ref() {
        None => return Ok(()), // 0.1-fallback: no enforcement
        Some(c) => c,
    };

    let declared: HashSet<&str> = caps
        .required
        .iter()
        .chain(caps.optional.iter())
        .map(String::as_str)
        .collect();

    for import_name in component_imports {
        if let Some(cap) = registry.wit_import_to_cap(import_name)
            && !declared.contains(cap)
        {
            return Err(CapabilityViolation {
                capability: cap.to_owned(),
                wit_import: import_name.to_owned(),
            });
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
        assert_eq!(err.capability, "http");
        assert_eq!(err.wit_import, "wasi:http/outgoing-handler@0.2.12");
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
        assert_eq!(err.capability, "remote-store");
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
}
