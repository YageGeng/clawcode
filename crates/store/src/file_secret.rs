//! Restricted-permission atomic file implementation of the Secret Store boundary.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::{SecretKey, SecretStore, SecretStoreError, SecretValue};

/// Current credential envelope revision independent from Session JSONL versions.
const SECRET_ENVELOPE_VERSION: u8 = 1;

/// On-disk envelope allowing explicit corruption and version checks.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecretEnvelope {
    version: u8,
    encoded_value: String,
}

/// Atomic file Secret Store rooted outside all Session transcript directories.
pub struct FileSecretStore {
    root: PathBuf,
}

impl FileSecretStore {
    /// Creates the credential directory and enforces owner-only access on Unix.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, SecretStoreError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Self::restrict_directory(&root)?;
        Ok(Self { root })
    }

    /// Applies owner-only directory permissions where Unix permission bits exist.
    #[cfg(unix)]
    fn restrict_directory(path: &Path) -> Result<(), SecretStoreError> {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    /// Leaves platform ACL enforcement to the operating system outside Unix.
    #[cfg(not(unix))]
    fn restrict_directory(_path: &Path) -> Result<(), SecretStoreError> {
        Ok(())
    }

    /// Applies owner-only file permissions before an atomic replacement becomes visible.
    #[cfg(unix)]
    fn restrict_file(file: &fs::File) -> Result<(), SecretStoreError> {
        use std::os::unix::fs::PermissionsExt;

        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    /// Leaves platform ACL enforcement to the operating system outside Unix.
    #[cfg(not(unix))]
    fn restrict_file(_file: &fs::File) -> Result<(), SecretStoreError> {
        Ok(())
    }
}

impl SecretStore for FileSecretStore {
    /// Loads and validates one versioned base64 credential envelope.
    fn load(
        &self,
        key: &SecretKey,
    ) -> Result<Option<SecretValue>, SecretStoreError> {
        let path = self.root.join(key.file_name());
        let content = match fs::read(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(SecretStoreError::Io(error)),
        };
        let envelope: SecretEnvelope = serde_json::from_slice(&content)
            .map_err(|error| SecretStoreError::Corrupt(error.to_string()))?;
        if envelope.version != SECRET_ENVELOPE_VERSION {
            return Err(SecretStoreError::Corrupt(format!(
                "unsupported envelope version {}",
                envelope.version
            )));
        }
        let value = base64::engine::general_purpose::STANDARD
            .decode(envelope.encoded_value)
            .map_err(|error| SecretStoreError::Corrupt(error.to_string()))?;
        SecretValue::new(value)
            .map(Some)
            .map_err(|error| SecretStoreError::Corrupt(error.to_string()))
    }

    /// Writes and synchronizes a restricted temporary file before atomic replacement.
    fn store(
        &self,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretStoreError> {
        let envelope = SecretEnvelope {
            version: SECRET_ENVELOPE_VERSION,
            encoded_value: base64::engine::general_purpose::STANDARD
                .encode(value.expose()),
        };
        let content = serde_json::to_vec(&envelope)
            .map_err(|error| SecretStoreError::Corrupt(error.to_string()))?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.root)?;
        Self::restrict_file(temporary.as_file())?;
        temporary.write_all(&content)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.root.join(key.file_name()))
            .map_err(|error| SecretStoreError::Io(error.error))?;
        fs::File::open(&self.root)?.sync_all()?;
        Ok(())
    }

    /// Removes one credential without failing when it is already absent.
    fn delete(&self, key: &SecretKey) -> Result<(), SecretStoreError> {
        match fs::remove_file(self.root.join(key.file_name())) {
            Ok(()) => {
                fs::File::open(&self.root)?.sync_all()?;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(())
            }
            Err(error) => Err(SecretStoreError::Io(error)),
        }
    }
}

impl std::fmt::Debug for FileSecretStore {
    /// Reports the credential root without enumerating keys or values.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileSecretStore")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}
