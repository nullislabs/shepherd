use super::*;

fn engine_cfg(toml_src: &str) -> EngineConfig {
    toml::from_str(toml_src).expect("engine config parses")
}

fn mainnet() -> Chain {
    Chain::from_id(1)
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
fn absent_config_yields_no_overrides() {
    let cow = CowConfig::try_from(&EngineConfig::default()).expect("empty config parses");
    assert!(cow.orderbook_urls.is_empty());
}

#[test]
fn misspelled_extension_key_errors() {
    // deny_unknown_fields turns a typo inside the new table into a
    // boot-time error instead of a silent fall-through to the live
    // orderbook.
    let cfg = engine_cfg(
        r#"
[extensions.cow]
orderbook_url = "http://localhost:9999"
"#,
    );
    let err = CowConfig::try_from(&cfg).expect_err("unknown key in [extensions.cow] rejected");
    assert!(matches!(err, CowConfigError::Section(_)));
}
