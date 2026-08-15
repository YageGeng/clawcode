use acp::AcpEventMapper;
use agent_client_protocol::schema::v2::SessionUpdate;
use protocol::{
    AgentEvent, AgentEventPayload, AgentMessage, AgentOutcome,
    AssistantMetadata, BashExecutionMessage, ContentBlock, EventMetadata,
    MessageContent, MessageId, MessageIdentity, MessageTiming, ModelUsage,
    ProductIdentity, RunId, Sequence, StopReason, TimestampMs, ToolCallId,
    ToolResult, ToolResultDetails, TurnId, UserBashDisposition, UserBashResult,
};

/// Native ACP chunks retain mandatory Turn and timestamp metadata in `_meta`.
#[test]
fn text_delta_maps_to_native_acp_v2_chunk_with_metadata() {
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-1").expect("turn id"),
            timestamp_ms: TimestampMs::from(9_007_199_254_740_999),
            sequence: Sequence::try_from(3).expect("sequence"),
        },
        payload: AgentEventPayload::MessageTextDelta {
            message_id: MessageId::try_from("message-1").expect("message id"),
            delta: "hello".to_string(),
        },
    };

    let updates = AcpEventMapper::map(event).expect("map event");
    assert_eq!(updates.len(), 1);
    assert!(matches!(
        updates.first(),
        Some(SessionUpdate::AgentMessageChunk(_))
    ));
    let value =
        serde_json::to_value(updates.first()).expect("serialize update");
    assert_eq!(value["sessionUpdate"], "agent_message_chunk");
    assert_eq!(value["content"]["text"], "hello");
    assert_eq!(value["content"]["_meta"]["clawcode"]["turnId"], "turn-1");
    assert_eq!(
        value["content"]["_meta"]["clawcode"]["timestampMs"],
        "9007199254740999"
    );
}

/// Pi lifecycle events without native ACP equivalents use the reserved v2 extension shape.
#[test]
fn turn_start_maps_to_acp_v2_other_session_update() {
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-2").expect("turn id"),
            timestamp_ms: TimestampMs::from(2_000),
            sequence: Sequence::try_from(4).expect("sequence"),
        },
        payload: AgentEventPayload::TurnStart {
            run_id: protocol::RunId::try_from("run-1").expect("run id"),
        },
    };

    let updates = AcpEventMapper::map(event).expect("map event");
    let value =
        serde_json::to_value(updates.first()).expect("serialize update");
    assert_eq!(value["sessionUpdate"], "_clawcode/event");
    assert_eq!(value["payload"]["event"], "turn_start");
    assert_eq!(value["payload"]["run_id"], "run-1");
}

/// User-bash replay preserves typed policy disposition in the ACP extension update.
#[test]
fn user_bash_replay_preserves_blocked_disposition() {
    let turn_id = TurnId::try_from("turn-bash").expect("turn id");
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: turn_id.clone(),
            timestamp_ms: TimestampMs::from(2_100),
            sequence: Sequence::try_from(5).expect("sequence"),
        },
        payload: AgentEventPayload::MessageEnd {
            message: AgentMessage {
                identity: MessageIdentity {
                    message_id: MessageId::try_from("message-bash")
                        .expect("message id"),
                    turn_id,
                },
                timing: MessageTiming::try_from((
                    TimestampMs::from(2_000),
                    TimestampMs::from(2_000),
                    TimestampMs::from(2_100),
                ))
                .expect("message timing"),
                content: MessageContent::BashExecution {
                    bash: BashExecutionMessage::builder()
                        .command("rm -rf /tmp/value".to_string())
                        .result(
                            UserBashResult::builder()
                                .disposition(UserBashDisposition::Blocked {
                                    reason: "policy denied".to_string(),
                                })
                                .output("policy denied".to_string())
                                .cancelled(false)
                                .truncated(false)
                                .build(),
                        )
                        .exclude_from_context(false)
                        .build(),
                },
            },
        },
    };

    let updates = AcpEventMapper::map(event).expect("map bash replay");
    let value =
        serde_json::to_value(updates.first()).expect("serialize update");
    assert_eq!(value["sessionUpdate"], "_clawcode/event");
    assert_eq!(
        value["payload"]["message"]["content"]["bash"]["result"]["disposition"]
            ["type"],
        "blocked"
    );
    assert_eq!(
        value["payload"]["message"]["content"]["bash"]["result"]["disposition"]
            ["reason"],
        "policy denied"
    );
}

/// Session title changes use ACP's native session-info update with string timing metadata.
#[test]
fn title_change_maps_to_native_session_info_update() {
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-title").expect("turn id"),
            timestamp_ms: TimestampMs::from(9_007_199_254_740_999),
            sequence: Sequence::try_from(5).expect("sequence"),
        },
        payload: AgentEventPayload::SessionTitleChanged {
            title: "New title".to_string(),
        },
    };

    let updates = AcpEventMapper::map(event).expect("map event");
    assert!(matches!(
        updates.first(),
        Some(SessionUpdate::SessionInfoUpdate(_))
    ));
    let value = serde_json::to_value(updates.first()).expect("serialize");
    assert_eq!(value["sessionUpdate"], "session_info_update");
    assert_eq!(value["title"], "New title");
    assert_eq!(
        value["_meta"]["clawcode"]["timestampMs"],
        "9007199254740999"
    );
}

/// Complete replay reconstructs text, reasoning, and tool calls with native ACP updates.
#[test]
fn assistant_replay_maps_reasoning_and_tool_calls_without_loss() {
    let turn_id = TurnId::try_from("turn-replay").expect("turn id");
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: turn_id.clone(),
            timestamp_ms: TimestampMs::from(8_000),
            sequence: Sequence::try_from(6).expect("sequence"),
        },
        payload: AgentEventPayload::MessageEnd {
            message: AgentMessage {
                identity: MessageIdentity {
                    message_id: MessageId::try_from("message-replay")
                        .expect("message id"),
                    turn_id,
                },
                timing: MessageTiming::try_from((
                    TimestampMs::from(7_000),
                    TimestampMs::from(7_100),
                    TimestampMs::from(8_000),
                ))
                .expect("message timing"),
                // Assistant metadata is mandatory so replay retains the exact
                // provider outcome together with visible content.
                content: MessageContent::Assistant {
                    blocks: vec![
                        ContentBlock::Reasoning {
                            text: "reasoning".to_string(),
                        },
                        ContentBlock::ToolCall {
                            tool_call_id: ToolCallId::try_from("tool-replay")
                                .expect("tool call id"),
                            name: "read".to_string(),
                            arguments: serde_json::json!({ "path": "README.md" }),
                        },
                    ],
                    metadata: AssistantMetadata::builder()
                        .provider_id("provider".to_string())
                        .model_id("model".to_string())
                        .stop_reason(StopReason::ToolUse)
                        .usage(
                            ModelUsage::builder()
                                .input_tokens(10)
                                .output_tokens(5)
                                .cache_read_tokens(0)
                                .cache_write_tokens(0)
                                .total_tokens(15)
                                .build(),
                        )
                        .build(),
                },
            },
        },
    };

    let values = AcpEventMapper::map(event)
        .expect("map replay")
        .into_iter()
        .map(|update| serde_json::to_value(update).expect("serialize update"))
        .collect::<Vec<_>>();
    assert!(values.iter().all(|value| {
        value["_meta"][ProductIdentity::ACP_NAMESPACE]["messageTiming"]
            ["timestamp_ms"]
            == "7000"
            && value["_meta"][ProductIdentity::ACP_NAMESPACE]
                ["messageTiming"]["started_at_ms"]
                == "7100"
            && value["_meta"][ProductIdentity::ACP_NAMESPACE]
                ["messageTiming"]["ended_at_ms"]
                == "8000"
    }));
    assert!(values.iter().any(|value| {
        value["sessionUpdate"] == "agent_thought"
            && value["content"][0]["text"] == "reasoning"
    }));
    assert!(values.iter().any(|value| {
        value["sessionUpdate"] == "tool_call_update"
            && value["toolCallId"] == "tool-replay"
            && value["title"] == "read"
    }));
}

/// Provider usage maps to ACP v2's native context-window update with exact metadata.
#[test]
fn usage_maps_to_native_acp_v2_update_with_exact_metadata() {
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-usage").expect("turn id"),
            timestamp_ms: TimestampMs::from(1_700_000_000_002),
            sequence: Sequence::try_from(7).expect("sequence"),
        },
        payload: AgentEventPayload::UsageUpdated {
            usage: ModelUsage::builder()
                .input_tokens(70_000)
                .output_tokens(10_000)
                .cache_read_tokens(0)
                .cache_write_tokens(0)
                .total_tokens(80_000)
                .build(),
            context_window: 100_000,
        },
    };

    let updates = AcpEventMapper::map(event).expect("map usage");
    let SessionUpdate::UsageUpdate(update) = &updates[0] else {
        panic!("native usage update");
    };
    assert_eq!(update.used, 80_000);
    assert_eq!(update.size, 100_000);
    let metadata = update.meta.as_ref().expect("usage metadata");
    assert_eq!(
        metadata[ProductIdentity::ACP_NAMESPACE]["turnId"],
        "turn-usage"
    );
    assert_eq!(
        metadata[ProductIdentity::ACP_NAMESPACE]["usage"]["input_tokens"],
        "70000"
    );
}

/// Automatic lifecycle work remains running until the kernel emits AgentSettled.
#[test]
fn only_agent_settled_maps_to_idle_state() {
    let run_id = RunId::try_from("run-state").expect("run id");
    let event = |sequence, payload| AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-state").expect("turn id"),
            timestamp_ms: TimestampMs::from(2_000_u64 + sequence),
            sequence: Sequence::try_from(sequence).expect("sequence"),
        },
        payload,
    };
    let intermediate = [
        AgentEventPayload::RunEnd {
            run_id: run_id.clone(),
            outcome: AgentOutcome::Succeeded,
        },
        AgentEventPayload::RetryScheduled {
            run_id: run_id.clone(),
            attempt: 1,
            max_attempts: 3,
            delay_ms: 2_000,
            error: "terminated".to_string(),
        },
        AgentEventPayload::CompactionStart {
            run_id: run_id.clone(),
            reason: protocol::CompactionReason::Threshold,
        },
    ];
    for (index, payload) in intermediate.into_iter().enumerate() {
        let updates = AcpEventMapper::map(event(
            u64::try_from(index + 1).expect("sequence"),
            payload,
        ))
        .expect("map intermediate event");
        assert!(updates.iter().all(|update| !matches!(
            update,
            SessionUpdate::StateUpdate(
                agent_client_protocol::schema::v2::StateUpdate::Idle(_)
            )
        )));
    }

    let updates = AcpEventMapper::map(event(
        9,
        AgentEventPayload::AgentSettled {
            run_id,
            outcome: AgentOutcome::Succeeded,
        },
    ))
    .expect("map settled");
    assert!(updates.iter().any(|update| matches!(
        update,
        SessionUpdate::StateUpdate(
            agent_client_protocol::schema::v2::StateUpdate::Idle(_)
        )
    )));
}

/// A failed agent run uses a product error reason instead of pretending cancellation.
#[test]
fn failed_agent_settlement_does_not_map_to_cancelled() {
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-failed").expect("turn id"),
            timestamp_ms: TimestampMs::from(2_100),
            sequence: Sequence::try_from(10).expect("sequence"),
        },
        payload: AgentEventPayload::AgentSettled {
            run_id: RunId::try_from("run-failed").expect("run id"),
            outcome: AgentOutcome::Failed {
                message: "provider unavailable".to_string(),
            },
        },
    };

    let updates = AcpEventMapper::map(event).expect("map failed settlement");
    let idle = updates
        .iter()
        .find_map(|update| match update {
            SessionUpdate::StateUpdate(
                agent_client_protocol::schema::v2::StateUpdate::Idle(idle),
            ) => Some(idle),
            _ => None,
        })
        .expect("idle update");
    assert_eq!(
        serde_json::to_value(&idle.stop_reason).expect("serialize reason"),
        format!("_{}/error", ProductIdentity::ACP_NAMESPACE)
    );
}

/// Complete Assistant updates expose provider diagnostics and string token counts in metadata.
#[test]
fn assistant_metadata_is_preserved_on_native_update() {
    let turn_id = TurnId::try_from("turn-diagnostics").expect("turn id");
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: turn_id.clone(),
            timestamp_ms: TimestampMs::from(3_000),
            sequence: Sequence::try_from(10).expect("sequence"),
        },
        payload: AgentEventPayload::MessageEnd {
            message: AgentMessage {
                identity: MessageIdentity {
                    message_id: MessageId::try_from("message-diagnostics")
                        .expect("message id"),
                    turn_id,
                },
                timing: MessageTiming::try_from((
                    TimestampMs::from(2_900),
                    TimestampMs::from(2_950),
                    TimestampMs::from(3_000),
                ))
                .expect("timing"),
                content: MessageContent::Assistant {
                    blocks: vec![ContentBlock::Text {
                        text: "answer".to_string(),
                    }],
                    metadata: AssistantMetadata::builder()
                        .provider_id("provider-a".to_string())
                        .model_id("model-a".to_string())
                        .stop_reason(StopReason::MaxTokens)
                        .usage(
                            ModelUsage::builder()
                                .input_tokens(10)
                                .output_tokens(5)
                                .cache_read_tokens(2)
                                .cache_write_tokens(1)
                                .total_tokens(18)
                                .build(),
                        )
                        .error(Some("truncated".to_string()))
                        .build(),
                },
            },
        },
    };

    let updates = AcpEventMapper::map(event).expect("map assistant");
    let SessionUpdate::AgentMessage(update) = &updates[0] else {
        panic!("native assistant update");
    };
    let metadata = update
        .meta
        .as_opt_ref()
        .flatten()
        .expect("assistant metadata");
    let product = &metadata[ProductIdentity::ACP_NAMESPACE];
    assert_eq!(product["assistant"]["provider_id"], "provider-a");
    assert_eq!(product["assistant"]["model_id"], "model-a");
    assert_eq!(product["assistant"]["stop_reason"], "max_tokens");
    assert_eq!(product["assistant"]["usage"]["total_tokens"], "18");
    assert_eq!(product["assistant"]["error"], "truncated");
}

/// Streaming tool snapshots preserve images and edit patches in native ACP v2 content.
#[test]
fn tool_update_maps_image_and_diff_to_native_acp_content() {
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-tool-update").expect("turn id"),
            timestamp_ms: TimestampMs::from(4_000),
            sequence: Sequence::try_from(11).expect("sequence"),
        },
        payload: AgentEventPayload::ToolExecutionUpdate {
            run_id: RunId::try_from("run-tool-update").expect("run id"),
            result: ToolResult::builder()
                .tool_call_id(
                    ToolCallId::try_from("tool-update").expect("tool id"),
                )
                .blocks(vec![ContentBlock::Image {
                    data: "aW1hZ2U=".to_string(),
                    mime_type: "image/png".to_string(),
                }])
                .is_error(false)
                .details(ToolResultDetails::Edit {
                    path: "/workspace/file.txt".to_string(),
                    diff: "+1 replacement".to_string(),
                    patch: "--- /workspace/file.txt\n+++ /workspace/file.txt\n"
                        .to_string(),
                    first_changed_line: Some(1),
                })
                .build(),
        },
    };

    let updates = AcpEventMapper::map(event).expect("map tool update");
    let value = serde_json::to_value(&updates[0]).expect("serialize update");
    assert_eq!(value["sessionUpdate"], "tool_call_update");
    assert_eq!(value["status"], "in_progress");
    assert_eq!(value["content"][0]["type"], "content");
    assert_eq!(value["content"][0]["content"]["type"], "image");
    assert_eq!(value["content"][0]["content"]["mimeType"], "image/png");
    assert_eq!(value["content"][1]["type"], "diff");
    assert_eq!(
        value["content"][1]["changes"][0]["path"],
        "/workspace/file.txt"
    );
    assert_eq!(value["content"][1]["patch"]["format"], "git_patch");
    assert!(
        value["content"][1]["patch"]["text"]
            .as_str()
            .expect("patch text")
            .starts_with(
                "diff --git /workspace/file.txt /workspace/file.txt\n---"
            )
    );
}
