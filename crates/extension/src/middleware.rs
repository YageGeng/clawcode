//! Request-scoped native middleware factories bound to immutable extension context.

use std::sync::Arc;

use protocol::ExtensionId;
use rig_core::http_client::{DynHttpClient, middleware::HttpMiddleware};

use crate::{ExtensionContext, HandlerRegistry};

/// An attachment closure retains the native middleware type at its registration boundary.
type AttachMiddleware =
    dyn Fn(DynHttpClient, ExtensionContext) -> DynHttpClient + Send + Sync;

/// A registered factory knows its owner but holds no request-specific middleware state.
pub(crate) struct RegisteredHttpMiddleware {
    owner: ExtensionId,
    attach: Arc<AttachMiddleware>,
}

impl RegisteredHttpMiddleware {
    /// Erases only the attachment closure, preserving the upstream middleware contract.
    pub(crate) fn new<F, M>(owner: ExtensionId, factory: F) -> Self
    where
        F: Fn(ExtensionContext) -> M + Send + Sync + 'static,
        M: HttpMiddleware + 'static,
    {
        Self {
            owner,
            attach: Arc::new(move |http, context| {
                http.with_middleware(factory(context))
            }),
        }
    }

    /// Binds this owner's immutable context when constructing one request's middleware.
    fn apply(
        &self,
        http: DynHttpClient,
        context: &ExtensionContext,
    ) -> DynHttpClient {
        (self.attach)(http, context.for_extension(&self.owner))
    }
}

impl Clone for RegisteredHttpMiddleware {
    /// Registry snapshots share the factory while preserving transactional registration.
    fn clone(&self) -> Self {
        Self {
            owner: self.owner.clone(),
            attach: Arc::clone(&self.attach),
        }
    }
}

/// Immutable request context and registration order used to attach native middleware.
pub struct ProviderMiddleware {
    registry: Arc<HandlerRegistry>,
    context: ExtensionContext,
}

impl ProviderMiddleware {
    /// Captures context without creating shared mutable middleware instances.
    pub(crate) fn new(
        registry: Arc<HandlerRegistry>,
        context: ExtensionContext,
    ) -> Self {
        Self { registry, context }
    }

    /// Attaches fresh middleware instances to a clone of the caller's pooled transport.
    pub fn apply(&self, mut base: DynHttpClient) -> DynHttpClient {
        for registered in &self.registry.http_middleware {
            base = registered.apply(base, &self.context);
        }
        base
    }
}

impl Clone for ProviderMiddleware {
    /// Retry attempts reuse immutable context and instantiate their own middleware on apply.
    fn clone(&self) -> Self {
        Self {
            registry: Arc::clone(&self.registry),
            context: self.context.clone(),
        }
    }
}
