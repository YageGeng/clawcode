mod agent;
mod input;
mod model;
mod provider;
mod session;
mod startup;
mod tool;

use std::sync::Arc;

use async_trait::async_trait;
use protocol::ExtensionId;

use crate::{
    ExtensionContext, ExtensionError, ExtensionPoint, HandlerRegistry,
    RegisterPoint, RegisteredHandler, RuntimeGeneration,
};

/// Destination for sanitized extension-handler diagnostics.
#[async_trait]
pub trait ExtensionDiagnosticSink: Send + Sync {
    /// Reports one handler error without exposing request secrets or configuration.
    async fn report(
        &self,
        extension_id: &ExtensionId,
        point: &'static str,
        message: &str,
    );
}

/// Diagnostic sink used when the host intentionally ignores handler failures.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiscardExtensionDiagnostics;

#[async_trait]
impl ExtensionDiagnosticSink for DiscardExtensionDiagnostics {
    /// Intentionally discards one already-sanitized diagnostic.
    async fn report(
        &self,
        _extension_id: &ExtensionId,
        _point: &'static str,
        _message: &str,
    ) {
    }
}

/// Typed extension composer isolated to one session runtime.
pub struct ExtensionRuntime {
    registry: Arc<HandlerRegistry>,
    diagnostics: Arc<dyn ExtensionDiagnosticSink>,
    generation: RuntimeGeneration,
}

impl ExtensionRuntime {
    /// Creates one session runtime from a frozen registry and diagnostic sink.
    #[must_use]
    pub fn new(
        registry: Arc<HandlerRegistry>,
        diagnostics: Arc<dyn ExtensionDiagnosticSink>,
    ) -> Self {
        Self {
            registry,
            diagnostics,
            generation: RuntimeGeneration::new(),
        }
    }

    /// Returns the shared validity token used by contexts for this runtime.
    #[must_use]
    pub fn generation(&self) -> RuntimeGeneration {
        self.generation.clone()
    }

    /// Invalidates every context created by this session runtime.
    pub fn invalidate(&self) {
        self.generation.invalidate();
    }

    /// Executes every observer for one typed point in registration order.
    pub(crate) async fn observe<P>(
        &self,
        event: &P::Event,
        context: &ExtensionContext,
    ) where
        P: RegisterPoint<Output = ()>,
    {
        for registered in self.registry.handlers::<P>() {
            let handler_context = context.for_extension(registered.source());
            if let Err(error) =
                registered.handler.handle(event, &handler_context).await
            {
                self.report::<P>(registered, &error).await;
            }
        }
    }

    /// Sends one sanitized error to the host diagnostic sink.
    pub(crate) async fn report<P>(
        &self,
        registered: &RegisteredHandler<P>,
        error: &ExtensionError,
    ) where
        P: ExtensionPoint,
    {
        self.diagnostics
            .report(registered.source(), P::NAME, &error.to_string())
            .await;
    }

    /// Returns typed handlers for one point without exposing the registry publicly.
    pub(crate) fn handlers<P: RegisterPoint>(&self) -> &[RegisteredHandler<P>] {
        self.registry.handlers::<P>()
    }
}
