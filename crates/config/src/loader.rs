//! Loader that resolves an [`AppConfig`] from figment sources and stores it
//! behind an [`ArcSwap`]-shared handle so future hot-reload can swap in place.

use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};

use crate::AppConfig;

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
}

/// Convenience: turn a bare `figment::Error` into our boxed variant via `?`.
impl From<figment::Error> for ConfigError {
    fn from(e: figment::Error) -> Self {
        ConfigError::Figment(Box::new(e))
    }
}

/// Shared handle holding the active [`AppConfig`] inside an [`ArcSwap`].
///
/// Readers always go through [`current`](Self::current) to obtain a consistent
/// snapshot. A future hot-reload path can call `self.0.store(Arc::new(new))`
/// without breaking the API.
#[derive(Debug, Clone)]
pub struct ConfigHandle(Arc<ArcSwap<AppConfig>>);

impl ConfigHandle {
    /// Construct a handle wrapping the supplied config; primarily used by tests
    /// and by the figment-backed loaders defined later in this module.
    #[must_use]
    pub fn from_config(cfg: AppConfig) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(cfg)))
    }

    /// Load a consistent snapshot of the active config.
    #[must_use]
    pub fn current(&self) -> Arc<AppConfig> {
        self.0.load_full()
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
    fig = fig.merge(Env::prefixed("CLAW_").split("__"));
    let cfg: AppConfig = fig.extract()?;
    Ok(ConfigHandle::from_config(cfg))
}

/// Resolve default configuration search paths in priority order:
///
/// 1. `$CLAW_CONFIG` if set and the path exists.
/// 2. `./claw.toml` in the current working directory if it exists.
/// 3. `$XDG_CONFIG_HOME/claw/config.toml` (or `~/.config/claw/config.toml`) if it exists.
///
/// Non-existent paths are silently skipped; only configured-but-missing files
/// raise errors via [`load_from`]. The returned vec may be empty, in which case
/// [`load`] yields an `AppConfig::default()` handle.
fn default_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = std::env::var("CLAW_CONFIG") {
        let path = PathBuf::from(p);
        if path.exists() {
            out.push(path);
        }
    }
    let cwd = PathBuf::from("./claw.toml");
    if cwd.exists() {
        out.push(cwd);
    }
    if let Some(base) = dirs::config_dir() {
        let xdg = base.join("claw").join("config.toml");
        if xdg.exists() {
            out.push(xdg);
        }
    }
    out
}

/// Load configuration from the default search paths plus the `CLAW_` env layer.
pub fn load() -> Result<ConfigHandle, ConfigError> {
    load_from(default_paths())
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
