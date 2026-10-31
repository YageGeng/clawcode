use std::sync::Arc;

use crate::{ExtensionContext, ExtensionRuntime, ProviderMiddleware};

impl ExtensionRuntime {
    /// Captures this request's context while sharing only the frozen registration factories.
    pub fn provider_middleware(
        &self,
        context: ExtensionContext,
    ) -> ProviderMiddleware {
        ProviderMiddleware::new(Arc::clone(&self.registry), context)
    }
}
