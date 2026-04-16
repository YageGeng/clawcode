mod runtime;

use std::sync::Arc;
use std::{env, io};

use kernel::{
    model::LlmAgentModel,
    session::InMemorySessionStore,
    tools::{
        builtin::{default_file_tools, default_read_only_tools},
        registry::ToolRegistry,
    },
};
use llm::providers::openai;
use llm::providers::openai::{OpenAiCodexConfig, OpenAiCodexSessionManager, build_codex_headers};
use tracing::info;
use tracing_subscriber::EnvFilter;

const DEFAULT_MODEL: &str = "gpt-5.4";

/// Supported authentication modes for the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthMode {
    /// Standard OpenAI API-key authentication.
    ApiKey,
    /// ChatGPT/Codex bearer-token authentication with optional device login.
    Codex,
}

/// Fully-resolved CLI configuration after environment parsing.
#[derive(Debug, Clone)]
struct CliConfig {
    /// Selected authentication mode.
    auth_mode: AuthMode,
    /// Final base URL used to build the client.
    base_url: String,
    /// Bearer token or API key passed into the HTTP client.
    api_key: String,
    /// Model name used for the request.
    model_name: String,
    /// Extra headers required by the chosen auth mode.
    headers: llm::http_client::HeaderMap,
}

/// Reads a required environment variable for the CLI adapter.
fn require_env(name: &str) -> Result<String, io::Error> {
    env::var(name)
        .map_err(|_| io::Error::other(format!("missing {name}; set it before running cli")))
}

/// Reads an optional environment variable and drops empty strings.
fn optional_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

/// Prints the CLI usage string expected by the integration test.
fn usage_message() -> &'static str {
    "usage: cargo run -p cli -- \"your prompt\""
}

/// Parses the selected auth mode from environment variables.
fn resolve_auth_mode() -> Result<AuthMode, io::Error> {
    match optional_env("OPENAI_AUTH_MODE")
        .unwrap_or_else(|| "api_key".to_string())
        .to_ascii_lowercase()
        .as_str()
    {
        "api_key" => Ok(AuthMode::ApiKey),
        "codex" => Ok(AuthMode::Codex),
        other => Err(io::Error::other(format!(
            "invalid OPENAI_AUTH_MODE `{other}`; expected `api_key` or `codex`"
        ))),
    }
}

/// Resolves the final CLI configuration, including optional Codex device login.
async fn resolve_cli_config() -> Result<CliConfig, Box<dyn std::error::Error>> {
    match resolve_auth_mode()? {
        AuthMode::ApiKey => Ok(CliConfig {
            auth_mode: AuthMode::ApiKey,
            base_url: optional_env("OPENAI_BASE_URL")
                .unwrap_or_else(|| openai::OPENAI_API_BASE_URL.to_string()),
            api_key: require_env("OPENAI_API_KEY")?,
            model_name: optional_env("OPENAI_MODEL").unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            headers: Default::default(),
        }),
        AuthMode::Codex => {
            let codex_config = OpenAiCodexConfig::default();
            let api_key = if let Some(token) = optional_env("OPENAI_API_KEY") {
                token
            } else {
                let session_manager = OpenAiCodexSessionManager::new(codex_config.clone())?;
                session_manager.get_access_token().await?
            };
            let headers = build_codex_headers(&api_key)?;

            Ok(CliConfig {
                auth_mode: AuthMode::Codex,
                base_url: optional_env("OPENAI_BASE_URL")
                    .unwrap_or_else(|| codex_config.api_base_url.clone()),
                api_key,
                model_name: optional_env("OPENAI_MODEL")
                    .unwrap_or_else(|| codex_config.model.clone()),
                headers,
            })
        }
    }
}

/// Initializes tracing output for the CLI when `RUST_LOG` is set.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("off"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .with_writer(std::io::stderr)
        .try_init();
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let prompt = env::args().skip(1).collect::<Vec<_>>().join(" ");
    if prompt.trim().is_empty() {
        info!("cli invoked without prompt");
        eprintln!("{}", usage_message());
        std::process::exit(2);
    }

    let config = resolve_cli_config().await?;

    info!(
        base_url = %config.base_url,
        model = %config.model_name,
        auth_mode = ?config.auth_mode,
        prompt = %runtime::prompt_preview(&prompt),
        "starting cli request"
    );

    let client = openai::Client::builder()
        .base_url(config.base_url)
        .http_headers(config.headers)
        .api_key(config.api_key)
        .build()?;
    let llm_model =
        openai::responses_api::ResponsesCompletionModel::with_model(client, &config.model_name);
    let model = Arc::new(LlmAgentModel::new(llm_model));
    let store = Arc::new(InMemorySessionStore::default());
    let registry = Arc::new(ToolRegistry::default());

    // Register read-only and filesystem tools for the CLI model session.
    for tool in default_read_only_tools() {
        registry.register_arc(tool).await;
    }
    for tool in default_file_tools() {
        registry.register_arc(tool).await;
    }

    info!(
        tool_count = registry.definitions().await.len(),
        "registered read-only and file tools"
    );

    let result = runtime::run_cli_prompt(model, store, registry, prompt).await?;

    println!("{result}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// Lock used to serialize tests that mutate environment variables.
    static ENV_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();

    /// Temporarily sets a single environment variable and restores its previous value when dropped.
    struct EnvVarGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvVarGuard {
        /// Sets `key` to `value`, or removes it when `value` is `None`.
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let original = env::var(key).ok();

            match value {
                Some(value) => unsafe { env::set_var(key, value) },
                None => unsafe { env::remove_var(key) },
            }

            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        /// Restores the environment variable to its original value.
        fn drop(&mut self) {
            match &self.original {
                Some(value) => unsafe { env::set_var(self.key, value) },
                None => unsafe { env::remove_var(self.key) },
            }
        }
    }

    /// Verifies unsupported auth mode values fail fast with a useful error.
    #[test]
    fn rejects_unknown_auth_modes() {
        let _env_guard = ENV_MUTEX.get_or_init(|| Mutex::new(())).lock().unwrap();
        let _auth_guard = EnvVarGuard::set("OPENAI_AUTH_MODE", Some("mystery"));

        let error = resolve_auth_mode().unwrap_err();
        assert!(error.to_string().contains("invalid OPENAI_AUTH_MODE"));
    }

    /// Verifies the default auth mode remains API-key based for existing callers.
    #[test]
    fn defaults_to_api_key_auth_mode() {
        let _env_guard = ENV_MUTEX.get_or_init(|| Mutex::new(())).lock().unwrap();
        let _auth_guard = EnvVarGuard::set("OPENAI_AUTH_MODE", None);

        assert_eq!(resolve_auth_mode().unwrap(), AuthMode::ApiKey);
    }
}
