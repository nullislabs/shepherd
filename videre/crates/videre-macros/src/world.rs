//! World wiring for the venue macro: the venue-adapter world synthesis.
//! The module world synthesis, the core capability table, and the
//! extension registry parsing live in `nexum-world`.

pub use nexum_world::ModuleWorld;

/// Capabilities a venue adapter may import: scoped transport only (chain,
/// messaging, and HTTP via the SDK's wasi:http client). `local-store`,
/// `remote-store`, `identity`, and `logging` are refused.
const VENUE_CAPABILITIES: &[&str] = &["chain", "messaging", "http"];

/// Build the venue-adapter world from the declared capability names: exports
/// `init` and the `videre:venue/adapter` face, imports exactly the declared
/// scoped transport. A capability outside the venue-permitted set is a
/// compile error.
pub fn synthesize_venue(declared: &[String]) -> Result<ModuleWorld, String> {
    for name in declared {
        if !VENUE_CAPABILITIES.contains(&name.as_str()) {
            let permitted = VENUE_CAPABILITIES.join(", ");
            return Err(format!(
                "capability `{name}` is not available to a venue adapter; a venue may import \
                 only scoped transport ({permitted}) and structurally cannot touch local-store, \
                 remote-store, identity, or logging"
            ));
        }
    }

    let mut imports = String::new();
    // The export face (`videre:venue/adapter`, its types, and the
    // value-flow vocabulary they are expressed in) needs the videre
    // packages on the resolve path beyond the leaf host package, in
    // dependency order: a package precedes its dependants.
    let mut packages: Vec<String> = [
        "videre-value-flow",
        "videre-types",
        "nexum-host",
        "videre-venue",
    ]
    .map(str::to_owned)
    .into();
    for cap in nexum_world::CORE {
        if !declared.iter().any(|d| d == cap.name.as_str()) {
            continue;
        }
        if let Some(import) = cap.import {
            imports.push_str(&format!("    import {import};\n"));
        }
        // Accumulate any extra WIT packages a venue capability needs,
        // exactly as the module synthesis does. All venue-permitted
        // capabilities are packageless today, so this leaves the base set
        // untouched; mirroring the loop keeps a future venue capability
        // from silently failing to reach its package onto the resolve
        // path.
        for package in cap.packages {
            if !packages.iter().any(|p| p == package) {
                packages.push((*package).to_owned());
            }
        }
    }

    let mut wit = String::from(
        "package nexum:venue-world;\n\nworld venue-adapter {\n    \
         use nexum:host/types@0.1.0.{config, fault};\n\n",
    );
    wit.push_str(&imports);
    wit.push_str(
        "\n    export init: func(config: config) -> result<_, fault>;\n    \
         export videre:venue/adapter@0.1.0;\n}\n",
    );

    Ok(ModuleWorld {
        wit,
        packages,
        // The venue export glue wires the adapter's associated functions
        // to the world's Guest traits directly; there is no host-trait
        // adapter to bind, so no capability idents to pass on.
        adapters: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The package set every venue world resolves against: the exported
    /// adapter face pulls the videre vocabulary, in dependency order.
    const VENUE_PACKAGES: [&str; 4] = [
        "videre-value-flow",
        "videre-types",
        "nexum-host",
        "videre-venue",
    ];

    #[test]
    fn venue_world_exports_the_adapter_face() {
        let world = synthesize_venue(&["chain".to_string()]).unwrap();
        assert!(world.wit.starts_with("package nexum:venue-world;"));
        assert!(world.wit.contains("world venue-adapter {"));
        assert!(
            world
                .wit
                .contains("export init: func(config: config) -> result<_, fault>;")
        );
        assert!(world.wit.contains("export videre:venue/adapter@0.1.0;"));
        assert_eq!(world.packages, VENUE_PACKAGES);
        assert!(world.adapters.is_empty());
    }

    #[test]
    fn venue_world_imports_only_declared_transport() {
        let world = synthesize_venue(&["chain".to_string()]).unwrap();
        assert!(world.wit.contains("import nexum:host/chain@0.1.0;"));
        assert!(!world.wit.contains("import nexum:host/messaging"));

        let both = synthesize_venue(&["chain".to_string(), "messaging".to_string()]).unwrap();
        assert!(both.wit.contains("import nexum:host/chain@0.1.0;"));
        assert!(both.wit.contains("import nexum:host/messaging@0.1.0;"));
    }

    #[test]
    fn venue_world_grants_http_without_a_world_import() {
        let world = synthesize_venue(&["http".to_string()]).unwrap();
        assert!(!world.wit.contains("import"));
        assert!(!world.wit.contains("wasi:http"));
        assert_eq!(world.packages, VENUE_PACKAGES);
    }

    #[test]
    fn venue_world_with_no_capabilities_imports_nothing() {
        let world = synthesize_venue(&[]).unwrap();
        assert!(!world.wit.contains("import"));
        assert!(world.wit.contains("export videre:venue/adapter@0.1.0;"));
    }

    #[test]
    fn venue_world_refuses_non_transport_capabilities() {
        for cap in [
            "local-store",
            "remote-store",
            "identity",
            "logging",
            "client",
        ] {
            let err = synthesize_venue(&[cap.to_string()]).unwrap_err();
            assert!(err.contains(cap), "message was: {err}");
            assert!(err.contains("venue adapter"), "message was: {err}");
        }
    }
}
