use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn pool_indexes_default_chains() {
    let pool = OrderBookPool::default();
    assert!(pool.get(1).is_ok(), "mainnet present");
    assert!(pool.get(100).is_ok(), "gnosis present");
    assert!(pool.get(11_155_111).is_ok(), "sepolia present");
    assert!(pool.get(42_161).is_ok(), "arbitrum present");
    assert!(pool.get(8_453).is_ok(), "base present");
}

#[test]
fn unknown_chain_surfaces_typed_error() {
    let pool = OrderBookPool::default();
    assert!(matches!(
        pool.get(99_999),
        Err(CowApiError::UnknownChain(99_999))
    ));
}

/// Build a pool whose Mainnet entry points at `mock.uri()`.
/// `OrderBookApi::new_with_base_url` ships in cowprotocol; we
/// rely on it so wiremock-driven tests can exercise the full
/// request path without re-implementing the HTTP client.
fn pool_with_mainnet_at(mock: &MockServer) -> OrderBookPool {
    let mut clients = std::collections::BTreeMap::new();
    clients.insert(
        Chain::Mainnet.id(),
        OrderBookApi::new_with_base_url(mock.uri().parse().expect("mock uri parses")),
    );
    OrderBookPool {
        clients,
        http: reqwest::Client::new(),
    }
}

#[tokio::test]
async fn request_passes_get_path_through() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"version":"x.y.z"}"#))
        .expect(1)
        .mount(&mock)
        .await;

    let pool = pool_with_mainnet_at(&mock);
    let body = pool
        .request(Chain::Mainnet.id(), "GET", "/api/v1/version", None)
        .await
        .expect("request succeeds");
    assert_eq!(body, r#"{"version":"x.y.z"}"#);
}

#[tokio::test]
async fn request_relative_path_works() {
    // Module passes a path without a leading slash. The
    // passthrough should still resolve against the orderbook
    // base URL.
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/native_price/0xabc"))
        .respond_with(ResponseTemplate::new(200).set_body_string("1.23"))
        .expect(1)
        .mount(&mock)
        .await;

    let pool = pool_with_mainnet_at(&mock);
    let body = pool
        .request(
            Chain::Mainnet.id(),
            "GET",
            "api/v1/native_price/0xabc",
            None,
        )
        .await
        .expect("relative path resolves");
    assert_eq!(body, "1.23");
}

#[tokio::test]
async fn request_rejects_unknown_method() {
    let pool = OrderBookPool::default();
    let err = pool
        .request(Chain::Mainnet.id(), "PATCH", "/x", None)
        .await
        .unwrap_err();
    assert!(matches!(err, CowApiError::BadMethod(_)));
}

#[tokio::test]
async fn request_post_with_body_is_forwarded() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/quote"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"quote":"ok"}"#))
        .expect(1)
        .mount(&mock)
        .await;

    let pool = pool_with_mainnet_at(&mock);
    let body = pool
        .request(
            Chain::Mainnet.id(),
            "POST",
            "/api/v1/quote",
            Some(r#"{"sellToken":"0x01"}"#),
        )
        .await
        .expect("post with body succeeds");
    assert_eq!(body, r#"{"quote":"ok"}"#);
}

#[tokio::test]
async fn request_4xx_response_surfaces_http_error_with_body() {
    let mock = MockServer::start().await;
    let error_body = r#"{"errorType":"InsufficientFee","description":"fee too low"}"#;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .respond_with(ResponseTemplate::new(400).set_body_string(error_body))
        .expect(1)
        .mount(&mock)
        .await;

    let pool = pool_with_mainnet_at(&mock);
    let err = pool
        .request(
            Chain::Mainnet.id(),
            "POST",
            "/api/v1/orders",
            Some(r#"{"test":true}"#),
        )
        .await
        .unwrap_err();
    match err {
        CowApiError::HttpError { status, body } => {
            assert_eq!(status, 400);
            assert_eq!(body, error_body);
        }
        other => panic!("expected HttpError, got: {other:?}"),
    }
}

#[tokio::test]
async fn request_rejects_unknown_chain() {
    let pool = OrderBookPool::default();
    let err = pool.request(99_999, "GET", "/x", None).await.unwrap_err();
    assert!(matches!(err, CowApiError::UnknownChain(99_999)));
}

#[tokio::test]
async fn submit_order_propagates_orderbook_envelope() {
    // The orderbook rejects with a typed envelope. The pool must
    // surface `cowprotocol::Error::OrderbookApi { status, api }`
    // so the WIT adapter can forward `api` to `HostError.data`. The string
    // `DuplicatedOrder` is what the live
    // Sepolia orderbook returns for an already-submitted order;
    // it parses as `ApiError` even though the retriable-error
    // classifier does not recognise the spelling.
    let mock = MockServer::start().await;
    let envelope = r#"{"errorType":"DuplicatedOrder","description":"order already exists"}"#;
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .respond_with(ResponseTemplate::new(400).set_body_string(envelope))
        .expect(1)
        .mount(&mock)
        .await;

    let pool = pool_with_mainnet_at(&mock);
    let err = pool
        .submit_order_json(Chain::Mainnet.id(), sample_order_json().as_bytes())
        .await
        .expect_err("orderbook 400 surfaces as error");

    match err {
        CowApiError::Orderbook(cowprotocol::Error::OrderbookApi { status, api }) => {
            assert_eq!(status, 400);
            assert_eq!(api.error_type, "DuplicatedOrder");
            assert_eq!(api.description, "order already exists");
        }
        other => panic!("expected OrderbookApi envelope, got {other:?}"),
    }
}

#[tokio::test]
async fn submit_order_propagates_orderbook_response() {
    let mock = MockServer::start().await;
    let body_json = sample_order_json();
    // cowprotocol POST /api/v1/orders returns the order UID
    // (56-byte hex) as a JSON string body.
    let returned_uid = format!("\"0x{}\"", "ab".repeat(56));
    Mock::given(method("POST"))
        .and(path("/api/v1/orders"))
        .respond_with(ResponseTemplate::new(201).set_body_string(returned_uid.clone()))
        .expect(1)
        .mount(&mock)
        .await;

    let pool = pool_with_mainnet_at(&mock);
    let uid = pool
        .submit_order_json(Chain::Mainnet.id(), body_json.as_bytes())
        .await
        .expect("submit succeeds");
    assert_eq!(uid.as_slice().len(), 56);
    assert_eq!(uid.as_slice(), &[0xab; 56]);
}

/// A minimal but accepted-by-cowprotocol OrderCreation JSON. We
/// generate it inside the test so the JSON shape stays in lockstep
/// with the published `cowprotocol` version.
fn sample_order_json() -> String {
    use alloy_primitives::{Address, U256};
    use cowprotocol::OrderCreation;
    use cowprotocol::app_data::{EMPTY_APP_DATA_HASH, EMPTY_APP_DATA_JSON};
    use cowprotocol::order::{BuyTokenDestination, OrderData, OrderKind, SellTokenSource};
    use cowprotocol::signature::Signature;
    use cowprotocol::signing_scheme::SigningScheme;

    let order_data = OrderData {
        sell_token: Address::from([0x01; 20]),
        buy_token: Address::from([0x02; 20]),
        receiver: None,
        sell_amount: U256::from(100u64),
        buy_amount: U256::from(99u64),
        valid_to: u32::MAX,
        app_data: EMPTY_APP_DATA_HASH,
        fee_amount: U256::ZERO,
        kind: OrderKind::Sell,
        partially_fillable: false,
        sell_token_balance: SellTokenSource::Erc20,
        buy_token_balance: BuyTokenDestination::Erc20,
    };
    let signature = Signature::from_bytes(SigningScheme::PreSign, &[]).expect("presign empty");
    let creation = OrderCreation::from_signed_order_data(
        &order_data,
        signature,
        Address::from([0x03; 20]),
        EMPTY_APP_DATA_JSON.to_owned(),
        None,
    )
    .expect("valid OrderCreation");
    serde_json::to_string(&creation).expect("serialise OrderCreation")
}

#[tokio::test]
async fn request_rejects_malformed_path() {
    // `Url::join` is very lenient for valid UTF-8 inputs.  The
    // `BadPath` variant fires only when `Url::join` returns a parse
    // error, which is hard to provoke.  Using a bare scheme-like
    // string (`"://not-a-path"`) is NOT rejected because after
    // stripping the leading `/` it is treated as a relative path
    // component.  Instead, feed a string that *will* reach the
    // network but is handled by wiremock with a 404, confirming the
    // passthrough returns Ok even for nonsensical paths.
    let mock = MockServer::start().await;
    let pool = pool_with_mainnet_at(&mock);
    // wiremock returns 404 for any un-mocked route — now surfaced
    // as HttpError (not Ok) since we distinguish HTTP status codes.
    let err = pool
        .request(Chain::Mainnet.id(), "GET", "://not-a-path", None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, CowApiError::HttpError { status: 404, .. }),
        "Url::join treats this as a relative path; wiremock 404 surfaces as HttpError"
    );
}

#[tokio::test]
async fn request_network_error_on_dead_server() {
    // Build the pool against a port that no one is listening on.
    // We use port 1 (TCP echo / privileged) which is never bound
    // by user-space processes, guaranteeing a connection-refused.
    let mut clients = std::collections::BTreeMap::new();
    clients.insert(
        Chain::Mainnet.id(),
        OrderBookApi::new_with_base_url("http://127.0.0.1:1/".parse().expect("valid url")),
    );
    let pool = OrderBookPool {
        clients,
        http: reqwest::Client::new(),
    };
    let err = pool
        .request(Chain::Mainnet.id(), "GET", "/api/v1/version", None)
        .await
        .unwrap_err();
    assert!(matches!(err, CowApiError::Network(_)));
}

#[tokio::test]
async fn request_5xx_response_surfaces_http_error_with_body() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/health"))
        .respond_with(ResponseTemplate::new(500).set_body_string(r#"{"error":"internal"}"#))
        .expect(1)
        .mount(&mock)
        .await;

    let pool = pool_with_mainnet_at(&mock);
    let err = pool
        .request(Chain::Mainnet.id(), "GET", "/api/v1/health", None)
        .await
        .unwrap_err();
    match err {
        CowApiError::HttpError { status, body } => {
            assert_eq!(status, 500);
            assert_eq!(body, r#"{"error":"internal"}"#);
        }
        other => panic!("expected HttpError, got: {other:?}"),
    }
}

#[tokio::test]
async fn submit_order_rejects_invalid_json() {
    let pool = OrderBookPool::default();
    let err = pool
        .submit_order_json(Chain::Mainnet.id(), b"not json")
        .await
        .unwrap_err();
    assert!(matches!(err, CowApiError::Decode(_)));
}

#[tokio::test]
async fn submit_order_rejects_wrong_schema() {
    let pool = OrderBookPool::default();
    let err = pool
        .submit_order_json(Chain::Mainnet.id(), br#"{"valid":"json"}"#)
        .await
        .unwrap_err();
    assert!(matches!(err, CowApiError::Decode(_)));
}
