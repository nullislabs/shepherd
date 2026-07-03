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

    /// Add an extension's namespace.
    pub fn register(&mut self, ns: NamespaceCaps) {
        self.namespaces.push(ns);
    }

    /// Whether `name` is a capability under any registered namespace.
    /// Used to validate declared capability names in a manifest.
    pub fn is_known(&self, name: &str) -> bool {
        self.namespaces.iter().any(|ns| ns.ifaces.contains(&name))
    }

    /// Comma-joined recognised capability names, for error messages.
    pub fn known_names(&self) -> String {
        self.namespaces
            .iter()
            .flat_map(|ns| ns.ifaces.iter().copied())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Map a WIT import name to a capability name, or `None` for
    /// non-capability imports.
    ///
    /// Returns `Some(iface)` only for interfaces under a registered
    /// namespace; type-only packages like `nexum:host/types` and unrelated
    /// namespaces (`wasi:*`) fall through to `None` so they do not need a
    /// manifest declaration.
    ///
    /// Examples:
    /// - `"nexum:host/chain@0.2.0"`     -> `Some("chain")`
    /// - `"shepherd:cow/cow-api@0.2.0"` -> `Some("cow-api")` once the cow
    ///   namespace is registered
    /// - `"nexum:host/types@0.2.0"`     -> `None` (type-only, not a capability)
    /// - `"wasi:io/streams@0.2.0"`      -> `None`
    pub fn wit_import_to_cap<'a>(&self, import_name: &'a str) -> Option<&'a str> {
        let without_version = import_name.split('@').next().unwrap_or(import_name);
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
        assert_eq!(r.wit_import_to_cap("nexum:host/http@0.2.0"), Some("http"));
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
    fn wit_import_to_cap_wasi_is_none() {
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
            "nexum:host/http@0.2.0",
            "wasi:io/streams@0.2.0", // wasi is always skipped
        ];
        let r = registry_with_cow();
        assert!(enforce_capabilities(&loaded, imports.into_iter(), &r).is_ok());
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
}
