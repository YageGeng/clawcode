mod config;
mod runtime;

use std::sync::Arc;
use std::{
    env, io,
    io::{BufRead, Write},
};

use config::{AppConfig, AuthMode, LinkMode};
use kernel::{
    AgentLoopConfig, ThreadHandle, ThreadRuntime,
    model::{AgentModel, FactoryLlmAgentModel},
    session::InMemorySessionStore,
    tools::router::ToolRouter,
};
use llm::providers::LlmModelFactory;
use tools::create::create_default_tool_router;
use tracing::info;
use tracing_subscriber::EnvFilter;

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

/// Builds the CLI model adapter selected by the loaded app config.
fn build_agent_model(
    config: &AppConfig,
) -> Result<FactoryLlmAgentModel, Box<dyn std::error::Error>> {
    let factory = LlmModelFactory::try_from_config(config.to_llm_config())?;
    let model = factory.completion_model_ref(&config.llm_model_ref())?;
    Ok(FactoryLlmAgentModel::new(model))
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

/// Runs the interactive CLI loop on one in-memory thread until stdin closes or the user exits.
async fn run_interactive_cli_loop<M, E, R, W>(
    runtime: &ThreadRuntime<M, E>,
    thread: &ThreadHandle,
    input: &mut R,
    output: &mut W,
) -> Result<(), Box<dyn std::error::Error>>
where
    M: AgentModel + 'static,
    E: kernel::events::EventSink + 'static,
    R: BufRead,
    W: Write,
{
    let mut line = String::new();

    loop {
        // Keep the prompt minimal so multi-turn mode behaves like a normal terminal REPL.
        write!(output, "> ")?;
        output.flush()?;

        line.clear();
        if input.read_line(&mut line)? == 0 {
            break;
        }

        let prompt = line.trim();
        if prompt.eq_ignore_ascii_case("exit") || prompt.eq_ignore_ascii_case("quit") {
            break;
        }
        if prompt.is_empty() {
            continue;
        }

        match runtime::run_cli_turn(runtime, thread, prompt.to_string()).await {
            Ok(_text) => {}
            Err(err) => writeln!(output, "error: {err}")?,
        }
    }

    Ok(())
}

/// Builds the CLI runtime stack once and reuses it for every interactive turn.
async fn run_interactive_cli(
    model: Arc<FactoryLlmAgentModel>,
    store: Arc<InMemorySessionStore>,
    router: Arc<ToolRouter>,
    skills: skills::SkillConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let thread = runtime::build_cli_thread_handle();
    let runtime = ThreadRuntime::new(
        model,
        store,
        router,
        Arc::new(runtime::TracingEventSink::stdout()),
    )
    .with_config(AgentLoopConfig {
        skills,
        ..AgentLoopConfig::default()
    });
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();

    run_interactive_cli_loop(&runtime, &thread, &mut input, &mut output).await
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let prompt = env::args().skip(1).collect::<Vec<_>>().join(" ");
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
    let skills = config.skills.to_skill_config();

    info!(
        tool_count = router.definitions().await.len(),
        "registered default tools through the extracted tools crate"
    );

    if prompt.trim().is_empty() {
        info!("starting interactive cli session");
        run_interactive_cli(
            Arc::clone(&model),
            Arc::clone(&store),
            Arc::clone(&router),
            skills,
        )
        .await?;
    } else {
        let _ = runtime::run_cli_prompt(Arc::clone(&model), store, router, prompt, skills).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::model::ModelRequest;
    use llm::usage::Usage;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    use tokio::sync::Mutex as AsyncMutex;

    #[derive(Clone)]
    struct LoopTestModel {
        responses: Arc<AsyncMutex<VecDeque<String>>>,
    }

    impl LoopTestModel {
        /// Builds a test model that returns queued text responses for each interactive turn.
        fn new(responses: &[&str]) -> Self {
            Self {
                responses: Arc::new(AsyncMutex::new(
                    responses
                        .iter()
                        .map(|text| text.to_string())
                        .collect::<VecDeque<_>>(),
                )),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl AgentModel for LoopTestModel {
        /// Returns queued test replies so the REPL loop can be exercised without external providers.
        async fn complete(
            &self,
            _request: ModelRequest,
        ) -> kernel::Result<kernel::model::ModelResponse> {
            let text = self
                .responses
                .lock()
                .await
                .pop_front()
                .ok_or(kernel::Error::Runtime {
                    message: "loop test model exhausted".to_string(),
                    stage: "cli-loop-test-model".to_string(),
                    inflight_snapshot: None,
                })?;
            Ok(kernel::model::ModelResponse::text(text, Usage::default()))
        }
    }

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

    /// Verifies the bundled config enables repo-local skill discovery for CLI requests.
    #[test]
    fn loads_default_skills_config_from_base_toml() {
        let _env_guard = lock_env();
        let _profile_guard = EnvVarGuard::set("APP_PROFILE", None);
        let _skills_enabled_guard = EnvVarGuard::set("APP_SKILLS__ENABLED", None);
        let _skills_cwd_guard = EnvVarGuard::set("APP_SKILLS__CWD", None);
        let _skills_roots_guard = EnvVarGuard::set("APP_SKILLS__ROOTS", None);

        let config = config::load_config().unwrap();

        assert!(config.skills.enabled);
        assert_eq!(config.skills.cwd, Some(PathBuf::from(".")));
        assert!(config.skills.roots.is_empty());
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

    /// Verifies OAuth config is converted into an OpenAI protocol provider with managed auth.
    #[test]
    fn converts_oauth_config_into_openai_managed_auth_provider() {
        let config = AppConfig {
            auth_mode: AuthMode::OAuth,
            link_mode: LinkMode::Response,
            ..AppConfig::default()
        };

        let llm_config = config.to_llm_config();
        let provider = llm_config
            .providers
            .first()
            .expect("CLI config should create one provider");

        assert_eq!(config.llm_model_ref(), "openai/gpt-5.3-codex");
        assert_eq!(provider.id, "openai");
        assert_eq!(
            provider.protocols,
            vec![llm::providers::ApiProtocol::OpenAI]
        );
        assert_eq!(provider.api_key, llm::providers::ApiKeyConfig::Auth);
    }

    /// Verifies the interactive loop skips blank input, prints replies, and exits on `quit`.
    #[test]
    fn interactive_loop_reuses_the_same_thread_until_quit() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime should build");
        let runtime = ThreadRuntime::new(
            Arc::new(LoopTestModel::new(&["first reply"])),
            Arc::new(InMemorySessionStore::default()),
            Arc::new(rt.block_on(create_default_tool_router())),
            Arc::new(runtime::TracingEventSink::stdout()),
        );
        let thread = runtime::build_cli_thread_handle();
        let mut input = io::Cursor::new(b"\nhello\nquit\n".to_vec());
        let mut output = Vec::new();

        let result = rt.block_on(async {
            run_interactive_cli_loop(&runtime, &thread, &mut input, &mut output).await
        });

        assert!(result.is_ok());
        let rendered = String::from_utf8(output).expect("output should be utf8");
        assert!(rendered.starts_with("> > "));
    }
}
