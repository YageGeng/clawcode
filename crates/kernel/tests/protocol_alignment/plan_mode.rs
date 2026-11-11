//! Plan mode: persisted session mode, model-facing notes, tool gating, and approval.

use std::path::PathBuf;

use protocol::{AgentEventPayload, SessionId, SessionMode};

use super::fixture::{Route, TestKernel};

/// Finds the JSONL file that backs one Session under the fixture's store root.
fn session_file(test: &TestKernel, session: &SessionId) -> PathBuf {
    let suffix = format!("_{session}.jsonl");
    let mut pending = vec![test.root.path().join("sessions")];
    let mut found = None;
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read sessions") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(&suffix))
            {
                found = Some(path);
            }
        }
    }
    found.expect("session file")
}

/// Returns every persisted line of a Session file as JSON.
fn session_lines(
    test: &TestKernel,
    session: &SessionId,
) -> Vec<serde_json::Value> {
    std::fs::read_to_string(session_file(test, session))
        .expect("read session file")
        .lines()
        .map(|line| serde_json::from_str(line).expect("json line"))
        .collect()
}

/// Returns the persisted session-mode entries in file order.
fn mode_entries(
    test: &TestKernel,
    session: &SessionId,
) -> Vec<serde_json::Value> {
    session_lines(test, session)
        .into_iter()
        .filter(|line| {
            line["type"] == "custom" && line["customType"] == "session_mode"
        })
        .collect()
}

/// Returns the mode carried by every `SettingsChanged` event of a Session.
fn reported_modes(test: &TestKernel, session: &SessionId) -> Vec<SessionMode> {
    test.sinks
        .payloads(session)
        .into_iter()
        .filter_map(|payload| match payload {
            AgentEventPayload::SettingsChanged { settings } => {
                Some(settings.mode)
            }
            _ => None,
        })
        .collect()
}

/// Closes and reopens a Session so its state is rebuilt from the Store.
async fn reopen(test: &TestKernel, session: &SessionId) {
    test.kernel
        .close_session(session)
        .await
        .expect("close Session");
    test.kernel
        .resume_session(session.clone(), test.root.path().to_path_buf())
        .await
        .expect("resume Session");
}

/// Appends one raw custom entry after the current main-lane leaf of a closed Session.
fn append_custom_entry(
    test: &TestKernel,
    session: &SessionId,
    id: &str,
    extension_id: &str,
    data: serde_json::Value,
) {
    let path = session_file(test, session);
    let lines = session_lines(test, session);
    let sequence = lines
        .iter()
        .filter_map(|line| line["seq"].as_u64())
        .max()
        .unwrap_or(0)
        + 1;
    let parent = lines
        .iter()
        .rev()
        .find(|line| line["kind"] == "entry" && line["lane"] == "main")
        .map(|line| line["id"].clone())
        .unwrap_or(serde_json::Value::Null);
    let entry = serde_json::json!({
        "kind": "entry",
        "lane": "main",
        "id": id,
        "seq": sequence,
        "parentId": parent,
        "timestamp": "1",
        "type": "custom",
        "extensionId": extension_id,
        "customType": "session_mode",
        "data": data});
    let mut content =
        std::fs::read_to_string(&path).expect("read session file");
    content.push_str(&entry.to_string());
    content.push('\n');
    std::fs::write(&path, content).expect("write session file");
}

/// A mode switch is written once as a clawcode custom entry, reported, and restored.
#[tokio::test]
async fn mode_change_is_persisted_and_restored() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let session = test.create("plan-mode-persist").await;
    assert_eq!(
        test.kernel
            .session_settings(&session)
            .expect("settings")
            .mode,
        SessionMode::Default
    );

    let mode = test
        .kernel
        .set_session_mode(&session, SessionMode::Plan)
        .await
        .expect("set mode");
    assert_eq!(mode, SessionMode::Plan);
    test.kernel
        .set_session_mode(&session, SessionMode::Plan)
        .await
        .expect("set the same mode");

    let entries = mode_entries(&test, &session);
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["extensionId"], "clawcode");
    assert_eq!(entries[0]["data"]["mode"], "plan");
    assert!(entries[0]["data"]["turnId"].is_string(), "{entries:?}");
    assert_eq!(reported_modes(&test, &session), vec![SessionMode::Plan]);

    reopen(&test, &session).await;
    assert_eq!(
        test.kernel
            .session_settings(&session)
            .expect("settings")
            .mode,
        SessionMode::Plan
    );
}

/// Navigating to an entry before the switch restores that branch's mode.
#[tokio::test]
async fn mode_follows_the_branch() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let session = test.create("plan-mode-branch").await;
    test.run_text(&session, "first")
        .await
        .expect("first prompt");
    let before = test
        .kernel
        .session_tree(&session)
        .expect("tree")
        .leaf_id
        .expect("leaf");
    test.kernel
        .set_session_mode(&session, SessionMode::Plan)
        .await
        .expect("set mode");

    test.kernel
        .navigate_session(&session, Some(before))
        .await
        .expect("navigate");
    assert_eq!(
        test.kernel
            .session_settings(&session)
            .expect("settings")
            .mode,
        SessionMode::Default
    );
    // Clients learn the branch's mode the way they learn any settings change.
    assert_eq!(
        reported_modes(&test, &session).last(),
        Some(&SessionMode::Default)
    );
}

/// Entries written by another extension, or holding values this version does not
/// know, leave the Session openable and the last valid mode in place.
#[tokio::test]
async fn foreign_or_unknown_mode_entries_are_skipped() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let session = test.create("plan-mode-skip").await;
    test.kernel
        .set_session_mode(&session, SessionMode::Plan)
        .await
        .expect("set mode");
    test.kernel
        .close_session(&session)
        .await
        .expect("close Session");
    append_custom_entry(
        &test,
        &session,
        "entry-foreign",
        "other",
        serde_json::json!({ "mode": "default", "turnId": "turn-foreign" }),
    );
    append_custom_entry(
        &test,
        &session,
        "entry-future",
        "clawcode",
        serde_json::json!({ "mode": "review", "turnId": "turn-future" }),
    );
    append_custom_entry(
        &test,
        &session,
        "entry-no-turn",
        "clawcode",
        serde_json::json!({ "mode": "default" }),
    );

    test.kernel
        .resume_session(session.clone(), test.root.path().to_path_buf())
        .await
        .expect("resume Session");
    assert_eq!(
        test.kernel
            .session_settings(&session)
            .expect("settings")
            .mode,
        SessionMode::Plan
    );
}

/// Returns whether a context message is the kernel's plan-mode note.
fn is_mode_note(message: &protocol::AgentMessage) -> bool {
    matches!(
        &message.content,
        protocol::MessageContent::Extension { extension }
            if extension.custom_type == "session_mode"
                && extension.extension_id.as_str() == "clawcode"
                && !extension.display
                && extension.include_in_context
    )
}

/// Returns the text of a plan-mode note.
fn note_text(message: &protocol::AgentMessage) -> String {
    match &message.content {
        protocol::MessageContent::Extension { extension } => extension
            .blocks
            .iter()
            .filter_map(|block| match block {
                protocol::ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect(),
        _ => String::new(),
    }
}

/// Returns every provider request the scripted model received.
fn requests(test: &TestKernel) -> Vec<protocol::ModelRequest> {
    test.model.requests.lock().expect("requests").clone()
}

/// Asserts that a later request replays an earlier request's history unchanged.
fn assert_history_prefix(
    earlier: &protocol::ModelRequest,
    later: &protocol::ModelRequest,
) {
    // The system prompt carries per-Turn identity, so compare its content only.
    assert_eq!(
        earlier.messages.first().map(|message| &message.content),
        later.messages.first().map(|message| &message.content)
    );
    assert_eq!(
        earlier.messages.get(1..),
        later.messages.get(1..earlier.messages.len()),
        "the later request must keep the earlier history as its prefix"
    );
}

/// Entering plan mode adds one note at the switch and changes nothing before it.
#[tokio::test]
async fn entering_plan_appends_a_note_without_touching_the_prefix() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let session = test.create("plan-note-prefix").await;
    test.run_text(&session, "first")
        .await
        .expect("first prompt");
    test.kernel
        .set_session_mode(&session, SessionMode::Plan)
        .await
        .expect("set mode");
    test.run_text(&session, "second")
        .await
        .expect("second prompt");

    let requests = requests(&test);
    let first = requests.first().expect("first request");
    let second = requests.get(1).expect("second request");
    assert_eq!(first.tools, second.tools);
    assert_history_prefix(first, second);
    let added = second
        .messages
        .get(first.messages.len()..)
        .expect("added messages");
    assert_eq!(added.len(), 3, "reply, note, and prompt: {added:?}");
    assert!(added.first().is_some_and(|message| matches!(
        message.content,
        protocol::MessageContent::Assistant { .. }
    )));
    let note = added.get(1).expect("note");
    assert!(is_mode_note(note), "{note:?}");
    assert!(note_text(note).contains("exit_plan_mode"));
    assert_eq!(
        added
            .get(2)
            .map(|message| super::fixture::text(&message.content))
            .as_deref(),
        Some("second")
    );
}

/// A switch made while tools run is noted after their results, live and when reopened.
#[tokio::test]
async fn note_waits_for_the_tool_results() {
    let test = TestKernel::new(|request| {
        match request.messages.last().map(|message| &message.content) {
            Some(protocol::MessageContent::User { .. }) => Route::Events(vec![
                protocol::ModelStreamEvent::ToolCall(
                    protocol::ToolCall::builder()
                        .tool_call_id(
                            protocol::ToolCallId::try_from("call-sleep")
                                .expect("id"),
                        )
                        .name("exec_command".into())
                        .arguments(serde_json::json!({ "cmd": "sleep 1", "login": false }))
                        .build(),
                ),
                super::fixture::finished(protocol::StopReason::ToolUse),
            ]),
            _ => Route::answer("done")}
    });
    let session = test.create("plan-note-tools").await;
    let prompt = test.start(&session, "run");
    test.wait_until(|| {
        test.sinks.payloads(&session).iter().any(|payload| {
            matches!(payload, AgentEventPayload::ToolExecutionStart { .. })
        })
    })
    .await;
    test.kernel
        .set_session_mode(&session, SessionMode::Plan)
        .await
        .expect("set mode");
    prompt.await.expect("join").expect("prompt");

    let live = requests(&test).get(1).cloned().expect("second request");
    let mut tail = live.messages.iter().rev();
    assert!(tail.next().is_some_and(is_mode_note));
    assert!(tail.next().is_some_and(|message| matches!(
        message.content,
        protocol::MessageContent::ToolResult { .. }
    )));

    reopen(&test, &session).await;
    test.run_text(&session, "again")
        .await
        .expect("reopened prompt");
    let rebuilt = requests(&test).last().expect("request").clone();
    assert_history_prefix(&live, &rebuilt);
}

/// A switch during a streaming reply is noted before that reply, live and reopened.
#[tokio::test]
async fn note_precedes_a_streaming_reply() {
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let route_gate = std::sync::Arc::clone(&gate);
    let test = TestKernel::new(move |request| {
        match request
            .messages
            .last()
            .map(|message| super::fixture::text(&message.content))
            .as_deref()
        {
            Some("first") => Route::Wait(
                std::sync::Arc::clone(&route_gate),
                vec![
                    protocol::ModelStreamEvent::TextDelta("ok".into()),
                    super::fixture::finished(protocol::StopReason::EndTurn),
                ],
            ),
            _ => Route::answer("ok"),
        }
    });
    let session = test.create("plan-note-streaming").await;
    let prompt = test.start(&session, "first");
    test.wait_until(|| !requests(&test).is_empty()).await;
    test.kernel
        .set_session_mode(&session, SessionMode::Plan)
        .await
        .expect("set mode");
    gate.notify_one();
    prompt.await.expect("join").expect("first prompt");
    test.run_text(&session, "second")
        .await
        .expect("second prompt");

    let live = requests(&test).get(1).cloned().expect("second request");
    let note = live.messages.iter().position(is_mode_note).expect("note");
    assert!(live.messages.get(note + 1).is_some_and(|message| matches!(
        message.content,
        protocol::MessageContent::Assistant { .. }
    )));

    reopen(&test, &session).await;
    test.run_text(&session, "third")
        .await
        .expect("third prompt");
    let rebuilt = requests(&test).last().expect("request").clone();
    assert_history_prefix(&live, &rebuilt);
}

/// A tool call whose results never arrived (a crash) does not swallow a later note.
#[tokio::test]
async fn dangling_tool_calls_do_not_swallow_the_note() {
    let test = TestKernel::new(|request| {
        match request
            .messages
            .last()
            .map(|message| super::fixture::text(&message.content))
            .as_deref()
        {
            Some("tool") => Route::Events(vec![
                protocol::ModelStreamEvent::ToolCall(
                    protocol::ToolCall::builder()
                        .tool_call_id(
                            protocol::ToolCallId::try_from("call-dangling")
                                .expect("id"),
                        )
                        .name("exec_command".into())
                        .arguments(
                            serde_json::json!({ "cmd": "pwd", "login": false }),
                        )
                        .build(),
                ),
                super::fixture::finished(protocol::StopReason::ToolUse),
            ]),
            _ => Route::answer("done"),
        }
    });
    let session = test.create("plan-note-dangling").await;
    test.run_text(&session, "tool").await.expect("tool prompt");
    test.kernel
        .close_session(&session)
        .await
        .expect("close Session");
    // Keep the file up to the Assistant tool call, as a crash mid-batch would.
    let path = session_file(&test, &session);
    let content = std::fs::read_to_string(&path).expect("read session file");
    let kept = content
        .lines()
        .take_while(|line| {
            !line.contains("\"tool_result\"")
                && !line.contains("\"toolResult\"")
        })
        .collect::<Vec<_>>();
    std::fs::write(&path, format!("{}\n", kept.join("\n"))).expect("truncate");
    test.kernel
        .resume_session(session.clone(), test.root.path().to_path_buf())
        .await
        .expect("resume Session");

    test.kernel
        .set_session_mode(&session, SessionMode::Plan)
        .await
        .expect("set mode");
    test.run_text(&session, "next").await.expect("next prompt");
    let live = requests(&test).last().expect("request").clone();
    let note = live
        .messages
        .iter()
        .position(is_mode_note)
        .expect("note must not be lost");
    let next = live
        .messages
        .iter()
        .position(|message| super::fixture::text(&message.content) == "next")
        .expect("next prompt");
    assert!(note < next, "the note precedes the new user message");

    reopen(&test, &session).await;
    test.run_text(&session, "again")
        .await
        .expect("reopened prompt");
    let rebuilt = requests(&test).last().expect("request").clone();
    assert_history_prefix(&live, &rebuilt);
}

/// A subagent started in plan mode is in plan mode and gets the delegated note.
#[tokio::test]
async fn subagent_inherits_plan_with_its_own_note() {
    let test = TestKernel::new(|request| {
        match request
            .messages
            .last()
            .map(|message| super::fixture::text(&message.content))
            .as_deref()
        {
            Some("Survey the code.") => Route::answer("findings"),
            _ => Route::answer("done"),
        }
    });
    let owner = test.create("plan-note-owner").await;
    let child = plan_child(&test, &owner).await;
    test.run_text(&child, "Survey the code.")
        .await
        .expect("child task");
    assert_eq!(
        test.kernel
            .session_settings(&child)
            .expect("child settings")
            .mode,
        SessionMode::Plan
    );
    let child_request = requests(&test)
        .into_iter()
        .find(|request| {
            request
                .messages
                .last()
                .map(|message| super::fixture::text(&message.content))
                .as_deref()
                == Some("Survey the code.")
        })
        .expect("child request");
    let note = child_request
        .messages
        .iter()
        .find(|message| is_mode_note(message))
        .expect("child note");
    let text = note_text(note);
    assert!(!text.contains("exit_plan_mode"), "{text}");
    assert!(text.contains("parent"), "{text}");
}

/// Puts the owner in plan mode, then creates its child through the ownership primitive.
async fn plan_child(test: &TestKernel, owner: &SessionId) -> SessionId {
    test.kernel
        .set_session_mode(owner, SessionMode::Plan)
        .await
        .expect("set mode");
    test.kernel
        .create_owned_session(
            kernel::OwnedSessionRequest::builder()
                .ownership(protocol::SessionOwnership {
                    session_id: owner.clone(),
                    name: protocol::AgentName::try_from("child").expect("name"),
                    nickname: "Euclid".into(),
                })
                .title(
                    protocol::SessionTitle::try_from("Euclid").expect("title"),
                )
                .model(None)
                .build(),
        )
        .await
        .expect("child")
}

/// Scripts one model response that calls the given tools.
fn tool_calls(calls: &[(&str, &str, serde_json::Value)]) -> Route {
    let mut events = calls
        .iter()
        .map(|(id, name, arguments)| {
            protocol::ModelStreamEvent::ToolCall(
                protocol::ToolCall::builder()
                    .tool_call_id(
                        protocol::ToolCallId::try_from(*id).expect("id"),
                    )
                    .name((*name).into())
                    .arguments(arguments.clone())
                    .build(),
            )
        })
        .collect::<Vec<_>>();
    events.push(super::fixture::finished(protocol::StopReason::ToolUse));
    Route::Events(events)
}

/// Routes the first prompt to `calls` and answers everything else.
fn calls_on(
    prompt: &'static str,
    calls: Vec<(&'static str, &'static str, serde_json::Value)>,
) -> impl Fn(&protocol::ModelRequest) -> Route + Send + Sync + 'static {
    move |request| {
        let last = request
            .messages
            .last()
            .map(|message| super::fixture::text(&message.content));
        if last.as_deref() == Some(prompt) {
            tool_calls(&calls)
        } else {
            Route::answer("done")
        }
    }
}

/// One tool result as the model saw it: error flag, text, and details.
struct SeenResult {
    is_error: bool,
    text: String,
    details: Option<protocol::ToolResultDetails>,
}

/// Finds the result the model received for one tool call.
fn seen_result(test: &TestKernel, call: &str) -> SeenResult {
    requests(test)
        .iter()
        .flat_map(|request| request.messages.iter())
        .find_map(|message| match &message.content {
            protocol::MessageContent::ToolResult {
                tool_call_id,
                blocks,
                is_error,
                details,
                ..
            } if tool_call_id.as_str() == call => Some(SeenResult {
                is_error: *is_error,
                text: blocks
                    .iter()
                    .filter_map(|block| match block {
                        protocol::ContentBlock::Text { text } => {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .collect(),
                details: details.clone(),
            }),
            _ => None,
        })
        .expect("tool result")
}

/// Finds a tool result in a Session's history, for calls the model never saw
/// answered because the Run ended after them.
fn result_in(test: &TestKernel, session: &SessionId, call: &str) -> SeenResult {
    test.kernel
        .session_messages(session)
        .expect("history")
        .iter()
        .find_map(|message| match &message.content {
            protocol::MessageContent::ToolResult {
                tool_call_id,
                is_error,
                details,
                ..
            } if tool_call_id.as_str() == call => Some(SeenResult {
                is_error: *is_error,
                text: result_text(message),
                details: details.clone(),
            }),
            _ => None,
        })
        .expect("tool result in history")
}

/// Plan mode refuses file changes before they run and keeps reading available.
#[tokio::test]
async fn plan_mode_blocks_edits_but_allows_reading() {
    let test = TestKernel::new(calls_on(
        "go",
        vec![
            (
                "call-write",
                "write",
                serde_json::json!({ "path": "created.txt", "content": "x" }),
            ),
            (
                "call-edit",
                "edit",
                serde_json::json!({ "path": "notes.txt", "edits": [{ "oldText": "hello", "newText": "bye" }] }),
            ),
            (
                "call-read",
                "read",
                serde_json::json!({ "path": "notes.txt" }),
            ),
            (
                "call-grep",
                "grep",
                serde_json::json!({ "pattern": "hello" }),
            ),
        ],
    ));
    std::fs::write(test.root.path().join("notes.txt"), "hello\n")
        .expect("seed");
    let session = test.create("plan-gate-edits").await;
    test.kernel
        .set_session_mode(&session, SessionMode::Plan)
        .await
        .expect("set mode");
    test.run_text(&session, "go").await.expect("prompt");

    for (call, tool) in [("call-write", "write"), ("call-edit", "edit")] {
        let result = seen_result(&test, call);
        assert!(result.is_error, "{call}");
        assert_eq!(
            result.text,
            format!(
                "Plan mode is active: {tool} is not allowed. Finish the plan and call exit_plan_mode."
            )
        );
        assert!(matches!(
            result.details,
            Some(protocol::ToolResultDetails::Blocked {
                terminate: false,
                ..
            })
        ));
    }
    assert!(!test.root.path().join("created.txt").exists());
    assert_eq!(
        std::fs::read_to_string(test.root.path().join("notes.txt"))
            .expect("read"),
        "hello\n"
    );
    assert!(!seen_result(&test, "call-read").is_error);
    assert!(!seen_result(&test, "call-grep").is_error);
}

/// Plan mode runs read-only commands and refuses writes without echoing the command.
#[tokio::test]
async fn plan_mode_runs_read_only_exec_only() {
    let logs = super::support::CapturedLogs::default();
    let test = TestKernel::new(calls_on(
        "go",
        vec![
            (
                "call-ls",
                "exec_command",
                serde_json::json!({ "cmd": "ls" }),
            ),
            (
                "call-echo",
                "exec_command",
                serde_json::json!({ "cmd": "echo hi > redirected.txt" }),
            ),
        ],
    ));
    let session = test.create("plan-gate-bash").await;
    test.kernel
        .set_session_mode(&session, SessionMode::Plan)
        .await
        .expect("set mode");
    test.run_text(&session, "go").await.expect("prompt");

    assert!(!seen_result(&test, "call-ls").is_error);
    let refused = seen_result(&test, "call-echo");
    assert!(refused.is_error);
    assert!(refused.text.contains("redirection"), "{}", refused.text);
    assert!(!refused.text.contains("echo hi"), "{}", refused.text);
    assert!(!test.root.path().join("redirected.txt").exists());
    assert!(!logs.content().contains("echo hi"));
}

/// Plan mode lets `write_stdin` poll or interrupt but never type input.
#[tokio::test]
async fn plan_mode_limits_write_stdin_to_poll_and_interrupt() {
    let test = TestKernel::new(calls_on(
        "go",
        vec![
            (
                "call-poll",
                "write_stdin",
                serde_json::json!({ "session_id": 1000, "yield_time_ms": 5000 }),
            ),
            (
                "call-type",
                "write_stdin",
                serde_json::json!({ "session_id": 1000, "chars": "y\n" }),
            ),
        ],
    ));
    let session = test.create("plan-gate-stdin").await;
    test.kernel
        .set_session_mode(&session, SessionMode::Plan)
        .await
        .expect("set mode");
    test.run_text(&session, "go").await.expect("prompt");

    let typed = seen_result(&test, "call-type");
    assert!(typed.is_error);
    assert!(
        typed.text.contains(
            "Plan mode is active: write_stdin may only poll or interrupt."
        ),
        "{}",
        typed.text
    );
    let polled = seen_result(&test, "call-poll");
    assert!(
        polled.text.contains("Unknown process id 1000"),
        "{}",
        polled.text
    );
}

/// A tool an extension supplies, with or without a read-only hint.
struct ProbeTool {
    name: &'static str,
    metadata: protocol::ToolMetadata,
}

impl ProbeTool {
    /// Builds a probe whose metadata carries the given read-only hint.
    fn new(name: &'static str, read_only: Option<bool>) -> Self {
        Self {
            name,
            metadata: protocol::ToolMetadata::builder()
                .annotations(
                    protocol::ToolAnnotations::builder()
                        .read_only_hint(read_only)
                        .build(),
                )
                .build(),
        }
    }
}

#[async_trait::async_trait]
impl tools::AgentTool for ProbeTool {
    /// Declares the probe under its configured name.
    fn definition(&self) -> protocol::ToolDefinition {
        protocol::ToolDefinition {
            name: self.name.into(),
            description: "Probe".into(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    /// Returns the configured read-only hint.
    fn metadata(&self) -> &protocol::ToolMetadata {
        &self.metadata
    }

    /// Answers with its own name so a test can tell it ran.
    async fn execute(
        &self,
        call: protocol::ToolCall,
        _context: &tools::ToolExecutionContext,
    ) -> Result<protocol::ToolResult, tools::ToolError> {
        Ok(protocol::ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![protocol::ContentBlock::Text {
                text: format!("ran {}", self.name),
            }])
            .is_error(false)
            .build())
    }
}

/// Extension that registers probes, including reserved names, statically and at start.
struct ProbeModule {
    runtime_registration: std::sync::Arc<std::sync::Mutex<Option<bool>>>,
}

impl extension::ExtensionModule for ProbeModule {
    /// Identifies the probe extension.
    fn descriptor(&self) -> protocol::ExtensionDescriptor {
        protocol::ExtensionDescriptor {
            id: protocol::ExtensionId::try_from("probe").expect("id"),
            name: "Probe".into(),
            version: "1".into(),
        }
    }

    /// Registers read-only and unmarked tools, a `spawn_agent` override, and reserved names.
    fn register(
        &self,
        registrar: &mut extension::ExtensionRegistrar,
    ) -> Result<(), extension::ExtensionError> {
        for tool in [
            ProbeTool::new("probe_ro", Some(true)),
            ProbeTool::new("probe_rw", None),
            ProbeTool::new(protocol::SPAWN_AGENT_TOOL, None),
            ProbeTool::new("todo_write", Some(true)),
            ProbeTool::new("exit_plan_mode", Some(true)),
        ] {
            registrar.register_tool(std::sync::Arc::new(tool))?;
        }
        registrar.on::<extension::SessionStartPoint, _>(ProbeStart {
            registration: std::sync::Arc::clone(&self.runtime_registration),
        })
    }
}

/// Records whether registering a reserved name at runtime succeeded.
struct ProbeStart {
    registration: std::sync::Arc<std::sync::Mutex<Option<bool>>>,
}

#[async_trait::async_trait]
impl extension::ExtensionHandler<extension::SessionStartPoint> for ProbeStart {
    /// Tries to take a reserved name after the Session started.
    async fn handle(
        &self,
        _event: &protocol::SessionStartEvent,
        context: &extension::ExtensionContext,
    ) -> Result<(), extension::ExtensionError> {
        let result = context
            .register_tool(std::sync::Arc::new(ProbeTool::new(
                "todo_write",
                Some(true),
            )))
            .await;
        *self.registration.lock().expect("registration") = Some(result.is_ok());
        Ok(())
    }
}

/// Builds a fixture whose only extension is the probe module.
fn probe_kernel(
    route: impl Fn(&protocol::ModelRequest) -> Route + Send + Sync + 'static,
) -> (TestKernel, std::sync::Arc<std::sync::Mutex<Option<bool>>>) {
    let registration = std::sync::Arc::new(std::sync::Mutex::new(None));
    let module_registration = std::sync::Arc::clone(&registration);
    let factory = extension::StaticExtensionFactory::new(
        Default::default(),
        vec![std::sync::Arc::new(move || {
            Ok(std::sync::Arc::new(ProbeModule {
                runtime_registration: std::sync::Arc::clone(
                    &module_registration,
                ),
            })
                as std::sync::Arc<dyn extension::ExtensionModule>)
        })],
    );
    (
        TestKernel::with_extensions(route, std::sync::Arc::new(factory)),
        registration,
    )
}

/// Other tools run in plan mode only when their metadata says they are read-only.
#[tokio::test]
async fn read_only_hints_decide_other_tools() {
    let (test, _registration) = probe_kernel(calls_on(
        "go",
        vec![
            ("call-ro", "probe_ro", serde_json::json!({})),
            ("call-rw", "probe_rw", serde_json::json!({})),
            (
                "call-sub",
                protocol::SPAWN_AGENT_TOOL,
                serde_json::json!({}),
            ),
        ],
    ));
    let session = test.create("plan-gate-hints").await;
    test.kernel
        .set_session_mode(&session, SessionMode::Plan)
        .await
        .expect("set mode");
    test.run_text(&session, "go").await.expect("prompt");

    assert_eq!(seen_result(&test, "call-ro").text, "ran probe_ro");
    assert!(seen_result(&test, "call-rw").is_error);
    // An extension's `spawn_agent` replaces the kernel's, so its name earns no exemption.
    assert!(seen_result(&test, "call-sub").is_error);
}

/// Extensions cannot take the names the kernel reserves for plan mode.
#[tokio::test]
async fn reserved_tool_names_are_rejected() {
    let logs = super::support::CapturedLogs::default();
    let (test, registration) = probe_kernel(|_| Route::answer("ok"));
    let session = test.create("plan-gate-reserved").await;
    test.run_text(&session, "hello").await.expect("prompt");

    let descriptions = requests(&test)
        .first()
        .expect("request")
        .tools
        .iter()
        .filter(|tool| {
            ["todo_write", "exit_plan_mode"].contains(&tool.name.as_str())
        })
        .map(|tool| tool.description.clone())
        .collect::<Vec<_>>();
    assert!(
        !descriptions
            .iter()
            .any(|description| description == "Probe"),
        "{descriptions:?}"
    );
    assert!(logs.content().contains(
        "rejected extension tool todo_write because the name is reserved"
    ));
    assert_eq!(*registration.lock().expect("registration"), Some(false));
}

/// Default mode does not gate tools.
#[tokio::test]
async fn default_mode_does_not_gate() {
    let test = TestKernel::new(calls_on(
        "go",
        vec![(
            "call-write",
            "write",
            serde_json::json!({ "path": "created.txt", "content": "x" }),
        )],
    ));
    let session = test.create("plan-gate-default").await;
    test.run_text(&session, "go").await.expect("prompt");
    assert!(!seen_result(&test, "call-write").is_error);
    assert!(test.root.path().join("created.txt").exists());
}

/// A subagent created in plan mode cannot edit either.
#[tokio::test]
async fn subagent_in_plan_mode_cannot_edit() {
    let test = TestKernel::new(|request| {
        match request
            .messages
            .last()
            .map(|message| super::fixture::text(&message.content))
            .as_deref()
        {
            Some("Write the file.") => tool_calls(&[(
                "call-child-write",
                "write",
                serde_json::json!({ "path": "child.txt", "content": "x" }),
            )]),
            _ => Route::answer("done"),
        }
    });
    let owner = test.create("plan-gate-owner").await;
    let child = plan_child(&test, &owner).await;
    test.run_text(&child, "Write the file.")
        .await
        .expect("child task");
    assert!(seen_result(&test, "call-child-write").is_error);
    assert!(!test.root.path().join("child.txt").exists());
}

/// Both modes declare the same tools, so switching keeps the cached prefix.
#[tokio::test]
async fn tool_definitions_do_not_change_with_mode() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let session = test.create("plan-gate-definitions").await;
    test.run_text(&session, "first")
        .await
        .expect("first prompt");
    test.kernel
        .set_session_mode(&session, SessionMode::Plan)
        .await
        .expect("set mode");
    test.run_text(&session, "second")
        .await
        .expect("second prompt");
    let requests = requests(&test);
    assert_eq!(
        requests.first().map(|request| &request.tools),
        requests.get(1).map(|request| &request.tools)
    );
}

/// Text the kernel returns when the user approves a plan.
const APPROVED_TEXT: &str = "The user approved the plan. Plan mode has ended; you can now edit files. Carry out the plan.";
/// Text the kernel returns when the user keeps planning.
const KEEP_PLANNING_TEXT: &str =
    "The user chose to keep planning. Stop and wait for their feedback.";

/// Returns the text of a tool result message, or an empty string.
fn result_text(message: &protocol::AgentMessage) -> String {
    match &message.content {
        protocol::MessageContent::ToolResult { blocks, .. } => blocks
            .iter()
            .filter_map(|block| match block {
                protocol::ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect(),
        _ => String::new(),
    }
}

/// Submits a plan on "plan it" and writes a file once the plan is approved.
fn plan_route(
    plan: &'static str,
) -> impl Fn(&protocol::ModelRequest) -> Route + Send + Sync + 'static {
    move |request| {
        let last = request.messages.last();
        if last
            .map(|message| super::fixture::text(&message.content))
            .as_deref()
            == Some("plan it")
        {
            tool_calls(&[(
                "call-exit",
                "exit_plan_mode",
                serde_json::json!({ "plan": plan }),
            )])
        } else if request
            .messages
            .iter()
            .rev()
            .find(|message| {
                matches!(
                    message.content,
                    protocol::MessageContent::ToolResult { .. }
                )
            })
            // The exit note follows the result, so look past it to the last result.
            .is_some_and(|message| result_text(message) == APPROVED_TEXT)
        {
            tool_calls(&[(
                "call-write",
                "write",
                serde_json::json!({ "path": "approved.txt", "content": "x" }),
            )])
        } else {
            Route::answer("done")
        }
    }
}

/// Returns every plan approval a Session requested.
fn approval_requests(
    test: &TestKernel,
    session: &SessionId,
) -> Vec<protocol::PlanApprovalRequest> {
    test.sinks
        .payloads(session)
        .into_iter()
        .filter_map(|payload| match payload {
            AgentEventPayload::PlanApprovalRequested { request } => {
                Some(request)
            }
            _ => None,
        })
        .collect()
}

/// Starts "plan it" in plan mode and waits until the plan waits for approval.
async fn submit_plan(
    test: &TestKernel,
    session: &SessionId,
) -> (
    tokio::task::JoinHandle<Result<protocol::RunResult, kernel::KernelError>>,
    protocol::PlanApprovalRequest,
) {
    test.kernel
        .set_session_mode(session, SessionMode::Plan)
        .await
        .expect("set mode");
    let prompt = test.start(session, "plan it");
    test.wait_until(|| !approval_requests(test, session).is_empty())
        .await;
    let request = approval_requests(test, session)
        .into_iter()
        .next()
        .expect("approval request");
    (prompt, request)
}

/// Answers a pending approval the way a client would.
async fn answer(
    test: &TestKernel,
    session: &SessionId,
    request_id: &str,
    decision: protocol::PlanApprovalDecision,
) -> Result<(), kernel::KernelError> {
    test.kernel
        .respond_plan_approval(protocol::PlanApprovalResponse {
            session_id: session.clone(),
            request_id: request_id.to_string(),
            decision,
        })
        .await
}

/// Outside plan mode there is nothing to approve.
#[tokio::test]
async fn exit_plan_mode_requires_plan_mode() {
    let test = TestKernel::new(plan_route("1. Write the file."));
    let session = test.create("plan-exit-default").await;
    test.run_text(&session, "plan it").await.expect("prompt");
    let result = seen_result(&test, "call-exit");
    assert!(result.is_error);
    assert_eq!(result.text, "Plan mode is not active.");
}

/// A subagent cannot leave plan mode; it reports to its owner instead.
#[tokio::test]
async fn subagent_cannot_exit_plan_mode() {
    let test = TestKernel::new(|request| {
        match request
            .messages
            .last()
            .map(|message| super::fixture::text(&message.content))
            .as_deref()
        {
            Some("Plan the child.") => tool_calls(&[(
                "call-child-exit",
                "exit_plan_mode",
                serde_json::json!({ "plan": "1. Child plan." }),
            )]),
            _ => Route::answer("done"),
        }
    });
    let owner = test.create("plan-exit-owner").await;
    let child = plan_child(&test, &owner).await;
    test.run_text(&child, "Plan the child.")
        .await
        .expect("child task");
    let result = seen_result(&test, "call-child-exit");
    assert!(result.is_error);
    assert!(result.text.contains("parent"), "{}", result.text);
}

/// Two submissions in one response: one waits for approval, the other is refused.
#[tokio::test]
async fn second_exit_in_one_batch_is_rejected() {
    let test = TestKernel::new(|request| {
        if request
            .messages
            .last()
            .map(|message| super::fixture::text(&message.content))
            .as_deref()
            == Some("plan it")
        {
            tool_calls(&[
                (
                    "call-exit-a",
                    "exit_plan_mode",
                    serde_json::json!({ "plan": "A" }),
                ),
                (
                    "call-exit-b",
                    "exit_plan_mode",
                    serde_json::json!({ "plan": "B" }),
                ),
            ])
        } else {
            Route::answer("done")
        }
    });
    let session = test.create("plan-exit-twice").await;
    let (prompt, pending) = submit_plan(&test, &session).await;
    answer(
        &test,
        &session,
        &pending.request_id,
        protocol::PlanApprovalDecision::Approve,
    )
    .await
    .expect("answer");
    prompt.await.expect("join").expect("prompt");
    let results = [
        seen_result(&test, "call-exit-a"),
        seen_result(&test, "call-exit-b"),
    ];
    assert_eq!(
        results
            .iter()
            .filter(|result| result.is_error && result.text.contains("already"))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| result.text == APPROVED_TEXT)
            .count(),
        1
    );
}

/// Approval ends plan mode, reports it, and the Run goes on to edit.
#[tokio::test]
async fn approval_switches_mode_and_continues() {
    let test = TestKernel::new(plan_route("  1. Write the file.  "));
    let session = test.create("plan-exit-approve").await;
    let (prompt, pending) = submit_plan(&test, &session).await;
    assert_eq!(pending.plan, "1. Write the file.");
    assert_eq!(pending.tool_call_id.as_str(), "call-exit");
    assert!(test.sinks.payloads(&session).iter().any(|payload| matches!(
        payload,
        AgentEventPayload::ActionRequired { .. }
    )));
    answer(
        &test,
        &session,
        &pending.request_id,
        protocol::PlanApprovalDecision::Approve,
    )
    .await
    .expect("answer");
    prompt.await.expect("join").expect("prompt");

    assert_eq!(
        test.kernel
            .session_settings(&session)
            .expect("settings")
            .mode,
        SessionMode::Default
    );
    assert_eq!(
        reported_modes(&test, &session).last(),
        Some(&SessionMode::Default)
    );
    let result = seen_result(&test, "call-exit");
    assert_eq!(result.text, APPROVED_TEXT);
    assert_eq!(
        result.details,
        Some(protocol::ToolResultDetails::PlanApproval {
            approved: true,
            plan: Some("1. Write the file.".into())
        })
    );
    assert!(!result_in(&test, &session, "call-write").is_error);
    assert!(test.root.path().join("approved.txt").exists());
}

/// Keeping the plan, or dismissing the prompt, leaves plan mode on and ends the Run.
#[tokio::test]
async fn keep_planning_ends_the_run() {
    for decision in [
        protocol::PlanApprovalDecision::KeepPlanning,
        protocol::PlanApprovalDecision::Cancelled,
    ] {
        let test = TestKernel::new(plan_route("1. Write the file."));
        let session = test.create("plan-exit-keep").await;
        let (prompt, pending) = submit_plan(&test, &session).await;
        answer(&test, &session, &pending.request_id, decision)
            .await
            .expect("answer");
        prompt.await.expect("join").expect("prompt");

        assert_eq!(
            test.kernel
                .session_settings(&session)
                .expect("settings")
                .mode,
            SessionMode::Plan
        );
        let result = result_in(&test, &session, "call-exit");
        assert_eq!(result.text, KEEP_PLANNING_TEXT, "{decision:?}");
        assert_eq!(
            result.details,
            Some(protocol::ToolResultDetails::PlanApproval {
                approved: false,
                plan: None
            })
        );
        assert_eq!(requests(&test).len(), 1, "the Run ends after the batch");
    }
}

/// A switch the user made while the plan waited is not undone by keeping the plan.
#[tokio::test]
async fn manual_switch_wins_over_keep_planning() {
    let test = TestKernel::new(plan_route("1. Write the file."));
    let session = test.create("plan-exit-manual").await;
    let (prompt, pending) = submit_plan(&test, &session).await;
    test.kernel
        .set_session_mode(&session, SessionMode::Default)
        .await
        .expect("set mode");
    answer(
        &test,
        &session,
        &pending.request_id,
        protocol::PlanApprovalDecision::KeepPlanning,
    )
    .await
    .expect("answer");
    prompt.await.expect("join").expect("prompt");
    assert_eq!(
        test.kernel
            .session_settings(&session)
            .expect("settings")
            .mode,
        SessionMode::Default
    );
}

/// Stopping the Run withdraws the approval and keeps plan mode.
#[tokio::test]
async fn cancel_withdraws_the_approval() {
    let test = TestKernel::new(plan_route("1. Write the file."));
    let session = test.create("plan-exit-cancel").await;
    let (prompt, pending) = submit_plan(&test, &session).await;
    test.kernel.cancel_session(&session).expect("cancel");
    let _outcome = prompt.await.expect("join");
    assert!(test.sinks.payloads(&session).iter().any(|payload| matches!(
        payload,
        AgentEventPayload::PlanApprovalResolved {
            request_id,
            decision: protocol::PlanApprovalDecision::Cancelled} if request_id == &pending.request_id
    )));
    assert_eq!(
        test.kernel
            .session_settings(&session)
            .expect("settings")
            .mode,
        SessionMode::Plan
    );
}

/// An answer to a settled approval is reported, not applied.
#[tokio::test]
async fn late_response_is_reported_resolved() {
    let test = TestKernel::new(plan_route("1. Write the file."));
    let session = test.create("plan-exit-late").await;
    let (prompt, pending) = submit_plan(&test, &session).await;
    answer(
        &test,
        &session,
        &pending.request_id,
        protocol::PlanApprovalDecision::KeepPlanning,
    )
    .await
    .expect("answer");
    prompt.await.expect("join").expect("prompt");
    let late = answer(
        &test,
        &session,
        &pending.request_id,
        protocol::PlanApprovalDecision::Approve,
    )
    .await;
    assert!(matches!(
        late,
        Err(kernel::KernelError::PlanApprovalResolved(_))
    ));
}

/// A late answer to an earlier plan never settles a later plan, even when the
/// model reuses the earlier tool call id.
#[tokio::test]
async fn late_response_does_not_settle_a_later_plan() {
    let test = TestKernel::new(plan_route("1. Write the file."));
    let session = test.create("plan-exit-reused-call").await;
    let (prompt, first) = submit_plan(&test, &session).await;
    answer(
        &test,
        &session,
        &first.request_id,
        protocol::PlanApprovalDecision::KeepPlanning,
    )
    .await
    .expect("answer");
    prompt.await.expect("join").expect("prompt");
    // `plan_route` submits every plan as "call-exit".
    let (prompt, second) = submit_plan_again(&test, &session).await;
    assert_eq!(second.tool_call_id, first.tool_call_id);
    let late = answer(
        &test,
        &session,
        &first.request_id,
        protocol::PlanApprovalDecision::Approve,
    )
    .await;
    assert!(matches!(
        late,
        Err(kernel::KernelError::PlanApprovalResolved(_))
    ));
    assert_eq!(
        test.kernel
            .plan_approval(&session)
            .expect("pending")
            .map(|request| request.request_id),
        Some(second.request_id.clone())
    );
    answer(
        &test,
        &session,
        &second.request_id,
        protocol::PlanApprovalDecision::KeepPlanning,
    )
    .await
    .expect("answer");
    prompt.await.expect("join").expect("prompt");
}

/// Sends "plan it" again and waits for the next approval request.
async fn submit_plan_again(
    test: &TestKernel,
    session: &SessionId,
) -> (
    tokio::task::JoinHandle<Result<protocol::RunResult, kernel::KernelError>>,
    protocol::PlanApprovalRequest,
) {
    let before = approval_requests(test, session).len();
    let prompt = test.start(session, "plan it");
    test.wait_until(|| approval_requests(test, session).len() > before)
        .await;
    let request = approval_requests(test, session)
        .into_iter()
        .nth(before)
        .expect("approval request");
    (prompt, request)
}

/// The plan never reaches the logs.
#[tokio::test]
async fn approval_logs_omit_the_plan() {
    let logs = super::support::CapturedLogs::default();
    let test = TestKernel::new(plan_route("SECRET-PLAN-TEXT"));
    let session = test.create("plan-exit-logs").await;
    let (prompt, pending) = submit_plan(&test, &session).await;
    answer(
        &test,
        &session,
        &pending.request_id,
        protocol::PlanApprovalDecision::KeepPlanning,
    )
    .await
    .expect("answer");
    prompt.await.expect("join").expect("prompt");
    assert!(!logs.content().contains("SECRET-PLAN-TEXT"));
}

/// Calls `todo_write` with each argument set on "todo", one call per Turn.
fn todo_route(
    calls: Vec<(&'static str, serde_json::Value)>,
) -> impl Fn(&protocol::ModelRequest) -> Route + Send + Sync + 'static {
    move |request| {
        if request
            .messages
            .last()
            .map(|message| super::fixture::text(&message.content))
            .as_deref()
            == Some("todo")
        {
            tool_calls(
                &calls
                    .iter()
                    .map(|(id, arguments)| {
                        (*id, "todo_write", arguments.clone())
                    })
                    .collect::<Vec<_>>(),
            )
        } else {
            Route::answer("done")
        }
    }
}

/// A valid list comes back as the tool's details; an empty list clears it.
#[tokio::test]
async fn todo_write_returns_the_validated_list() {
    let items = serde_json::json!([
        { "content": "Read the code", "status": "completed", "activeForm": "Reading the code" },
        { "content": "Write the plan", "status": "in_progress", "activeForm": "Writing the plan" },
    ]);
    let test = TestKernel::new(todo_route(vec![
        ("call-todo", serde_json::json!({ "todos": items })),
        ("call-empty", serde_json::json!({ "todos": [] })),
    ]));
    let session = test.create("todo-valid").await;
    test.run_text(&session, "todo").await.expect("prompt");

    let result = seen_result(&test, "call-todo");
    assert!(!result.is_error);
    assert_eq!(
        result.details,
        Some(protocol::ToolResultDetails::TodoList {
            todos: vec![
                protocol::TodoItem {
                    content: "Read the code".into(),
                    status: protocol::TodoStatus::Completed,
                    active_form: "Reading the code".into()
                },
                protocol::TodoItem {
                    content: "Write the plan".into(),
                    status: protocol::TodoStatus::InProgress,
                    active_form: "Writing the plan".into()
                },
            ]
        })
    );
    assert_eq!(
        seen_result(&test, "call-empty").details,
        Some(protocol::ToolResultDetails::TodoList { todos: Vec::new() })
    );
}

/// Unknown statuses and blank text are refused without a list.
#[tokio::test]
async fn todo_write_rejects_invalid_items() {
    let test = TestKernel::new(todo_route(vec![
        (
            "call-status",
            serde_json::json!({ "todos": [{ "content": "a", "status": "blocked", "activeForm": "a" }] }),
        ),
        (
            "call-content",
            serde_json::json!({ "todos": [{ "content": "  ", "status": "pending", "activeForm": "a" }] }),
        ),
        (
            "call-active",
            serde_json::json!({ "todos": [{ "content": "a", "status": "pending", "activeForm": "" }] }),
        ),
    ]));
    let session = test.create("todo-invalid").await;
    test.run_text(&session, "todo").await.expect("prompt");
    for call in ["call-status", "call-content", "call-active"] {
        let result = seen_result(&test, call);
        assert!(result.is_error, "{call}");
        assert!(
            !matches!(
                result.details,
                Some(protocol::ToolResultDetails::TodoList { .. })
            ),
            "{call}"
        );
    }
}

/// The todo list is not a file change, so plan mode allows it.
#[tokio::test]
async fn todo_write_works_in_plan_mode() {
    let test = TestKernel::new(todo_route(vec![(
        "call-todo",
        serde_json::json!({ "todos": [{ "content": "a", "status": "pending", "activeForm": "a" }] }),
    )]));
    let session = test.create("todo-plan").await;
    test.kernel
        .set_session_mode(&session, SessionMode::Plan)
        .await
        .expect("set mode");
    test.run_text(&session, "todo").await.expect("prompt");
    assert!(!seen_result(&test, "call-todo").is_error);
}

/// The reserved names always reach the kernel's tools, whatever extensions register.
#[tokio::test]
async fn reserved_names_resolve_to_kernel_tools() {
    let (test, _registration) = probe_kernel(todo_route(vec![(
        "call-todo",
        serde_json::json!({ "todos": [] }),
    )]));
    let session = test.create("todo-reserved").await;
    test.run_text(&session, "todo").await.expect("prompt");
    assert!(matches!(
        seen_result(&test, "call-todo").details,
        Some(protocol::ToolResultDetails::TodoList { .. })
    ));
    let tools = requests(&test)
        .first()
        .expect("request")
        .tools
        .iter()
        .filter(|tool| {
            ["todo_write", "exit_plan_mode"].contains(&tool.name.as_str())
        })
        .map(|tool| tool.description.clone())
        .collect::<Vec<_>>();
    assert_eq!(tools.len(), 2);
    assert!(!tools.iter().any(|description| description == "Probe"));
}

/// Both reserved tools are declared in every mode.
#[tokio::test]
async fn plan_tools_are_declared_in_every_mode() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let session = test.create("todo-declared").await;
    test.run_text(&session, "first")
        .await
        .expect("first prompt");
    test.kernel
        .set_session_mode(&session, SessionMode::Plan)
        .await
        .expect("set mode");
    test.run_text(&session, "second")
        .await
        .expect("second prompt");
    for request in requests(&test) {
        let names = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"exit_plan_mode"), "{names:?}");
        assert!(names.contains(&"todo_write"), "{names:?}");
    }
}

/// Todo text never reaches the logs.
#[tokio::test]
async fn todo_logs_omit_items() {
    let logs = super::support::CapturedLogs::default();
    let test = TestKernel::new(todo_route(vec![(
        "call-todo",
        serde_json::json!({ "todos": [{ "content": "SECRET-TODO", "status": "pending", "activeForm": "SECRET-ACTIVE" }] }),
    )]));
    let session = test.create("todo-logs").await;
    test.run_text(&session, "todo").await.expect("prompt");
    let content = logs.content();
    assert!(!content.contains("SECRET-TODO"));
    assert!(!content.contains("SECRET-ACTIVE"));
}
