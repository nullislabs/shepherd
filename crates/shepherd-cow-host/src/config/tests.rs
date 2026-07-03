use super::*;

fn engine_cfg(toml_src: &str) -> EngineConfig {
    toml::from_str(toml_src).expect("engine config parses")
}

fn mainnet() -> Chain {
    Chain::from_id(1)
}

#[test]
fn legacy_chain_key_still_resolves() {
    // The pre-extension location must keep working for existing
    // deployments.
    let cfg = engine_cfg(
        r#"
[chains.1]
rpc_url = "wss://example.test/mainnet"
orderbook_url = "http://localhost:9999"
"#,
    );
    let cow = CowConfig::try_from(&cfg).expect("legacy location parses");
    assert_eq!(
        cow.orderbook_urls.get(&mainnet()).map(String::as_str),
        Some("http://localhost:9999"),
    );
}

#[test]
fn extension_table_resolves() {
    let cfg = engine_cfg(
        r#"
[extensions.cow.orderbook_urls]
1 = "http://localhost:8888"
"#,
    );
    let cow = CowConfig::try_from(&cfg).expect("extension table parses");
    assert_eq!(
        cow.orderbook_urls.get(&mainnet()).map(String::as_str),
        Some("http://localhost:8888"),
    );
}

#[test]
fn extension_table_wins_over_legacy_key() {
    let cfg = engine_cfg(
        r#"
[chains.1]
rpc_url = "wss://example.test/mainnet"
orderbook_url = "http://localhost:9999"

[extensions.cow.orderbook_urls]
1 = "http://localhost:8888"
"#,
    );
    let cow = CowConfig::try_from(&cfg).expect("both locations parse");
    assert_eq!(
        cow.orderbook_urls.get(&mainnet()).map(String::as_str),
        Some("http://localhost:8888"),
        "the extension-owned table takes precedence",
    );
}

#[test]
fn absent_config_yields_no_overrides() {
    let cow = CowConfig::try_from(&EngineConfig::default()).expect("empty config parses");
    assert!(cow.orderbook_urls.is_empty());
}

#[test]
fn misspelled_extension_key_errors() {
    // The old field name inside the new table is the realistic typo;
    // deny_unknown_fields turns it into a boot-time error instead of a
    // silent fall-through to the live orderbook.
    let cfg = engine_cfg(
        r#"
[extensions.cow]
orderbook_url = "http://localhost:9999"
"#,
    );
    let err = CowConfig::try_from(&cfg).expect_err("unknown key in [extensions.cow] rejected");
    assert!(matches!(err, CowConfigError::Section(_)));
}

#[test]
fn legacy_non_string_value_errors() {
    let cfg = engine_cfg(
        r#"
[chains.1]
rpc_url = "wss://example.test/mainnet"
orderbook_url = 5
"#,
    );
    let err = CowConfig::try_from(&cfg).expect_err("non-string legacy value rejected");
    assert!(matches!(err, CowConfigError::LegacyType { chain_id: 1 }));
}

/// `MakeWriter` capturing formatted log lines for assertion.
#[derive(Clone, Default)]
struct CaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("writer lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

#[test]
fn legacy_chain_key_warns_deprecation() {
    let writer = CaptureWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(writer.clone())
        .with_ansi(false)
        .finish();
    let cfg = engine_cfg(
        r#"
[chains.1]
rpc_url = "wss://example.test/mainnet"
orderbook_url = "http://localhost:9999"
"#,
    );
    let cow = tracing::subscriber::with_default(subscriber, || CowConfig::try_from(&cfg))
        .expect("legacy location parses");
    assert!(cow.orderbook_urls.contains_key(&mainnet()));
    let logs = String::from_utf8(writer.0.lock().expect("writer lock").clone()).expect("utf8");
    assert!(
        logs.contains("WARN") && logs.contains("deprecated"),
        "deprecation warning emitted, got: {logs}",
    );
}
