//! Integration tests for the figment-backed loader.
//!
//! All tests use `figment::Jail` so env-var manipulation is serialized and
//! isolated; running multiple loader tests concurrently without Jail can let
//! one test's `CLAW_*` env mutations bleed into another test's load() call.

// Jail::expect_with's closure returns `figment::Error` (~208 bytes) which trips
// `clippy::result_large_err`; we can't change figment's API.

use config::{ApiKeyConfig, load_from};
use std::path::PathBuf;

/// Build a minimal provider config with a distinguishable API key.
fn provider_config(api_key: &str) -> String {
    format!(
        r#"
[[providers]]
id = "deepseek"
display_name = "DeepSeek"
base_url = "https://api.deepseek.com"
api_key = "{api_key}"
"#
    )
}

/// `load_from` reads a TOML file and populates AppConfig.providers.
#[test]
fn load_from_reads_toml_file() {
    #[allow(clippy::result_large_err)]
    figment::Jail::expect_with(|jail| {
        jail.create_file(
            "claw.toml",
            r#"
[[providers]]
id = "deepseek"
display_name = "DeepSeek"
base_url = "https://api.deepseek.com"
api_key = "sk-from-file"

[[providers.models]]
id = "deepseek-v4-flash"
"#,
        )?;
        let handle = load_from([PathBuf::from("claw.toml")]).unwrap();
        let cfg = handle.current();
        assert_eq!(cfg.providers.len(), 1);
        assert_eq!(
            cfg.providers[0].api_key,
            Some(ApiKeyConfig::Plaintext("sk-from-file".to_string()))
        );
        assert_eq!(cfg.providers[0].models[0].id, "deepseek-v4-flash");
        Ok(())
    });
}

/// Missing file returns Err rather than panicking.
#[test]
fn missing_file_yields_error() {
    #[allow(clippy::result_large_err)]
    figment::Jail::expect_with(|_jail| {
        let res = load_from([PathBuf::from("/nonexistent/claw.toml")]);
        res.unwrap_err();
        Ok(())
    });
}

/// `load` honors the CLAW_CONFIG env var when set.
#[test]
fn load_uses_claw_config_env_var() {
    #[allow(clippy::result_large_err)]
    figment::Jail::expect_with(|jail| {
        jail.create_file(
            "custom.toml",
            r#"
[[providers]]
id = "deepseek"
display_name = "DeepSeek"
base_url = "https://api.deepseek.com"
api_key = "sk-custom"
"#,
        )?;
        let abs = jail.directory().join("custom.toml");
        jail.set_env("CLAW_CONFIG", abs.to_str().unwrap());
        let handle = config::load().unwrap();
        let cfg = handle.current();
        assert_eq!(
            cfg.providers[0].api_key,
            Some(ApiKeyConfig::Plaintext("sk-custom".to_string()))
        );
        Ok(())
    });
}

/// `CLAW_CONFIG` remains the highest-priority explicit config path.
#[test]
fn load_prefers_claw_config_over_default_paths() {
    #[allow(clippy::result_large_err)]
    figment::Jail::expect_with(|jail| {
        let config_home = jail.directory().join(".config");
        let user_config_dir = config_home.join("clawcode");
        std::fs::create_dir_all(&user_config_dir).unwrap();
        std::fs::write(
            user_config_dir.join("config.toml"),
            provider_config("sk-from-xdg"),
        )
        .unwrap();
        jail.create_file("custom.toml", &provider_config("sk-from-explicit"))?;
        let explicit = jail.directory().join("custom.toml");
        jail.set_env("CLAW_CONFIG", explicit.to_str().unwrap());
        jail.set_env("XDG_CONFIG_HOME", config_home.to_str().unwrap());

        let handle = config::load().unwrap();
        let cfg = handle.current();

        assert_eq!(
            cfg.providers[0].api_key,
            Some(ApiKeyConfig::Plaintext("sk-from-explicit".to_string()))
        );
        Ok(())
    });
}

/// `load` prefers the XDG user config path before cwd fallbacks.
#[test]
fn load_prefers_xdg_clawcode_config() {
    #[allow(clippy::result_large_err)]
    figment::Jail::expect_with(|jail| {
        let config_home = jail.directory().join(".config");
        let user_config_dir = config_home.join("clawcode");
        std::fs::create_dir_all(&user_config_dir).unwrap();
        std::fs::write(
            user_config_dir.join("config.toml"),
            provider_config("sk-from-xdg"),
        )
        .unwrap();
        jail.create_file("claw.conf", &provider_config("sk-from-cwd"))?;
        jail.set_env("XDG_CONFIG_HOME", config_home.to_str().unwrap());

        let handle = config::load().unwrap();
        let cfg = handle.current();

        assert_eq!(
            cfg.providers[0].api_key,
            Some(ApiKeyConfig::Plaintext("sk-from-xdg".to_string()))
        );
        Ok(())
    });
}

/// `load` falls back to ./claw.conf when no user config exists.
#[test]
fn load_falls_back_to_cwd_claw_conf() {
    #[allow(clippy::result_large_err)]
    figment::Jail::expect_with(|jail| {
        let config_home = jail.directory().join(".config");
        std::fs::create_dir_all(&config_home).unwrap();
        jail.set_env("XDG_CONFIG_HOME", config_home.to_str().unwrap());
        jail.create_file("claw.conf", &provider_config("sk-from-fallback"))?;

        let handle = config::load().unwrap();
        let cfg = handle.current();

        assert_eq!(
            cfg.providers[0].api_key,
            Some(ApiKeyConfig::Plaintext("sk-from-fallback".to_string()))
        );
        Ok(())
    });
}
