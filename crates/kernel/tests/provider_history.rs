use std::sync::Arc;

use futures::StreamExt;
use kernel::ModelFactory;
use protocol::{
    AgentMessage, AssistantMetadata, ContentBlock, EntryId, LaneId,
    MessageContent, MessageId, ModelUsage, SessionId, StopReason,
};
use rig_core::message::Message;
use serde_json::json;
use store::{
    EntryKind, JsonlStoreFactory, NewEntry, SessionCreateOptions,
    SessionForkOptions, StoreFactory, SystemClock,
};
use tokio_util::sync::CancellationToken;

#[path = "support/provider_server.rs"]
mod provider_server;
use provider_server::Server;

/// Mixed historical and native records survive reopen/fork and preserve encrypted replay data.
#[tokio::test]
async fn native_and_legacy_history_survives_reopen_fork_and_provider_replay() {
    let directory = tempfile::tempdir().unwrap();
    let factory =
        JsonlStoreFactory::new(directory.path(), Arc::new(SystemClock));
    let session = SessionId::try_from("history").unwrap();
    let lane = LaneId::try_from("main").unwrap();
    let mut store = factory
        .create(SessionCreateOptions {
            session_id: session.clone(),
            cwd: directory.path().into(),
            parent_session_id: None,
        })
        .unwrap();
    // Decode a complete provider reply so native grouping and provenance match upstream.
    let wire = rig_core::providers::openai::responses_api::wire::Responses::new(
        rig_core::providers::openai::OpenAIConfig::new("synthetic"),
        "audit",
    );
    let reply=rig_core::test_utils::history::decode(&wire,rig_core::wire::Mode::Unary,[rig_core::wire::WireFrame::Text(json!({
        "id":"old","object":"response","status":"completed","model":"audit","output":[
            {"id":"rs","type":"reasoning","summary":[{"type":"summary_text","text":"thought"}],"encrypted_content":"opaque-ciphertext"},
            {"id":"msg-old","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"old answer","annotations":[]}]}
        ]
    }).to_string())]).unwrap();
    let Some(Message::Assistant(native)) = reply.message() else {
        panic!("assistant reply")
    };
    let mut message = Server::request("seed").messages.remove(0);
    message.identity.message_id = MessageId::try_from("native").unwrap();
    message.content = MessageContent::Assistant {
        blocks: vec![
            ContentBlock::Reasoning {
                text: "thought".into(),
            },
            ContentBlock::Text {
                text: "old answer".into(),
            },
        ],
        metadata: AssistantMetadata::builder()
            .provider_id("openai".into())
            .model_id("audit".into())
            .stop_reason(StopReason::EndTurn)
            .usage(
                protocol::Usage::new()
                    .input_tokens(37)
                    .output_tokens(3)
                    .total_tokens(40)
                    .cached_input_tokens(10)
                    .cache_creation_input_tokens(20)
                    .into(),
            )
            .build(),
        model_message: Some(Box::new(native.clone())),
    };
    let mut legacy = serde_json::to_value(&message).unwrap();
    legacy["message_id"] = json!("legacy");
    legacy["content"]["model_message"] = serde_json::Value::Null;
    legacy["content"]["blocks"] =
        json!([{"type":"text","text":"legacy answer"}]);
    legacy["content"]["metadata"]["usage"] = json!({"input_tokens":"7","output_tokens":"3","total_tokens":"40","cache_read_tokens":"10","cache_write_tokens":"20"});
    store
        .append_entry(
            &lane,
            NewEntry {
                id: EntryId::try_from("legacy").unwrap(),
                kind: EntryKind::Message,
                payload: legacy,
            },
        )
        .unwrap();
    let leaf = EntryId::try_from("native").unwrap();
    store
        .append_entry(
            &lane,
            NewEntry {
                id: leaf.clone(),
                kind: EntryKind::Message,
                payload: serde_json::to_value(&message).unwrap(),
            },
        )
        .unwrap();
    store.sync().unwrap();
    let path = store.path().to_path_buf();
    let original = std::fs::read(&path).unwrap();
    drop(store);
    let reopened = factory.open(&path).unwrap();
    let messages = reopened
        .branch(&leaf)
        .unwrap()
        .into_iter()
        .map(|entry| {
            serde_json::from_value::<AgentMessage>(serde_json::Value::Object(
                entry.payload,
            ))
            .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let MessageContent::Assistant { metadata, .. } = &messages[0].content
    else {
        panic!("legacy assistant")
    };
    assert_eq!(metadata.usage.as_ref().input_tokens, Some(37));
    let MessageContent::Assistant { model_message, .. } = &messages[1].content
    else {
        panic!("native assistant")
    };
    assert_eq!(model_message.as_deref(), Some(&native));
    let fork = factory
        .fork(
            &path,
            SessionForkOptions {
                create: SessionCreateOptions {
                    session_id: SessionId::try_from("fork").unwrap(),
                    cwd: directory.path().into(),
                    parent_session_id: Some(session),
                },
                source_leaf: leaf.clone(),
                lane,
            },
        )
        .unwrap();
    let forked: Vec<AgentMessage> = fork
        .branch(&leaf)
        .unwrap()
        .into_iter()
        .map(|entry| {
            serde_json::from_value(serde_json::Value::Object(entry.payload))
                .unwrap()
        })
        .collect();
    assert_eq!(forked, messages);
    assert_eq!(std::fs::read(&path).unwrap(), original);

    let response = json!({"type":"response.completed","sequence_number":0,"response":{"id":"r","object":"response","status":"completed","created_at":0,"model":"audit","tools":[],"output":[{"id":"msg","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"new answer","annotations":[]}]}]}});
    let server =
        Server::start(vec![(200, format!("data: {response}\n\n"))]).await;
    let model = server.factory(true).create().unwrap();
    let mut request = Server::request("continue");
    request.messages.splice(0..0, forked);
    for item in model
        .stream(request, CancellationToken::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await
    {
        item.unwrap();
    }
    assert!(
        server.requests()[0]["input"]
            .to_string()
            .contains("opaque-ciphertext")
    );
}

/// A future storage version fails with the entry identity and never rewrites the source file.
#[tokio::test]
async fn unknown_usage_version_is_located_during_session_resume() {
    let directory = tempfile::tempdir().unwrap();
    let factory =
        JsonlStoreFactory::new(directory.path(), Arc::new(SystemClock));
    let session = SessionId::try_from("future").unwrap();
    let lane = LaneId::try_from("main").unwrap();
    let mut store = factory
        .create(SessionCreateOptions {
            session_id: session.clone(),
            cwd: directory.path().into(),
            parent_session_id: None,
        })
        .unwrap();
    let mut message = Server::request("seed").messages.remove(0);
    message.content = MessageContent::Assistant {
        blocks: vec![ContentBlock::Text { text: "old".into() }],
        model_message: None,
        metadata: AssistantMetadata::builder()
            .provider_id("openai".into())
            .model_id("audit".into())
            .stop_reason(StopReason::EndTurn)
            .usage(ModelUsage::default())
            .build(),
    };
    let mut value = serde_json::to_value(message).unwrap();
    value["content"]["metadata"]["usage"] =
        json!({"format":"future-v9","usage":{}});
    store
        .append_entry(
            &lane,
            NewEntry {
                id: EntryId::try_from("future-entry").unwrap(),
                kind: EntryKind::Message,
                payload: value,
            },
        )
        .unwrap();
    store.sync().unwrap();
    let path = store.path().to_path_buf();
    let before = std::fs::read(&path).unwrap();
    drop(store);
    let server = Server::start(Vec::new()).await;
    let kernel =
        server.kernel(directory.path(), Default::default(), Default::default());
    let error = kernel
        .resume_session(session, directory.path().into())
        .await
        .expect_err("unknown version must fail");
    let message = error.to_string();
    assert!(
        message.contains("future-entry") && message.contains("future-v9"),
        "{message}"
    );
    assert_eq!(std::fs::read(path).unwrap(), before);
}

/// Native final messages pass normal settlement, persistence, and the next real request.
#[tokio::test]
async fn kernel_settles_and_reopens_native_provider_messages() {
    let directory = tempfile::tempdir().expect("history directory");
    let response = json!({"type":"response.completed","sequence_number":0,"response":{"id":"r","object":"response","status":"completed","created_at":0,"model":"audit","tools":[],"output":[{"id":"msg","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"native answer","annotations":[]}]}]}});
    let body = format!("data: {response}\n\n");
    let server = Server::start(vec![(200, body.clone()), (200, body)]).await;
    let kernel =
        server.kernel(directory.path(), Default::default(), Default::default());
    let session = SessionId::try_from("settlement").expect("session ID");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session.clone(),
            cwd: directory.path().into(),
            parent_session_id: None,
        })
        .await
        .expect("create session");
    let (sender, _receiver) = tokio::sync::mpsc::channel(1024);
    let sink: Arc<dyn kernel::EventSink> =
        Arc::new(kernel::ChannelEventSink::new(sender));
    let result = kernel
        .run(
            protocol::RunRequest {
                session_id: session.clone(),
                input: "first".into(),
            },
            Arc::clone(&sink),
        )
        .await
        .expect("native settlement");
    assert!(result.messages.iter().any(|message| matches!(&message.content, MessageContent::Assistant { model_message:Some(native),.. } if native.text()=="native answer")));
    kernel.close_session(&session).await.expect("close");
    kernel
        .resume_session(session.clone(), directory.path().into())
        .await
        .expect("resume");
    kernel
        .run(
            protocol::RunRequest {
                session_id: session,
                input: "next".into(),
            },
            sink,
        )
        .await
        .expect("continued native history");
    assert!(
        server.requests()[1]["input"]
            .to_string()
            .contains("native answer")
    );
}

/// Both dialects retain their own signed/native reasoning on the next serialized request.
#[tokio::test]
async fn anthropic_and_deepseek_native_reasoning_replays() {
    let anthropic = [
        json!({"type":"message_start","message":{"id":"m","type":"message","role":"assistant","model":"audit","content":[],"stop_reason":null,"usage":{"input_tokens":10,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"native thought"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"synthetic-signature"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"answer"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":2}}),
        json!({"type":"message_stop"}),
    ].iter().map(|event|format!("event: {}\ndata: {event}\n\n",event["type"].as_str().expect("event type"))).collect::<String>();
    let deepseek = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"id":"m","object":"chat.completion.chunk","model":"audit","choices":[{"index":0,"delta":{"reasoning_content":"native thought","content":"answer"},"finish_reason":"stop"}]})
    );
    for (id, protocol, body, marker) in [
        ("anthropic", "anthropic", anthropic, "synthetic-signature"),
        (
            "deepseek",
            "openai-completions",
            deepseek,
            "reasoning_content",
        ),
    ] {
        let server =
            Server::start(vec![(200, body.clone()), (200, body)]).await;
        let factory = kernel::ProviderModelFactory::from_config(
            config::ConfigHandle::from_config(
                server.configuration(id, protocol),
            ),
        );
        let model = factory.create().expect("model");
        let mut stream = model
            .stream(Server::request("first"), CancellationToken::new())
            .await
            .expect("stream");
        let mut final_ = None;
        while let Some(item) = stream.next().await {
            if let protocol::ModelStreamEvent::Finished(value) =
                item.expect("event")
            {
                final_ = Some(value);
            }
        }
        let final_ = final_.expect("final response");
        let native = final_.model_message.expect("native message");
        let mut assistant = Server::request("placeholder").messages.remove(0);
        assistant.content = MessageContent::Assistant {
            blocks: vec![
                ContentBlock::Reasoning {
                    text: "native thought".into(),
                },
                ContentBlock::Text {
                    text: "answer".into(),
                },
            ],
            metadata: AssistantMetadata::builder()
                .provider_id(id.into())
                .model_id("audit".into())
                .stop_reason(final_.stop_reason)
                .usage(final_.usage)
                .build(),
            model_message: Some(native),
        };
        let mut next = Server::request("next");
        next.messages.insert(0, assistant);
        for item in model
            .stream(next, CancellationToken::new())
            .await
            .expect("next request")
            .collect::<Vec<_>>()
            .await
        {
            item.expect("next event");
        }
        assert!(
            server.requests()[1]["messages"]
                .to_string()
                .contains(marker),
            "{id} lost native reasoning"
        );
    }
}

/// Compares typed upstream refusal accounting with the persisted project attempt.
#[tokio::test]
async fn anthropic_refusal_preserves_reported_usage() {
    let frames = [
        json!({"type":"message_start","message":{"id":"m","type":"message","role":"assistant","model":"audit","content":[],"stop_reason":null,"usage":{"input_tokens":10,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"declined"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"refusal","stop_sequence":null,"stop_details":{"type":"refusal","explanation":"PRIVATE_REFUSAL_SENTINEL"}},"usage":{"output_tokens":2}}),
        json!({"type":"message_stop"}),
    ];
    let body = frames
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect();
    let server = Server::start(vec![(200, body)]).await;
    let config = config::ConfigHandle::from_config(
        server.configuration("anthropic", "anthropic"),
    );
    let definition = provider::ProviderFactory::new(config.clone())
        .resolve("anthropic", "audit")
        .unwrap();
    let provider::ProviderCompletion::Anthropic(native_model) =
        definition.build_model(None).unwrap()
    else {
        panic!("Anthropic")
    };
    let native = rig_core::test_utils::history::decode(
        &native_model.wire,
        rig_core::wire::Mode::Streaming,
        frames
            .iter()
            .map(|event| rig_core::wire::WireFrame::Text(event.to_string())),
    )
    .unwrap();
    assert_eq!(native.usage.total_tokens, Some(12));
    let directory = tempfile::tempdir().unwrap();
    let clock: Arc<dyn store::Clock> = Arc::new(store::SystemClock);
    let kernel = kernel::KernelFactory::builder()
        .model_factory(Arc::new(kernel::ProviderModelFactory::from_config(
            config,
        )))
        .tool_factory(Arc::new(tools::BuiltinToolFactory::new()))
        .store_factory(Arc::new(store::JsonlStoreFactory::new(
            directory.path(),
            Arc::clone(&clock),
        )))
        .extension_factory(Arc::new(
            extension::StaticExtensionFactory::default(),
        ))
        .prompt_factory(Arc::new(prompt::FilesystemPromptFactory::new(
            directory.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .clock(clock)
        .id_generator(Arc::new(kernel::NanoidIdGenerator))
        .build()
        .build()
        .unwrap();
    let session = SessionId::try_from("anthropic").unwrap();
    kernel
        .create_session(store::SessionCreateOptions {
            session_id: session.clone(),
            cwd: directory.path().into(),
            parent_session_id: None,
        })
        .await
        .unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1024);
    let run = kernel
        .run(
            protocol::RunRequest {
                session_id: session.clone(),
                input: "test".into(),
            },
            Arc::new(kernel::ChannelEventSink::new(sender)),
        )
        .await
        .unwrap();
    let metadata = run
        .messages
        .iter()
        .find_map(|message| match &message.content {
            MessageContent::Assistant { metadata, .. } => Some(metadata),
            _ => None,
        })
        .unwrap();
    assert_eq!(metadata.usage.as_ref().total_tokens, Some(12));
    // Refusal explanations are untrusted text; only the typed category reaches diagnostics.
    let error = metadata.error.as_deref().expect("failure summary");
    assert!(error.contains("content_filter"), "{error}");
    assert!(!error.contains("PRIVATE_REFUSAL_SENTINEL"));
    assert!(matches!(
        run.turns.last().expect("turn").outcome,
        protocol::TurnOutcome::Failed { .. }
    ));
    let settled = std::iter::from_fn(|| receiver.try_recv().ok())
        .find_map(|event| match event.payload {
            protocol::AgentEventPayload::AgentSettled { usage, .. } => {
                Some(usage)
            }
            _ => None,
        })
        .expect("run accounting");
    assert_eq!(settled.as_ref().total_tokens, Some(12));
    assert!(kernel.session_messages(&session).expect("stored history").iter().any(|message| matches!(&message.content,MessageContent::Assistant {metadata,..} if metadata.usage.as_ref().total_tokens==Some(12))));
}
