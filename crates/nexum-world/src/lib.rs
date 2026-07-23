//! Per-module world synthesis: turn a manifest's `[capabilities]`
//! declarations into an inline WIT world whose imports are exactly the
//! declared capability interfaces.
//!
//! The one non-obvious invariant: the capability rows here must agree
//! with the runtime's capability registry (`nexum-runtime`'s manifest
//! enforcement) on both the capability names and the WIT interfaces they
//! map to. The runtime cross-checks a component's imports against the
//! manifest at load time; because the imports are derived from the same
//! manifest, a macro-built component passes that check by construction
//! rather than by relying on the toolchain eliding unused imports.
//!
//! The table here carries only the core `nexum:host` rows. Per-namespace
//! rows come from the composition root's `extensions.toml` registry
//! ([`manifest_extensions`]): the caller passes them to [`synthesize`],
//! so this crate carries no downstream name.

use std::path::{Path, PathBuf};
use strum::{EnumString, IntoStaticStr, VariantNames};

/// A core capability name: the single source the [`CORE`] table and the
/// runtime's capability registry emit from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, EnumString, VariantNames)]
#[strum(serialize_all = "kebab-case")]
#[non_exhaustive]
pub enum Cap {
    /// `nexum:host/chain`.
    Chain,
    /// `nexum:host/identity`.
    Identity,
    /// `nexum:host/local-store`.
    LocalStore,
    /// `nexum:host/remote-store`.
    RemoteStore,
    /// `nexum:host/messaging`.
    Messaging,
    /// `nexum:host/logging`.
    Logging,
    /// Gates `wasi:http/*`; no world import.
    Http,
}

impl Cap {
    /// The declared name, as a manifest spells it. Hand-written rather
    /// than derived: [`CORE`] and [`CORE_IFACES`] evaluate it in const
    /// context, and strum's `IntoStaticStr` emits a non-const `From`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chain => "chain",
            Self::Identity => "identity",
            Self::LocalStore => "local-store",
            Self::RemoteStore => "remote-store",
            Self::Messaging => "messaging",
            Self::Logging => "logging",
            Self::Http => "http",
        }
    }
}

/// A `nexum:host/types.fault` case as a stable snake_case label, in WIT
/// declaration order: the single source every label mirror emits from.
/// `IntoStaticStr` yields the label, `VARIANTS` the whole vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, EnumString, IntoStaticStr, VariantNames)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum FaultLabel {
    /// `fault.unsupported`.
    Unsupported,
    /// `fault.unavailable`.
    Unavailable,
    /// `fault.denied`.
    Denied,
    /// `fault.rate-limited`.
    RateLimited,
    /// `fault.timeout`.
    Timeout,
    /// `fault.invalid-input`.
    InvalidInput,
    /// `fault.internal`.
    Internal,
}

/// One manifest capability and its world wiring.
pub struct Capability {
    /// The name declared under `[capabilities].required` / `optional`.
    pub name: Cap,
    /// The WIT import the declaration turns into, or `None` for
    /// capabilities with no world import (`http` is granted through the
    /// SDK's wasi:http client and the host allowlist, not the world).
    pub import: Option<&'static str>,
    /// WIT package directories the import needs on the resolve path,
    /// beyond `nexum-host`.
    pub packages: &'static [&'static str],
    /// The `bind_host_via_wit_bindgen!` capability ident carrying this
    /// capability's host-adapter pieces, if the SDK has a trait seam
    /// for it.
    pub adapter: Option<&'static str>,
}

/// The core capability rows, in emission order. Mirrors the runtime's
/// core registry and nothing else; extension rows are the caller's.
pub const CORE: &[Capability] = &[
    Capability {
        name: Cap::Chain,
        import: Some("nexum:host/chain@0.1.0"),
        packages: &[],
        adapter: Some("chain"),
    },
    Capability {
        name: Cap::Identity,
        import: Some("nexum:host/identity@0.1.0"),
        packages: &[],
        adapter: Some("identity"),
    },
    Capability {
        name: Cap::LocalStore,
        import: Some("nexum:host/local-store@0.1.0"),
        packages: &[],
        adapter: Some("local_store"),
    },
    Capability {
        name: Cap::RemoteStore,
        import: Some("nexum:host/remote-store@0.1.0"),
        packages: &[],
        adapter: Some("remote_store"),
    },
    Capability {
        name: Cap::Messaging,
        import: Some("nexum:host/messaging@0.1.0"),
        packages: &[],
        adapter: Some("messaging"),
    },
    Capability {
        name: Cap::Logging,
        import: Some("nexum:host/logging@0.1.0"),
        packages: &[],
        adapter: Some("logging"),
    },
    Capability {
        name: Cap::Http,
        import: None,
        packages: &[],
        adapter: None,
    },
];

/// Number of import-bearing [`CORE`] rows.
const fn core_iface_count() -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < CORE.len() {
        if CORE[i].import.is_some() {
            n += 1;
        }
        i += 1;
    }
    n
}

/// Names of the import-bearing [`CORE`] rows, in emission order: the
/// `nexum:host` interface set the runtime's capability registry
/// enforces. `http` is absent (no world import).
pub const CORE_IFACES: [&str; core_iface_count()] = {
    let mut out = [""; core_iface_count()];
    let mut n = 0;
    let mut i = 0;
    while i < CORE.len() {
        if CORE[i].import.is_some() {
            out[n] = CORE[i].name.as_str();
            n += 1;
        }
        i += 1;
    }
    out
};

/// One registered extension row: a per-namespace capability a
/// composition root declares in its `extensions.toml`. An extension
/// always has a WIT import and never a host-adapter ident (adapter
/// seams are core-only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionRow {
    /// The name modules declare under `[capabilities]`.
    pub name: String,
    /// The WIT import the declaration turns into.
    pub import: String,
    /// WIT package directories the import needs on the resolve path,
    /// beyond `nexum-host`, in dependency order.
    pub packages: Vec<String>,
}

/// The synthesized world plus what the `generate!` call and the host
/// adapter need to go with it.
#[derive(Debug)]
pub struct ModuleWorld {
    /// Inline WIT text defining `nexum:module-world/module`.
    pub wit: String,
    /// WIT package directories the resolve path must carry, in
    /// dependency order (a package precedes its dependants). Always
    /// starts with the base set the host `event` variant needs.
    pub packages: Vec<String>,
    /// Capability idents to pass to `bind_host_via_wit_bindgen!`.
    pub adapters: Vec<&'static str>,
}

/// Extract the declared capability names (`required` then `optional`)
/// from the manifest text. A missing or malformed `[capabilities]`
/// section is an error: the emitted world is derived from it, so the
/// synthesis has nothing to build from without one.
pub fn manifest_capabilities(text: &str) -> Result<Vec<String>, String> {
    let value: toml::Table = text
        .parse()
        .map_err(|e| format!("module.toml is not valid TOML: {e}"))?;
    let caps = value.get("capabilities").ok_or_else(|| {
        "module.toml has no [capabilities] section; the module/adapter macro derives the \
         component's WIT world from [capabilities].required/optional, so declare it (an empty \
         `required = []` is valid)"
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

/// Extract the declared `[module] kind` from the manifest text, `None`
/// when absent (the runtime defaults an absent kind to the worker).
pub fn manifest_kind(text: &str) -> Result<Option<String>, String> {
    let value: toml::Table = text
        .parse()
        .map_err(|e| format!("module.toml is not valid TOML: {e}"))?;
    match value.get("module").and_then(|module| module.get("kind")) {
        None => Ok(None),
        Some(kind) => kind
            .as_str()
            .map(|kind| Some(kind.to_owned()))
            .ok_or_else(|| "[module].kind must be a string".to_string()),
    }
}

/// Parse the registered extension rows from an `extensions.toml`. Each
/// `[extensions.<name>]` table carries the WIT `import` the declaration
/// turns into and the extra `packages` its resolve path needs. A file
/// without an `[extensions]` section registers nothing.
pub fn manifest_extensions(text: &str) -> Result<Vec<ExtensionRow>, String> {
    let value: toml::Table = text
        .parse()
        .map_err(|e| format!("extensions.toml is not valid TOML: {e}"))?;
    let Some(extensions) = value.get("extensions") else {
        return Ok(Vec::new());
    };
    let extensions = extensions
        .as_table()
        .ok_or_else(|| "[extensions] must be a table of `[extensions.<name>]` rows".to_string())?;
    extensions
        .iter()
        .map(|(name, row)| {
            let row = row
                .as_table()
                .ok_or_else(|| format!("[extensions.{name}] must be a table"))?;
            let import = row
                .get("import")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| format!("[extensions.{name}] must carry a string `import`"))?
                .to_owned();
            let packages = match row.get("packages") {
                None => Vec::new(),
                Some(value) => value
                    .as_array()
                    .ok_or_else(|| {
                        format!("[extensions.{name}].packages must be an array of strings")
                    })?
                    .iter()
                    .map(|item| {
                        item.as_str().map(str::to_owned).ok_or_else(|| {
                            format!("[extensions.{name}].packages must contain only strings")
                        })
                    })
                    .collect::<Result<_, _>>()?,
            };
            Ok(ExtensionRow {
                name: name.clone(),
                import,
                packages,
            })
        })
        .collect()
}

/// Find the extension registry for a build rooted at `start`: the
/// nearest ancestor `extensions.toml`. `None` means no registered
/// extensions.
pub fn find_extensions_manifest(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(cur) = dir {
        let candidate = cur.join("extensions.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = cur.parent();
    }
    None
}

/// Build the per-module world from the declared capability names
/// (required and optional alike: an optional capability must still be
/// importable, the host decides at load time whether to back or stub
/// it). `extensions` carries the per-namespace rows of the registered
/// extensions, emitted after the core rows. Unknown names are an error
/// so a typo cannot silently drop an import; a registered name that
/// shadows a core row or another registration is an error so a
/// colliding registry cannot emit a duplicate import.
pub fn synthesize(declared: &[String], extensions: &[ExtensionRow]) -> Result<ModuleWorld, String> {
    for (idx, ext) in extensions.iter().enumerate() {
        if CORE.iter().any(|c| c.name.as_str() == ext.name)
            || extensions[..idx].iter().any(|prior| prior.name == ext.name)
        {
            return Err(format!(
                "extension capability `{}` collides with an already-registered capability; \
                 names must be unique across the core table and the registered extensions",
                ext.name
            ));
        }
    }

    let known = || {
        CORE.iter()
            .map(|c| c.name.as_str())
            .chain(extensions.iter().map(|e| e.name.as_str()))
    };
    for name in declared {
        if !known().any(|k| k == name.as_str()) {
            let names = known().collect::<Vec<_>>().join(", ");
            return Err(format!(
                "unknown capability `{name}` in module.toml [capabilities]; expected one of: \
                 {names}"
            ));
        }
    }

    let mut imports = String::new();
    // `nexum:host` is a leaf package (the `event` variant carries status
    // transitions as opaque bytes), so the base resolve set
    // is the host package alone; capability declarations append their
    // own packages. Dependency order: each directory is parsed against
    // the packages before it, so a package precedes its dependants.
    let mut packages = vec!["nexum-host".to_owned()];
    let mut adapters = Vec::new();
    for cap in CORE {
        if !declared.iter().any(|d| d == cap.name.as_str()) {
            continue;
        }
        if let Some(import) = cap.import {
            imports.push_str(&format!("    import {import};\n"));
        }
        for package in cap.packages {
            if !packages.iter().any(|p| p == package) {
                packages.push((*package).to_owned());
            }
        }
        if let Some(adapter) = cap.adapter {
            adapters.push(adapter);
        }
    }
    for ext in extensions {
        if !declared.contains(&ext.name) {
            continue;
        }
        imports.push_str(&format!("    import {};\n", ext.import));
        for package in &ext.packages {
            if !packages.contains(package) {
                packages.push(package.clone());
            }
        }
    }

    let mut wit = String::from(
        "package nexum:module-world;\n\nworld module {\n    \
         use nexum:host/types@0.1.0.{config, event, fault};\n\n",
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

/// Resolve each WIT package directory for a component build rooted at
/// `start` (the consuming crate's manifest directory). A package
/// resolves crate-locally, vendored `wit/deps/<package>` before own
/// `wit/<package>`; a crate not carrying it falls back to the nearest
/// ancestor `wit/` that does (the transitional monorepo layout).
pub fn resolve_wit_packages<S: AsRef<str>>(
    start: &Path,
    packages: &[S],
) -> Result<Vec<PathBuf>, String> {
    packages
        .iter()
        .map(|package| {
            let package = package.as_ref();
            resolve_wit_package(start, package).ok_or_else(|| {
                format!(
                    "declared capabilities need the `{package}` WIT package, but neither \
                     `wit/deps/{package}` nor `wit/{package}` exists under {} or any ancestor",
                    start.display()
                )
            })
        })
        .collect()
}

/// The consuming crate's manifest directory, the root every crate-local
/// lookup starts from.
pub fn manifest_dir() -> Result<PathBuf, String> {
    std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .map_err(|_| "CARGO_MANIFEST_DIR is not set".to_string())
}

/// [`resolve_wit_packages`] rooted at [`manifest_dir`], as the strings
/// `wit_bindgen::generate!` takes for its `path`.
pub fn manifest_wit_packages<S: AsRef<str>>(packages: &[S]) -> Result<Vec<String>, String> {
    Ok(resolve_wit_packages(&manifest_dir()?, packages)?
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect())
}

/// Whether a type is a plain named path (`Foo`), the only shape a module
/// export type may take.
#[cfg(feature = "macros")]
pub fn is_plain_type(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Path(tp) if tp.qself.is_none())
}

/// Find one package directory: crate-local `wit/deps/<package>` then
/// `wit/<package>`, walking up on a miss.
fn resolve_wit_package(start: &Path, package: &str) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(cur) = dir {
        let wit = cur.join("wit");
        for candidate in [wit.join("deps").join(package), wit.join(package)] {
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
        dir = cur.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The base package set every module world resolves against:
    /// `nexum:host` is a leaf package, so it stands alone.
    const MODULE_PACKAGES: [&str; 1] = ["nexum-host"];

    /// A stand-in extension row, as a registered extension would pass.
    fn ext() -> Vec<ExtensionRow> {
        vec![ExtensionRow {
            name: "acme".to_owned(),
            import: "acme:ext/api@0.1.0".to_owned(),
            packages: vec!["acme-ext".to_owned()],
        }]
    }

    #[test]
    fn logging_only_world_imports_logging_alone() {
        let world = synthesize(&["logging".to_string()], &[]).unwrap();
        assert!(world.wit.contains("import nexum:host/logging@0.1.0;"));
        assert!(!world.wit.contains("import nexum:host/chain"));
        assert_eq!(world.packages, MODULE_PACKAGES);
        assert_eq!(world.adapters, vec!["logging"]);
    }

    #[test]
    fn extension_row_emits_its_import_and_packages() {
        let world = synthesize(&["logging".to_string(), "acme".to_string()], &ext()).unwrap();
        assert!(world.wit.contains("import acme:ext/api@0.1.0;"));
        assert_eq!(world.packages, vec!["nexum-host", "acme-ext"]);
    }

    #[test]
    fn undeclared_extension_row_stays_out_of_the_world() {
        let world = synthesize(&["logging".to_string()], &ext()).unwrap();
        assert!(!world.wit.contains("acme"));
        assert_eq!(world.packages, MODULE_PACKAGES);
    }

    #[test]
    fn extension_shadowing_a_core_name_is_rejected() {
        let rows = vec![ExtensionRow {
            name: "chain".to_owned(),
            import: "acme:ext/chain@0.1.0".to_owned(),
            packages: Vec::new(),
        }];
        let err = synthesize(&["chain".to_string()], &rows).unwrap_err();
        assert!(err.contains("extension capability `chain` collides"));
    }

    #[test]
    fn duplicate_extension_registration_is_rejected() {
        let mut rows = ext();
        rows.extend(ext());
        let err = synthesize(&[], &rows).unwrap_err();
        assert!(err.contains("extension capability `acme` collides"));
    }

    #[test]
    fn core_ifaces_are_the_import_bearing_rows() {
        assert_eq!(
            CORE_IFACES,
            [
                Cap::Chain.as_str(),
                Cap::Identity.as_str(),
                Cap::LocalStore.as_str(),
                Cap::RemoteStore.as_str(),
                Cap::Messaging.as_str(),
                Cap::Logging.as_str(),
            ],
        );
        assert!(!CORE_IFACES.contains(&Cap::Http.as_str()));
    }

    /// The const accessor is hand-written, so pin it to the derived
    /// vocabulary in both directions.
    #[test]
    fn cap_accessor_agrees_with_the_derived_vocabulary() {
        let names: Vec<&str> = CORE.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, Cap::VARIANTS);
        for name in Cap::VARIANTS {
            assert_eq!(name.parse::<Cap>().unwrap().as_str(), *name);
        }
    }

    #[test]
    fn fault_labels_are_snake_case_and_distinct() {
        for label in FaultLabel::VARIANTS {
            assert!(label.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        }
        let mut labels = FaultLabel::VARIANTS.to_vec();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), FaultLabel::VARIANTS.len());
    }

    #[test]
    fn fault_label_parses_back_from_its_label() {
        for label in FaultLabel::VARIANTS {
            let parsed: FaultLabel = label.parse().unwrap();
            assert_eq!(<&'static str>::from(parsed), *label);
        }
        assert!("nonesuch".parse::<FaultLabel>().is_err());
    }

    #[test]
    fn core_table_carries_no_extension_row() {
        assert!(
            CORE.iter()
                .all(|c| c.import.is_none_or(|i| i.starts_with("nexum:host/")))
        );
        assert!(CORE.iter().all(|c| c.packages.is_empty()));
    }

    #[test]
    fn every_import_bearing_core_row_carries_an_adapter() {
        // `http` has no world import (SDK wasi:http client) and no
        // adapter; every other core row has both.
        for cap in CORE {
            assert_eq!(
                cap.import.is_some(),
                cap.adapter.is_some(),
                "{}",
                cap.name.as_str()
            );
        }
    }

    #[test]
    fn full_declaration_emits_the_six_adapters_in_core_order() {
        let declared: Vec<String> = CORE.iter().map(|c| c.name.as_str().to_owned()).collect();
        let world = synthesize(&declared, &[]).unwrap();
        assert_eq!(
            world.adapters,
            vec![
                "chain",
                "identity",
                "local_store",
                "remote_store",
                "messaging",
                "logging",
            ],
        );
    }

    #[test]
    fn http_declares_no_world_import() {
        let world = synthesize(&["logging".to_string(), "http".to_string()], &[]).unwrap();
        assert!(!world.wit.contains("wasi:http"));
        assert_eq!(world.packages, MODULE_PACKAGES);
    }

    #[test]
    fn duplicate_declarations_emit_one_import() {
        let world = synthesize(&["chain".to_string(), "chain".to_string()], &[]).unwrap();
        assert_eq!(world.wit.matches("import nexum:host/chain").count(), 1);
        assert_eq!(world.adapters, vec!["chain"]);
    }

    #[test]
    fn unknown_capability_is_rejected_with_the_known_list() {
        let err = synthesize(&["telepathy".to_string()], &ext()).unwrap_err();
        assert!(err.contains("unknown capability `telepathy`"));
        assert!(err.contains("logging"));
        assert!(err.contains("acme"));
    }

    #[test]
    fn manifest_extensions_reads_rows() {
        let rows = manifest_extensions(
            r#"
[extensions.acme]
import = "acme:ext/api@0.1.0"
packages = ["acme-base", "acme-ext"]

[extensions.beta]
import = "beta:ext/api@0.1.0"
"#,
        )
        .unwrap();
        assert_eq!(rows, {
            let mut expected = ext();
            expected[0].packages = vec!["acme-base".to_owned(), "acme-ext".to_owned()];
            expected.push(ExtensionRow {
                name: "beta".to_owned(),
                import: "beta:ext/api@0.1.0".to_owned(),
                packages: Vec::new(),
            });
            expected
        });
    }

    #[test]
    fn manifest_without_extensions_section_registers_nothing() {
        assert_eq!(manifest_extensions("").unwrap(), Vec::new());
    }

    #[test]
    fn extension_row_without_an_import_is_an_error() {
        let err = manifest_extensions("[extensions.acme]\npackages = []\n").unwrap_err();
        assert!(err.contains("[extensions.acme] must carry a string `import`"));
    }

    #[test]
    fn extension_row_with_non_string_package_is_an_error() {
        let err =
            manifest_extensions("[extensions.acme]\nimport = \"a:b/c@0.1.0\"\npackages = [1]\n")
                .unwrap_err();
        assert!(err.contains("only strings"));
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
    fn manifest_kind_reads_the_module_kind() {
        let kind = manifest_kind("[module]\nname = \"x\"\nkind = \"venue-adapter\"\n").unwrap();
        assert_eq!(kind.as_deref(), Some("venue-adapter"));
    }

    #[test]
    fn manifest_without_a_kind_is_none() {
        assert_eq!(manifest_kind("[module]\nname = \"x\"\n").unwrap(), None);
        assert_eq!(manifest_kind("").unwrap(), None);
    }

    #[test]
    fn manifest_with_a_non_string_kind_is_an_error() {
        let err = manifest_kind("[module]\nkind = 3\n").unwrap_err();
        assert!(err.contains("[module].kind must be a string"));
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
        let world = synthesize(&["logging".to_string()], &[]).unwrap();
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

    #[test]
    fn resolution_prefers_vendored_deps_over_own_wit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("wit/deps/pkg")).unwrap();
        std::fs::create_dir_all(root.join("wit/pkg")).unwrap();
        let paths = resolve_wit_packages(root, &["pkg"]).unwrap();
        assert_eq!(paths, vec![root.join("wit/deps/pkg")]);
    }

    #[test]
    fn resolution_falls_back_to_the_nearest_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("wit/pkg")).unwrap();
        let leaf = root.join("crates/leaf");
        std::fs::create_dir_all(&leaf).unwrap();
        let paths = resolve_wit_packages(&leaf, &["pkg"]).unwrap();
        assert_eq!(paths, vec![root.join("wit/pkg")]);
    }

    #[test]
    fn crate_local_package_shadows_the_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("wit/pkg")).unwrap();
        let leaf = root.join("crates/leaf");
        std::fs::create_dir_all(leaf.join("wit/deps/pkg")).unwrap();
        let paths = resolve_wit_packages(&leaf, &["pkg"]).unwrap();
        assert_eq!(paths, vec![leaf.join("wit/deps/pkg")]);
    }

    #[test]
    fn extension_registry_resolves_from_the_nearest_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("extensions.toml"), "").unwrap();
        let leaf = root.join("crates/leaf");
        std::fs::create_dir_all(&leaf).unwrap();
        assert_eq!(
            find_extensions_manifest(&leaf),
            Some(root.join("extensions.toml"))
        );
    }

    #[test]
    fn absent_extension_registry_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_extensions_manifest(dir.path()), None);
    }

    #[test]
    fn missing_package_names_the_paths_tried() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_wit_packages(dir.path(), &["pkg"]).unwrap_err();
        assert!(err.contains("`pkg` WIT package"));
        assert!(err.contains("wit/deps/pkg"));
    }
}
