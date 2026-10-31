//! Request-scoped provider behavior implemented with the upstream HTTP contract.

use extension::{ExtensionContext, ExtensionError, ExtensionRegistrar};
use rig_core::{
    http_client::{
        self,
        middleware::{Exchange, HttpMiddleware},
    },
    wasm_compat::WasmBoxedFuture,
};

struct ProviderPolicy {
    context: ExtensionContext,
}

impl HttpMiddleware for ProviderPolicy {
    /// Adds an owner-scoped diagnostic header without recording the request or credentials.
    fn before_request_headers<'a>(
        &'a self,
        _: Exchange<'a>,
        headers: &'a mut http::HeaderMap,
    ) -> WasmBoxedFuture<'a, http_client::Result<()>> {
        Box::pin(async move {
            let value = http::HeaderValue::try_from(
                self.context.invocation.extension_id.as_str(),
            )
            .map_err(|error| http_client::Error::Instance(error.into()))?;
            headers.insert("x-extension-example", value);
            Ok(())
        })
    }
}

/// Registers a fresh native middleware instance for each model request context.
pub(super) fn register(
    registrar: &mut ExtensionRegistrar,
) -> Result<(), ExtensionError> {
    registrar.register_http_middleware(|context| ProviderPolicy { context })
}
