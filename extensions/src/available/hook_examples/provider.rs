//! Provider request and response Hook examples.

use async_trait::async_trait;
use extension::{
    AfterProviderResponsePoint, BeforeProviderHeadersPoint,
    BeforeProviderRequestPoint, ExtensionContext, ExtensionError,
    ExtensionHandler, ExtensionRegistrar,
};
use protocol::{
    AfterProviderResponseEvent, BeforeProviderHeadersEvent,
    BeforeProviderRequestEvent, HeaderMutation, HeaderPatch,
};

struct ProviderPolicy;

#[async_trait]
impl ExtensionHandler<BeforeProviderRequestPoint> for ProviderPolicy {
    /// Adds provider-private metadata when the serialized request is an object.
    async fn handle(
        &self,
        event: &BeforeProviderRequestEvent,
        _context: &ExtensionContext,
    ) -> Result<serde_json::Value, ExtensionError> {
        let mut payload = event.payload.clone();
        if let Some(object) = payload.as_object_mut() {
            object
                .insert("extension_trace".to_string(), serde_json::json!(true));
        }
        Ok(payload)
    }
}

#[async_trait]
impl ExtensionHandler<BeforeProviderHeadersPoint> for ProviderPolicy {
    /// Adds one non-secret diagnostic header after provider headers are assembled.
    async fn handle(
        &self,
        _event: &BeforeProviderHeadersEvent,
        _context: &ExtensionContext,
    ) -> Result<HeaderPatch, ExtensionError> {
        Ok(HeaderPatch {
            mutations: vec![HeaderMutation::Set {
                name: "x-extension-example".to_string(),
                value: "enabled".to_string(),
            }],
        })
    }
}

#[async_trait]
impl ExtensionHandler<AfterProviderResponsePoint> for ProviderPolicy {
    /// Observes sanitized response status and headers before stream consumption.
    async fn handle(
        &self,
        event: &AfterProviderResponseEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let _response = (event.status, &event.headers);
        Ok(())
    }
}

/// Registers request-body, request-header, and response provider hooks.
pub(super) fn register(
    registrar: &mut ExtensionRegistrar,
) -> Result<(), ExtensionError> {
    registrar.on::<BeforeProviderRequestPoint, _>(ProviderPolicy)?;
    registrar.on::<BeforeProviderHeadersPoint, _>(ProviderPolicy)?;
    registrar.on::<AfterProviderResponsePoint, _>(ProviderPolicy)
}
