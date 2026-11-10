//! ACP projection of unified exec: terminals, background results, and exits.

use std::path::PathBuf;
use std::time::Duration;

use acp::AcpEventMapper;
use protocol::{
    AgentEvent, AgentEventPayload, ContentBlock, EventMetadata, ExecExit,
    RunId, Sequence, TimestampMs, ToolCall, ToolCallId, ToolResult,
    ToolResultDetails, TurnId,
};

/// Maps one event and serializes its updates.
fn mapped(payload: AgentEventPayload) -> Vec<serde_json::Value> {
    AcpEventMapper::map(AgentEvent {
        metadata: EventMetadata {
            turn_id: TurnId::try_from("turn-exec").expect("turn id"),
            timestamp_ms: TimestampMs::from(6_000),
            sequence: Sequence::try_from(1).expect("sequence"),
        },
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

/// The single update of a kind, failing when there is not exactly one.
fn only<'a>(
    updates: &'a [serde_json::Value],
    kind: &str,
) -> &'a serde_json::Value {
    let matching: Vec<_> = updates
        .iter()
        .filter(|update| update["sessionUpdate"] == kind)
        .collect();
    assert_eq!(matching.len(), 1, "{kind} in {updates:?}");
    matching.into_iter().next().expect("one update")
}

/// Builds an exec tool result with the given text and details.
fn exec_result(
    call: &str,
    text: &str,
    details: ToolResultDetails,
    is_error: bool,
) -> ToolResult {
    ToolResult::builder()
        .tool_call_id(ToolCallId::try_from(call).expect("call id"))
        .blocks(vec![ContentBlock::Text {
            text: text.to_string(),
        }])
        .is_error(is_error)
        .details(details)
        .build()
}

/// Exec details for a call on the terminal of `call-1`.
fn details(
    process_id: Option<i32>,
    exit_code: Option<i32>,
) -> ToolResultDetails {
    ToolResultDetails::Exec {
        process_id,
        exit_code,
        signal: None,
        chunk_id: "a".to_string(),
        wall_time_ms: 100,
        original_token_count: 1,
        full_output_path: None,
        terminal_call_id: ToolCallId::try_from("call-1").expect("call id"),
    }
}

/// A finished end event for one result.
fn end(result: ToolResult) -> AgentEventPayload {
    AgentEventPayload::ToolExecutionEnd {
        run_id: RunId::try_from("run-exec").expect("run id"),
        result,
    }
}

/// A background exit of `call-1`.
fn exited(exit_code: Option<i32>, lost: bool) -> AgentEventPayload {
    AgentEventPayload::ExecExited {
        exit: ExecExit::builder()
            .tool_call_id(ToolCallId::try_from("call-1").expect("call id"))
            .turn_id(TurnId::try_from("turn-exec").expect("turn id"))
            .process_id(48213)
            .exit_code(exit_code)
            .output("final\n".to_string())
            .wall_time_ms(1_000)
            .lost(lost)
            .build(),
    }
}

/// The terminal shows exactly the output the tool rendered, without its
/// header or full-output footer; the footer becomes its own content item.
#[test]
fn exec_terminal_body_comes_from_the_rendered_result() {
    let mut buffer =
        tools::exec::HeadTailBuffer::new(tools::exec::OUTPUT_MAX_BYTES);
    buffer.push(b"hello\n");
    let response = tools::exec::ExecResponse::builder()
        .chunk_id("a".to_string())
        .wall_time(Duration::from_millis(5))
        .exit_code(Some(0))
        .output(buffer.take())
        .max_output_tokens(10_000)
        .terminal_call_id(ToolCallId::try_from("call-1").expect("call id"))
        .full_output_path(Some(PathBuf::from("/tmp/full.log")))
        .build();

    let updates = mapped(end(exec_result(
        "call-1",
        &response.model_text(),
        response.details(),
        false,
    )));

    assert_eq!(
        terminal_output(only(&updates, "terminal_update")),
        "hello\n"
    );
    let tool = only(&updates, "tool_call_update");
    assert_eq!(
        tool["content"][1]["content"]["text"],
        "Full output: /tmp/full.log"
    );
}

/// Starting a command opens its terminal with the command and directory.
#[test]
fn exec_start_opens_terminal_with_cmd() {
    let updates = mapped(AgentEventPayload::ToolExecutionStart {
        run_id: RunId::try_from("run-exec").expect("run id"),
        call: ToolCall::builder()
            .tool_call_id(ToolCallId::try_from("call-1").expect("call id"))
            .name("exec_command".to_string())
            .arguments(serde_json::json!({ "cmd": "ls -la" }))
            .build(),
        cwd: "/work".into(),
        locations: Vec::new(),
    });

    let tool = only(&updates, "tool_call_update");
    assert_eq!(tool["kind"], "execute");
    assert_eq!(
        tool["content"][0],
        serde_json::json!({ "type": "terminal", "terminalId": "call-1" })
    );
    let terminal = only(&updates, "terminal_update");
    assert_eq!(terminal["terminalId"], "call-1");
    assert_eq!(terminal["command"], "ls -la");
    assert_eq!(terminal["cwd"], "/work");
}

/// Output snapshots replace the terminal's whole output.
#[test]
fn exec_output_replaces_terminal_output() {
    let updates = mapped(AgentEventPayload::ExecOutput {
        tool_call_id: ToolCallId::try_from("call-1").expect("call id"),
        process_id: 48213,
        output: "line 1\nline 2".to_string(),
    });

    assert_eq!(updates.len(), 1, "{updates:?}");
    assert_eq!(updates[0]["terminalId"], "call-1");
    assert_eq!(terminal_output(&updates[0]), "line 1\nline 2");
}

/// A command still running sets no status and leaves the terminal open.
#[test]
fn background_result_sets_no_status() {
    let updates = mapped(end(exec_result(
        "call-1",
        "Chunk ID: a\nProcess running with session ID 48213\nOutput:\n",
        details(Some(48213), None),
        false,
    )));

    let tool = only(&updates, "tool_call_update");
    assert!(tool.get("status").is_none(), "{tool}");
    assert_eq!(tool["content"][0]["terminalId"], "call-1");
    assert!(
        updates
            .iter()
            .all(|update| update["sessionUpdate"] != "terminal_update")
    );
}

/// A command that finished closes its terminal with the result body.
#[test]
fn exited_result_closes_terminal_with_body() {
    let updates = mapped(end(exec_result(
        "call-1",
        "Chunk ID: a\nWall time: 0.1 seconds\nProcess exited with code 0\nOriginal token count: 1\nOutput:\nhi",
        details(None, Some(0)),
        false,
    )));

    let terminal = only(&updates, "terminal_update");
    assert_eq!(terminal_output(terminal), "hi");
    assert_eq!(terminal["exitStatus"]["exitCode"], 0);
    assert_eq!(only(&updates, "tool_call_update")["status"], "completed");
}

/// A background exit closes the terminal; a lost one fails with an explanation.
#[test]
fn exec_exited_closes_terminal_and_sets_status() {
    let done = mapped(exited(Some(0), false));
    let lost = mapped(exited(None, true));

    let terminal = only(&done, "terminal_update");
    assert_eq!(terminal_output(terminal), "final\n");
    assert_eq!(terminal["exitStatus"]["exitCode"], 0);
    assert_eq!(only(&done, "tool_call_update")["status"], "completed");
    let lost_tool = only(&lost, "tool_call_update");
    assert_eq!(lost_tool["status"], "failed");
    assert!(
        lost_tool
            .to_string()
            .contains("进程已不在本会话中运行（服务重启或会话分叉）"),
        "{lost_tool}"
    );
}

/// An exit that arrives before the "running" result keeps its final status.
#[test]
fn exit_before_result_keeps_final_status() {
    let exit = mapped(exited(Some(0), false));
    let result = mapped(end(exec_result(
        "call-1",
        "Process running with session ID 48213\nOutput:\n",
        details(Some(48213), None),
        false,
    )));

    assert_eq!(only(&exit, "tool_call_update")["status"], "completed");
    assert!(only(&result, "tool_call_update").get("status").is_none());
}

/// `write_stdin` cards show what was sent and never duplicate the output.
#[test]
fn write_stdin_card_shows_sent_chars_without_output() {
    let start = mapped(AgentEventPayload::ToolExecutionStart {
        run_id: RunId::try_from("run-exec").expect("run id"),
        call: ToolCall::builder()
            .tool_call_id(ToolCallId::try_from("call-2").expect("call id"))
            .name("write_stdin".to_string())
            .arguments(
                serde_json::json!({ "session_id": 48213, "chars": "y\n" }),
            )
            .build(),
        cwd: "/work".into(),
        locations: Vec::new(),
    });
    let poll = mapped(AgentEventPayload::ToolExecutionStart {
        run_id: RunId::try_from("run-exec").expect("run id"),
        call: ToolCall::builder()
            .tool_call_id(ToolCallId::try_from("call-3").expect("call id"))
            .name("write_stdin".to_string())
            .arguments(serde_json::json!({ "session_id": 48213 }))
            .build(),
        cwd: "/work".into(),
        locations: Vec::new(),
    });
    let finished = mapped(end(exec_result(
        "call-2",
        "Process exited with code 0\nOutput:\nsecret output",
        details(None, Some(0)),
        false,
    )));

    assert!(
        start
            .iter()
            .all(|update| update["sessionUpdate"] != "terminal_update")
    );
    assert_eq!(
        only(&start, "tool_call_update")["content"][0]["content"]["text"],
        "向 session 48213 发送：y\\n"
    );
    assert_eq!(
        only(&poll, "tool_call_update")["content"][0]["content"]["text"],
        "等待 session 48213 输出"
    );
    assert!(
        finished
            .iter()
            .all(|update| update["sessionUpdate"] != "terminal_update")
    );
    let tool = only(&finished, "tool_call_update");
    assert_eq!(tool["status"], "completed");
    assert!(tool.get("content").is_none(), "{tool}");
}
