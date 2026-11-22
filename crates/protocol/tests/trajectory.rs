//! Trajectory protocol: request snapshots, content hashes, and page types.

use protocol::{
    AcpExtensionMethod, AgentMessage, AssistantMetadata, ContentBlock,
    ContentHash, MessageContent, MessageId, MessageIdentity, MessageTiming,
    ModelProfile, ModelRequest, ModelRequestOptions, ModelUsage, PromptCache,
    PromptCacheRetention, RequestContentKind, RequestOptions, RequestRecord,
    RequestSnapshot, RunId, RunRef, StopReason, ThinkingLevel, TimestampMs,
    ToolCallId, ToolDefinition, ToolRecord, TrajectoryCursor,
    TrajectoryParameters, TrajectoryRecord, TrajectoryRecordKind, TurnId,
};

/// Builds one message with fixed identity and timing.
fn message(id: &str, content: MessageContent) -> AgentMessage {
    let at = TimestampMs::from(100);
    AgentMessage {
        identity: MessageIdentity {
            message_id: MessageId::try_from(id).expect("message id"),
            turn_id: TurnId::try_from("turn").expect("turn id"),
        },
        timing: MessageTiming::try_from((at, at, at)).expect("timing"),
        content,
    }
}

/// Builds a request with a two-block system prompt, one user message, and
/// one tool described by `description`.
fn request(description: &str) -> ModelRequest {
    let text = |text: &str| ContentBlock::Text {
        text: text.to_string(),
    };
    ModelRequest {
        messages: vec![
            message(
                "system",
                MessageContent::System {
                    blocks: vec![text("a"), text("b")],
                },
            ),
            message(
                "user",
                MessageContent::User {
                    blocks: vec![text("hi")],
                },
            ),
        ],
        tools: vec![ToolDefinition {
            name: "read".to_string(),
            description: description.to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }],
        options: ModelRequestOptions::builder()
            .thinking_level(ThinkingLevel::High)
            .prompt_cache(Some(PromptCache {
                retention: PromptCacheRetention::Short,
                key: "k".to_string(),
            }))
            .build(),
    }
}

/// The model profile every capture in this file uses.
fn profile() -> ModelProfile {
    ModelProfile::builder()
        .provider_id("fixture".to_string())
        .model_id("model-a".to_string())
        .display_name("Model A".to_string())
        .context_tokens(8_192)
        .max_output_tokens(1_024)
        .build()
}

/// A snapshot with fixed hashes for round-trip tests.
fn snapshot() -> RequestSnapshot {
    RequestSnapshot::builder()
        .system_prompt(ContentHash::of(b"prompt"))
        .tools(ContentHash::of(b"tools"))
        .options(
            RequestOptions::builder()
                .provider_id("fixture".to_string())
                .model_id("model-a".to_string())
                .thinking_level(ThinkingLevel::Off)
                .build(),
        )
        .message_count(3)
        .build()
}

/// Content hash is sha256 hex.
#[test]
fn content_hash_is_sha256_hex() {
    assert_eq!(
        ContentHash::of(b"abc").as_str(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    ContentHash::try_from("xyz".to_string()).expect_err("not a digest");
}

/// Capture hashes what the request sends.
#[test]
fn capture_hashes_what_the_request_sends() {
    let profile = profile();
    let capture = request("Reads a file").capture(&profile).expect("capture");

    assert_eq!(capture.snapshot.system_prompt, ContentHash::of(b"a\nb"));
    assert_eq!(capture.snapshot.message_count, 1);
    assert_eq!(capture.snapshot.options.model_id, profile.model_id);
    assert_eq!(
        capture.snapshot.options.prompt_cache,
        Some(PromptCacheRetention::Short)
    );
    assert_eq!(capture.contents[0].kind, RequestContentKind::SystemPrompt);
    assert_eq!(capture.contents[0].content, "a\nb");
    assert_eq!(capture.contents[1].kind, RequestContentKind::Tools);
    let tools: Vec<ToolDefinition> =
        serde_json::from_str(&capture.contents[1].content).expect("tools");
    assert_eq!(tools, request("Reads a file").tools);
    assert_eq!(
        capture.contents[1].hash,
        ContentHash::of(capture.contents[1].content.as_bytes())
    );
}

/// Capture changes only with the tools.
#[test]
fn capture_changes_only_with_the_tools() {
    let profile = profile();
    let first = request("Reads a file").capture(&profile).expect("capture");
    let second = request("Reads any file")
        .capture(&profile)
        .expect("capture");

    assert_ne!(first.snapshot.tools, second.snapshot.tools);
    assert_eq!(first.snapshot.system_prompt, second.snapshot.system_prompt);
}

/// Assistant metadata without request round trips.
#[test]
fn assistant_metadata_without_request_round_trips() {
    let metadata = AssistantMetadata::builder()
        .provider_id("fixture".to_string())
        .model_id("model-a".to_string())
        .stop_reason(StopReason::EndTurn)
        .usage(ModelUsage::default())
        .build();
    let json = serde_json::to_value(&metadata).expect("json");

    assert!(json.get("request").is_none());
    let back: AssistantMetadata =
        serde_json::from_value(json).expect("metadata");
    assert_eq!(back.request, None);

    let with_request = AssistantMetadata {
        request: Some(Box::new(snapshot())),
        ..metadata
    };
    let json = serde_json::to_string(&with_request).expect("json");
    let back: AssistantMetadata = serde_json::from_str(&json).expect("back");
    assert_eq!(back, with_request);
}

/// Trajectory parameters reject two cursors.
#[test]
fn trajectory_parameters_reject_two_cursors() {
    let both: TrajectoryParameters =
        serde_json::from_str(r#"{"sessionId":"s","before":3,"after":5}"#)
            .expect("parameters");
    both.cursor().expect_err("two cursors");

    let tail: TrajectoryParameters =
        serde_json::from_str(r#"{"sessionId":"s"}"#).expect("parameters");
    assert_eq!(tail.cursor().expect("cursor"), TrajectoryCursor::Tail);

    let after: TrajectoryParameters =
        serde_json::from_str(r#"{"sessionId":"s","after":5,"limit":10}"#)
            .expect("parameters");
    assert_eq!(after.cursor().expect("cursor"), TrajectoryCursor::After(5));

    serde_json::from_str::<TrajectoryParameters>(
        r#"{"sessionId":"s","page":1}"#,
    )
    .expect_err("unknown field");
}

/// Trajectory record round trips.
#[test]
fn trajectory_record_round_trips() {
    let run = RunRef {
        id: RunId::try_from("run-1").expect("run id"),
        index: 1,
    };
    let tool = TrajectoryRecord::builder()
        .id("tool:call-1".to_string())
        .sequence(4)
        .run(Some(run.clone()))
        .started_at_ms(Some(TimestampMs::from(10)))
        .kind(TrajectoryRecordKind::Tool(
            ToolRecord::builder()
                .call_id(ToolCallId::try_from("call-1").expect("call id"))
                .name("read".to_string())
                .arguments(serde_json::json!({"path": "a.rs"}))
                .result_text(Some("fn main() {}".to_string()))
                .build(),
        ))
        .build();
    let request = TrajectoryRecord::builder()
        .id("request:entry-1".to_string())
        .sequence(3)
        .run(Some(run))
        .kind(TrajectoryRecordKind::Request(
            RequestRecord::builder()
                .index(1)
                .snapshot(snapshot())
                .first(true)
                .build(),
        ))
        .build();

    for record in [tool, request] {
        let json = serde_json::to_string(&record).expect("json");
        let back: TrajectoryRecord = serde_json::from_str(&json).expect("back");
        assert_eq!(back, record);
    }
    let json = serde_json::to_value(TrajectoryRecordKind::Tool(
        ToolRecord::builder()
            .call_id(ToolCallId::try_from("c").expect("call id"))
            .name("read".to_string())
            .arguments(serde_json::Value::Null)
            .build(),
    ))
    .expect("json");
    assert_eq!(json["type"], "tool");
}

/// Trajectory methods are queries.
#[test]
fn trajectory_methods_are_queries() {
    for (method, name) in [
        (
            AcpExtensionMethod::Trajectory,
            "_clawcode/session/trajectory",
        ),
        (
            AcpExtensionMethod::TrajectoryContent,
            "_clawcode/session/trajectory/content",
        ),
    ] {
        assert_eq!(method.as_str(), name);
        assert_eq!(AcpExtensionMethod::parse(name), Some(method));
        assert!(method.is_query());
    }
}

/// Content kind names match serde.
#[test]
fn content_kind_names_match_serde() {
    for kind in [RequestContentKind::SystemPrompt, RequestContentKind::Tools] {
        assert_eq!(serde_json::to_value(kind).expect("json"), kind.as_str());
        assert_eq!(kind.as_str().parse::<RequestContentKind>(), Ok(kind));
    }
    "prompt"
        .parse::<RequestContentKind>()
        .expect_err("unknown kind");
}

/// Queue kind names match serde.
#[test]
fn queue_kind_names_match_serde() {
    use protocol::QueueKind;
    for kind in [QueueKind::Steering, QueueKind::FollowUp] {
        assert_eq!(serde_json::to_value(kind).expect("json"), kind.as_str());
        assert_eq!(kind.as_str().parse::<QueueKind>(), Ok(kind));
    }
    "next_run".parse::<QueueKind>().expect_err("unknown kind");
}
