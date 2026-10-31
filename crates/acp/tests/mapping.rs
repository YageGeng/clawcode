use acp::AcpEventMapper;
use agent_client_protocol::schema::v2::SessionUpdate;
use protocol::{
    AgentEvent, AgentEventPayload, AgentMessage, AgentOutcome,
    AssistantMetadata, BashExecutionMessage, CompactionOutcome,
    CompactionReason, CompactionResult, ContentBlock, EntryId, EventMetadata,
    MessageContent, MessageId, MessageIdentity, MessageTiming, ModelUsage,
    ProductIdentity, RunId, Sequence, SkillDiagnostic, SkillDiagnosticCode,
    SkillDiagnosticSeverity, SlashCommandAliasKind, SlashCommandDefinition,
    SlashCommandMessage, SlashCommandOutput, SlashCommandSource,
    SlashCommandStatus, StopReason, TimestampMs, ToolCallId, ToolResult,
    ToolResultDetails, TurnId, UserBashDisposition, UserBashResult,
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

/// Skill diagnostics use the reserved ACP extension update with exact Turn timing.
#[test]
fn skill_diagnostic_maps_to_acp_v2_other_session_update() {
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-skill").expect("turn id"),
            timestamp_ms: TimestampMs::from(2_050),
            sequence: Sequence::try_from(5).expect("sequence"),
        },
        payload: AgentEventPayload::SkillDiagnostic {
            diagnostic: SkillDiagnostic::builder()
                .severity(SkillDiagnosticSeverity::Warning)
                .code(SkillDiagnosticCode::FileReadFailed)
                .message("Skill file disappeared".to_string())
                .path(Some("/skills/review/SKILL.md".into()))
                .build(),
        },
    };

    let updates = AcpEventMapper::map(event).expect("map Skill diagnostic");
    let value =
        serde_json::to_value(updates.first()).expect("serialize update");

    assert_eq!(value["sessionUpdate"], "_clawcode/event");
    assert_eq!(value["payload"]["event"], "skill_diagnostic");
    assert_eq!(value["payload"]["diagnostic"]["code"], "file_read_failed");
    assert_eq!(value["_meta"]["clawcode"]["turnId"], "turn-skill");
    assert_eq!(value["_meta"]["clawcode"]["timestampMs"], "2050");
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
    let value = updates
        .iter()
        .map(|update| serde_json::to_value(update).expect("serialize update"))
        .find(|value| value["sessionUpdate"] == "tool_call_update")
        .expect("bash tool update");
    // A blocked shell is a failed tool whose metadata keeps why it was blocked.
    assert_eq!(value["status"], "failed");
    assert_eq!(value["_meta"]["clawcode"]["disposition"]["type"], "blocked");
    assert_eq!(
        value["_meta"]["clawcode"]["disposition"]["reason"],
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

/// Available Commands map to ACP v2's native complete command snapshot.
#[test]
fn available_commands_map_to_native_acp_v2_update() {
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-commands").expect("turn id"),
            timestamp_ms: TimestampMs::from(9_007_199_254_740_999),
            sequence: Sequence::try_from(12).expect("sequence"),
        },
        payload: AgentEventPayload::AvailableCommandsChanged {
            commands: vec![
                SlashCommandDefinition::builder()
                    .name("review".to_string())
                    .description("Review changes".to_string())
                    .argument_hint(Some("<path>".to_string()))
                    .source(SlashCommandSource::PromptTemplate)
                    .qualified_name(None)
                    .alias_kind(SlashCommandAliasKind::Canonical)
                    .build(),
            ],
        },
    };

    let updates = AcpEventMapper::map(event).expect("map Commands event");
    assert!(matches!(
        updates.first(),
        Some(SessionUpdate::AvailableCommandsUpdate(_))
    ));
    let value = serde_json::to_value(&updates[0]).expect("serialize update");
    assert_eq!(value["sessionUpdate"], "available_commands_update");
    assert_eq!(value["availableCommands"][0]["name"], "review");
    assert_eq!(
        value["availableCommands"][0]["description"],
        "Review changes"
    );
    assert_eq!(value["availableCommands"][0]["input"]["type"], "text");
    assert_eq!(value["availableCommands"][0]["input"]["hint"], "<path>");
    assert_eq!(
        value["availableCommands"][0]["_meta"][ProductIdentity::ACP_NAMESPACE]
            [ProductIdentity::ACP_SLASH_COMMAND_METADATA]["source"],
        "prompt_template"
    );
    assert_eq!(
        value["_meta"][ProductIdentity::ACP_NAMESPACE]["turnId"],
        "turn-commands"
    );
    assert_eq!(
        value["_meta"][ProductIdentity::ACP_NAMESPACE]["timestampMs"],
        "9007199254740999"
    );
    assert_eq!(
        value["_meta"][ProductIdentity::ACP_NAMESPACE]["sequence"],
        12
    );
}

/// Direct command output maps to native AgentMessage with product metadata.
#[test]
fn command_output_maps_to_agent_message_with_product_metadata() {
    let turn_id = TurnId::try_from("turn-command").expect("TurnId");
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: turn_id.clone(),
            timestamp_ms: TimestampMs::from(8_500),
            sequence: Sequence::try_from(7).expect("sequence"),
        },
        payload: AgentEventPayload::MessageEnd {
            message: AgentMessage {
                identity: MessageIdentity {
                    message_id: MessageId::try_from("message-command")
                        .expect("MessageId"),
                    turn_id,
                },
                timing: MessageTiming::try_from((
                    TimestampMs::from(8_400),
                    TimestampMs::from(8_400),
                    TimestampMs::from(8_500),
                ))
                .expect("message timing"),
                content: MessageContent::SlashCommand {
                    message: SlashCommandMessage::Output(
                        SlashCommandOutput::builder()
                            .command("session".to_string())
                            .source(SlashCommandSource::Builtin)
                            .status(SlashCommandStatus::Succeeded)
                            .blocks(vec![ContentBlock::Text {
                                text: "Session ID: session-1".to_string(),
                            }])
                            .build(),
                    ),
                },
            },
        },
    };

    let updates = AcpEventMapper::map(event).expect("map command output");
    let value = serde_json::to_value(&updates[0]).expect("serialize update");
    assert_eq!(value["sessionUpdate"], "agent_message");
    assert_eq!(
        value["_meta"][ProductIdentity::ACP_NAMESPACE]
            [ProductIdentity::ACP_SLASH_COMMAND_METADATA]["messageKind"],
        "output"
    );
    assert_eq!(
        value["_meta"][ProductIdentity::ACP_NAMESPACE]
            [ProductIdentity::ACP_SLASH_COMMAND_METADATA]["source"],
        "builtin"
    );
    assert_eq!(
        value["_meta"][ProductIdentity::ACP_NAMESPACE]["messageTiming"]["ended_at_ms"],
        "8500"
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
                    model_message: None,
                    blocks: vec![
                        ContentBlock::Reasoning {
                            text: "reasoning".to_string(),
                        },
                        ContentBlock::ToolCall {
                            invalid_arguments: None,
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
                        .usage(protocol::ModelUsage::from(
                            protocol::Usage::new()
                                .input_tokens(10)
                                .output_tokens(5)
                                .cached_input_tokens(0)
                                .cache_creation_input_tokens(0)
                                .total_tokens(15),
                        ))
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
            usage: protocol::ModelUsage::from(
                protocol::Usage::new()
                    .input_tokens(70_000)
                    .output_tokens(10_000)
                    .cached_input_tokens(0)
                    .cache_creation_input_tokens(0)
                    .total_tokens(80_000),
            ),
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
            usage: ModelUsage::default(),
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

/// Completed compaction updates carry all data required for a durable WebUI card.
#[test]
fn completed_compaction_maps_to_product_extension_update() {
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-compaction").expect("turn id"),
            timestamp_ms: TimestampMs::from(2_050),
            sequence: Sequence::try_from(8).expect("sequence"),
        },
        payload: AgentEventPayload::CompactionEnd {
            run_id: RunId::try_from("run-compaction").expect("run id"),
            reason: CompactionReason::Manual,
            outcome: CompactionOutcome::Completed {
                result: CompactionResult::builder()
                    .entry_id(
                        EntryId::try_from("entry-compaction")
                            .expect("entry id"),
                    )
                    .turn_id(
                        TurnId::try_from("turn-compaction").expect("turn id"),
                    )
                    .summary("## Goal\nContinue implementation".to_string())
                    .tokens_before(42_000)
                    .usage(None)
                    .started_at_ms(TimestampMs::from(2_000))
                    .ended_at_ms(TimestampMs::from(2_050))
                    .build()
                    .into(),
            },
        },
    };

    let updates = AcpEventMapper::map(event).expect("map compaction");
    let value = serde_json::to_value(&updates[0]).expect("serialize update");

    assert_eq!(value["sessionUpdate"], ProductIdentity::ACP_EVENT_UPDATE);
    assert_eq!(value["payload"]["event"], "compaction_end");
    assert_eq!(value["payload"]["outcome"]["status"], "completed");
    assert_eq!(
        value["payload"]["outcome"]["result"]["entryId"],
        "entry-compaction"
    );
    assert_eq!(
        value["payload"]["outcome"]["result"]["tokensBefore"],
        "42000"
    );
}

/// A failed agent run uses the standard error reason instead of pretending cancellation.
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
            usage: ModelUsage::default(),
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
        serde_json::to_value(&idle.stop_reason).expect("serialize reason")["stopReason"],
        "error"
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
                    model_message: None,
                    blocks: vec![ContentBlock::Text {
                        text: "answer".to_string(),
                    }],
                    metadata: AssistantMetadata::builder()
                        .provider_id("provider-a".to_string())
                        .model_id("model-a".to_string())
                        .stop_reason(StopReason::MaxTokens)
                        .usage(protocol::ModelUsage::from(
                            protocol::Usage::new()
                                .input_tokens(10)
                                .output_tokens(5)
                                .cached_input_tokens(2)
                                .cache_creation_input_tokens(1)
                                .total_tokens(18),
                        ))
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

/// ACP 1.13 available commands carry the `type` discriminator for the command variant.
#[test]
fn available_commands_use_command_type_discriminator() {
    let event = AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-commands").expect("turn id"),
            timestamp_ms: TimestampMs::from(1_000),
            sequence: Sequence::try_from(1).expect("sequence"),
        },
        payload: AgentEventPayload::AvailableCommandsChanged {
            commands: vec![
                SlashCommandDefinition::builder()
                    .name("review".to_string())
                    .description("Review changes".to_string())
                    .argument_hint(None)
                    .source(SlashCommandSource::PromptTemplate)
                    .qualified_name(None)
                    .alias_kind(SlashCommandAliasKind::Canonical)
                    .build(),
            ],
        },
    };

    let updates = AcpEventMapper::map(event).expect("map Commands event");
    let json = serde_json::to_value(&updates[0]).expect("serialize update");
    assert_eq!(json["availableCommands"][0]["type"], "command");
}

/// Builds event metadata for user-bash mapping tests.
fn bash_metadata(sequence: u64) -> EventMetadata {
    EventMetadata {
        turn_id: TurnId::try_from("turn-bash").expect("turn id"),
        timestamp_ms: TimestampMs::from(3_000),
        sequence: Sequence::try_from(sequence).expect("sequence"),
    }
}

/// A started user shell becomes the running state and its accepted, origin-tagged user message.
#[test]
fn user_bash_started_maps_to_tagged_user_message() {
    let updates = AcpEventMapper::map(AgentEvent {
        metadata: bash_metadata(1),
        payload: AgentEventPayload::UserBashStarted {
            run_id: RunId::try_from("run-bash").expect("run id"),
            message_id: MessageId::try_from("message-bash").expect("id"),
            command: "echo hi".to_string(),
            cwd: "/work".into(),
            exclude_from_context: false,
        },
    })
    .expect("map started shell");
    let values: Vec<_> = updates
        .iter()
        .map(|update| serde_json::to_value(update).expect("serialize update"))
        .collect();
    assert_eq!(values[0]["sessionUpdate"], "_clawcode/event");
    assert_eq!(values[1]["sessionUpdate"], "state_update");
    assert_eq!(values[1]["state"], "running");
    let json = &values[2];
    assert_eq!(json["sessionUpdate"], "user_message");
    assert_eq!(json["messageId"], "message-bash");
    assert_eq!(json["content"][0]["text"], "!echo hi");
    assert_eq!(json["_meta"]["clawcode"]["origin"], "user_bash");
}

/// Replaying a persisted shell recreates the same-id user message, keeping the `!!` prefix.
#[test]
fn bash_execution_replay_emits_user_message_with_same_id() {
    let updates = AcpEventMapper::map(AgentEvent {
        metadata: bash_metadata(2),
        payload: AgentEventPayload::MessageEnd {
            message: AgentMessage {
                identity: MessageIdentity {
                    message_id: MessageId::try_from("message-bash")
                        .expect("message id"),
                    turn_id: TurnId::try_from("turn-bash").expect("turn id"),
                },
                timing: MessageTiming::try_from((
                    TimestampMs::from(3_000),
                    TimestampMs::from(3_000),
                    TimestampMs::from(3_100),
                ))
                .expect("message timing"),
                content: MessageContent::BashExecution {
                    bash: BashExecutionMessage::builder()
                        .command("ls".to_string())
                        .result(
                            UserBashResult::builder()
                                .disposition(UserBashDisposition::Completed)
                                .output("file".to_string())
                                .cancelled(false)
                                .truncated(false)
                                .build(),
                        )
                        .exclude_from_context(true)
                        .build(),
                },
            },
        },
    })
    .expect("map bash replay");
    let message = updates
        .iter()
        .map(|update| serde_json::to_value(update).expect("serialize update"))
        .find(|value| value["sessionUpdate"] == "user_message")
        .expect("user message");
    assert_eq!(message["messageId"], "message-bash");
    assert_eq!(message["content"][0]["text"], "!!ls");
    assert_eq!(message["_meta"]["clawcode"]["origin"], "user_bash");
}

/// The shell finish maps to one idle state carrying the operation outcome.
#[test]
fn user_bash_finished_maps_to_idle_with_outcome() {
    for (outcome, stop_reason) in [
        (AgentOutcome::Succeeded, "end_turn"),
        (AgentOutcome::Cancelled, "cancelled"),
    ] {
        let updates = AcpEventMapper::map(AgentEvent {
            metadata: bash_metadata(3),
            payload: AgentEventPayload::UserBashFinished {
                run_id: RunId::try_from("run-bash").expect("run id"),
                message_id: MessageId::try_from("message-bash").expect("id"),
                outcome,
                result_emitted: true,
            },
        })
        .expect("map finished shell");
        let json = updates
            .iter()
            .map(|update| serde_json::to_value(update).expect("serialize"))
            .find(|value| value["sessionUpdate"] == "state_update")
            .expect("idle update");
        assert_eq!(json["state"], "idle");
        assert_eq!(json["stopReason"], stop_reason);
    }
}

/// Maps one event and returns the serialized idle state update.
fn idle_json(payload: AgentEventPayload) -> serde_json::Value {
    let updates = AcpEventMapper::map(AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-stop").expect("turn id"),
            timestamp_ms: TimestampMs::from(4_000),
            sequence: Sequence::try_from(1).expect("sequence"),
        },
        payload,
    })
    .expect("map event");
    updates
        .iter()
        .map(|update| serde_json::to_value(update).expect("serialize update"))
        .find(|value| value["state"] == "idle")
        .expect("idle update")
}

/// A failed run carries its failure as the standard error stop reason.
#[test]
fn failed_run_maps_to_error_stop_reason() {
    let json = idle_json(AgentEventPayload::AgentSettled {
        run_id: RunId::try_from("run-failed").expect("run id"),
        outcome: AgentOutcome::Failed {
            message: "provider failed".to_string(),
        },
        usage: ModelUsage::default(),
    });
    assert_eq!(json["stopReason"], "error");
    assert_eq!(json["error"]["code"], -32603);
    assert_eq!(json["error"]["message"], "provider failed");
}

/// A failed Slash Command reports its summary, or a fixed default when it has none.
#[test]
fn failed_slash_command_uses_message_or_default() {
    for (message, expected) in [
        (Some("compaction failed".to_string()), "compaction failed"),
        (None, "Slash Command failed"),
    ] {
        let json = idle_json(AgentEventPayload::SlashCommandEnd {
            run_id: RunId::try_from("run-command").expect("run id"),
            status: SlashCommandStatus::Failed,
            message,
        });
        assert_eq!(json["stopReason"], "error");
        assert_eq!(json["error"]["message"], expected);
    }
}

/// A cancelled Slash Command ends with the cancelled stop reason.
#[test]
fn cancelled_slash_command_maps_to_cancelled_stop_reason() {
    let json = idle_json(AgentEventPayload::SlashCommandEnd {
        run_id: RunId::try_from("run-command").expect("run id"),
        status: SlashCommandStatus::Cancelled,
        message: None,
    });
    assert_eq!(json["stopReason"], "cancelled");
}

/// A pending elicitation reports requires_action, and its resolution reports running again.
#[test]
fn action_required_maps_to_requires_action_state() {
    let map = |payload| {
        let updates = AcpEventMapper::map(AgentEvent {
            metadata: EventMetadata {
                turn_id: TurnId::try_from("turn-action").expect("turn id"),
                timestamp_ms: TimestampMs::from(5_000),
                sequence: Sequence::try_from(1).expect("sequence"),
            },
            payload,
        })
        .expect("map action event");
        updates
            .iter()
            .map(|update| serde_json::to_value(update).expect("serialize"))
            .find(|value| value["sessionUpdate"] == "state_update")
            .expect("state update")
    };
    let required = map(AgentEventPayload::ActionRequired {
        request_id: "elicit-1".to_string(),
    });
    assert_eq!(required["state"], "requires_action");
    let resolved = map(AgentEventPayload::ActionResolved {
        request_id: "elicit-1".to_string(),
    });
    assert_eq!(resolved["state"], "running");
}

/// Builds event metadata for tool mapping tests.
fn tool_metadata() -> EventMetadata {
    EventMetadata {
        turn_id: TurnId::try_from("turn-tool").expect("turn id"),
        timestamp_ms: TimestampMs::from(6_000),
        sequence: Sequence::try_from(1).expect("sequence"),
    }
}

/// Maps one tool event and returns its single serialized tool update.
fn tool_update(payload: AgentEventPayload) -> serde_json::Value {
    let updates = AcpEventMapper::map(AgentEvent {
        metadata: tool_metadata(),
        payload,
    })
    .expect("map tool event");
    updates
        .iter()
        .map(|update| serde_json::to_value(update).expect("serialize"))
        .find(|value| value["sessionUpdate"] == "tool_call_update")
        .expect("tool update")
}

/// Builds a cancelled tool result as the Kernel reports it.
fn cancelled_result() -> ToolResult {
    ToolResult::builder()
        .tool_call_id(ToolCallId::try_from("call-1").expect("id"))
        .blocks(vec![ContentBlock::Text {
            text: "aborted".to_string(),
        }])
        .is_error(true)
        .cancelled(true)
        .build()
}

/// A starting tool reports its real name, kind, and absolute file location.
#[test]
fn tool_start_sets_name_kind_and_absolute_location() {
    let json = tool_update(AgentEventPayload::ToolExecutionStart {
        run_id: RunId::try_from("run-tool").expect("run id"),
        call: protocol::ToolCall::builder()
            .tool_call_id(ToolCallId::try_from("call-1").expect("id"))
            .name("read".to_string())
            .arguments(
                serde_json::json!({ "path": "src/main.rs", "offset": 10 }),
            )
            .build(),
        cwd: "/work".into(),
        locations: vec![protocol::ToolLocation {
            path: "/work/src/main.rs".into(),
            line: Some(10),
        }],
    });
    assert_eq!(json["name"], "read");
    assert_eq!(json["kind"], "read");
    assert_eq!(json["title"], "read");
    assert_eq!(
        json["locations"][0],
        serde_json::json!({"path": "/work/src/main.rs", "line": 10})
    );
}

/// A cancelled tool ends as cancelled rather than failed.
#[test]
fn cancelled_tool_result_maps_to_cancelled_status() {
    let json = tool_update(AgentEventPayload::ToolExecutionEnd {
        run_id: RunId::try_from("run-tool").expect("run id"),
        result: cancelled_result(),
    });
    assert_eq!(json["status"], "cancelled");
}

/// A persisted message must not overwrite the live cancelled state.
#[test]
fn cancelled_tool_message_end_preserves_cancelled_status() {
    let result = cancelled_result();
    let json = tool_update(AgentEventPayload::MessageEnd {
        message: AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from("message-tool").expect("id"),
                turn_id: TurnId::try_from("turn-tool").expect("turn id"),
            },
            timing: MessageTiming::try_from((
                TimestampMs::from(6_000),
                TimestampMs::from(6_000),
                TimestampMs::from(6_100),
            ))
            .expect("timing"),
            content: MessageContent::ToolResult {
                tool_call_id: result.tool_call_id,
                blocks: result.blocks,
                is_error: true,
                details: None,
                output: None,
                cancelled: true,
            },
        },
    });
    assert_eq!(json["status"], "cancelled");
}

/// Existing tool records remain readable with the previous terminal status.
#[test]
fn old_tool_result_without_cancelled_defaults_to_false() {
    let content: MessageContent = serde_json::from_value(serde_json::json!({
        "type": "tool_result",
        "tool_call_id": "call-1",
        "blocks": [],
        "is_error": true
    }))
    .expect("old tool result");
    assert!(matches!(
        content,
        MessageContent::ToolResult {
            cancelled: false,
            ..
        }
    ));
}

/// Maps one compaction event and returns its standard compaction entity update.
fn compaction_update(payload: AgentEventPayload) -> serde_json::Value {
    let updates = AcpEventMapper::map(AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-compaction").expect("turn id"),
            timestamp_ms: TimestampMs::from(7_000),
            sequence: Sequence::try_from(1).expect("sequence"),
        },
        payload,
    })
    .expect("map compaction event");
    updates
        .iter()
        .map(|update| serde_json::to_value(update).expect("serialize"))
        .find(|value| value["sessionUpdate"] == "compaction_update")
        .expect("compaction update")
}

/// A reported compaction starts in progress and ends with a terminal status under one id.
#[test]
fn compaction_maps_to_standard_entity_lifecycle() {
    let run_id = RunId::try_from("run-compact").expect("run id");
    let start = compaction_update(AgentEventPayload::CompactionStart {
        run_id: run_id.clone(),
        reason: CompactionReason::Manual,
    });
    let end = compaction_update(AgentEventPayload::CompactionEnd {
        run_id,
        reason: CompactionReason::Manual,
        outcome: CompactionOutcome::Completed {
            result: CompactionResult::builder()
                .entry_id(EntryId::try_from("entry-compact").expect("entry id"))
                .turn_id(TurnId::try_from("turn-compaction").expect("turn id"))
                .summary("## Goal\nContinue".to_string())
                .tokens_before(1_000)
                .usage(None)
                .started_at_ms(TimestampMs::from(7_000))
                .ended_at_ms(TimestampMs::from(7_100))
                .build()
                .into(),
        },
    });
    assert_eq!(start["status"], "in_progress");
    assert_eq!(end["status"], "completed");
    assert_eq!(start["compactionId"], end["compactionId"]);
    assert_eq!(end["summary"][0]["text"], "## Goal\nContinue");
    // Card details the standard entity lacks ride in product metadata.
    let compaction = &end["_meta"][ProductIdentity::ACP_NAMESPACE]
        [ProductIdentity::ACP_COMPACTION_METADATA];
    assert_eq!(compaction["reason"], "manual");
    assert_eq!(compaction["result"]["entryId"], "entry-compact");
    assert_eq!(compaction["result"]["tokensBefore"], "1000");
    // The summary travels once, in the standard field, never again in metadata.
    assert!(compaction["result"].get("summary").is_none());
    assert_eq!(
        serde_json::to_string(&end)
            .expect("serialize")
            .matches("## Goal")
            .count(),
        1
    );
    assert_eq!(
        start["_meta"][ProductIdentity::ACP_NAMESPACE]
            [ProductIdentity::ACP_COMPACTION_METADATA]["reason"],
        "manual"
    );
}

/// A failed compaction carries its sanitized error text.
#[test]
fn failed_compaction_carries_error_text() {
    let end = compaction_update(AgentEventPayload::CompactionEnd {
        run_id: RunId::try_from("run-compact").expect("run id"),
        reason: CompactionReason::Threshold,
        outcome: CompactionOutcome::Failed {
            message: "summary failed".to_string(),
        },
    });
    assert_eq!(end["status"], "failed");
    assert_eq!(end["error"], "summary failed");
    assert!(
        end["_meta"][ProductIdentity::ACP_NAMESPACE]
            [ProductIdentity::ACP_COMPACTION_METADATA]["result"]
            .is_null()
    );
}

/// A Skill expansion diagnostic surfaces as a warning notice.
#[test]
fn skill_diagnostic_maps_to_warning_notice() {
    let updates = AcpEventMapper::map(AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-notice").expect("turn id"),
            timestamp_ms: TimestampMs::from(8_000),
            sequence: Sequence::try_from(1).expect("sequence"),
        },
        payload: AgentEventPayload::SkillDiagnostic {
            diagnostic: SkillDiagnostic::builder()
                .severity(SkillDiagnosticSeverity::Warning)
                .code(SkillDiagnosticCode::FileReadFailed)
                .message("skill file is unreadable".to_string())
                .build(),
        },
    })
    .expect("map diagnostic");
    let json = updates
        .iter()
        .map(|update| serde_json::to_value(update).expect("serialize"))
        .find(|value| value["sessionUpdate"] == "notice")
        .expect("notice");
    assert_eq!(json["severity"], "warning");
    assert_eq!(json["title"], "Skill expansion failed");
    assert_eq!(json["description"], "skill file is unreadable");
}

/// The idle state reports the Run's token usage.
#[test]
fn idle_state_carries_usage() {
    let updates = AcpEventMapper::map(AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-usage").expect("turn id"),
            timestamp_ms: TimestampMs::from(9_000),
            sequence: Sequence::try_from(1).expect("sequence"),
        },
        payload: AgentEventPayload::AgentSettled {
            run_id: RunId::try_from("run-usage").expect("run id"),
            outcome: AgentOutcome::Succeeded,
            usage: protocol::ModelUsage::from(
                protocol::Usage::new()
                    .input_tokens(20)
                    .output_tokens(10)
                    .cached_input_tokens(4)
                    .cache_creation_input_tokens(1)
                    .total_tokens(30),
            ),
        },
    })
    .expect("map settlement");
    let json = updates
        .iter()
        .map(|update| serde_json::to_value(update).expect("serialize"))
        .find(|value| value["sessionUpdate"] == "state_update")
        .expect("idle update");
    assert_eq!(json["usage"]["totalTokens"], 30);
    assert_eq!(json["usage"]["cachedReadTokens"], 4);
    assert!(json["usage"].get("thoughtTokens").is_none());
}

/// Maps one event and returns every serialized update in order.
fn mapped(payload: AgentEventPayload) -> Vec<serde_json::Value> {
    AcpEventMapper::map(AgentEvent {
        metadata: tool_metadata(),
        payload,
    })
    .expect("map event")
    .iter()
    .map(|update| serde_json::to_value(update).expect("serialize"))
    .collect()
}

/// Decodes one terminal update's base64 output.
fn terminal_output(update: &serde_json::Value) -> String {
    use base64::Engine as _;
    String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(
                update
                    .pointer("/output/data")
                    .and_then(serde_json::Value::as_str)
                    .expect("output data"),
            )
            .expect("base64"),
    )
    .expect("utf-8")
}

/// A bash snapshot as the bash tool publishes it while running.
fn bash_progress(text: &str) -> ToolResult {
    ToolResult::builder()
        .tool_call_id(ToolCallId::try_from("call-bash").expect("id"))
        .blocks(vec![ContentBlock::Text {
            text: text.to_string(),
        }])
        .is_error(false)
        .details(ToolResultDetails::Bash {
            truncation: None,
            full_output_path: None,
            exit_code: None,
            signal: None,
        })
        .build()
}

/// The bash tool opens a terminal, streams it, and closes it before the tool ends.
#[test]
fn bash_tool_maps_to_terminal_lifecycle() {
    let start = mapped(AgentEventPayload::ToolExecutionStart {
        run_id: RunId::try_from("run-tool").expect("run id"),
        call: protocol::ToolCall::builder()
            .tool_call_id(ToolCallId::try_from("call-bash").expect("id"))
            .name("bash".to_string())
            .arguments(serde_json::json!({ "command": "ls -la" }))
            .build(),
        cwd: "/work".into(),
        locations: Vec::new(),
    });
    assert_eq!(start[0]["sessionUpdate"], "tool_call_update");
    assert_eq!(start[0]["kind"], "execute");
    assert_eq!(
        start[0]["content"][0],
        serde_json::json!({ "type": "terminal", "terminalId": "call-bash" })
    );
    assert_eq!(start[1]["sessionUpdate"], "terminal_update");
    assert_eq!(start[1]["terminalId"], "call-bash");
    assert_eq!(start[1]["command"], "ls -la");
    assert_eq!(start[1]["cwd"], "/work");

    let mut finished = bash_progress("total 0");
    finished.details = Some(ToolResultDetails::Bash {
        truncation: None,
        full_output_path: None,
        exit_code: Some(0),
        signal: None,
    });
    let end = mapped(AgentEventPayload::ToolExecutionEnd {
        run_id: RunId::try_from("run-tool").expect("run id"),
        result: finished,
    });
    assert_eq!(end[0]["sessionUpdate"], "terminal_update");
    assert_eq!(terminal_output(&end[0]), "total 0");
    assert_eq!(end[0]["exitStatus"]["exitCode"], 0);
    assert_eq!(end[1]["sessionUpdate"], "tool_call_update");
    assert_eq!(end[1]["status"], "completed");
    assert_eq!(end[1]["content"][0]["type"], "terminal");
}

/// Progress carries the tail once instead of duplicating it in tool content and raw output.
#[test]
fn bash_progress_maps_only_to_terminal_update() {
    let updates = mapped(AgentEventPayload::ToolExecutionUpdate {
        run_id: RunId::try_from("run-tool").expect("run id"),
        result: bash_progress("line 1\nline 2"),
    });
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["sessionUpdate"], "terminal_update");
    assert_eq!(terminal_output(&updates[0]), "line 1\nline 2");
}

/// A started user shell becomes a user-tagged execute tool backed by a terminal.
#[test]
fn user_bash_maps_to_tagged_tool_call_and_terminal() {
    let updates = mapped(AgentEventPayload::UserBashStarted {
        run_id: RunId::try_from("run-bash").expect("run id"),
        message_id: MessageId::try_from("message-bash").expect("id"),
        command: "echo hi".to_string(),
        cwd: "/work".into(),
        exclude_from_context: true,
    });
    let tool = updates
        .iter()
        .find(|update| update["sessionUpdate"] == "tool_call_update")
        .expect("tool update");
    assert_eq!(tool["toolCallId"], "message-bash");
    assert_eq!(tool["kind"], "execute");
    assert_eq!(tool["name"], "bash");
    assert_eq!(tool["title"], "echo hi");
    assert_eq!(tool["status"], "in_progress");
    assert_eq!(tool["content"][0]["terminalId"], "message-bash");
    assert_eq!(tool["_meta"]["clawcode"]["origin"], "user");
    assert_eq!(tool["_meta"]["clawcode"]["excludeFromContext"], true);
    let terminal = updates
        .iter()
        .find(|update| update["sessionUpdate"] == "terminal_update")
        .expect("terminal update");
    assert_eq!(terminal["command"], "echo hi");
    assert_eq!(terminal["cwd"], "/work");
}

/// Live shell output is an authoritative replacement snapshot that survives any bytes.
#[test]
fn terminal_output_round_trips_unicode_and_ansi() {
    let output = "中文🙂\u{1b}[31mred\u{1b}[0m";
    let updates = mapped(AgentEventPayload::UserBashOutput {
        run_id: RunId::try_from("run-bash").expect("run id"),
        message_id: MessageId::try_from("message-bash").expect("id"),
        output: output.to_string(),
    });
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["terminalId"], "message-bash");
    assert_eq!(terminal_output(&updates[0]), output);
}

/// A persisted shell replays its user message, final tool status, and saved terminal.
#[test]
fn bash_replay_emits_final_terminal_snapshot() {
    let updates = mapped(AgentEventPayload::MessageEnd {
        message: AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from("message-bash").expect("id"),
                turn_id: TurnId::try_from("turn-bash").expect("turn id"),
            },
            timing: MessageTiming::try_from((
                TimestampMs::from(3_000),
                TimestampMs::from(3_000),
                TimestampMs::from(3_100),
            ))
            .expect("message timing"),
            content: MessageContent::BashExecution {
                bash: BashExecutionMessage::builder()
                    .command("serve".to_string())
                    .result(
                        UserBashResult::builder()
                            .disposition(UserBashDisposition::Completed)
                            .output("listening".to_string())
                            .signal(Some("SIGTERM".to_string()))
                            .cancelled(false)
                            .truncated(false)
                            .build(),
                    )
                    .exclude_from_context(false)
                    .cwd("/saved".into())
                    .build(),
            },
        },
    });
    let kinds = updates
        .iter()
        .map(|update| update["sessionUpdate"].as_str().expect("kind"))
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        ["user_message", "tool_call_update", "terminal_update"]
    );
    assert_eq!(updates[1]["status"], "failed");
    assert_eq!(updates[1]["title"], "serve");
    assert_eq!(updates[2]["cwd"], "/saved");
    assert_eq!(updates[2]["exitStatus"]["signal"], "SIGTERM");
    assert!(updates[2]["exitStatus"].get("exitCode").is_none());
    assert_eq!(terminal_output(&updates[2]), "listening");
}

/// Maps a shell finish that produced no result message.
fn finished_without_result(outcome: AgentOutcome) -> Vec<serde_json::Value> {
    mapped(AgentEventPayload::UserBashFinished {
        run_id: RunId::try_from("run-bash").expect("run id"),
        message_id: MessageId::try_from("message-bash").expect("id"),
        outcome,
        result_emitted: false,
    })
}

/// An executor failure closes the displayed terminal and tool before idle(error).
#[test]
fn user_bash_failure_without_result_emits_terminal_and_tool_end() {
    let updates = finished_without_result(AgentOutcome::Failed {
        message: "spawn failed".to_string(),
    });
    let terminal = updates
        .iter()
        .position(|update| update["sessionUpdate"] == "terminal_update")
        .expect("terminal end");
    let tool = updates
        .iter()
        .position(|update| update["sessionUpdate"] == "tool_call_update")
        .expect("tool end");
    let idle = updates
        .iter()
        .position(|update| update["state"] == "idle")
        .expect("idle");
    assert!(terminal < tool && tool < idle);
    // Unknown exit details stay unknown.
    assert_eq!(updates[terminal]["exitStatus"], serde_json::json!({}));
    assert_eq!(updates[tool]["status"], "failed");
    assert_eq!(updates[idle]["stopReason"], "error");
}

/// Cancellation before a result still closes the already accepted terminal and tool.
#[test]
fn user_bash_cancellation_without_result_emits_cancelled_tool_end() {
    let updates = finished_without_result(AgentOutcome::Cancelled);
    let tool = updates
        .iter()
        .find(|update| update["sessionUpdate"] == "tool_call_update")
        .expect("tool end");
    assert_eq!(tool["status"], "cancelled");
    let idle = updates
        .iter()
        .find(|update| update["state"] == "idle")
        .expect("idle");
    assert_eq!(idle["stopReason"], "cancelled");
}

/// A normal result already closed the terminal and tool, so the finish adds only idle.
#[test]
fn user_bash_finished_with_result_does_not_duplicate_tool_end() {
    let updates = mapped(AgentEventPayload::UserBashFinished {
        run_id: RunId::try_from("run-bash").expect("run id"),
        message_id: MessageId::try_from("message-bash").expect("id"),
        outcome: AgentOutcome::Succeeded,
        result_emitted: true,
    });
    assert!(updates.iter().all(|update| {
        update["sessionUpdate"] != "tool_call_update"
            && update["sessionUpdate"] != "terminal_update"
    }));
}

/// Queue updates preserve the submitting client's correlation without creating transcript messages.
#[test]
fn queue_changed_maps_to_product_event_with_submission_identity() {
    let pending: protocol::PendingMessages = serde_json::from_value(serde_json::json!({"steering":[],"followUp":[{
        "queueId":"queue-map", "kind":"followUp", "submissionId":"client-map",
        "message":{"message_id":"message-map","turn_id":"turn-map","timestamp_ms":"1","started_at_ms":"1","ended_at_ms":"1","content":{"type":"user","blocks":[{"type":"text","text":"queued"}]}}
    }]})).unwrap();
    let updates = AcpEventMapper::map(AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-map").unwrap(),
            timestamp_ms: TimestampMs::from(1),
            sequence: Sequence::try_from(1).unwrap(),
        },
        payload: AgentEventPayload::QueueChanged { pending },
    })
    .unwrap();
    assert_eq!(updates.len(), 1);
    let value = serde_json::to_value(&updates[0]).unwrap();
    assert_eq!(value["sessionUpdate"], "_clawcode/event");
    assert_eq!(
        value["payload"]["pending"]["followUp"][0]["submissionId"],
        "client-map"
    );
}
