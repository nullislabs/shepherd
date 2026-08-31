//! Venue registration for the shepherd composition root.
//!
//! A venue is a native Rust `VenueInvoker` linked into this binary. The
//! runtime deleted the extension-installed component path, so the
//! `[[adapters]]` table is gone from `engine.toml` and two operator
//! capabilities went with it.
//!
//! 1. Per-venue outbound-HTTP confinement. `[[adapters]].http_allow` fed
//!    the runtime's wasi:http gate, which reaches guest components only. A
//!    native venue owns its own HTTP client, so no allowlist bounds it.
//!    `[policy].http_deny` and `[policy.component.<id>].http_allow` still
//!    scope a guest component; neither reaches a venue.
//! 2. Operator-swappable venues. `path` and `manifest` have no successor:
//!    the venue set is fixed at compile time, and changing it means a new
//!    binary.
//!
//! What the adapter manifest's `[config]` table carried survives as
//! `[extensions.videre.venues.<id>]` in `engine.toml`. The venue id is the
//! field name, so an id this binary does not link refuses at load. The
//! body-schema versions the venue decodes are no longer operator-written:
//! they are `cow_venue::BODY_VERSIONS`, passed to the registry by
//! `cow_venue::register`.

use std::time::Duration;

use anyhow::Context;
use cow_venue::{CowAdapter, CowConfig};
use nexum_runtime::config::EngineConfig;
use serde::Deserialize;
use tracing::warn;
use url::Url;
use videre_host::VenueRegistry;

/// The `[extensions.<name>]` table this composition root reads.
const NAMESPACE: &str = "videre";

/// The id the cow venue registers under.
const COW: &str = "cow";

/// `[extensions.videre]`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct VidereSection {
    #[serde(default)]
    venues: Venues,
}

/// `[extensions.videre.venues]`: one field per venue this binary links.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Venues {
    cow: Option<CowVenue>,
}

/// `[extensions.videre.venues.cow]`, the successor to the adapter
/// manifest's `[config]` table. Keys are snake_case like the rest of
/// `engine.toml`, not the kebab-case the manifest table used.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CowVenue {
    /// Chain id or orderbook slug; selects the orderbook.
    chain: cow_venue::Chain,
    /// Overrides that chain's public orderbook, for a barn or a mock.
    orderbook_url: Option<String>,
    /// Address the pre-sign submit path posts `from`. Without it an
    /// unsigned body is refused.
    owner: Option<alloy_primitives::Address>,
    /// Per-request bound on every orderbook call.
    timeout_ms: Option<u64>,
}

/// Register every venue the operator configured. Configuring none is not
/// an error: the keeper handshake then refuses any keeper that declares
/// `[venue] body_version`, which is the loud failure.
///
/// # Errors
///
/// Returns the parse, construction, or registration failure. A venue the
/// operator named but this root cannot open must stop the boot, because
/// the handshake would otherwise refuse its keepers with no cause named.
pub fn register(registry: &VenueRegistry, config: &EngineConfig) -> anyhow::Result<()> {
    let section = section(config)?;
    match &section.venues.cow {
        Some(venue) => register_cow(registry, venue).context("venue cow")?,
        None => warn!(
            "no [extensions.{NAMESPACE}.venues.{COW}] table: no venue is routable, and a \
             keeper declaring [venue] body_version refuses to boot",
        ),
    }
    Ok(())
}

/// The `[extensions.videre]` table, or the empty section when absent.
fn section(config: &EngineConfig) -> anyhow::Result<VidereSection> {
    let Some(value) = config.extensions.get(NAMESPACE) else {
        return Ok(VidereSection::default());
    };
    value
        .clone()
        .try_into()
        .with_context(|| format!("[extensions.{NAMESPACE}]"))
}

fn register_cow(registry: &VenueRegistry, venue: &CowVenue) -> anyhow::Result<()> {
    let mut config = CowConfig::new(venue.chain);
    if let Some(url) = &venue.orderbook_url {
        config = config.orderbook_url(Url::parse(url).context("orderbook-url")?);
    }
    if let Some(owner) = venue.owner {
        config = config.owner(owner);
    }
    if let Some(ms) = venue.timeout_ms {
        config = config.timeout(Duration::from_millis(ms));
    }
    let adapter = CowAdapter::new(config).context("orderbook http client")?;
    // The returned liveness flag is dropped: no path outside videre's own
    // tests marks a venue dead, so a wedged native venue cannot be
    // quarantined and the registry keeps routing to it. Holding the flag
    // here would not change that; the supervision it belongs to went with
    // the extension-installed component path.
    // The id is cow-venue's own `venue_id()`, so the registered id cannot
    // drift from the id `CowVenue::ID` routes to.
    cow_venue::register(registry, adapter)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexum_runtime::toml;

    fn config(table: &str) -> EngineConfig {
        let mut config = EngineConfig::default();
        config.extensions.insert(
            NAMESPACE.to_owned(),
            toml::from_str::<toml::Value>(table).expect("table parses"),
        );
        config
    }

    #[test]
    fn an_absent_section_yields_no_venue() {
        let section = section(&EngineConfig::default()).expect("absent is not an error");
        assert!(section.venues.cow.is_none());
    }

    #[test]
    fn the_cow_table_carries_the_chain_and_its_optional_overrides() {
        let section = section(&config(
            "[venues.cow]\nchain = 11155111\norderbook_url = \"http://localhost:9999\"\n",
        ))
        .expect("parses");
        let cow = section.venues.cow.expect("the cow venue");
        assert_eq!(cow.chain, cow_venue::Chain::Sepolia);
        assert_eq!(cow.orderbook_url.as_deref(), Some("http://localhost:9999"));
        assert!(cow.owner.is_none());
    }

    /// An orderbook slug reaches the same chain as its id, because the
    /// venue's own `Chain` decides.
    #[test]
    fn a_chain_slug_resolves_like_the_id() {
        let section = section(&config("[venues.cow]\nchain = \"sepolia\"\n")).expect("parses");
        let cow = section.venues.cow.expect("the cow venue");
        assert_eq!(cow.chain, cow_venue::Chain::Sepolia);
    }

    /// A venue id this binary does not link refuses at load rather than
    /// booting an engine that cannot route it.
    #[test]
    fn an_unlinked_venue_id_refuses() {
        let err = section(&config("[venues.uni]\nchain = 1\n")).expect_err("unknown venue");
        let chain = format!("{err:#}");
        assert!(chain.contains("uni"), "{chain}");
        assert!(chain.contains("unknown field"), "{chain}");
    }

    #[test]
    fn an_unsupported_chain_refuses_and_names_it() {
        let err = section(&config("[venues.cow]\nchain = 999\n")).expect_err("unsupported chain");
        assert!(format!("{err:#}").contains("999"), "{err:#}");
    }
}
