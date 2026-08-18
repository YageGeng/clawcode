use std::sync::Arc;

use http::{HeaderMap, HeaderName, HeaderValue, Request, Response};
use protocol::hooks::model::{
    ModelHeaders, ModelHookError, ModelRequestHooks, ModelResponseMetadata,
};
use serde::Serialize;

use super::CompletionError;

/// Provider-visible alias for the shared request-hook contract.
///
/// The contract itself lives in `protocol` so the kernel's `Model` trait can
/// accept hooks without depending on this crate; this alias preserves the
/// rig-flavored name used throughout provider-internal plumbing.
pub use protocol::hooks::model::ModelRequestHooks as CompletionRequestHooks;

/// Provider-visible alias for normalized non-secret request headers.
pub type ProviderHeaders = ModelHeaders;

/// Provider-visible alias for sanitized provider response metadata.
pub type ProviderResponseMetadata = ModelResponseMetadata;

/// Prepared immutable hook handle attached to one concrete HTTP request.
#[derive(Clone)]
pub struct PreparedCompletionHooks(Arc<dyn ModelRequestHooks>);

impl PreparedCompletionHooks {
    /// Attaches this request-local handle to the concrete transport request.
    pub async fn attach(
        self,
        request: &mut Request<Vec<u8>>,
    ) -> Result<(), CompletionError> {
        request.headers_mut().insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        let partition = ProviderHeaderPartition::from(request.headers());
        let replacement =
            self.0.before_headers(partition.visible.clone()).await?;
        partition.apply(replacement, request.headers_mut())?;
        // Pi exposes headers before payload, so body transformation happens
        // only after the final visible transport headers have been composed.
        let payload = serde_json::from_slice(request.body())?;
        let payload = self.0.before_payload(payload).await?;
        *request.body_mut() = serde_json::to_vec(&payload)?;
        request.extensions_mut().insert(self);
        Ok(())
    }

    /// Extracts sanitized metadata without retaining a response-body borrow.
    pub fn response_metadata<B>(
        response: &Response<B>,
    ) -> ProviderResponseMetadata {
        ProviderResponseMetadata {
            status: response.status().as_u16(),
            headers: ProviderHeaderVisibility::Response
                .project(response.headers()),
        }
    }

    /// Notifies the attached hook with already detached response metadata.
    pub async fn observe_response(
        &self,
        metadata: ProviderResponseMetadata,
    ) -> Result<(), CompletionError> {
        self.0.after_response(metadata).await.map_err(Into::into)
    }
}

/// Serializes one provider-native request before transport hooks are attached.
pub async fn prepare_json_request<T: Serialize>(
    request: &T,
    hooks: Option<Arc<dyn ModelRequestHooks>>,
) -> Result<(Vec<u8>, Option<PreparedCompletionHooks>), CompletionError> {
    let payload = serde_json::to_vec(request)?;
    let prepared = hooks.map(PreparedCompletionHooks);
    Ok((payload, prepared))
}

/// Security projection used for request mutation and response observation.
#[derive(Debug, Clone, Copy)]
enum ProviderHeaderVisibility {
    /// Headers that extensions may inspect before sending one request.
    Request,
    /// Non-secret diagnostics that extensions may observe from one response.
    Response,
}

impl ProviderHeaderVisibility {
    /// Returns whether one normalized header belongs to this explicit safe set.
    fn allows(self, name: &HeaderName) -> bool {
        let name = name.as_str();
        match self {
            Self::Request => matches!(
                name,
                "accept"
                    | "accept-encoding"
                    | "content-type"
                    | "user-agent"
                    | "anthropic-version"
                    | "anthropic-beta"
                    | "openai-organization"
                    | "openai-project"
            ),
            Self::Response => {
                matches!(
                    name,
                    "content-type"
                        | "content-length"
                        | "date"
                        | "retry-after"
                        | "request-id"
                        | "x-request-id"
                        | "cf-ray"
                ) || name.starts_with("x-ratelimit-")
                    || name.starts_with("ratelimit-")
                    || name.starts_with("anthropic-ratelimit-")
            }
        }
    }

    /// Projects one HeaderMap into normalized UTF-8 values allowed by this policy.
    fn project(self, headers: &HeaderMap) -> ModelHeaders {
        headers
            .iter()
            .filter(|(name, _value)| self.allows(name))
            .filter_map(|(name, value)| {
                value.to_str().ok().map(|value| {
                    (name.as_str().to_ascii_lowercase(), value.to_string())
                })
            })
            .collect()
    }
}

/// Separates extension-visible request headers from immutable provider data.
struct ProviderHeaderPartition {
    visible: ModelHeaders,
    protected: HeaderMap,
}

impl From<&HeaderMap> for ProviderHeaderPartition {
    /// Partitions one request with a deny-by-default policy for existing headers.
    fn from(headers: &HeaderMap) -> Self {
        let visibility = ProviderHeaderVisibility::Request;
        let visible = visibility.project(headers);
        let protected = headers
            .iter()
            .filter(|(name, _value)| !visibility.allows(name))
            .fold(HeaderMap::new(), |mut protected, (name, value)| {
                protected.append(name.clone(), value.clone());
                protected
            });
        Self { visible, protected }
    }
}

impl ProviderHeaderPartition {
    /// Applies extension headers before restoring every immutable provider value.
    fn apply(
        self,
        replacement: ModelHeaders,
        headers: &mut HeaderMap,
    ) -> Result<(), CompletionError> {
        headers.clear();
        for (name, value) in replacement {
            let name = HeaderName::try_from(name).map_err(|error| {
                CompletionError::RequestError(Box::new(error))
            })?;
            let value = HeaderValue::try_from(value).map_err(|error| {
                CompletionError::RequestError(Box::new(error))
            })?;
            headers.insert(name, value);
        }
        // Protected values are merged last so a hook cannot replace an
        // unknown provider credential merely by guessing its header name.
        let protected_names =
            self.protected.keys().cloned().collect::<Vec<_>>();
        for name in protected_names {
            headers.remove(name);
        }
        headers.extend(self.protected);
        Ok(())
    }
}

/// Maps a shared hook failure into the provider completion error boundary.
impl From<ModelHookError> for CompletionError {
    /// Classifies hook failures as request-local provider errors.
    fn from(error: ModelHookError) -> Self {
        Self::ResponseError(error.to_string())
    }
}
