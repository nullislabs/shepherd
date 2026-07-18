//! `remote-store` backend: the Swarm network over a Bee node's HTTP
//! API. Uploads and feed updates are stamped with the configured
//! postage batch; feed updates are signed host-side with the
//! configured feed key.

use std::sync::Arc;

use bee::swarm::{BatchId, EthAddress, PrivateKey, Reference, Topic};

use crate::engine_config::RemoteStoreSection;

/// Canonical feed-update payload prefix: a big-endian unix timestamp.
const FEED_TIMESTAMP_LEN: usize = 8;

/// Boot-time `[remote_store]` validation failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RemoteStoreConfigError {
    /// The Bee API base URL failed to parse.
    #[error("remote-store api url: {0}")]
    Api(bee::Error),
    /// The postage batch id is not 32-byte hex.
    #[error("remote-store postage_batch: {0}")]
    PostageBatch(bee::Error),
    /// The feed key is not a 32-byte hex private key.
    #[error("remote-store feed_key: {0}")]
    FeedKey(bee::Error),
}

/// Runtime failures surfaced by [`RemoteStore`] operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RemoteStoreError {
    /// No `[remote_store]` table is configured.
    #[error("remote-store is not configured")]
    NotConfigured,
    /// The operation stamps chunks but no postage batch is configured.
    #[error("remote-store has no postage batch configured")]
    NoPostageBatch,
    /// `write-feed` needs a signing key and none is configured.
    #[error("remote-store has no feed key configured")]
    NoFeedKey,
    /// A guest-supplied value has the wrong shape.
    #[error("invalid {what}: {source}")]
    Input {
        /// Which argument was rejected.
        what: &'static str,
        /// The typed-byte constructor failure.
        source: bee::Error,
    },
    /// The referenced content did not resolve on the network.
    #[error("reference {0} not found")]
    NotFound(String),
    /// A feed update shorter than the timestamp prefix.
    #[error("malformed feed payload: {0} bytes")]
    MalformedFeed(usize),
    /// The Bee API refused or failed the request.
    #[error("bee api: {0}")]
    Api(bee::Error),
}

/// The configured Bee endpoint plus its write credentials.
struct Backend {
    client: bee::Client,
    batch: Option<BatchId>,
    feed_key: Option<PrivateKey>,
}

/// Shared remote-store handle threaded into every module store; cheap
/// to clone. Unconfigured handles report [`RemoteStoreError::NotConfigured`]
/// on every operation.
#[derive(Clone)]
pub struct RemoteStore(Option<Arc<Backend>>);

impl std::fmt::Debug for RemoteStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RemoteStore")
            .field(&self.0.as_ref().map(|_| "bee"))
            .finish()
    }
}

impl RemoteStore {
    /// A handle with no backend: every operation reports
    /// [`RemoteStoreError::NotConfigured`].
    pub fn disabled() -> Self {
        Self(None)
    }

    /// Open from the `[remote_store]` table; `None` yields a disabled
    /// handle.
    pub fn from_config(
        section: Option<&RemoteStoreSection>,
    ) -> Result<Self, RemoteStoreConfigError> {
        let Some(section) = section else {
            return Ok(Self::disabled());
        };
        let client = bee::Client::new(&section.api).map_err(RemoteStoreConfigError::Api)?;
        let batch = section
            .postage_batch
            .as_deref()
            .map(BatchId::from_hex)
            .transpose()
            .map_err(RemoteStoreConfigError::PostageBatch)?;
        let feed_key = section
            .feed_key
            .as_deref()
            .map(PrivateKey::from_hex)
            .transpose()
            .map_err(RemoteStoreConfigError::FeedKey)?;
        Ok(Self(Some(Arc::new(Backend {
            client,
            batch,
            feed_key,
        }))))
    }

    fn backend(&self) -> Result<&Backend, RemoteStoreError> {
        self.0.as_deref().ok_or(RemoteStoreError::NotConfigured)
    }

    /// Upload raw data; returns the 32-byte content reference.
    pub async fn upload(&self, data: Vec<u8>) -> Result<Vec<u8>, RemoteStoreError> {
        let backend = self.backend()?;
        let batch = backend
            .batch
            .as_ref()
            .ok_or(RemoteStoreError::NoPostageBatch)?;
        let result = backend
            .client
            .file()
            .upload_data(batch, data, None)
            .await
            .map_err(RemoteStoreError::Api)?;
        Ok(result.reference.to_vec())
    }

    /// Download raw data by content reference.
    pub async fn download(&self, reference: Vec<u8>) -> Result<Vec<u8>, RemoteStoreError> {
        let backend = self.backend()?;
        let reference = Reference::new(&reference).map_err(|source| RemoteStoreError::Input {
            what: "reference",
            source,
        })?;
        match backend.client.file().download_data(&reference, None).await {
            Ok(bytes) => Ok(bytes.to_vec()),
            Err(e) if e.status() == Some(404) => {
                Err(RemoteStoreError::NotFound(reference.to_hex()))
            }
            Err(e) => Err(RemoteStoreError::Api(e)),
        }
    }

    /// Latest value of the `(owner, topic)` feed, with the canonical
    /// timestamp prefix stripped. `Ok(None)` on a lookup miss (Bee
    /// reports a miss as 404 or 500).
    pub async fn read_feed(
        &self,
        owner: Vec<u8>,
        topic: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, RemoteStoreError> {
        let backend = self.backend()?;
        let owner = EthAddress::new(&owner).map_err(|source| RemoteStoreError::Input {
            what: "owner",
            source,
        })?;
        let topic = Topic::new(&topic).map_err(|source| RemoteStoreError::Input {
            what: "topic",
            source,
        })?;
        match backend
            .client
            .file()
            .fetch_latest_feed_update(&owner, &topic)
            .await
        {
            Ok(update) => match update.payload.get(FEED_TIMESTAMP_LEN..) {
                Some(data) => Ok(Some(data.to_vec())),
                None => Err(RemoteStoreError::MalformedFeed(update.payload.len())),
            },
            Err(e) if matches!(e.status(), Some(404 | 500)) => Ok(None),
            Err(e) => Err(RemoteStoreError::Api(e)),
        }
    }

    /// Publish `data` as the next update of the configured identity's
    /// `topic` feed; returns the update's chunk reference.
    pub async fn write_feed(
        &self,
        topic: Vec<u8>,
        data: Vec<u8>,
    ) -> Result<Vec<u8>, RemoteStoreError> {
        let backend = self.backend()?;
        let batch = backend
            .batch
            .as_ref()
            .ok_or(RemoteStoreError::NoPostageBatch)?;
        let key = backend
            .feed_key
            .as_ref()
            .ok_or(RemoteStoreError::NoFeedKey)?;
        let topic = Topic::new(&topic).map_err(|source| RemoteStoreError::Input {
            what: "topic",
            source,
        })?;
        let result = backend
            .client
            .file()
            .update_feed(batch, key, &topic, &data)
            .await
            .map_err(RemoteStoreError::Api)?;
        Ok(result.reference.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{header, method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const BATCH_HEX: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const REF_HEX: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const KEY_HEX: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    fn section(api: &str, batch: bool, key: bool) -> RemoteStoreSection {
        RemoteStoreSection {
            api: api.to_owned(),
            postage_batch: batch.then(|| BATCH_HEX.to_owned()),
            feed_key: key.then(|| KEY_HEX.to_owned()),
        }
    }

    fn store(api: &str, batch: bool, key: bool) -> RemoteStore {
        RemoteStore::from_config(Some(&section(api, batch, key))).expect("valid config")
    }

    #[tokio::test]
    async fn upload_stamps_and_returns_the_reference() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/bytes"))
            .and(header("swarm-postage-batch-id", BATCH_HEX))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "reference": REF_HEX })))
            .mount(&server)
            .await;

        let reference = store(&server.uri(), true, false)
            .upload(b"payload".to_vec())
            .await
            .expect("upload");
        assert_eq!(reference, [0xaa; 32]);
    }

    #[tokio::test]
    async fn upload_without_a_batch_is_refused() {
        let server = MockServer::start().await;
        let err = store(&server.uri(), false, false)
            .upload(b"payload".to_vec())
            .await
            .expect_err("no batch");
        assert!(matches!(err, RemoteStoreError::NoPostageBatch), "{err}");
    }

    #[tokio::test]
    async fn download_returns_the_bytes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/bytes/{REF_HEX}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"payload".to_vec()))
            .mount(&server)
            .await;

        let data = store(&server.uri(), false, false)
            .download(vec![0xaa; 32])
            .await
            .expect("download");
        assert_eq!(data, b"payload");
    }

    #[tokio::test]
    async fn download_miss_is_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let err = store(&server.uri(), false, false)
            .download(vec![0xaa; 32])
            .await
            .expect_err("missing reference");
        assert!(matches!(err, RemoteStoreError::NotFound(_)), "{err}");
    }

    #[tokio::test]
    async fn download_rejects_a_malformed_reference() {
        let server = MockServer::start().await;
        let err = store(&server.uri(), false, false)
            .download(vec![0xaa; 3])
            .await
            .expect_err("short reference");
        assert!(
            matches!(
                err,
                RemoteStoreError::Input {
                    what: "reference",
                    ..
                }
            ),
            "{err}"
        );
    }

    #[tokio::test]
    async fn read_feed_strips_the_timestamp_prefix() {
        let server = MockServer::start().await;
        let mut payload = 7_u64.to_be_bytes().to_vec();
        payload.extend_from_slice(b"latest");
        Mock::given(method("GET"))
            .and(path_regex("^/feeds/[0-9a-f]{40}/[0-9a-f]{64}$"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("swarm-feed-index", "0000000000000005")
                    .insert_header("swarm-feed-index-next", "0000000000000006")
                    .set_body_bytes(payload),
            )
            .mount(&server)
            .await;

        let value = store(&server.uri(), false, false)
            .read_feed(vec![0x11; 20], vec![0x22; 32])
            .await
            .expect("read feed");
        assert_eq!(value.as_deref(), Some(b"latest".as_slice()));
    }

    #[tokio::test]
    async fn read_feed_miss_is_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let value = store(&server.uri(), false, false)
            .read_feed(vec![0x11; 20], vec![0x22; 32])
            .await
            .expect("read feed");
        assert_eq!(value, None);
    }

    #[tokio::test]
    async fn write_feed_signs_the_next_update() {
        let server = MockServer::start().await;
        // No prior update: the writer starts at index 0.
        Mock::given(method("GET"))
            .and(path_regex("^/feeds/"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path_regex("^/soc/[0-9a-f]{40}/[0-9a-f]{64}$"))
            .and(header("swarm-postage-batch-id", BATCH_HEX))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "reference": REF_HEX })))
            .mount(&server)
            .await;

        let reference = store(&server.uri(), true, true)
            .write_feed(vec![0x22; 32], b"value".to_vec())
            .await
            .expect("write feed");
        assert_eq!(reference, [0xaa; 32]);
    }

    #[tokio::test]
    async fn write_feed_without_a_key_is_refused() {
        let server = MockServer::start().await;
        let err = store(&server.uri(), true, false)
            .write_feed(vec![0x22; 32], b"value".to_vec())
            .await
            .expect_err("no key");
        assert!(matches!(err, RemoteStoreError::NoFeedKey), "{err}");
    }

    #[tokio::test]
    async fn disabled_handle_reports_not_configured() {
        let err = RemoteStore::disabled()
            .upload(b"payload".to_vec())
            .await
            .expect_err("disabled");
        assert!(matches!(err, RemoteStoreError::NotConfigured), "{err}");
    }

    #[test]
    fn bad_config_fails_at_boot() {
        let mut bad = section("http://localhost:1633", true, false);
        bad.postage_batch = Some("nothex".to_owned());
        let err = RemoteStore::from_config(Some(&bad)).expect_err("bad batch");
        assert!(
            matches!(err, RemoteStoreConfigError::PostageBatch(_)),
            "{err}"
        );
    }
}
