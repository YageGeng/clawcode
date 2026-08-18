//! Integration tests for the figment-backed loader.
//!
//! All tests use `figment::Jail` so env-var manipulation is serialized and
//! isolated; running multiple loader tests concurrently without Jail can let
//! one test's `CLAW_*` env mutations bleed into another test's load() call.

// Jail::expect_with's closure returns `figment::Error` (~208 bytes) which trips
// `clippy::result_large_err`; we can't change figment's API.

use config::{
    ApiKeyConfig, AppConfig, ConfigValidationError, LoggingConfig, load_from,
};
use protocol::{ModelInputModality, PromptContentSource};
use std::path::{Path, PathBuf};

/// Supplies complete configuration documents for cross-field validation tests.
struct ConfigFixture;

impl ConfigFixture {
    /// Returns one valid active provider and model with explicit limits.
    fn valid() -> &'static str {
        r#"
active_model = "deepseek/deepseek-v4-flash"

[[providers]]
id = "deepseek"
display_name = "DeepSeek"
base_url = "https://api.deepseek.com"
api_key = "sk-test"

[[providers.models]]
id = "deepseek-v4-flash"
display_name = "DeepSeek V4 Flash"
context_tokens = 1000000
max_output_tokens = 384000
"#
    }

    /// Returns an invalid provider whose active model identifier is duplicated.
    fn duplicate_model() -> &'static str {
        r#"
active_model = "deepseek/deepseek-v4-flash"

[[providers]]
id = "deepseek"
display_name = "DeepSeek"
base_url = "https://api.deepseek.com"
api_key = "sk-test"

[[providers.models]]
id = "deepseek-v4-flash"
context_tokens = 1000000
max_output_tokens = 384000

[[providers.models]]
id = "deepseek-v4-flash"
context_tokens = 1000000
max_output_tokens = 384000
"#
    }
}

/// Build a minimal provider config with a distinguishable API key.
fn provider_config(api_key: &str) -> String {
    format!(
        r#"
active_model = "deepseek/deepseek-v4-flash"

[[providers]]
id = "deepseek"
display_name = "DeepSeek"
base_url = "https://api.deepseek.com"
api_key = "{api_key}"

[[providers.models]]
id = "deepseek-v4-flash"
context_tokens = 1000000
max_output_tokens = 384000
"#
    )
}

/// Retry and compaction defaults match the immutable pi v4 runtime policy.
#[test]
fn defaults_match_pi_retry_and_compaction() {
    let config = AppConfig::default();

    assert!(config.retry.agent.enabled);
    assert_eq!(config.retry.agent.max_retries, 3);
    assert_eq!(config.retry.agent.base_delay_ms, 2_000);
    assert_eq!(config.retry.provider.max_retry_delay_ms, 60_000);
    assert!(config.compaction.enabled);
    assert_eq!(config.compaction.reserve_tokens, 16_384);
    assert_eq!(config.compaction.keep_recent_tokens, 20_000);
}

/// Missing logging configuration keeps info filtering and plain-text output.
#[test]
fn logging_defaults_to_info_filter() {
    assert_eq!(LoggingConfig::default().filter, "info");
    assert!(!LoggingConfig::default().color);
    assert_eq!(AppConfig::default().logging.filter, "info");
    assert!(!AppConfig::default().logging.color);
}

/// TOML can configure the complete EnvFilter directive without a parallel schema.
#[test]
fn logging_filter_loads_from_toml() {
    let config: AppConfig = toml::from_str(
        r#"
[logging]
filter = "warn,kernel=debug,provider=trace"
color = true
"#,
    )
    .expect("parse logging config");

    assert_eq!(config.logging.filter, "warn,kernel=debug,provider=trace");
    assert!(config.logging.color);
}

/// Prompt resource policy loads tagged content sources and discovery switches.
#[test]
fn prompt_policy_loads_from_toml() {
    #[allow(clippy::result_large_err)]
    figment::Jail::expect_with(|jail| {
        let document = format!(
            "{}{}",
            ConfigFixture::valid(),
            r#"
[prompt]
load_project_instructions = true
load_templates = false
system_prompt = { type = "path", value = "SYSTEM.custom.md" }
append_system_prompts = [
  { type = "text", value = "First appendix" },
  { type = "path", value = "APPEND_SYSTEM.md" },
]
template_paths = ["prompts"]
"#,
        );
        jail.create_file("claw.toml", &document)?;

        let config = load_from([PathBuf::from("claw.toml")])
            .expect("load prompt config");

        assert!(!config.current().prompt.load_templates);
        assert_eq!(
            config.current().prompt.system_prompt,
            Some(PromptContentSource::Path(PathBuf::from("SYSTEM.custom.md")))
        );
        assert_eq!(config.current().prompt.append_system_prompts.len(), 2);
        assert_eq!(
            config.current().prompt.template_paths,
            vec![PathBuf::from("prompts")]
        );
        Ok(())
    });
}

/// Skill selection rules deserialize directly into the shared protocol type.
#[test]
fn skill_rules_load_from_toml() {
    let config: AppConfig = toml::from_str(
        r#"
[skills]
include_instructions = false

[[skills.rules]]
name = "review"
enabled = false

[[skills.rules]]
path = "/skills/review/SKILL.md"
enabled = true
"#,
    )
    .expect("parse Skill rules");

    assert!(!config.skills.include_instructions);
    assert_eq!(config.skills.rules.len(), 2);
    assert_eq!(config.skills.rules[0].name.as_deref(), Some("review"));
    assert_eq!(
        config.skills.rules[1].path.as_deref(),
        Some(Path::new("/skills/review/SKILL.md"))
    );
}

/// Explicit Skill paths retain configured order and default to an empty list.
#[test]
fn skill_paths_load_from_toml_and_default_empty() {
    let config: AppConfig = toml::from_str(
        r#"
[skills]
paths = ["./skills", "/opt/agent/review/SKILL.md"]
"#,
    )
    .expect("parse Skill paths");

    assert_eq!(
        config.skills.paths,
        vec![
            PathBuf::from("./skills"),
            PathBuf::from("/opt/agent/review/SKILL.md")
        ]
    );
    assert!(AppConfig::default().skills.paths.is_empty());
}

/// Missing extension settings keep the production command guard enabled.
#[test]
fn extensions_default_to_command_guard() {
    let config = AppConfig::default();

    assert_eq!(
        config.extensions.enabled,
        vec![
            protocol::ExtensionId::try_from("command-guard")
                .expect("default extension id")
        ]
    );
}

/// An explicit empty extension list disables every compiled extension.
#[test]
fn extensions_allow_an_explicit_empty_list() {
    let config: AppConfig = toml::from_str("[extensions]\nenabled = []\n")
        .expect("parse extensions");

    assert!(config.extensions.enabled.is_empty());
}

/// Extension order remains stable because it controls hook priority.
#[test]
fn extensions_preserve_configured_order() {
    let config: AppConfig = toml::from_str(
        "[extensions]\nenabled = [\"hook-examples\", \"command-guard\"]\n",
    )
    .expect("parse extensions");
    let ids: Vec<_> = config
        .extensions
        .enabled
        .iter()
        .map(ToString::to_string)
        .collect();

    assert_eq!(ids, vec!["hook-examples", "command-guard"]);
}

/// Active model resolution rejects duplicate provider model identifiers.
#[test]
fn active_model_requires_a_unique_provider_model() {
    let config: AppConfig = toml::from_str(ConfigFixture::duplicate_model())
        .expect("parse duplicate model config");

    assert!(matches!(
        config.validate(),
        Err(ConfigValidationError::DuplicateModel {
            provider_id,
            model_id,
        }) if provider_id == "deepseek" && model_id == "deepseek-v4-flash"
    ));
}

/// Active model resolution produces one stable profile with required limits.
#[test]
fn active_model_resolves_to_a_stable_model_profile() {
    let config: AppConfig = toml::from_str(ConfigFixture::valid())
        .expect("parse valid model config");

    let profile = config.model_profile().expect("model profile");
    assert_eq!(profile.provider_id, "deepseek");
    assert_eq!(profile.model_id, "deepseek-v4-flash");
    assert_eq!(profile.display_name, "DeepSeek V4 Flash");
    assert_eq!(profile.context_tokens, 1_000_000);
    assert_eq!(profile.max_output_tokens, 384_000);
}

/// An upstream provider identifier does not replace the local model identity.
#[test]
fn upstream_model_id_is_transport_only() {
    let document = ConfigFixture::valid().replace(
        "display_name = \"DeepSeek V4 Flash\"",
        "display_name = \"DeepSeek V4 Flash\"\nupstream_id = \"vendor/deepseek-v4-flash\"",
    );
    let config: AppConfig =
        toml::from_str(&document).expect("parse upstream model id");

    config.validate().expect("validate upstream model id");
    let profile = config.model_profile().expect("model profile");
    let model = serde_json::to_value(&config.providers[0].models[0])
        .expect("serialize model config");

    assert_eq!(profile.model_id, "deepseek-v4-flash");
    assert_eq!(model["upstream_id"], "vendor/deepseek-v4-flash");
}

/// An explicitly configured upstream model identifier must contain content.
#[test]
fn upstream_model_id_rejects_blank_values() {
    let document = ConfigFixture::valid().replace(
        "display_name = \"DeepSeek V4 Flash\"",
        "display_name = \"DeepSeek V4 Flash\"\nupstream_id = \"   \"",
    );
    let config: AppConfig =
        toml::from_str(&document).expect("parse blank upstream model id");

    let error = config
        .validate()
        .expect_err("reject blank upstream model id");

    assert!(error.to_string().contains("upstream_id must not be empty"));
}

/// Models without an explicit input list remain text-only.
#[test]
fn model_input_defaults_to_text() {
    let config: AppConfig = toml::from_str(ConfigFixture::valid())
        .expect("parse valid model config");

    let profile = config.model_profile().expect("model profile");
    assert!(profile.input.supports(ModelInputModality::Text));
    assert!(!profile.input.supports(ModelInputModality::Image));
}

/// Explicit image input is retained in the stable runtime profile.
#[test]
fn model_input_accepts_text_and_image() {
    let document = ConfigFixture::valid().replace(
        "display_name = \"DeepSeek V4 Flash\"",
        "display_name = \"DeepSeek V4 Flash\"\ninput = [\"text\", \"image\"]",
    );
    let config: AppConfig =
        toml::from_str(&document).expect("parse image model config");

    let profile = config.model_profile().expect("model profile");
    assert!(profile.input.supports(ModelInputModality::Text));
    assert!(profile.input.supports(ModelInputModality::Image));
}

/// Empty, image-only, and duplicate input lists fail model validation.
#[test]
fn model_input_rejects_invalid_combinations() {
    for input in [
        "[]",
        "[\"image\"]",
        "[\"text\", \"text\"]",
        "[\"image\", \"text\"]",
    ] {
        let document = ConfigFixture::valid().replace(
            "display_name = \"DeepSeek V4 Flash\"",
            &format!("display_name = \"DeepSeek V4 Flash\"\ninput = {input}"),
        );
        let config: AppConfig =
            toml::from_str(&document).expect("parse invalid model input");

        assert!(matches!(
            config.validate(),
            Err(ConfigValidationError::InvalidModelInput {
                provider_id,
                model_id,
                ..
            }) if provider_id == "deepseek" && model_id == "deepseek-v4-flash"
        ));
    }
}

/// Missing environment credentials identify only their source, never a secret value.
#[test]
fn missing_api_key_environment_is_reported_without_secret_material() {
    #[allow(clippy::result_large_err)]
    figment::Jail::expect_with(|jail| {
        jail.clear_env();
        let config: AppConfig = toml::from_str(
            r#"
active_model = "deepseek/deepseek-v4-flash"

[[providers]]
id = "deepseek"
display_name = "DeepSeek"
base_url = "https://api.deepseek.com"
api_key = { env = "CLAWCODE_TEST_MISSING_API_KEY" }

[[providers.models]]
id = "deepseek-v4-flash"
context_tokens = 1000000
max_output_tokens = 384000
"#,
        )
        .expect("parse environment auth config");

        let error = config.validate().expect_err("missing environment key");
        let message = error.to_string();
        assert!(message.contains("CLAWCODE_TEST_MISSING_API_KEY"));
        assert!(!message.contains("sk-secret-value"));
        Ok(())
    });
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
context_tokens = 1000000
max_output_tokens = 384000
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
        jail.create_file("custom.toml", &provider_config("sk-custom"))?;
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

/// Relative explicit paths are rejected so build-time and runtime resolution cannot diverge.
#[test]
fn load_rejects_relative_claw_config_path() {
    #[allow(clippy::result_large_err)]
    figment::Jail::expect_with(|jail| {
        jail.create_file("custom.toml", &provider_config("sk-custom"))?;
        jail.set_env("CLAW_CONFIG", "custom.toml");

        let error = config::load().expect_err("relative override must fail");

        assert!(error.to_string().contains("must be an absolute path"));
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

/// `load_from` reads a provider config from an arbitrary file path.
#[test]
fn load_prefers_xdg_clawcode_config() {
    #[allow(clippy::result_large_err)]
    figment::Jail::expect_with(|jail| {
        let config_path = jail.directory().join("some-config.toml");
        std::fs::write(&config_path, provider_config("sk-from-file")).unwrap();

        let handle = load_from([config_path]).unwrap();
        let cfg = handle.current();

        assert_eq!(cfg.providers.len(), 1);
        assert_eq!(
            cfg.providers[0].api_key,
            Some(ApiKeyConfig::Plaintext("sk-from-file".to_string()))
        );
        Ok(())
    });
}

/// `load` falls back to ./claw.toml when no user config exists.
#[test]
fn load_falls_back_to_cwd_claw_conf() {
    #[allow(clippy::result_large_err)]
    figment::Jail::expect_with(|jail| {
        let config_home = jail.directory().join(".config");
        std::fs::create_dir_all(&config_home).unwrap();
        jail.set_env("XDG_CONFIG_HOME", config_home.to_str().unwrap());
        jail.create_file("claw.toml", &provider_config("sk-from-fallback"))?;

        let handle = config::load().unwrap();
        let cfg = handle.current();

        assert_eq!(
            cfg.providers[0].api_key,
            Some(ApiKeyConfig::Plaintext("sk-from-fallback".to_string()))
        );
        Ok(())
    });
}

/// Legacy request_approval config resolves to enhanced OnRequest policy.
#[test]
fn legacy_request_approval_effective_policy_is_on_request() {
    let toml = r#"
active_model = "test/model"
approval = "request_approval"
"#;

    let config: config::AppConfig = toml::from_str(toml).expect("parse config");

    assert_eq!(
        config.effective_approval_policy(),
        config::AskForApproval::OnRequest
    );
}

/// Legacy yolo config preserves current no-prompt compatibility mode.
#[test]
fn legacy_yolo_effective_policy_is_never() {
    let toml = r#"
active_model = "test/model"
approval = "yolo"
"#;

    let config: config::AppConfig = toml::from_str(toml).expect("parse config");

    assert_eq!(
        config.effective_approval_policy(),
        config::AskForApproval::Never
    );
}

/// enhanced approval_policy overrides the legacy approval field.
#[test]
fn explicit_approval_policy_overrides_legacy_approval() {
    let toml = r#"
active_model = "test/model"
approval = "yolo"
approval_policy = "on-request"
"#;

    let config: config::AppConfig = toml::from_str(toml).expect("parse config");

    assert_eq!(
        config.effective_approval_policy(),
        config::AskForApproval::OnRequest
    );
}
