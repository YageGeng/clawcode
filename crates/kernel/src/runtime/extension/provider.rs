use std::sync::Arc;

use async_trait::async_trait;
use extension::{ExtensionContext, ExtensionRuntime};
use protocol::{
    ModelHeaders, ModelHookError, ModelRequestHooks, ModelResponseMetadata,
    RunId, TurnId,
};

use super::super::{Kernel, KernelError, SessionRuntime};

impl Kernel {
    /// Creates one request-local bridge from provider hooks to the session runtime.
    pub(in crate::runtime) fn completion_hooks(
        &self,
        session: &Arc<SessionRuntime>,
        run_id: &RunId,
        turn_id: &TurnId,
    ) -> Result<Arc<dyn ModelRequestHooks>, KernelError> {
        Ok(Arc::new(
            ExtensionCompletionHooks::builder()
                .runtime(Arc::clone(&session.extensions))
                .context(self.extension_context(
                    session,
                    Some(run_id),
                    Some(turn_id),
                )?)
                .payload(tokio::sync::Mutex::new(None))
                .headers(tokio::sync::Mutex::new(None))
                .response_seen(std::sync::atomic::AtomicBool::new(false))
                .build(),
        ))
    }
}

/// Request-local bridge that composes provider lifecycle points without provider coupling.
#[derive(typed_builder::TypedBuilder)]
struct ExtensionCompletionHooks {
    runtime: Arc<ExtensionRuntime>,
    context: ExtensionContext,
    payload: tokio::sync::Mutex<Option<serde_json::Value>>,
    headers: tokio::sync::Mutex<Option<ModelHeaders>>,
    response_seen: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl ModelRequestHooks for ExtensionCompletionHooks {
    /// Applies ordered provider-payload replacements once per logical request.
    async fn before_payload(
        &self,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, ModelHookError> {
        let mut prepared = self.payload.lock().await;
        if let Some(payload) = prepared.as_ref() {
            return Ok(payload.clone());
        }
        let payload = self
            .runtime
            .emit_before_provider_request(
                protocol::BeforeProviderRequestEvent { payload },
                &self.context,
            )
            .await;
        *prepared = Some(payload.clone());
        Ok(payload)
    }

    /// Applies ordered mutations to normalized non-secret headers once per request.
    async fn before_headers(
        &self,
        mut headers: ModelHeaders,
    ) -> Result<ModelHeaders, ModelHookError> {
        let mut prepared = self.headers.lock().await;
        if let Some(headers) = prepared.as_ref() {
            return Ok(headers.clone());
        }
        let patch = self
            .runtime
            .emit_before_provider_headers(
                protocol::BeforeProviderHeadersEvent {
                    headers: headers.clone(),
                },
                &self.context,
            )
            .await;
        for mutation in patch.mutations {
            match mutation {
                protocol::HeaderMutation::Set { name, value } => {
                    headers.insert(name.to_ascii_lowercase(), value);
                }
                protocol::HeaderMutation::Remove { name } => {
                    headers.remove(&name.to_ascii_lowercase());
                }
            }
        }
        *prepared = Some(headers.clone());
        Ok(headers)
    }

    /// Forwards sanitized response status and headers to observers once.
    async fn after_response(
        &self,
        response: ModelResponseMetadata,
    ) -> Result<(), ModelHookError> {
        if self
            .response_seen
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return Ok(());
        }
        self.runtime
            .emit_after_provider_response(
                &protocol::AfterProviderResponseEvent {
                    status: response.status,
                    headers: response.headers,
                },
                &self.context,
            )
            .await;
        Ok(())
    }
}
