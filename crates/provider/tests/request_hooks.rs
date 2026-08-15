use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use provider::completion::{
    CompletionError, CompletionRequestHooks, ProviderHeaders,
    ProviderResponseMetadata, prepare_json_request,
};

#[derive(Default)]
struct RecordingHooks {
    payload_calls: AtomicUsize,
    header_calls: AtomicUsize,
    response_calls: AtomicUsize,
    order: Mutex<Vec<&'static str>>,
}

#[derive(Default)]
struct CredentialProbeHooks {
    visible: Mutex<Option<ProviderHeaders>>,
}

#[async_trait]
impl CompletionRequestHooks for CredentialProbeHooks {
    /// Preserves the request payload while the test exercises header isolation.
    async fn before_payload(
        &self,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, CompletionError> {
        Ok(payload)
    }

    /// Records visible headers and attempts to replace every protected credential.
    async fn before_headers(
        &self,
        mut headers: ProviderHeaders,
    ) -> Result<ProviderHeaders, CompletionError> {
        *self.visible.lock().expect("visible header lock") =
            Some(headers.clone());
        for name in [
            "authorization",
            "x-goog-api-key",
            "x-auth-token",
            "x-custom-credential",
        ] {
            headers.insert(name.to_string(), "replacement".to_string());
        }
        headers.insert("x-extension-trace".to_string(), "trace-1".to_string());
        Ok(headers)
    }

    /// Accepts response metadata because this test only exercises request protection.
    async fn after_response(
        &self,
        _response: ProviderResponseMetadata,
    ) -> Result<(), CompletionError> {
        Ok(())
    }
}

#[async_trait]
impl CompletionRequestHooks for RecordingHooks {
    /// Adds one marker to the provider-native JSON payload.
    async fn before_payload(
        &self,
        mut payload: serde_json::Value,
    ) -> Result<serde_json::Value, CompletionError> {
        self.order.lock().expect("order lock").push("payload");
        self.payload_calls.fetch_add(1, Ordering::Relaxed);
        payload
            .as_object_mut()
            .expect("provider payload object")
            .insert("hooked".to_string(), serde_json::Value::Bool(true));
        Ok(payload)
    }

    /// Replaces one visible header without receiving authentication secrets.
    async fn before_headers(
        &self,
        mut headers: ProviderHeaders,
    ) -> Result<ProviderHeaders, CompletionError> {
        self.order.lock().expect("order lock").push("headers");
        self.header_calls.fetch_add(1, Ordering::Relaxed);
        assert!(!headers.contains_key("authorization"));
        headers.remove("user-agent");
        headers.insert("x-added".to_string(), "yes".to_string());
        Ok(headers)
    }

    /// Records one sanitized transport response.
    async fn after_response(
        &self,
        response: ProviderResponseMetadata,
    ) -> Result<(), CompletionError> {
        self.order.lock().expect("order lock").push("response");
        self.response_calls.fetch_add(1, Ordering::Relaxed);
        assert_eq!(response.status, 202);
        assert_eq!(
            response.headers.get("x-request-id"),
            Some(&"id-1".to_string())
        );
        assert!(!response.headers.contains_key("set-cookie"));
        Ok(())
    }
}

/// Prepared hooks transform final JSON and visible transport metadata without exposing secrets.
#[tokio::test]
async fn prepared_hooks_apply_at_the_transport_boundary() {
    let hooks = Arc::new(RecordingHooks::default());
    let hooks_object: Arc<dyn CompletionRequestHooks> = hooks.clone();
    let (body, prepared) = prepare_json_request(
        &serde_json::json!({ "model": "fixture" }),
        Some(hooks_object),
    )
    .await
    .expect("prepare JSON request");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body)
            .expect("decode body")
            .get("hooked"),
        None
    );

    let mut request = http::Request::builder()
        .header("authorization", "Bearer secret")
        .header("user-agent", "old")
        .body(body)
        .expect("build request");
    let prepared = prepared.expect("prepared hook handle");
    prepared
        .clone()
        .attach(&mut request)
        .await
        .expect("attach hooks");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(request.body())
            .expect("decode hooked body")
            .get("hooked"),
        Some(&serde_json::Value::Bool(true))
    );
    assert_eq!(request.headers()["authorization"], "Bearer secret");
    assert_eq!(request.headers()["x-added"], "yes");
    assert!(!request.headers().contains_key("user-agent"));

    let response = http::Response::builder()
        .status(202)
        .header("x-request-id", "id-1")
        .header("set-cookie", "private")
        .body(())
        .expect("build response");
    prepared
        .observe_response(
            provider::completion::PreparedCompletionHooks::response_metadata(
                &response,
            ),
        )
        .await
        .expect("observe response");

    assert_eq!(hooks.payload_calls.load(Ordering::Relaxed), 1);
    assert_eq!(hooks.header_calls.load(Ordering::Relaxed), 1);
    assert_eq!(hooks.response_calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        *hooks.order.lock().expect("order lock"),
        vec!["headers", "payload", "response"]
    );
}

/// Existing provider credentials are hidden from hooks and cannot be replaced.
#[tokio::test]
async fn provider_credentials_are_hidden_and_immutable() {
    let hooks = Arc::new(CredentialProbeHooks::default());
    let hooks_object: Arc<dyn CompletionRequestHooks> = hooks.clone();
    let (body, prepared) = prepare_json_request(
        &serde_json::json!({ "model": "fixture" }),
        Some(hooks_object),
    )
    .await
    .expect("prepare JSON request");
    let mut request = http::Request::builder()
        .header("authorization", "Bearer original")
        .header("x-goog-api-key", "google-original")
        .header("x-auth-token", "token-original")
        .header("x-custom-credential", "custom-original")
        .body(body)
        .expect("build request");

    prepared
        .expect("prepared hook handle")
        .attach(&mut request)
        .await
        .expect("attach hooks");

    let visible = hooks
        .visible
        .lock()
        .expect("visible header lock")
        .clone()
        .expect("visible headers");
    for name in [
        "authorization",
        "x-goog-api-key",
        "x-auth-token",
        "x-custom-credential",
    ] {
        assert!(!visible.contains_key(name));
    }
    assert_eq!(request.headers()["authorization"], "Bearer original");
    assert_eq!(request.headers()["x-goog-api-key"], "google-original");
    assert_eq!(request.headers()["x-auth-token"], "token-original");
    assert_eq!(request.headers()["x-custom-credential"], "custom-original");
    assert_eq!(request.headers()["x-extension-trace"], "trace-1");
}

/// Provider header maps remain deterministic for ordered extension composition.
#[test]
fn provider_headers_use_stable_lexical_order() {
    let headers = ProviderHeaders::from([
        ("z-last".to_string(), "2".to_string()),
        ("a-first".to_string(), "1".to_string()),
    ]);
    assert_eq!(
        headers.keys().cloned().collect::<Vec<_>>(),
        vec!["a-first", "z-last"]
    );
    assert_eq!(
        BTreeMap::from_iter(headers),
        BTreeMap::from([
            ("a-first".to_string(), "1".to_string()),
            ("z-last".to_string(), "2".to_string()),
        ])
    );
}
