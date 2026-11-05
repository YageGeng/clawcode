use std::sync::{Arc, RwLock};

use protocol::{ModelProfile, ThinkingLevel};

use super::KernelError;
use crate::{Model, ModelCatalog};

/// Current model handle and thinking level published as one consistent snapshot.
struct ModelSelection {
    model: Arc<dyn Model>,
    thinking_level: ThinkingLevel,
}

/// Session-local model catalog and mutable selection state.
pub(super) struct SessionModelState {
    catalog: Arc<ModelCatalog>,
    selection: RwLock<ModelSelection>,
}

impl SessionModelState {
    /// Creates model state from restored Session settings.
    pub(super) fn new(
        catalog: Arc<ModelCatalog>,
        model: Arc<dyn Model>,
        thinking_level: ThinkingLevel,
    ) -> Self {
        Self {
            catalog,
            selection: RwLock::new(ModelSelection {
                model,
                thinking_level,
            }),
        }
    }

    /// Returns the active model handle without retaining the selection lock.
    pub(super) fn active(&self) -> Result<Arc<dyn Model>, KernelError> {
        self.selection
            .read()
            .map(|selection| Arc::clone(&selection.model))
            .map_err(KernelError::from)
    }

    /// Returns the current thinking level.
    pub(super) fn thinking_level(&self) -> Result<ThinkingLevel, KernelError> {
        self.selection
            .read()
            .map(|selection| selection.thinking_level)
            .map_err(KernelError::from)
    }

    /// Captures the active model and thinking level under one read lock.
    pub(super) fn snapshot(
        &self,
    ) -> Result<(Arc<dyn Model>, ThinkingLevel), KernelError> {
        self.selection
            .read()
            .map(|selection| {
                (Arc::clone(&selection.model), selection.thinking_level)
            })
            .map_err(KernelError::from)
    }

    /// Resolves one configured model without changing the active selection.
    pub(super) fn resolve(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Option<Arc<dyn Model>> {
        self.catalog.resolve(provider_id, model_id)
    }

    /// Resolves a `provider/model` reference, splitting at the first slash.
    ///
    /// The error names the reference and lists every configured model so a model
    /// caller can correct itself.
    pub(super) fn resolve_reference(
        &self,
        reference: &str,
    ) -> Result<Arc<dyn Model>, String> {
        reference
            .split_once('/')
            .and_then(|(provider_id, model_id)| {
                self.catalog.resolve(provider_id, model_id)
            })
            .ok_or_else(|| {
                let configured = self
                    .catalog
                    .profiles()
                    .iter()
                    .map(|profile| {
                        format!("{}/{}", profile.provider_id, profile.model_id)
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "unknown model '{reference}'; configured models: {configured}"
                )
            })
    }

    /// Returns every configured model profile in catalog order.
    pub(super) fn profiles(&self) -> Vec<ModelProfile> {
        self.catalog.profiles()
    }

    /// Replaces only the model dimension of the current selection.
    pub(super) fn replace_model(
        &self,
        model: Arc<dyn Model>,
    ) -> Result<(), KernelError> {
        self.selection.write()?.model = model;
        Ok(())
    }

    /// Replaces only the thinking dimension of the current selection.
    pub(super) fn replace_thinking_level(
        &self,
        level: ThinkingLevel,
    ) -> Result<(), KernelError> {
        self.selection.write()?.thinking_level = level;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::{Stream, stream};
    use protocol::{
        ModelFinal, ModelProfile, ModelRequest, ModelStreamEvent, StopReason,
        ThinkingLevel,
    };
    use tokio_util::sync::CancellationToken;

    use super::SessionModelState;
    use crate::{Model, ModelCatalog, ModelError};

    /// Deterministic model used to exercise session-local selection snapshots.
    struct TestModel(ModelProfile);

    #[async_trait]
    impl Model for TestModel {
        /// Returns this fixture's immutable profile.
        fn profile(&self) -> &ModelProfile {
            &self.0
        }

        /// Avoids external provider dependencies in the state unit test.
        async fn preflight(&self) -> Result<(), ModelError> {
            Ok(())
        }

        /// Returns one terminal response when invoked unexpectedly.
        async fn stream(
            &self,
            _request: ModelRequest,
            _cancellation: CancellationToken,
        ) -> Result<
            Pin<
                Box<
                    dyn Stream<Item = Result<ModelStreamEvent, ModelError>>
                        + Send,
                >,
            >,
            ModelError,
        > {
            Ok(Box::pin(stream::iter([Ok(ModelStreamEvent::Finished(
                ModelFinal::builder()
                    .stop_reason(StopReason::EndTurn)
                    .raw_stop_reason(None)
                    .usage(protocol::ModelUsage::from(
                        protocol::Usage::new()
                            .input_tokens(0)
                            .output_tokens(0)
                            .cached_input_tokens(0)
                            .cache_creation_input_tokens(0)
                            .total_tokens(0),
                    ))
                    .build(),
            ))])))
        }
    }

    /// Builds one fixture model with a stable identifier.
    fn model(id: &str) -> Arc<dyn Model> {
        Arc::new(TestModel(
            ModelProfile::builder()
                .provider_id("fixture".to_string())
                .model_id(id.to_string())
                .display_name(id.to_string())
                .context_tokens(128_000)
                .max_output_tokens(8_000)
                .build(),
        ))
    }

    /// Verifies that independent model and thinking updates cannot overwrite each other.
    #[test]
    fn model_snapshot_keeps_both_selection_dimensions() {
        let primary = model("primary");
        let secondary = model("secondary");
        let catalog = Arc::new(
            ModelCatalog::new(
                "fixture",
                "primary",
                vec![Arc::clone(&primary), Arc::clone(&secondary)],
            )
            .expect("model catalog"),
        );
        let state =
            SessionModelState::new(catalog, primary, ThinkingLevel::Off);

        state
            .replace_model(secondary)
            .expect("replace active model");
        state
            .replace_thinking_level(ThinkingLevel::High)
            .expect("replace thinking level");
        let (selected, level) = state.snapshot().expect("model snapshot");

        assert_eq!(selected.profile().model_id, "secondary");
        assert_eq!(level, ThinkingLevel::High);
    }
    /// References split at the first slash, so model ids may contain their own slashes.
    #[test]
    fn resolve_reference_splits_at_first_slash() {
        let routed: Arc<dyn Model> = Arc::new(TestModel(
            ModelProfile::builder()
                .provider_id("openrouter".to_string())
                .model_id("aion-labs/aion-2.0".to_string())
                .display_name("Aion".to_string())
                .context_tokens(128_000)
                .max_output_tokens(8_000)
                .build(),
        ));
        let catalog = Arc::new(
            ModelCatalog::new(
                "openrouter",
                "aion-labs/aion-2.0",
                vec![Arc::clone(&routed)],
            )
            .expect("model catalog"),
        );
        let state = SessionModelState::new(catalog, routed, ThinkingLevel::Off);

        let found = state
            .resolve_reference("openrouter/aion-labs/aion-2.0")
            .map(|model| model.profile().model_id.clone());
        assert_eq!(found.as_deref(), Ok("aion-labs/aion-2.0"));
        for reference in ["openrouter/missing", "nomodel"] {
            let Err(error) = state.resolve_reference(reference) else {
                panic!("{reference} must not resolve");
            };
            assert!(error.contains(reference), "{error}");
            assert!(error.contains("openrouter/aion-labs/aion-2.0"), "{error}");
        }
    }
    /// Builds a kernel over `root` whose provider offers the listed models.
    fn kernel_with_models(
        root: &std::path::Path,
        models: &[&str],
    ) -> crate::Kernel {
        let entries = models
            .iter()
            .map(|id| {
                format!(
                    "[[providers.models]]\nid=\"{id}\"\ncontext_tokens=32768\nmax_output_tokens=128\n"
                )
            })
            .collect::<String>();
        let config: config::AppConfig = toml::from_str(&format!(
            "active_model=\"openai/audit\"\n[[providers]]\nid=\"openai\"\ndisplay_name=\"audit\"\nprovider_type=\"responses\"\nbase_url=\"https://provider.invalid\"\napi_key=\"synthetic-key\"\n{entries}"
        ))
        .expect("config");
        let clock: Arc<dyn store::Clock> = Arc::new(store::SystemClock);
        crate::KernelFactory::builder()
            .model_factory(Arc::new(crate::ProviderModelFactory::from_config(
                config::ConfigHandle::from_config(config),
            )))
            .tool_factory(Arc::new(tools::BuiltinToolFactory::new()))
            .store_factory(Arc::new(store::JsonlStoreFactory::new(
                root,
                Arc::clone(&clock),
            )))
            .extension_factory(Arc::new(
                extension::StaticExtensionFactory::default(),
            ))
            .prompt_factory(Arc::new(prompt::FilesystemPromptFactory::new(
                root.join("config"),
                protocol::PromptPolicy::default(),
            )))
            .clock(clock)
            .id_generator(Arc::new(crate::NanoidIdGenerator))
            .build()
            .build()
            .expect("kernel")
    }

    /// Sessions without a persisted choice start at the default thinking level.
    #[tokio::test]
    async fn new_sessions_start_at_the_default_thinking_level() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let session = protocol::SessionId::try_from("a").expect("id");
        let kernel = kernel_with_models(temporary.path(), &["audit"]);
        kernel
            .create_session(store::SessionCreateOptions {
                session_id: session.clone(),
                cwd: temporary.path().into(),
                parent_session_id: None,
            })
            .await
            .expect("create");
        assert_eq!(
            kernel
                .session_settings(&session)
                .expect("settings")
                .thinking_level,
            ThinkingLevel::default()
        );
    }

    /// A persisted model that is no longer configured falls back instead of blocking Resume.
    #[tokio::test]
    async fn resume_falls_back_when_persisted_model_is_gone() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let session = protocol::SessionId::try_from("a").expect("id");
        let before = kernel_with_models(temporary.path(), &["audit", "other"]);
        before
            .create_session(store::SessionCreateOptions {
                session_id: session.clone(),
                cwd: temporary.path().into(),
                parent_session_id: None,
            })
            .await
            .expect("create");
        before
            .set_session_model(&session, "openai", "other")
            .await
            .expect("select other");
        before.close_session(&session).await.expect("close");

        let after = kernel_with_models(temporary.path(), &["audit"]);
        after
            .resume_session(session.clone(), temporary.path().into())
            .await
            .expect("resume without the persisted model");
        assert_eq!(
            after
                .session_settings(&session)
                .expect("settings")
                .model
                .model_id,
            "audit"
        );
    }

    /// Real Session creation never installs a model handle from another Session's catalog.
    #[tokio::test]
    async fn sessions_own_distinct_model_catalogs_and_release_them_on_close() {
        let temporary = tempfile::tempdir().unwrap();
        let config: config::AppConfig = toml::from_str(
            r#"
active_model="openai/audit"
[[providers]]
id="openai"
display_name="audit"
provider_type="responses"
base_url="https://provider.invalid"
api_key="synthetic-key"
[[providers.models]]
id="audit"
context_tokens=32768
max_output_tokens=128
[[providers.models]]
id="other"
context_tokens=32768
max_output_tokens=128
"#,
        )
        .unwrap();
        let clock: Arc<dyn store::Clock> = Arc::new(store::SystemClock);
        let kernel = crate::KernelFactory::builder()
            .model_factory(Arc::new(crate::ProviderModelFactory::from_config(
                config::ConfigHandle::from_config(config),
            )))
            .tool_factory(Arc::new(tools::BuiltinToolFactory::new()))
            .store_factory(Arc::new(store::JsonlStoreFactory::new(
                temporary.path(),
                Arc::clone(&clock),
            )))
            .extension_factory(Arc::new(
                extension::StaticExtensionFactory::default(),
            ))
            .prompt_factory(Arc::new(prompt::FilesystemPromptFactory::new(
                temporary.path().join("config"),
                protocol::PromptPolicy::default(),
            )))
            .clock(clock)
            .id_generator(Arc::new(crate::NanoidIdGenerator))
            .build()
            .build()
            .unwrap();
        let a = protocol::SessionId::try_from("a").unwrap();
        let b = protocol::SessionId::try_from("b").unwrap();
        for id in [&a, &b] {
            kernel
                .create_session(store::SessionCreateOptions {
                    session_id: id.clone(),
                    cwd: temporary.path().into(),
                    parent_session_id: None,
                })
                .await
                .unwrap();
        }
        let first = kernel.session(&a).unwrap().model.active().unwrap();
        let second = kernel.session(&b).unwrap().model.active().unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        let session = kernel.session(&a).unwrap();
        let other = session.model.resolve("openai", "other").unwrap();
        session.model.replace_model(other).unwrap();
        session
            .model
            .replace_model(session.model.resolve("openai", "audit").unwrap())
            .unwrap();
        assert!(Arc::ptr_eq(&first, &session.model.active().unwrap()));
        let weak = Arc::downgrade(&first);
        drop(first);
        drop(session);
        kernel.close_session(&a).await.unwrap();
        assert!(weak.upgrade().is_none());
        assert!(Arc::ptr_eq(
            &second,
            &kernel.session(&b).unwrap().model.active().unwrap()
        ));
        kernel.close_session(&b).await.unwrap();
    }
}
