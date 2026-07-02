//! Capability enforcement: cross-checks the component's WIT imports
//! against the `[capabilities]` block declared in `module.toml`.

use std::collections::HashSet;

use super::error::CapabilityViolation;
use super::types::{KNOWN_CAPABILITIES, LoadedManifest};

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
/// (`&str`) of each `(&str, ComponentItem)` tuple.
pub fn enforce_capabilities<'a>(
    loaded: &LoadedManifest,
    component_imports: impl Iterator<Item = &'a str>,
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
        if let Some(cap) = wit_import_to_cap(import_name)
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

/// Map a WIT import name to a capability name, or `None` for non-capability
/// imports.
///
/// Returns `Some(iface)` only for interfaces in [`KNOWN_CAPABILITIES`];
/// type-only packages like `nexum:host/types` and unrelated namespaces
/// (`wasi:*`) fall through to `None` so they do not need a manifest
/// declaration.
///
/// Examples:
/// - `"nexum:host/chain@0.2.0"`     -> `Some("chain")`
/// - `"shepherd:cow/cow-api@0.2.0"` -> `Some("cow-api")`
/// - `"nexum:host/types@0.2.0"`     -> `None` (type-only, not a capability)
/// - `"wasi:io/streams@0.2.0"`      -> `None`
pub(super) fn wit_import_to_cap(import_name: &str) -> Option<&str> {
    let without_version = import_name.split('@').next().unwrap_or(import_name);
    let iface = without_version
        .strip_prefix("nexum:host/")
        .or_else(|| without_version.strip_prefix("shepherd:cow/"))?;
    if KNOWN_CAPABILITIES.contains(&iface) {
        Some(iface)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::types::{CapabilitiesSection, Manifest};

    #[test]
    fn wit_import_to_cap_nexum_host() {
        assert_eq!(wit_import_to_cap("nexum:host/chain@0.2.0"), Some("chain"));
        assert_eq!(
            wit_import_to_cap("nexum:host/local-store@0.2.0"),
            Some("local-store")
        );
        assert_eq!(wit_import_to_cap("nexum:host/http@0.2.0"), Some("http"));
    }

    #[test]
    fn wit_import_to_cap_shepherd_cow() {
        assert_eq!(
            wit_import_to_cap("shepherd:cow/cow-api@0.2.0"),
            Some("cow-api")
        );
    }

    #[test]
    fn wit_import_to_cap_wasi_is_none() {
        assert_eq!(wit_import_to_cap("wasi:io/streams@0.2.0"), None);
        assert_eq!(wit_import_to_cap("wasi:cli/stdin@0.2.0"), None);
        assert_eq!(wit_import_to_cap("wasi:sockets/tcp@0.2.0"), None);
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
        assert!(enforce_capabilities(&loaded, imports.into_iter()).is_ok());
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
        assert!(enforce_capabilities(&loaded, imports.into_iter()).is_ok());
    }

    #[test]
    fn enforce_rejects_undeclared_import() {
        let loaded = manifest_with_caps(&["chain"], &[]);
        // module imports remote-store but didn't declare it
        let imports = ["nexum:host/chain@0.2.0", "nexum:host/remote-store@0.2.0"];
        let err = enforce_capabilities(&loaded, imports.into_iter()).unwrap_err();
        assert_eq!(err.capability, "remote-store");
    }

    #[test]
    fn enforce_optional_caps_are_also_allowed() {
        let loaded = manifest_with_caps(&["chain"], &["remote-store"]);
        let imports = ["nexum:host/chain@0.2.0", "nexum:host/remote-store@0.2.0"];
        assert!(enforce_capabilities(&loaded, imports.into_iter()).is_ok());
    }
}
