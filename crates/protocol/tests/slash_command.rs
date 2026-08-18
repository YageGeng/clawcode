//! Integration tests for the shared Slash Command protocol model.

use protocol::{
    AgentEvent, AgentEventPayload, AgentMessage, ContentBlock, EventMetadata,
    MessageContent, MessageId, MessageIdentity, MessageTiming, Role, RunId,
    RunInput, Sequence, SlashCommandError, SlashCommandExpansion,
    SlashCommandInvocation, SlashCommandMessage, SlashCommandOutput,
    SlashCommandParseError, SlashCommandSource, SlashCommandStatus,
    TimestampMs, TurnId,
};

/// The parser removes exactly one ASCII delimiter and preserves the remaining argument tail.
#[test]
fn slash_invocation_preserves_ascii_space_arguments() {
    let invocation = SlashCommandInvocation::try_from("/compact  keep details")
        .expect("Slash Command");

    assert_eq!(invocation.name, "compact");
    assert_eq!(invocation.arguments, " keep details");
    assert_eq!(invocation.original, "/compact  keep details");
}

/// Non-leading slashes and empty names cannot become executable commands.
#[test]
fn slash_invocation_rejects_non_commands_and_empty_names() {
    assert_eq!(
        SlashCommandInvocation::try_from(" /compact"),
        Err(SlashCommandParseError::NotCommand)
    );
    assert_eq!(
        SlashCommandInvocation::try_from("/"),
        Err(SlashCommandParseError::EmptyName)
    );
}

/// Pi uses only U+0020 as the command-name delimiter.
#[test]
fn slash_invocation_keeps_tabs_and_newlines_in_the_name() {
    let tabbed = SlashCommandInvocation::try_from("/review\tpath")
        .expect("tabbed command name");
    let multiline = SlashCommandInvocation::try_from("/review\npath")
        .expect("multiline command name");

    assert_eq!(tabbed.name, "review\tpath");
    assert_eq!(multiline.name, "review\npath");
    assert!(tabbed.arguments.is_empty());
    assert!(multiline.arguments.is_empty());
}

/// Multiblock ACP input is never dispatched as a Slash Command.
#[test]
fn multiblock_input_is_not_a_slash_candidate() {
    let input = RunInput::Blocks(vec![
        ContentBlock::Text {
            text: "/compact".to_string(),
        },
        ContentBlock::Image {
            data: "iVBORw0KGgo=".to_string(),
            mime_type: "image/png".to_string(),
        },
    ]);

    assert!(input.slash_command_text().is_none());
}

/// Expanded user messages display their invocation while retaining model-only expanded blocks.
#[test]
fn expanded_user_message_has_user_role_and_model_text() {
    let invocation =
        SlashCommandInvocation::try_from("/skill:review src/lib.rs")
            .expect("Skill command");
    let content = MessageContent::ExpandedUser {
        expansion: SlashCommandExpansion::builder()
            .invocation(invocation)
            .source(SlashCommandSource::Skill)
            .model_blocks(vec![ContentBlock::Text {
                text: "<skill name=\"review\">review body</skill>".to_string(),
            }])
            .build(),
    };

    assert_eq!(content.role(), Some(Role::User));
    assert_eq!(
        content.text_content().as_deref(),
        Some("<skill name=\"review\">review body</skill>")
    );
}

/// Persisted command output retains precision-safe Turn and message timing fields.
#[test]
fn command_output_message_serializes_exact_identity_and_timing() {
    let message = AgentMessage {
        identity: MessageIdentity {
            message_id: MessageId::try_from("message-command")
                .expect("MessageId"),
            turn_id: TurnId::try_from("turn-command").expect("TurnId"),
        },
        timing: MessageTiming::try_from((
            TimestampMs::from(9_007_199_254_740_993),
            TimestampMs::from(9_007_199_254_740_994),
            TimestampMs::from(9_007_199_254_740_995),
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
    };

    let value = serde_json::to_value(message).expect("serialize message");

    assert_eq!(value["message_id"], "message-command");
    assert_eq!(value["turn_id"], "turn-command");
    assert_eq!(value["timestamp_ms"], "9007199254740993");
    assert_eq!(value["started_at_ms"], "9007199254740994");
    assert_eq!(value["ended_at_ms"], "9007199254740995");
    assert_eq!(value["content"]["type"], "slash_command");
    assert_eq!(value["content"]["message"]["type"], "output");
}

/// Slash Command lifecycle events round-trip with the shared event envelope.
#[test]
fn slash_command_events_round_trip() {
    let invocation =
        SlashCommandInvocation::try_from("/session").expect("Session command");
    let events = [
        AgentEvent {
            metadata: EventMetadata {
                turn_id: TurnId::try_from("turn-start").expect("TurnId"),
                timestamp_ms: TimestampMs::from(1_000),
                sequence: Sequence::try_from(1).expect("Sequence"),
            },
            payload: AgentEventPayload::SlashCommandStart {
                run_id: RunId::try_from("run-command").expect("RunId"),
                invocation,
            },
        },
        AgentEvent {
            metadata: EventMetadata {
                turn_id: TurnId::try_from("turn-start").expect("TurnId"),
                timestamp_ms: TimestampMs::from(1_001),
                sequence: Sequence::try_from(2).expect("Sequence"),
            },
            payload: AgentEventPayload::SlashCommandEnd {
                run_id: RunId::try_from("run-command").expect("RunId"),
                status: SlashCommandStatus::Succeeded,
            },
        },
    ];

    for event in events {
        let value = serde_json::to_value(&event).expect("serialize event");
        let decoded: AgentEvent =
            serde_json::from_value(value).expect("deserialize event");
        assert_eq!(decoded, event);
    }
}

/// Client-facing command failures remain stable and list executable ambiguity candidates.
#[test]
fn slash_command_errors_hide_internal_details_and_explain_ambiguity() {
    let failed = SlashCommandError::ExecutionFailed {
        command: "sample:fails".to_string(),
    };
    let ambiguous = SlashCommandError::Ambiguous {
        command: "inspect".to_string(),
        candidates: vec![
            "first:inspect".to_string(),
            "second:inspect".to_string(),
        ],
    };

    assert_eq!(failed.to_string(), "/sample:fails failed");
    assert_eq!(
        ambiguous.to_string(),
        "/inspect is ambiguous; use /first:inspect or /second:inspect"
    );
}
