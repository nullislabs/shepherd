//! Install-time body-version handshake over the `[venue]` manifest
//! section: a keeper declares the one body-schema version it encodes,
//! an adapter the set it decodes, and a keeper boots only when every
//! installed venue adapter decodes its version, since any of them is a
//! legal runtime submit target.

use std::collections::BTreeSet;

use anyhow::{anyhow, bail};
use nexum_runtime::host::extension::ProviderManifest;
use nexum_runtime::manifest::ExtensionSections;
use serde::Deserialize;
use tracing::error;

use crate::registry::VenueAdapterKind;

/// The manifest section the videre platform claims.
pub(crate) const SECTION: &str = "venue";

/// The claimed-section list handed to the runtime.
pub(crate) const SECTIONS: &[&str] = &[SECTION];

/// Keeper-side `[venue]`: the one body-schema version it encodes.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeeperSection {
    body_version: u32,
}

/// Adapter-side `[venue]`: the body-schema versions it decodes.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdapterSection {
    body_versions: BTreeSet<u32>,
}

/// Parse one `[venue]` value as `S`, tagging failures with the owner.
fn parse<S: for<'de> Deserialize<'de>>(owner: &str, value: &toml::Value) -> anyhow::Result<S> {
    value
        .clone()
        .try_into()
        .map_err(|e| anyhow!("{owner} [venue]: {e}"))
}

/// Admit one provider: a `[venue]` section, when present, must be the
/// adapter shape with a non-empty version set.
pub(crate) fn admit_provider(provider: &str, sections: &ExtensionSections) -> anyhow::Result<()> {
    let Some(value) = sections.get(SECTION) else {
        return Ok(());
    };
    let section: AdapterSection = parse(provider, value)?;
    if section.body_versions.is_empty() {
        bail!("{provider} [venue]: body_versions must not be empty");
    }
    Ok(())
}

/// An adapter's declared decode set: its `[venue] body_versions`, empty
/// when the section is absent.
pub(crate) fn declared_versions(
    provider: &str,
    sections: &ExtensionSections,
) -> anyhow::Result<BTreeSet<u32>> {
    let Some(value) = sections.get(SECTION) else {
        return Ok(BTreeSet::new());
    };
    let section: AdapterSection = parse(provider, value)?;
    Ok(section.body_versions)
}

/// Assert an adapter's `body-versions()` export equals its manifest
/// claim, refusing the install on divergence so the two sources of the
/// decode set cannot drift.
pub(crate) fn verify_exported_versions(
    provider: &str,
    declared: &BTreeSet<u32>,
    exported: Vec<u32>,
) -> anyhow::Result<()> {
    let exported: BTreeSet<u32> = exported.into_iter().collect();
    if exported != *declared {
        bail!(
            "{provider} exports body versions {exported:?}; the manifest [venue] \
             body_versions declares {declared:?}"
        );
    }
    Ok(())
}

/// The membership install predicate: a worker declaring `[venue]
/// body_version` is admitted only when every installed venue adapter's
/// `[venue] body_versions` set contains that version. Every installed
/// venue is a legal runtime submit target, so one non-decoding adapter
/// refuses the keeper.
pub(crate) fn admit_worker(
    worker: &str,
    sections: &ExtensionSections,
    providers: &[ProviderManifest],
) -> anyhow::Result<()> {
    let Some(value) = sections.get(SECTION) else {
        return Ok(());
    };
    let KeeperSection { body_version } = parse(worker, value)?;
    let adapters: Vec<&ProviderManifest> = providers
        .iter()
        .filter(|p| p.kind == VenueAdapterKind::KIND)
        .collect();
    if adapters.is_empty() {
        return Err(refuse(
            worker,
            body_version,
            "no venue adapter declares [venue] body_versions",
        ));
    }
    for provider in adapters {
        let Some(value) = provider.sections.get(SECTION) else {
            return Err(refuse(
                worker,
                body_version,
                &format!("{} declares no [venue] body_versions", provider.name),
            ));
        };
        let section: AdapterSection = parse(&provider.name, value)?;
        if !section.body_versions.contains(&body_version) {
            return Err(refuse(
                worker,
                body_version,
                &format!("{} decodes {:?}", provider.name, section.body_versions),
            ));
        }
    }
    Ok(())
}

/// Log and build the refusal for one keeper/adapter pairing.
fn refuse(worker: &str, body_version: u32, decoded: &str) -> anyhow::Error {
    error!(
        keeper = %worker,
        body_version,
        %decoded,
        "body-version handshake refused the keeper/adapter pair",
    );
    anyhow!("keeper {worker} encodes body version {body_version}; {decoded}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sections(toml: &str) -> ExtensionSections {
        let table: toml::Table = toml.parse().expect("parse");
        table.into_iter().collect()
    }

    fn adapter(name: &str, toml: &str) -> ProviderManifest {
        ProviderManifest {
            name: name.to_owned(),
            kind: VenueAdapterKind::KIND,
            sections: sections(toml),
        }
    }

    /// A keeper whose version every installed adapter decodes is admitted.
    #[test]
    fn matching_pair_is_admitted() {
        let keeper = sections("[venue]\nbody_version = 2");
        let adapters = [
            adapter("cow", "[venue]\nbody_versions = [1, 2]"),
            adapter("uni", "[venue]\nbody_versions = [2, 3]"),
        ];
        admit_worker("keeper", &keeper, &adapters).expect("admitted");
    }

    /// A keeper whose version no adapter decodes is refused, and the
    /// refusal names the version and the declared set.
    #[test]
    fn mismatched_pair_is_refused() {
        let keeper = sections("[venue]\nbody_version = 2");
        let adapters = [adapter("cow", "[venue]\nbody_versions = [1]")];
        let err = admit_worker("keeper", &keeper, &adapters).expect_err("refused");
        let msg = err.to_string();
        assert!(msg.contains("body version 2"), "{msg}");
        assert!(msg.contains("cow decodes {1}"), "{msg}");
    }

    /// Membership is a conjunction over the installed adapters: one
    /// non-decoding adapter refuses the keeper even when another decodes
    /// its version, since either is a legal runtime submit target.
    #[test]
    fn one_non_decoding_adapter_refuses_the_keeper() {
        let keeper = sections("[venue]\nbody_version = 2");
        let adapters = [
            adapter("cow", "[venue]\nbody_versions = [1, 2]"),
            adapter("uni", "[venue]\nbody_versions = [1]"),
        ];
        let err = admit_worker("keeper", &keeper, &adapters).expect_err("refused");
        let msg = err.to_string();
        assert!(msg.contains("body version 2"), "{msg}");
        assert!(msg.contains("uni decodes {1}"), "{msg}");
    }

    /// A declaring keeper with no installed venue adapter is refused.
    #[test]
    fn undeclared_adapters_refuse_a_declaring_keeper() {
        let keeper = sections("[venue]\nbody_version = 1");
        let err = admit_worker("keeper", &keeper, &[]).expect_err("refused");
        assert!(err.to_string().contains("no venue adapter declares"));
    }

    /// An installed adapter without a `[venue]` section refuses a
    /// declaring keeper: its decode set is undeclared, not universal.
    #[test]
    fn a_section_less_adapter_refuses_a_declaring_keeper() {
        let keeper = sections("[venue]\nbody_version = 1");
        let adapters = [adapter("cow", "")];
        let err = admit_worker("keeper", &keeper, &adapters).expect_err("refused");
        assert!(err.to_string().contains("cow declares no [venue]"));
    }

    /// A provider of another kind never satisfies the membership check.
    #[test]
    fn other_provider_kinds_are_ignored() {
        let keeper = sections("[venue]\nbody_version = 1");
        let mut other = adapter("oracle", "[venue]\nbody_versions = [1]");
        other.kind = "price-oracle";
        admit_worker("keeper", &keeper, &[other]).expect_err("refused");
    }

    /// Workers and providers without a `[venue]` section are admitted.
    #[test]
    fn undeclared_sections_are_admitted() {
        admit_worker("keeper", &ExtensionSections::new(), &[]).expect("worker admitted");
        admit_provider("venue", &ExtensionSections::new()).expect("provider admitted");
    }

    /// The wrong-side spelling fails loudly on both faces.
    #[test]
    fn wrong_side_spelling_is_refused() {
        let keeper = sections("[venue]\nbody_versions = [1]");
        admit_worker("keeper", &keeper, &[]).expect_err("keeper with the adapter key");

        let venue = sections("[venue]\nbody_version = 1");
        admit_provider("venue", &venue).expect_err("adapter with the keeper key");
    }

    /// An adapter declaring an empty decode set is refused at install.
    #[test]
    fn empty_adapter_set_is_refused() {
        let venue = sections("[venue]\nbody_versions = []");
        let err = admit_provider("venue", &venue).expect_err("refused");
        assert!(err.to_string().contains("must not be empty"));
    }

    /// A well-formed adapter declaration is admitted.
    #[test]
    fn adapter_declaration_is_admitted() {
        let venue = sections("[venue]\nbody_versions = [1, 2]");
        admit_provider("venue", &venue).expect("admitted");
    }

    /// The exported set must equal the manifest claim exactly; either
    /// direction of drift refuses the install.
    #[test]
    fn exported_versions_must_equal_the_manifest_claim() {
        let declared = declared_versions("venue", &sections("[venue]\nbody_versions = [1, 2]"))
            .expect("declared");
        verify_exported_versions("venue", &declared, vec![2, 1]).expect("equal sets");

        let err = verify_exported_versions("venue", &declared, vec![1]).expect_err("narrower");
        assert!(
            err.to_string().contains("exports body versions {1}"),
            "{err}"
        );
        verify_exported_versions("venue", &declared, vec![1, 2, 3]).expect_err("wider");
    }

    /// A section-less adapter must export an empty set: an undeclared
    /// manifest with a declaring export is drift, not a default.
    #[test]
    fn a_section_less_adapter_must_export_no_versions() {
        let declared = declared_versions("venue", &ExtensionSections::new()).expect("declared");
        assert!(declared.is_empty());
        verify_exported_versions("venue", &declared, Vec::new()).expect("both undeclared");
        verify_exported_versions("venue", &declared, vec![1]).expect_err("export-only drift");
    }
}
