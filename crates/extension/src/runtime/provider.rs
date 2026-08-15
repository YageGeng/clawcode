use std::collections::BTreeMap;

use protocol::{
    AfterProviderResponseEvent, BeforeProviderHeadersEvent,
    BeforeProviderRequestEvent, HeaderMutation, HeaderPatch,
};

use crate::{
    AfterProviderResponsePoint, BeforeProviderHeadersPoint,
    BeforeProviderRequestPoint, ExtensionContext, ExtensionRuntime,
};

impl ExtensionRuntime {
    /// Chains provider-private JSON payload replacements in registration order.
    pub async fn emit_before_provider_request(
        &self,
        mut event: BeforeProviderRequestEvent,
        context: &ExtensionContext,
    ) -> serde_json::Value {
        for registered in self.handlers::<BeforeProviderRequestPoint>() {
            let handler_context = context.for_extension(registered.source());
            match registered.handler.handle(&event, &handler_context).await {
                Ok(payload) => event.payload = payload,
                Err(error) => {
                    self.report::<BeforeProviderRequestPoint>(
                        registered, &error,
                    )
                    .await;
                }
            }
        }
        event.payload
    }

    /// Chains explicit header mutations while exposing each current value to later handlers.
    pub async fn emit_before_provider_headers(
        &self,
        mut event: BeforeProviderHeadersEvent,
        context: &ExtensionContext,
    ) -> HeaderPatch {
        let mut combined = HeaderPatch::default();
        for registered in self.handlers::<BeforeProviderHeadersPoint>() {
            let handler_context = context.for_extension(registered.source());
            match registered.handler.handle(&event, &handler_context).await {
                Ok(patch) => {
                    event.headers.apply(&patch);
                    combined.mutations.extend(patch.mutations);
                }
                Err(error) => {
                    self.report::<BeforeProviderHeadersPoint>(
                        registered, &error,
                    )
                    .await;
                }
            }
        }
        combined
    }

    /// Notifies extensions after sanitized provider response metadata arrives.
    pub async fn emit_after_provider_response(
        &self,
        event: &AfterProviderResponseEvent,
        context: &ExtensionContext,
    ) {
        self.observe::<AfterProviderResponsePoint>(event, context)
            .await;
    }
}

/// Applies a typed header patch to normalized request headers.
trait HeaderMapPatch {
    /// Applies every mutation in order using case-normalized names.
    fn apply(&mut self, patch: &HeaderPatch);
}

impl HeaderMapPatch for BTreeMap<String, String> {
    /// Applies set and remove operations without using null as business state.
    fn apply(&mut self, patch: &HeaderPatch) {
        for mutation in &patch.mutations {
            match mutation {
                HeaderMutation::Set { name, value } => {
                    self.insert(name.to_ascii_lowercase(), value.clone());
                }
                HeaderMutation::Remove { name } => {
                    self.remove(&name.to_ascii_lowercase());
                }
            }
        }
    }
}
