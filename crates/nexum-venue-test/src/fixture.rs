//! The shared fixture-file plumbing: JSON on disk, byte fields as
//! lowercase hex, and the typed [`FixtureError`] both file formats
//! load and save through.

use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Why a fixture file failed to load or save. The JSON case carries
/// serde's rendered detail rather than the error value so the type
/// stays independent of `serde_json`'s feature set.
#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    /// The file could not be read.
    #[error("failed to read {path}: {source}")]
    Read {
        /// Path the read targeted.
        path: String,
        /// The underlying io failure.
        source: std::io::Error,
    },
    /// The file could not be written.
    #[error("failed to write {path}: {source}")]
    Write {
        /// Path the write targeted.
        path: String,
        /// The underlying io failure.
        source: std::io::Error,
    },
    /// The content did not parse as the fixture format.
    #[error("malformed fixture json: {0}")]
    Format(String),
}

/// Render a fixture as its canonical published form: pretty-printed
/// JSON with a trailing newline, so a regenerated file diffs cleanly.
pub(crate) fn to_json<T: Serialize>(value: &T) -> String {
    let mut json = serde_json::to_string_pretty(value).expect("fixture types serialize infallibly");
    json.push('\n');
    json
}

/// Parse a fixture from its JSON text.
pub(crate) fn from_json<T: DeserializeOwned>(json: &str) -> Result<T, FixtureError> {
    serde_json::from_str(json).map_err(|err| FixtureError::Format(err.to_string()))
}

/// Load a fixture file from disk.
pub(crate) fn load<T: DeserializeOwned>(path: &Path) -> Result<T, FixtureError> {
    let json = std::fs::read_to_string(path).map_err(|source| FixtureError::Read {
        path: path.display().to_string(),
        source,
    })?;
    from_json(&json)
}

/// Write a fixture file in its canonical published form.
pub(crate) fn write<T: Serialize>(path: &Path, value: &T) -> Result<(), FixtureError> {
    std::fs::write(path, to_json(value)).map_err(|source| FixtureError::Write {
        path: path.display().to_string(),
        source,
    })
}

/// Serde codec for byte fields: lowercase hex, no prefix, so the file
/// is legible without a borsh decoder.
pub(crate) mod hex_bytes {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(crate) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(bytes))
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        hex::decode(&text).map_err(D::Error::custom)
    }
}
