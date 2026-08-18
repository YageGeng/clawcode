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
            .map_err(|_poison_error| KernelError::Poisoned)
    }

    /// Returns the current thinking level.
    pub(super) fn thinking_level(&self) -> Result<ThinkingLevel, KernelError> {
        self.selection
            .read()
            .map(|selection| selection.thinking_level)
            .map_err(|_poison_error| KernelError::Poisoned)
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
            .map_err(|_poison_error| KernelError::Poisoned)
    }

    /// Resolves one configured model without changing the active selection.
    pub(super) fn resolve(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Option<Arc<dyn Model>> {
        self.catalog.resolve(provider_id, model_id)
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
        self.selection
            .write()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .model = model;
        Ok(())
    }

    /// Replaces only the thinking dimension of the current selection.
    pub(super) fn replace_thinking_level(
        &self,
        level: ThinkingLevel,
    ) -> Result<(), KernelError> {
        self.selection
            .write()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .thinking_level = level;
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
        ModelFinal, ModelProfile, ModelRequest, ModelStreamEvent, ModelUsage,
        StopReason, ThinkingLevel,
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
                ModelFinal {
                    stop_reason: StopReason::EndTurn,
                    raw_stop_reason: None,
                    usage: ModelUsage::builder()
                        .input_tokens(0)
                        .output_tokens(0)
                        .cache_read_tokens(0)
                        .cache_write_tokens(0)
                        .total_tokens(0)
                        .build(),
                },
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
}
