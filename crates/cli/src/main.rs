mod config;
mod runtime;

use std::sync::Arc;
use std::{env, io};

use config::{AppConfig, AuthMode, LinkMode};
use kernel::{
    model::{AgentModel, LlmAgentModel, ModelRequest},
    session::InMemorySessionStore,
};
use llm::providers::{
    chatgpt,
    openai::{
        self, completion::CompletionModel as OpenAiCompletionsModel,
        responses_api::ResponsesCompletionModel,
    },
};
use tools::create::create_default_tool_router;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// CLI-local adapter that selects one of the supported provider/model combinations.
#[derive(Clone)]
enum CliAgentModel {
    OpenAiResponse(Box<LlmAgentModel<ResponsesCompletionModel>>),
    ChatGptResponse(Box<LlmAgentModel<chatgpt::ResponsesCompletionModel>>),
    Completion(Box<LlmAgentModel<OpenAiCompletionsModel>>),
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

/// Prints the CLI usage string expected by the integration test.
fn usage_message() -> &'static str {
    "usage: cargo run -p cli -- \"your prompt\""
}

/// Validates the loaded config before a provider client is constructed.
fn validate_config(config: &AppConfig) -> Result<(), io::Error> {
    if matches!(config.auth_mode, AuthMode::OAuth)
        && matches!(config.link_mode, LinkMode::Completion)
    {
        return Err(io::Error::other(
            "codex auth only supports response link mode",
        ));
    }

    Ok(())
}

/// Builds the standard OpenAI client used for API-key based response/completion modes.
fn build_openai_client(config: &AppConfig) -> Result<openai::Client, Box<dyn std::error::Error>> {
    let api_key = config.openai.api_key.clone().ok_or_else(|| {
        io::Error::other("missing openai.api_key; set it in config or APP_OPENAI__API_KEY")
    })?;

    Ok(openai::Client::builder()
        .base_url(config.openai.base_url.clone())
        .api_key(api_key)
        .build()?)
}

/// Builds the ChatGPT client so OAuth and bearer-token handling stay inside `llm::providers`.
fn build_chatgpt_client(config: &AppConfig) -> Result<chatgpt::Client, Box<dyn std::error::Error>> {
    if let Some(access_token) = config.chatgpt.access_token.clone() {
        Ok(chatgpt::Client::builder()
            .base_url(config.chatgpt.base_url.clone())
            .api_key(chatgpt::ChatGPTAuth::AccessToken { access_token })
            .build()?)
    } else {
        Ok(chatgpt::Client::builder()
            .base_url(config.chatgpt.base_url.clone())
            .oauth()
            .build()?)
    }
}

/// Builds the CLI model adapter selected by the loaded app config.
fn build_agent_model(config: &AppConfig) -> Result<CliAgentModel, Box<dyn std::error::Error>> {
    let model = match (config.auth_mode, config.link_mode) {
        (AuthMode::ApiKey, LinkMode::Response) => {
            let client = build_openai_client(config)?;
            CliAgentModel::OpenAiResponse(Box::new(LlmAgentModel::new(
                ResponsesCompletionModel::with_model(client, &config.openai.model),
            )))
        }
        (AuthMode::ApiKey, LinkMode::Completion) => {
            let client = build_openai_client(config)?;
            CliAgentModel::Completion(Box::new(LlmAgentModel::new(
                OpenAiCompletionsModel::with_model(client.completions_api(), &config.openai.model),
            )))
        }
        (AuthMode::OAuth, LinkMode::Response) => {
            let client = build_chatgpt_client(config)?;
            CliAgentModel::ChatGptResponse(Box::new(LlmAgentModel::new(
                chatgpt::ResponsesCompletionModel::new(client, &config.chatgpt.model),
            )))
        }
        (AuthMode::OAuth, LinkMode::Completion) => unreachable!("validated in validate_config"),
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

    let config = config::app_config();
    validate_config(&config)?;

    info!(
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

    /// Acquires the shared env-var mutation lock even if a previous test panicked.
    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_MUTEX
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Verifies unsupported auth mode values fail fast during config loading.
    #[test]
    fn rejects_unknown_auth_modes() {
        let _env_guard = lock_env();
        let _auth_guard = EnvVarGuard::set("APP_AUTH_MODE", Some("mystery"));

        let error = config::load_config().unwrap_err();
        assert!(error.to_string().contains("mystery"));
    }

    /// Verifies the default auth mode matches the bundled base config.
    #[test]
    fn defaults_to_api_key_auth_mode() {
        let _env_guard = lock_env();
        let _auth_guard = EnvVarGuard::set("APP_AUTH_MODE", None);

        assert_eq!(config::load_config().unwrap().auth_mode, AuthMode::OAuth);
    }

    /// Verifies unsupported link mode values fail fast during config loading.
    #[test]
    fn rejects_unknown_link_modes() {
        let _env_guard = lock_env();
        let _link_guard = EnvVarGuard::set("APP_LINK_MODE", Some("mystery"));

        let error = config::load_config().unwrap_err();
        assert!(error.to_string().contains("mystery"));
    }

    /// Verifies the default link mode remains Responses API based for existing callers.
    #[test]
    fn defaults_to_response_link_mode() {
        let _env_guard = lock_env();
        let _link_guard = EnvVarGuard::set("APP_LINK_MODE", None);

        assert_eq!(config::load_config().unwrap().link_mode, LinkMode::Response);
    }

    /// Verifies OAuth auth rejects the unsupported completion transport combination.
    #[test]
    fn rejects_codex_completion_mode() {
        let _env_guard = lock_env();
        let _auth_guard = EnvVarGuard::set("APP_AUTH_MODE", Some("o_auth"));
        let _link_guard = EnvVarGuard::set("APP_LINK_MODE", Some("completion"));

        let error = validate_config(&config::load_config().unwrap()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("codex auth only supports response link mode")
        );
    }
}
