mod runtime;

use std::sync::Arc;
use std::{env, io};

use kernel::{
    model::{AgentModel, LlmAgentModel, ModelRequest},
    session::InMemorySessionStore,
};
use llm::providers::{
    chatgpt,
    openai::{
        self, codex::OPENAI_CODEX_DEFAULT_MODEL,
        completion::CompletionModel as OpenAiCompletionsModel,
        responses_api::ResponsesCompletionModel,
    },
};
use tools::create::create_default_tool_router;
use tracing::info;
use tracing_subscriber::EnvFilter;

const DEFAULT_MODEL: &str = "gpt-5.4";

/// Supported authentication modes for the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthMode {
    /// Standard OpenAI API-key authentication.
    ApiKey,
    /// ChatGPT/Codex provider authentication backed by llm provider OAuth handling.
    Codex,
}

/// Supported transport/link modes for the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkMode {
    /// Use the Responses-style provider.
    Response,
    /// Use the traditional OpenAI Chat Completions API.
    Completion,
}

/// Fully-resolved CLI configuration after environment parsing.
#[derive(Debug, Clone)]
struct CliConfig {
    /// Selected authentication mode.
    auth_mode: AuthMode,
    /// Selected provider link mode.
    link_mode: LinkMode,
    /// Final base URL used to build the client.
    base_url: String,
    /// Model name used for the request.
    model_name: String,
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

/// Parses the selected transport mode from environment variables.
fn resolve_link_mode() -> Result<LinkMode, io::Error> {
    match optional_env("OPENAI_LINK_MODE")
        .unwrap_or_else(|| "response".to_string())
        .to_ascii_lowercase()
        .as_str()
    {
        "response" => Ok(LinkMode::Response),
        "completion" => Ok(LinkMode::Completion),
        other => Err(io::Error::other(format!(
            "invalid OPENAI_LINK_MODE `{other}`; expected `response` or `completion`"
        ))),
    }
}

/// Resolves the final CLI configuration without performing provider-side authentication.
fn resolve_cli_config() -> Result<CliConfig, Box<dyn std::error::Error>> {
    let auth_mode = resolve_auth_mode()?;
    let link_mode = resolve_link_mode()?;

    if matches!(auth_mode, AuthMode::Codex) && matches!(link_mode, LinkMode::Completion) {
        return Err(io::Error::other(
            "OPENAI_AUTH_MODE=codex only supports OPENAI_LINK_MODE=response",
        )
        .into());
    }

    match auth_mode {
        AuthMode::ApiKey => Ok(CliConfig {
            auth_mode,
            link_mode,
            base_url: optional_env("OPENAI_BASE_URL")
                .unwrap_or_else(|| openai::OPENAI_API_BASE_URL.to_string()),
            model_name: optional_env("OPENAI_MODEL").unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        }),
        AuthMode::Codex => Ok(CliConfig {
            auth_mode,
            link_mode,
            base_url: optional_env("OPENAI_BASE_URL")
                .or_else(|| optional_env("CHATGPT_API_BASE"))
                .or_else(|| optional_env("OPENAI_CHATGPT_API_BASE"))
                .unwrap_or_else(|| openai::codex::OPENAI_CODEX_API_BASE_URL.to_string()),
            model_name: optional_env("OPENAI_MODEL")
                .unwrap_or_else(|| OPENAI_CODEX_DEFAULT_MODEL.to_string()),
        }),
    }
}

/// CLI-local adapter that selects one of the supported provider/model combinations.
#[derive(Clone)]
enum CliAgentModel {
    OpenAiResponse(LlmAgentModel<ResponsesCompletionModel>),
    ChatGptResponse(Box<LlmAgentModel<chatgpt::ResponsesCompletionModel>>),
    Completion(LlmAgentModel<OpenAiCompletionsModel>),
}

#[async_trait::async_trait(?Send)]
impl AgentModel for CliAgentModel {
    /// Delegates one compact completion request to the selected provider adapter.
    async fn complete(
        &self,
        request: ModelRequest,
    ) -> kernel::Result<kernel::model::ModelResponse> {
        match self {
            Self::OpenAiResponse(model) => model.complete(request).await,
            Self::ChatGptResponse(model) => model.complete(request).await,
            Self::Completion(model) => model.complete(request).await,
        }
    }

    /// Preserves provider-native streaming for the selected provider adapter.
    async fn stream(
        &self,
        request: ModelRequest,
    ) -> kernel::Result<kernel::model::ResponseEventStream> {
        match self {
            Self::OpenAiResponse(model) => model.stream(request).await,
            Self::ChatGptResponse(model) => model.stream(request).await,
            Self::Completion(model) => model.stream(request).await,
        }
    }
}

impl CliAgentModel {
    /// Releases transport resources held by the selected adapter.
    async fn close(&self) -> kernel::Result<()> {
        Ok(())
    }
}

/// Builds the standard OpenAI client used for API-key based response/completion modes.
fn build_openai_client(config: &CliConfig) -> Result<openai::Client, Box<dyn std::error::Error>> {
    Ok(openai::Client::builder()
        .base_url(config.base_url.clone())
        .api_key(require_env("OPENAI_API_KEY")?)
        .build()?)
}

/// Builds the ChatGPT client so OAuth and bearer-token handling stay inside `llm::providers`.
fn build_chatgpt_client(config: &CliConfig) -> Result<chatgpt::Client, Box<dyn std::error::Error>> {
    if let Some(access_token) =
        optional_env("CHATGPT_ACCESS_TOKEN").or_else(|| optional_env("OPENAI_API_KEY"))
    {
        Ok(chatgpt::Client::builder()
            .base_url(config.base_url.clone())
            .api_key(chatgpt::ChatGPTAuth::AccessToken { access_token })
            .build()?)
    } else {
        Ok(chatgpt::Client::builder()
            .base_url(config.base_url.clone())
            .oauth()
            .build()?)
    }
}

/// Builds the CLI model adapter selected by `OPENAI_AUTH_MODE` and `OPENAI_LINK_MODE`.
fn build_agent_model(config: &CliConfig) -> Result<CliAgentModel, Box<dyn std::error::Error>> {
    let model = match (config.auth_mode, config.link_mode) {
        (AuthMode::ApiKey, LinkMode::Response) => {
            let client = build_openai_client(config)?;
            CliAgentModel::OpenAiResponse(LlmAgentModel::new(ResponsesCompletionModel::with_model(
                client,
                &config.model_name,
            )))
        }
        (AuthMode::ApiKey, LinkMode::Completion) => {
            let client = build_openai_client(config)?;
            CliAgentModel::Completion(LlmAgentModel::new(OpenAiCompletionsModel::with_model(
                client.completions_api(),
                &config.model_name,
            )))
        }
        (AuthMode::Codex, LinkMode::Response) => {
            let client = build_chatgpt_client(config)?;
            CliAgentModel::ChatGptResponse(Box::new(LlmAgentModel::new(
                chatgpt::ResponsesCompletionModel::new(client, &config.model_name),
            )))
        }
        (AuthMode::Codex, LinkMode::Completion) => unreachable!("validated in resolve_cli_config"),
    };

    Ok(model)
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

    let config = resolve_cli_config()?;

    info!(
        base_url = %config.base_url,
        model = %config.model_name,
        auth_mode = ?config.auth_mode,
        link_mode = ?config.link_mode,
        prompt = %runtime::prompt_preview(&prompt),
        "starting cli request"
    );

    let model = Arc::new(build_agent_model(&config)?);
    let store = Arc::new(InMemorySessionStore::default());
    let router = Arc::new(create_default_tool_router().await);

    info!(
        tool_count = router.definitions().await.len(),
        "registered default tools through the extracted tools crate"
    );

    let run_result = runtime::run_cli_prompt(Arc::clone(&model), store, router, prompt).await;
    let close_result = model.close().await;
    let result = run_result?;
    close_result?;

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

    /// Verifies unsupported link mode values fail fast with a useful error.
    #[test]
    fn rejects_unknown_link_modes() {
        let _env_guard = ENV_MUTEX.get_or_init(|| Mutex::new(())).lock().unwrap();
        let _link_guard = EnvVarGuard::set("OPENAI_LINK_MODE", Some("mystery"));

        let error = resolve_link_mode().unwrap_err();
        assert!(error.to_string().contains("invalid OPENAI_LINK_MODE"));
    }

    /// Verifies the default link mode remains Responses API based for existing callers.
    #[test]
    fn defaults_to_response_link_mode() {
        let _env_guard = ENV_MUTEX.get_or_init(|| Mutex::new(())).lock().unwrap();
        let _link_guard = EnvVarGuard::set("OPENAI_LINK_MODE", None);

        assert_eq!(resolve_link_mode().unwrap(), LinkMode::Response);
    }

    /// Verifies Codex auth mode no longer accepts the removed completion-link fallback.
    #[test]
    fn rejects_codex_completion_mode() {
        let _env_guard = ENV_MUTEX.get_or_init(|| Mutex::new(())).lock().unwrap();
        let _auth_guard = EnvVarGuard::set("OPENAI_AUTH_MODE", Some("codex"));
        let _link_guard = EnvVarGuard::set("OPENAI_LINK_MODE", Some("completion"));

        let error = resolve_cli_config().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("OPENAI_AUTH_MODE=codex only supports OPENAI_LINK_MODE=response")
        );
    }
}
