//! Records the outgoing HTTP request of one model call without network access.

use rig_core::test_utils::{CapturedHttpRequest, RecordingHttpClient};

/// Sends one prepared request through a configured model and returns what went out.
pub trait SentRequest {
    /// Connects over a recording client, calls once, and returns the recorded request.
    async fn sent_request(&self) -> CapturedHttpRequest;
}

impl SentRequest for provider::ConfiguredModel {
    async fn sent_request(&self) -> CapturedHttpRequest {
        let capture = RecordingHttpClient::new("{}");
        let model = self
            .connect(rig_core::http_client::DynHttpClient::new(capture.clone()))
            .await
            .expect("connect");
        // The canned body is not a valid reply; only the outgoing request matters here.
        let _reply =
            model
                .call(self.prepare(
                    rig_core::completion::CompletionRequest::new("hi"),
                ))
                .await;
        capture.requests().into_iter().next().expect("one request")
    }
}
