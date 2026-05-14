//! OAuth token storage — file-based persistence for MCP auth credentials.

use async_trait::async_trait;
use rmcp::transport::auth::{AuthError, CredentialStore, StoredCredentials};
use std::io::Write;
use std::path::PathBuf;

/// File-based credential store for MCP OAuth tokens.
///
/// Tokens are stored as JSON at `<auth_dir>/<server_name>.json`.
/// On Unix, files are created with permissions `0o600` before writing secrets.
pub(crate) struct FileCredentialStore {
    path: PathBuf,
}

impl FileCredentialStore {
    /// Create a new file-based credential store.
    ///
    /// `auth_dir` is typically `~/.config/clawcode/auth/mcp`.
    pub(crate) fn new(auth_dir: &std::path::Path, server_name: &str) -> Self {
        let mut path = auth_dir.to_path_buf();
        path.push(auth_file_name(server_name));
        Self { path }
    }
}

/// Return the default MCP auth directory under the user config directory.
pub fn default_auth_dir() -> PathBuf {
    // Keep macOS and Linux behavior consistent by using the XDG-style config root.
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("clawcode")
        .join("auth")
        .join("mcp")
}

/// Convert a server name into a safe auth credential file name.
fn auth_file_name(server_name: &str) -> String {
    // Server names come from config, so reuse tool-name normalization to avoid path separators.
    let normalized = crate::tool::normalize_tool_name(server_name);
    let stem = if normalized.is_empty() {
        "server".to_string()
    } else {
        normalized
    };
    format!("{stem}.json")
}

#[async_trait]
impl CredentialStore for FileCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let data = match std::fs::read_to_string(&self.path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(AuthError::OAuthError(format!(
                    "failed to read {}: {e}",
                    self.path.display()
                )));
            }
        };
        let creds: StoredCredentials = serde_json::from_str(&data).map_err(|e| {
            AuthError::OAuthError(format!("failed to parse {}: {e}", self.path.display()))
        })?;
        Ok(Some(creds))
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AuthError::OAuthError(format!("failed to create dir {}: {e}", parent.display()))
            })?;
        }
        let data = serde_json::to_string_pretty(&credentials)
            .map_err(|e| AuthError::OAuthError(format!("failed to serialize credentials: {e}")))?;

        // Open the credential file directly while keeping Unix permissions restrictive.
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        let mut file = options.open(&self.path).map_err(|e| {
            AuthError::OAuthError(format!("failed to open {}: {e}", self.path.display()))
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600)).map_err(
                |e| {
                    AuthError::OAuthError(format!(
                        "failed to set permissions on {}: {e}",
                        self.path.display()
                    ))
                },
            )?;
        }

        file.write_all(data.as_bytes()).map_err(|e| {
            AuthError::OAuthError(format!("failed to write {}: {e}", self.path.display()))
        })?;
        file.sync_all().map_err(|e| {
            AuthError::OAuthError(format!("failed to sync {}: {e}", self.path.display()))
        })?;

        Ok(())
    }

    async fn clear(&self) -> Result<(), AuthError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AuthError::OAuthError(format!(
                "failed to remove {}: {e}",
                self.path.display()
            ))),
        }
    }
}
