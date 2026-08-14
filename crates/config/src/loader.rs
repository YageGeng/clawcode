//! Loader that resolves an immutable [`AppConfig`] from figment sources.

use std::path::PathBuf;
use std::sync::Arc;

use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};

use crate::AppConfig;
use protocol::ProductIdentity;

/// Errors surfaced while constructing or loading configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// figment failed to merge or extract the config.
    /// Boxed because `figment::Error` is large (~200 bytes) and would bloat
    /// every `Result<_, ConfigError>` return.
    #[error("figment error: {0}")]
    Figment(#[from] Box<figment::Error>),
    /// IO failure while reading a config file.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// MCP config failed validation after TOML/env extraction.
    #[error("mcp config error: {0}")]
    Mcp(#[from] crate::mcp::McpConfigError),
    /// Cross-field application configuration failed validation.
    #[error("config validation error: {0}")]
    Validation(#[from] crate::config::ConfigValidationError),
    /// No configuration found at any default search path and no providers
    /// were set via `CLAW_*` environment variables.
    #[error("{0}")]
    NotFound(String),
}

/// Convenience: turn a bare `figment::Error` into our boxed variant via `?`.
impl From<figment::Error> for ConfigError {
    fn from(e: figment::Error) -> Self {
        ConfigError::Figment(Box::new(e))
    }
}

/// Cheaply clonable immutable configuration handle.
#[derive(Debug, Clone)]
pub struct ConfigHandle(Arc<AppConfig>);

impl ConfigHandle {
    /// Construct a handle wrapping the supplied config; primarily used by tests
    /// and by the figment-backed loaders defined later in this module.
    #[must_use]
    pub fn from_config(cfg: AppConfig) -> Self {
        Self(Arc::new(cfg))
    }

    /// Load a consistent snapshot of the active config.
    #[must_use]
    pub fn current(&self) -> Arc<AppConfig> {
        Arc::clone(&self.0)
    }
}

/// Load configuration by merging defaults, the provided files (in order), and
/// environment variables prefixed with `CLAW_`. Later sources override earlier.
///
/// Files that do not exist cause an error; this is intentional so callers know
/// which path resolution failed. To make a file optional, omit it from `paths`.
///
/// **Env layer limitation**: figment's `Env::split("__")` cannot index into
/// array-typed fields (e.g. `providers[N]`); numeric segments are treated
/// as map keys, not sequence indices. The env layer is therefore useful for
/// any future flat scalar fields but cannot override per-provider keys via env.
/// Provide arrays through TOML files instead.
pub fn load_from<P>(paths: P) -> Result<ConfigHandle, ConfigError>
where
    P: IntoIterator<Item = PathBuf>,
{
    let mut fig = Figment::from(Serialized::defaults(AppConfig::default()));
    for p in paths {
        // Reject missing files explicitly so misconfiguration surfaces early.
        if !p.exists() {
            return Err(ConfigError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("config file not found: {}", p.display()),
            )));
        }
        fig = fig.merge(Toml::file(p));
    }
    // Env keys: CLAW_PROVIDERS__0__API_KEY -> providers[0].api_key
    fig = fig
        .merge(Env::prefixed(ProductIdentity::CONFIG_ENV_PREFIX).split("__"));
    let cfg: AppConfig = fig.extract()?;
    cfg.validate()?;
    for server in &cfg.mcp_servers {
        server.validate()?;
    }
    Ok(ConfigHandle::from_config(cfg))
}

/// Resolve default configuration search paths in priority order:
///
/// 1. `$CLAW_CONFIG` if set and the path exists.
/// 2. `$XDG_CONFIG_HOME/clawcode/config.toml` (or `~/.config/clawcode/config.toml`) if it exists.
/// 3. `./claw.toml` in the current working directory if the user config does not exist.
///
/// Missing default candidates are silently skipped. Missing paths passed
/// directly to [`load_from`] still raise an error. The returned vec may be
/// empty, in which case [`load`] yields an `AppConfig::default()` handle.
fn default_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = std::env::var(ProductIdentity::CONFIG_PATH_ENV) {
        let path = PathBuf::from(p);
        if path.exists() {
            out.push(path);
            // An explicit config path should not be merged with implicit defaults.
            return out;
        }
    }
    if let Some(base) = dirs::config_dir() {
        let xdg = base
            .join(ProductIdentity::CONFIG_DIR_NAME)
            .join(ProductIdentity::USER_CONFIG_FILE_NAME);
        if xdg.exists() {
            out.push(xdg);
            return out;
        }
    }
    let cwd = PathBuf::from(ProductIdentity::CONFIG_FILE_NAME);
    if cwd.exists() {
        out.push(cwd);
    }
    out
}

/// Load configuration from the default search paths plus the `CLAW_` env layer.
///
/// When no config file is found at any default path and no providers are set via
/// environment variables, returns [`ConfigError::NotFound`] with a message
/// listing the searched locations.
pub fn load() -> Result<ConfigHandle, ConfigError> {
    let paths = default_paths();
    let no_files_found = paths.is_empty();
    if no_files_found {
        return Err(ConfigError::NotFound(format!(
            "no config file found.\n\n\
             Searched:\n  - ${} (not set)\n  - \
             $XDG_CONFIG_HOME/{}/{} (not found)\n  - \
             ./{} (not found)\n\n\
             Create a config file at one of these paths with at least one\n\
             [[providers]] section and active_model set. Example:\n\n  \
             active_model = \"deepseek/deepseek-v4-flash\"\n\n  \
             [[providers]]\n  id = \"deepseek\"\n  \
             provider_type = \"openai-completions\"\n  \
             base_url = \"https://api.deepseek.com\"\n  \
             api_key = \"your-api-key\"\n\n  [[providers.models]]\n  \
             id = \"deepseek-v4-flash\"\n  \
             context_tokens = 1000000\n  \
             max_output_tokens = 384000\n\n\
             Or set providers via {}* environment variables.",
            ProductIdentity::CONFIG_PATH_ENV,
            ProductIdentity::CONFIG_DIR_NAME,
            ProductIdentity::USER_CONFIG_FILE_NAME,
            ProductIdentity::CONFIG_FILE_NAME,
            ProductIdentity::CONFIG_ENV_PREFIX,
        )));
    }
    let handle = load_from(paths)?;
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `from_config` round-trips an AppConfig and snapshots are cheap clones.
    #[test]
    fn handle_returns_current_config() {
        let cfg = AppConfig::default();
        let handle = ConfigHandle::from_config(cfg);
        let snap_a = handle.current();
        let snap_b = handle.current();
        // Both snapshots must point at the same AppConfig data.
        assert!(Arc::ptr_eq(&snap_a, &snap_b));
    }
}
