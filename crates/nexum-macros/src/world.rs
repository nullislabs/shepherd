//! Per-module world synthesis: turn the manifest's `[capabilities]`
//! declarations into an inline WIT world whose imports are exactly the
//! declared capability interfaces.
//!
//! The one non-obvious invariant: the capability table here must agree
//! with the runtime's capability registry (`nexum-runtime`'s manifest
//! enforcement) on both the capability names and the WIT interfaces they
//! map to. The runtime cross-checks a component's imports against the
//! manifest at load time; because this module derives the imports from
//! the same manifest, a component built through `#[nexum_sdk::module]`
//! passes that check by construction rather than by relying on the
//! toolchain eliding unused imports.

use std::fmt::Write as _;

/// One manifest capability and its world wiring.
struct Capability {
    /// The name declared under `[capabilities].required` / `optional`.
    name: &'static str,
    /// The WIT import the declaration turns into, or `None` for
    /// capabilities with no world import (`http` is granted through the
    /// SDK's wasi:http client and the host allowlist, not the world).
    import: Option<&'static str>,
    /// WIT package directories (under the workspace `wit/` root) the
    /// import needs on the resolve path, beyond `nexum-host`.
    packages: &'static [&'static str],
    /// The `bind_host_via_wit_bindgen!` capability ident carrying this
    /// capability's host-adapter pieces, if the SDK has a trait seam
    /// for it.
    adapter: Option<&'static str>,
}

/// Every capability the macro recognizes, in emission order. Mirrors
/// the runtime's core registry plus the extension namespaces the
/// workspace ships (`nexum:intent/pool`, `shepherd:cow/cow-api`).
const KNOWN: &[Capability] = &[
    Capability {
        name: "chain",
        import: Some("nexum:host/chain@0.2.0"),
        packages: &[],
        adapter: Some("chain"),
    },
    Capability {
        name: "identity",
        import: Some("nexum:host/identity@0.2.0"),
        packages: &[],
        adapter: None,
    },
    Capability {
        name: "local-store",
        import: Some("nexum:host/local-store@0.2.0"),
        packages: &[],
        adapter: Some("local_store"),
    },
    Capability {
        name: "remote-store",
        import: Some("nexum:host/remote-store@0.2.0"),
        packages: &[],
        adapter: None,
    },
    Capability {
        name: "messaging",
        import: Some("nexum:host/messaging@0.2.0"),
        packages: &[],
        adapter: None,
    },
    Capability {
        name: "logging",
        import: Some("nexum:host/logging@0.2.0"),
        packages: &[],
        adapter: Some("logging"),
    },
    Capability {
        name: "pool",
        import: Some("nexum:intent/pool@0.1.0"),
        packages: &["nexum-intent", "nexum-value-flow"],
        adapter: None,
    },
    Capability {
        name: "cow-api",
        import: Some("shepherd:cow/cow-api@0.2.0"),
        packages: &["shepherd-cow"],
        adapter: None,
    },
    Capability {
        name: "http",
        import: None,
        packages: &[],
        adapter: None,
    },
];

/// The synthesized world plus what the `generate!` call and the host
/// adapter need to go with it.
#[derive(Debug)]
pub struct ModuleWorld {
    /// Inline WIT text defining `nexum:module-world/module`.
    pub wit: String,
    /// WIT package directories (relative to the workspace `wit/` root)
    /// the resolve path must carry. Always starts with `nexum-host`.
    pub packages: Vec<&'static str>,
    /// Capability idents to pass to `bind_host_via_wit_bindgen!`.
    pub adapters: Vec<&'static str>,
}

/// Extract the declared capability names (`required` then `optional`)
/// from the manifest text. A missing or malformed `[capabilities]`
/// section is an error: the emitted world is derived from it, so the
/// macro has nothing to build from without one.
pub fn manifest_capabilities(text: &str) -> Result<Vec<String>, String> {
    let value: toml::Table = text
        .parse()
        .map_err(|e| format!("module.toml is not valid TOML: {e}"))?;
    let caps = value.get("capabilities").ok_or_else(|| {
        "module.toml has no [capabilities] section; #[nexum_sdk::module] derives the module's \
         WIT world from [capabilities].required/optional, so declare it (an empty `required = []` \
         is valid)"
            .to_string()
    })?;
    let list = |key: &str| -> Result<Vec<String>, String> {
        match caps.get(key) {
            None => Ok(Vec::new()),
            Some(v) => v
                .as_array()
                .ok_or_else(|| format!("[capabilities].{key} must be an array of strings"))?
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| format!("[capabilities].{key} must contain only strings"))
                })
                .collect(),
        }
    };
    let mut names = list("required")?;
    names.extend(list("optional")?);
    Ok(names)
}

/// Build the per-module world from the declared capability names
/// (required and optional alike: an optional capability must still be
/// importable, the host decides at load time whether to back or stub
/// it). Unknown names are a compile error so a typo cannot silently
/// drop an import.
pub fn synthesize(declared: &[String]) -> Result<ModuleWorld, String> {
    for name in declared {
        if !KNOWN.iter().any(|c| c.name == name.as_str()) {
            let known = KNOWN.iter().map(|c| c.name).collect::<Vec<_>>().join(", ");
            return Err(format!(
                "unknown capability `{name}` in module.toml [capabilities]; expected one of: \
                 {known}"
            ));
        }
    }

    let mut imports = String::new();
    let mut packages = vec!["nexum-host"];
    let mut adapters = Vec::new();
    for cap in KNOWN {
        if !declared.iter().any(|d| d == cap.name) {
            continue;
        }
        if let Some(import) = cap.import {
            writeln!(imports, "    import {import};").expect("write to String");
        }
        for package in cap.packages {
            if !packages.contains(package) {
                packages.push(package);
            }
        }
        if let Some(adapter) = cap.adapter {
            adapters.push(adapter);
        }
    }

    let mut wit = String::from(
        "package nexum:module-world;\n\nworld module {\n    \
         use nexum:host/types@0.2.0.{config, event, fault};\n\n",
    );
    wit.push_str(&imports);
    wit.push_str(
        "\n    export init: func(config: config) -> result<_, fault>;\n    \
         export on-event: func(event: event) -> result<_, fault>;\n}\n",
    );

    Ok(ModuleWorld {
        wit,
        packages,
        adapters,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_only_world_imports_logging_alone() {
        let world = synthesize(&["logging".to_string()]).unwrap();
        assert!(world.wit.contains("import nexum:host/logging@0.2.0;"));
        assert!(!world.wit.contains("import nexum:host/chain"));
        assert!(!world.wit.contains("shepherd:cow"));
        assert_eq!(world.packages, vec!["nexum-host"]);
        assert_eq!(world.adapters, vec!["logging"]);
    }

    #[test]
    fn cow_api_pulls_the_shepherd_cow_package() {
        let world = synthesize(&["logging".to_string(), "cow-api".to_string()]).unwrap();
        assert!(world.wit.contains("import shepherd:cow/cow-api@0.2.0;"));
        assert_eq!(world.packages, vec!["nexum-host", "shepherd-cow"]);
    }

    #[test]
    fn pool_pulls_the_intent_and_value_flow_packages() {
        let world = synthesize(&["pool".to_string()]).unwrap();
        assert!(world.wit.contains("import nexum:intent/pool@0.1.0;"));
        assert_eq!(
            world.packages,
            vec!["nexum-host", "nexum-intent", "nexum-value-flow"]
        );
        assert!(world.adapters.is_empty());
    }

    #[test]
    fn http_declares_no_world_import() {
        let world = synthesize(&["logging".to_string(), "http".to_string()]).unwrap();
        assert!(!world.wit.contains("wasi:http"));
        assert_eq!(world.packages, vec!["nexum-host"]);
    }

    #[test]
    fn duplicate_declarations_emit_one_import() {
        let world = synthesize(&["chain".to_string(), "chain".to_string()]).unwrap();
        assert_eq!(world.wit.matches("import nexum:host/chain").count(), 1);
        assert_eq!(world.adapters, vec!["chain"]);
    }

    #[test]
    fn unknown_capability_is_rejected_with_the_known_list() {
        let err = synthesize(&["telepathy".to_string()]).unwrap_err();
        assert!(err.contains("unknown capability `telepathy`"));
        assert!(err.contains("logging"));
    }

    #[test]
    fn manifest_capabilities_reads_required_and_optional() {
        let caps = manifest_capabilities(
            r#"
[capabilities]
required = ["logging", "chain"]
optional = ["remote-store"]

[capabilities.http]
allow = []
"#,
        )
        .unwrap();
        assert_eq!(caps, vec!["logging", "chain", "remote-store"]);
    }

    #[test]
    fn manifest_without_capabilities_section_is_an_error() {
        let err = manifest_capabilities("[module]\nname = \"x\"\n").unwrap_err();
        assert!(err.contains("[capabilities]"));
    }

    #[test]
    fn manifest_with_non_string_capability_is_an_error() {
        let err = manifest_capabilities("[capabilities]\nrequired = [1]\n").unwrap_err();
        assert!(err.contains("only strings"));
    }

    #[test]
    fn world_is_valid_wit_shape() {
        // Not a full WIT parse (that is the module build's job); pin the
        // structural pieces the runtime contract depends on.
        let world = synthesize(&["logging".to_string()]).unwrap();
        assert!(world.wit.starts_with("package nexum:module-world;"));
        assert!(world.wit.contains("world module {"));
        assert!(
            world
                .wit
                .contains("export init: func(config: config) -> result<_, fault>;")
        );
        assert!(
            world
                .wit
                .contains("export on-event: func(event: event) -> result<_, fault>;")
        );
    }
}
