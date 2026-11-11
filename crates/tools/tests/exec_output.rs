//! Output buffering, argument rules, and model-facing text of unified exec.

use std::time::Duration;

use protocol::{ToolCall, ToolCallId};
use tools::ToolError;
use tools::exec::{
    Captured, ExecCommandArgs, ExecResponse, HeadTailBuffer, OUTPUT_MAX_BYTES,
    OutputBuffers, WriteStdinArgs,
};

/// Collects literal text the way a process buffer hands it to a response.
fn captured(text: &str) -> Captured {
    let mut buffer = HeadTailBuffer::new(OUTPUT_MAX_BYTES);
    buffer.push(text.as_bytes());
    buffer.take()
}

/// Builds one response around literal output for header assertions.
fn response(
    process_id: Option<i32>,
    exit_code: Option<i32>,
    text: &str,
) -> ExecResponse {
    ExecResponse::builder()
        .chunk_id("3fa2c1".to_string())
        .wall_time(Duration::from_millis(1_250))
        .process_id(process_id)
        .exit_code(exit_code)
        .output(captured(text))
        .max_output_tokens(10_000)
        .terminal_call_id(ToolCallId::try_from("call-1").expect("call id"))
        .build()
}

/// Builds one tool call with literal JSON arguments.
fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall::builder()
        .tool_call_id(ToolCallId::try_from("call-1").expect("call id"))
        .name(name.to_string())
        .arguments(arguments)
        .build()
}

/// The buffer keeps the first and last halves and counts what it dropped.
#[test]
fn head_tail_buffer_keeps_both_ends_and_counts_omitted() {
    let mut buffer = HeadTailBuffer::new(16);
    buffer.push(b"0123456789012345678901234567890123456789");

    let captured = buffer.take();

    assert_eq!(captured.omitted_bytes, 24);
    let text = captured.text();
    assert!(text.starts_with("01234567"), "{text}");
    assert!(text.ends_with("23456789"), "{text}");
    assert!(text.contains("... 24 bytes omitted ..."), "{text}");
    assert!(buffer.take().bytes.is_empty(), "take drains the buffer");
}

/// The newest bytes decode on their own and never reach across a dropped middle.
#[test]
fn head_tail_buffer_decodes_only_its_newest_bytes() {
    let mut contiguous = HeadTailBuffer::new(8);
    contiguous.push(b"abcdef");
    let mut omitted = HeadTailBuffer::new(16);
    omitted.push(b"0123456789012345678901234567890123456789");

    assert_eq!(
        contiguous.last_text(3),
        "def",
        "head and tail are contiguous"
    );
    assert_eq!(contiguous.last_text(99), "abcdef");
    assert_eq!(omitted.last_text(5), "56789");
    assert_eq!(
        omitted.last_text(99),
        "23456789",
        "only the tail after a gap"
    );
}

/// A running process reports its session ID before the output.
#[test]
fn response_header_for_running_process() {
    let text = response(Some(48213), None, "hi").model_text();

    assert!(text.starts_with("Chunk ID: 3fa2c1\nWall time: 1.2500 seconds\n"));
    assert!(text.contains("\nProcess running with session ID 48213\n"));
    assert!(
        text.ends_with("\nOriginal token count: 1\nOutput:\nhi"),
        "{text}"
    );
}

/// A finished process reports its exit code and no session ID.
#[test]
fn response_header_for_exited_process() {
    let response = response(None, Some(3), "boom");
    let text = response.model_text();

    assert!(text.contains("\nProcess exited with code 3\n"), "{text}");
    assert!(!text.contains("session ID"));
    assert!(response.is_error());
}

/// Output over the token budget keeps both ends around a token marker.
#[test]
fn response_truncates_middle_by_token_budget() {
    let output = "a".repeat(200) + &"b".repeat(200);
    let text = ExecResponse::builder()
        .chunk_id("3fa2c1".to_string())
        .wall_time(Duration::from_millis(5))
        .exit_code(Some(0))
        .output(captured(&output))
        .max_output_tokens(10)
        .terminal_call_id(ToolCallId::try_from("call-1").expect("call id"))
        .build()
        .model_text();

    let body = text.split_once("\nOutput:\n").expect("body").1;
    assert!(
        body.starts_with(
            "Warning: truncated output (original token count: 100)\n"
        ),
        "{body}"
    );
    assert!(body.contains("tokens truncated…"), "{body}");
    assert!(body.trim_end().ends_with('b'), "{body}");
}

/// Exec yield times are clamped to codex's 250-60000 ms window.
#[cfg(not(windows))]
#[test]
fn exec_args_clamp_yield_time() {
    let args = |yield_ms: Option<u64>| {
        let mut value = serde_json::json!({ "cmd": "true" });
        if let Some(yield_ms) = yield_ms {
            value["yield_time_ms"] = serde_json::json!(yield_ms);
        }
        ExecCommandArgs::try_from(&call("exec_command", value)).expect("args")
    };

    assert_eq!(args(Some(1)).yield_time(), Duration::from_millis(250));
    assert_eq!(
        args(Some(99_999)).yield_time(),
        Duration::from_millis(60_000)
    );
    assert_eq!(args(None).yield_time(), Duration::from_millis(60_000));
    assert!(args(None).login, "login defaults to true");
    assert!(!args(None).tty, "tty defaults to false");
}

/// Polls wait at least five seconds; writes stay responsive.
#[test]
fn write_args_clamp_yield_time() {
    let max_background = Duration::from_millis(300_000);
    let args = |chars: &str, yield_ms: Option<u64>| {
        let mut value =
            serde_json::json!({ "session_id": 1000, "chars": chars });
        if let Some(yield_ms) = yield_ms {
            value["yield_time_ms"] = serde_json::json!(yield_ms);
        }
        WriteStdinArgs::try_from(&call("write_stdin", value)).expect("args")
    };

    assert_eq!(
        args("", Some(100)).yield_time(max_background),
        Duration::from_millis(5_000)
    );
    assert_eq!(
        args("", Some(999_999)).yield_time(max_background),
        max_background
    );
    assert_eq!(
        args("y\n", None).yield_time(max_background),
        Duration::from_millis(250)
    );
    assert_eq!(
        args("y\n", Some(99_999)).yield_time(max_background),
        Duration::from_millis(60_000)
    );
}

/// A call without `cmd` is rejected before anything runs.
#[test]
fn exec_args_require_cmd() {
    let error = ExecCommandArgs::try_from(&call(
        "exec_command",
        serde_json::json!({ "workdir": "/tmp" }),
    ))
    .expect_err("missing cmd");

    assert!(
        matches!(error, ToolError::InvalidArguments { .. }),
        "{error}"
    );
}

/// Once memory would drop bytes, the whole transcript goes to a file, and only
/// a model view that lost bytes points there.
#[test]
fn transcript_spills_every_byte_once_memory_would_drop() {
    let input: Vec<u8> = (0..40_u8).map(|byte| b'a' + byte % 26).collect();
    let mut buffers = OutputBuffers::new(16);
    buffers.push_chunk(&input[..10]);
    let (early, early_file) = buffers.take_pending();
    assert_eq!(early.omitted_bytes, 0);
    assert_eq!(early_file, None, "nothing dropped, nothing spilled");

    for chunk in input[10..].chunks(10) {
        buffers.push_chunk(chunk);
    }
    let (late, late_file) = buffers.take_pending();

    assert!(late.omitted_bytes > 0);
    let path = late_file.expect("spilled transcript");
    let kept = std::fs::read(&path).expect("spill file");
    std::fs::remove_file(&path).expect("remove spill");
    assert_eq!(kept, input, "file holds the whole transcript");
    assert_eq!(buffers.spilled_transcript(), Some(path));
}

/// The spill file stops at 64 times the buffer and says so.
#[test]
fn transcript_spill_stops_at_its_cap() {
    let input: Vec<u8> = (0..2_000_u32)
        .map(|index| b'a' + u8::try_from(index % 26).expect("letter"))
        .collect();
    let mut buffers = OutputBuffers::new(16);

    for chunk in input.chunks(100) {
        buffers.push_chunk(chunk);
    }

    let path = buffers.spilled_transcript().expect("spilled transcript");
    let kept = std::fs::read(&path).expect("spill file");
    std::fs::remove_file(&path).expect("remove spill");
    let (written, marker) = kept.split_at(1_024);
    assert_eq!(written, &input[..1_024]);
    assert_eq!(
        String::from_utf8_lossy(marker),
        "\n... output past 1024 bytes was not kept ...\n"
    );
}
