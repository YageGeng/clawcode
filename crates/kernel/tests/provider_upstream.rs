use std::sync::Arc;

use futures::StreamExt;
use kernel::{ModelFactory, ProviderModelFactory};
use protocol::{ContentBlock, MessageContent, ModelStreamEvent};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[path = "support/provider_server.rs"]
mod provider_server;
use provider_server::Server;

/// Completion metadata comes from finish while deltas stay incremental.
#[tokio::test]
async fn production_stream_retains_native_message_and_usage() {
    let body = concat!(
        "data: {\"id\":\"r\",\"object\":\"chat.completion.chunk\",\"model\":\"audit\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"r\",\"object\":\"chat.completion.chunk\",\"model\":\"audit\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"total_tokens\":15}}\n\n",
        "data: [DONE]\n\n"
    );
    let server = Server::start(vec![(200, body.into())]).await;
    let model = server.factory(false).create().unwrap();
    model.preflight().await.unwrap();
    assert!(server.requests().is_empty());
    let events = model
        .stream(Server::request("hi"), CancellationToken::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    let final_ = events
        .into_iter()
        .find_map(|event| match event.unwrap() {
            ModelStreamEvent::Finished(final_) => Some(final_),
            _ => None,
        })
        .unwrap();
    assert_eq!(final_.usage.as_ref().input_tokens, Some(12));
    assert_eq!(final_.model_message.unwrap().text(), "Hello");
}

/// A provider's corrected terminal text remains authoritative over earlier deltas.
#[tokio::test]
async fn final_native_response_corrects_streamed_text() {
    let item = json!({"id":"msg","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Hello","annotations":[]}]});
    let response = json!({"id":"r","object":"response","status":"completed","created_at":0,"model":"audit","output":[item.clone()],"tools":[]});
    let events = [
        json!({"type":"response.output_item.added","output_index":0,"sequence_number":0,"item":{"id":"msg","type":"message","role":"assistant","status":"in_progress","content":[]}}),
        json!({"type":"response.output_text.delta","output_index":0,"item_id":"msg","content_index":0,"sequence_number":1,"delta":"Bye"}),
        json!({"type":"response.output_item.done","output_index":0,"sequence_number":2,"item":item}),
        json!({"type":"response.completed","sequence_number":3,"response":response}),
    ];
    let body = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect();
    let server = Server::start(vec![(200, body)]).await;
    let model = server.factory(true).create().unwrap();
    let mut stream = model
        .stream(Server::request("hi"), CancellationToken::new())
        .await
        .unwrap();
    let mut text = String::new();
    let mut final_message = None;
    while let Some(event) = stream.next().await {
        match event.unwrap() {
            ModelStreamEvent::TextDelta(delta) => text.push_str(&delta),
            ModelStreamEvent::Finished(final_) => {
                final_message = final_.model_message
            }
            _ => {}
        }
    }
    assert_eq!(text, "Bye");
    assert_eq!(final_message.unwrap().text(), "Hello");
}

/// Fresh catalogs are separate session bindings over the same configured definitions.
#[test]
fn model_catalogs_create_distinct_session_handles() {
    let config: config::AppConfig = toml::from_str(
        r#"
active_model="openai/audit"
[[providers]]
id="openai"
display_name="audit"
provider_type="responses"
base_url="https://provider.invalid"
api_key="synthetic-key"
[[providers.models]]
id="audit"
context_tokens=32768
max_output_tokens=128
"#,
    )
    .unwrap();
    let factory = ProviderModelFactory::from_config(
        config::ConfigHandle::from_config(config),
    );
    let first = factory.create_catalog(&Default::default()).unwrap();
    let second = factory.create_catalog(&Default::default()).unwrap();
    assert!(!Arc::ptr_eq(&first.active(), &second.active()));
    assert!(Arc::ptr_eq(
        &first.active(),
        &first.resolve("openai", "audit").unwrap()
    ));
}

/// Inclusive usage counts cache once, and explicit zero remains different from unknown.
#[test]
fn context_usage_preserves_cache_subsets_and_unknown_counts() {
    for (usage, expected) in [
        (
            protocol::Usage::new()
                .input_tokens(1000)
                .output_tokens(200)
                .cached_input_tokens(600)
                .cache_creation_input_tokens(100),
            Some(1200),
        ),
        (
            protocol::Usage::new()
                .input_tokens(0)
                .output_tokens(0)
                .total_tokens(0),
            Some(0),
        ),
        (protocol::Usage::new(), None),
    ] {
        let mut message = Server::request("history").messages.remove(0);
        message.content = MessageContent::Assistant {
            blocks: vec![ContentBlock::Text {
                text: "some history for estimation".into(),
            }],
            metadata: protocol::AssistantMetadata::builder()
                .provider_id("openai".into())
                .model_id("audit".into())
                .stop_reason(protocol::StopReason::EndTurn)
                .usage(usage.into())
                .build(),
            model_message: None,
        };
        let estimate = kernel::ContextUsageEstimate::from_history(&[message]);
        match expected {
            Some(tokens) => {
                assert_eq!(estimate.tokens, tokens);
                assert_eq!(estimate.last_usage_index, Some(0));
            }
            None => {
                assert!(estimate.tokens > 0);
                assert_eq!(estimate.last_usage_index, None);
            }
        }
    }
}

/// Idle timeout applies before the first frame and between frames; cancellation exits either wait.
#[tokio::test]
async fn idle_timeout_and_cancellation_interrupt_network_waits() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for (first_frame, cancel) in
        [(false, false), (true, false), (false, true), (true, true)]
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = [0; 8192];
            let count = socket.read(&mut request).await.expect("request");
            assert!(count > 0, "HTTP request started");
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").await.expect("headers");
            if first_frame {
                socket.write_all(b"data: {\"id\":\"r\",\"object\":\"chat.completion.chunk\",\"model\":\"audit\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n").await.expect("first frame");
            }
            ready_tx.send(()).expect("reader");
            std::future::pending::<()>().await;
        });
        let config: config::AppConfig = toml::from_str(&format!(
            r#"
active_model="openai/audit"
[retry.provider]
timeout_ms={timeout}
[[providers]]
id="openai"
display_name="audit"
provider_type="openai-completions"
base_url="http://{address}"
api_key="synthetic"
[[providers.models]]
id="audit"
context_tokens=32768
max_output_tokens=128
"#,
            timeout = if cancel { 0 } else { 30 }
        ))
        .expect("config");
        let model = kernel::ProviderModelFactory::from_config(
            config::ConfigHandle::from_config(config),
        )
        .create()
        .expect("model");
        let cancellation = CancellationToken::new();
        let stream = model
            .stream(Server::request("hi"), cancellation.clone())
            .await
            .expect("lazy stream");
        let reader =
            tokio::spawn(async move { stream.collect::<Vec<_>>().await });
        ready_rx.await.expect("HTTP started");
        if cancel {
            cancellation.cancel();
        }
        let events =
            tokio::time::timeout(std::time::Duration::from_secs(2), reader)
                .await
                .expect("bounded wait")
                .expect("reader");
        assert!(events.iter().any(Result::is_err));
        if cancel {
            assert!(events.iter().any(|event| matches!(
                event,
                Err(kernel::ModelError::Cancelled)
            )));
        }
        task.abort();
    }
}
