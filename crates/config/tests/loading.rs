//! Integration tests for the figment-backed loader.
//!
//! All tests use `figment::Jail` so env-var manipulation is serialized and
//! isolated; running multiple loader tests concurrently without Jail can let
//! one test's `CLAW_*` env mutations bleed into another test's load() call.

// Jail::expect_with's closure returns `figment::Error` (~208 bytes) which trips
// `clippy::result_large_err`; we can't change figment's API.

use config::load_from;
use std::path::PathBuf;

/// `load_from` reads a TOML file and populates AppConfig.providers.
#[test]
fn load_from_reads_toml_file() {
    #[allow(clippy::result_large_err)]
    figment::Jail::expect_with(|jail| {
        jail.create_file(
            "claw.toml",
            r#"
[[llm.providers]]
id = "deepseek"
display_name = "DeepSeek"
base_url = "https://api.deepseek.com"
api_key = "sk-from-file"

[[llm.providers.models]]
id = "deepseek-v4-flash"
"#,
        )?;
        let handle = load_from([PathBuf::from("claw.toml")]).unwrap();
        let cfg = handle.current();
        assert_eq!(cfg.llm.providers.len(), 1);
        assert_eq!(cfg.llm.providers[0].api_key, "sk-from-file");
        assert_eq!(cfg.llm.providers[0].models[0].id, "deepseek-v4-flash");
        Ok(())
    });
}

/// Missing file returns Err rather than panicking.
#[test]
fn missing_file_yields_error() {
    #[allow(clippy::result_large_err)]
    figment::Jail::expect_with(|_jail| {
        let res = load_from([PathBuf::from("/nonexistent/claw.toml")]);
        assert!(res.is_err());
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
[[llm.providers]]
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
        assert_eq!(cfg.llm.providers[0].api_key, "sk-custom");
        Ok(())
    });
}
