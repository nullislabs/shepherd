//! The shared fixture-file plumbing: JSON on disk, a leading
//! [`FormatVersion`] (unknown versions fail closed), byte fields as
//! lowercase hex, non-empty entry lists, and the typed
//! [`FixtureError`] both file formats load and save through.

use std::path::Path;

use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The one published fixture file-format version.
const FORMAT_VERSION: u32 = 1;

/// Fixture file-format discriminator: serializes as the current
/// version, refuses any other on parse (fail-closed), so a reader
/// never guesses at a future layout.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FormatVersion;

impl Serialize for FormatVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(FORMAT_VERSION)
    }
}

impl<'de> Deserialize<'de> for FormatVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let version = u32::deserialize(deserializer)?;
        if version == FORMAT_VERSION {
            Ok(Self)
        } else {
            Err(D::Error::custom(format!(
                "unknown fixture format version {version}; this reader speaks {FORMAT_VERSION}",
            )))
        }
    }
}

/// Deserialize a fixture's entry list, refusing an empty one: an empty
/// set would conform vacuously.
pub(crate) fn non_empty<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let entries = Vec::<T>::deserialize(deserializer)?;
    if entries.is_empty() {
        return Err(D::Error::custom("a published fixture set is never empty"));
    }
    Ok(entries)
}

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
