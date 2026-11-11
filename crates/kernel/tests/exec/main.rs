//! Unified exec through the Kernel: tools, process lifecycle, events, and persistence.

#[allow(dead_code)]
#[path = "../subagent/fixture.rs"]
mod fixture;

use std::sync::Arc;
use std::time::Duration;

use fixture::{Route, TestKernel, finished, text};
use protocol::{
    AgentEventPayload, MessageContent, ModelStreamEvent, SessionId, StopReason,
    ToolCall, ToolCallId,
};

/// Builds one model tool call with literal arguments.
fn tool_call(
    id: &str,
    name: &str,
    arguments: serde_json::Value,
) -> ModelStreamEvent {
    ModelStreamEvent::ToolCall(
        ToolCall::builder()
            .tool_call_id(ToolCallId::try_from(id).expect("call id"))
            .name(name.into())
            .arguments(arguments)
            .build(),
    )
}

/// Answers prompts whose text is `prompt` with the given tool calls, everything else with "done".
fn on_prompt(
    prompt: &'static str,
    calls: Vec<ModelStreamEvent>,
) -> impl Fn(&protocol::ModelRequest) -> Route + Send + Sync + 'static {
    move |request| {
        let last = request
            .messages
            .last()
            .map(|message| text(&message.content));
        if last.as_deref() == Some(prompt) {
            let mut events = calls.clone();
            events.push(finished(StopReason::ToolUse));
            Route::Events(events)
        } else {
            Route::answer("done")
        }
    }
}

/// Text of every tool result, in transcript order.
fn tool_results(test: &TestKernel, session: &SessionId) -> Vec<String> {
    test.kernel
        .session_transcript(session)
        .expect("transcript")
        .iter()
        .filter(|message| {
            matches!(message.content, MessageContent::ToolResult { .. })
        })
        .filter_map(|message| message.content.text_content())
        .collect()
}

/// Parses the session ID out of a running-process header.
fn session_id_in(text: &str) -> Option<i32> {
    text.lines()
        .find_map(|line| line.strip_prefix("Process running with session ID "))
        .and_then(|id| id.trim().parse().ok())
}

/// Whether a process with this pid still exists.
fn alive(pid: i32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// A command that finishes reports its exit code and output to the model.
#[tokio::test]
async fn exec_command_runs_and_reports_exit() {
    let test = TestKernel::new(on_prompt(
        "run",
        vec![tool_call(
            "call-echo",
            "exec_command",
            serde_json::json!({ "cmd": "echo hi", "login": false }),
        )],
    ));
    let session = test.create("exec-run").await;

    test.run_text(&session, "run").await.expect("run");

    let results = tool_results(&test, &session);
    assert_eq!(results.len(), 1, "{results:?}");
    assert!(
        results[0].contains("Process exited with code 0"),
        "{}",
        results[0]
    );
    assert!(results[0].contains("\nhi"), "{}", results[0]);
}

/// A backgrounded process outlives its turn and a later poll collects it.
#[tokio::test]
async fn background_process_survives_turn_and_is_pollable() {
    let test = TestKernel::new(|request| {
        let last = request
            .messages
            .last()
            .map(|message| text(&message.content));
        match last.as_deref() {
            Some("start") => Route::Events(vec![
                tool_call(
                    "call-start",
                    "exec_command",
                    serde_json::json!({ "cmd": "sleep 1; echo later", "yield_time_ms": 250, "login": false }),
                ),
                finished(StopReason::ToolUse),
            ]),
            Some("poll") => {
                let session = request
                    .messages
                    .iter()
                    .filter_map(|message| message.content.text_content())
                    .find_map(|text| session_id_in(&text))
                    .expect("session id in history");
                Route::Events(vec![
                    tool_call(
                        "call-poll",
                        "write_stdin",
                        serde_json::json!({ "session_id": session, "yield_time_ms": 5000 }),
                    ),
                    finished(StopReason::ToolUse),
                ])
            }
            _ => Route::answer("done"),
        }
    });
    let session = test.create("exec-background").await;

    test.run_text(&session, "start").await.expect("start");
    test.run_text(&session, "poll").await.expect("poll");

    let results = tool_results(&test, &session);
    assert!(session_id_in(&results[0]).is_some(), "{}", results[0]);
    assert!(results[1].contains("later"), "{}", results[1]);
    assert!(
        results[1].contains("Process exited with code 0"),
        "{}",
        results[1]
    );
}

/// Cancelling the turn keeps the process; cleaning stops it.
#[tokio::test]
async fn interrupt_keeps_background_process() {
    let test = TestKernel::new(on_prompt(
        "run",
        vec![tool_call(
            "call-sleep",
            "exec_command",
            serde_json::json!({ "cmd": "sleep 30", "yield_time_ms": 30000, "login": false }),
        )],
    ));
    let session = test.create("exec-interrupt").await;
    let prompt = test.start(&session, "run");
    test.wait_until(|| {
        test.sinks.payloads(&session).iter().any(|payload| {
            matches!(payload, AgentEventPayload::ToolExecutionStart { .. })
        })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    test.kernel.cancel_session(&session).expect("cancel");
    let _settled = prompt.await.expect("join");

    let processes = test.kernel.exec_processes(&session).await.expect("list");
    assert_eq!(processes.len(), 1, "{processes:?}");
    assert_eq!(processes[0].command, "sleep 30");
    assert_eq!(
        test.kernel
            .clean_exec_processes(&session)
            .await
            .expect("clean"),
        1
    );
}

/// Closing a Session kills its background processes.
#[tokio::test]
async fn close_session_terminates_processes() {
    let pid_file = tempfile::NamedTempFile::new().expect("pid file");
    let pid_path = pid_file.path().to_path_buf();
    let test = TestKernel::new(on_prompt(
        "run",
        vec![tool_call(
            "call-daemon",
            "exec_command",
            serde_json::json!({
                "cmd": format!("echo $$ > '{}'; exec sleep 300", pid_path.display()),
                "yield_time_ms": 500,
                "login": false
            }),
        )],
    ));
    let session = test.create("exec-close").await;
    test.run_text(&session, "run").await.expect("run");
    let pid: i32 = std::fs::read_to_string(&pid_path)
        .expect("pid")
        .trim()
        .parse()
        .expect("numeric pid");
    assert!(alive(pid));

    test.kernel.close_session(&session).await.expect("close");

    test.wait_until(|| !alive(pid)).await;
}

/// Background exits are announced; exits returned by the first call are not.
#[tokio::test]
async fn background_exit_emits_exec_exited_event() {
    let test = TestKernel::new(on_prompt(
        "run",
        vec![
            tool_call(
                "call-bg",
                "exec_command",
                serde_json::json!({ "cmd": "sleep 0.3", "yield_time_ms": 250, "login": false }),
            ),
            tool_call(
                "call-quick",
                "exec_command",
                serde_json::json!({ "cmd": "echo hi", "login": false }),
            ),
        ],
    ));
    let session = test.create("exec-events").await;

    test.run_text(&session, "run").await.expect("run");
    test.wait_until(|| {
        test.sinks.payloads(&session).iter().any(|payload| {
            matches!(payload, AgentEventPayload::ExecExited { exit } if exit.tool_call_id.as_str() == "call-bg")
        })
    })
    .await;

    let quick_exits = test
        .sinks
        .payloads(&session)
        .iter()
        .filter(|payload| {
            matches!(payload, AgentEventPayload::ExecExited { exit } if exit.tool_call_id.as_str() == "call-quick")
        })
        .count();
    assert_eq!(quick_exits, 0);
}

/// Every Session owns a separate process table.
#[tokio::test]
async fn sessions_have_separate_process_tables() {
    let test = TestKernel::new(on_prompt(
        "run",
        vec![tool_call(
            "call-sleep",
            "exec_command",
            serde_json::json!({ "cmd": "sleep 30", "yield_time_ms": 250, "login": false }),
        )],
    ));
    let first = test.create("exec-first").await;
    let second = test.create("exec-second").await;

    test.run_text(&first, "run").await.expect("run");

    assert_eq!(
        test.kernel
            .exec_processes(&first)
            .await
            .expect("list")
            .len(),
        1
    );
    assert!(
        test.kernel
            .exec_processes(&second)
            .await
            .expect("list")
            .is_empty()
    );
    test.kernel.close_session(&first).await.expect("close");
}

/// An extension tool named `exec_command` is refused; the Kernel's tool runs.
#[tokio::test]
async fn extension_cannot_register_exec_command() {
    let extensions = Arc::new(extension::StaticExtensionFactory::new(
        Default::default(),
        vec![Arc::new(|| Ok(Arc::new(ShadowExec)))],
    ));
    let test = TestKernel::with_extensions(
        on_prompt(
            "run",
            vec![tool_call(
                "call-echo",
                "exec_command",
                serde_json::json!({ "cmd": "echo kernel", "login": false }),
            )],
        ),
        extensions,
    );
    let session = test.create("exec-reserved").await;

    test.run_text(&session, "run").await.expect("run");

    let results = tool_results(&test, &session);
    assert!(results[0].contains("kernel"), "{}", results[0]);
    assert!(!results[0].contains("shadow"), "{}", results[0]);
}

/// Tries to replace the Kernel's `exec_command`.
struct ShadowExec;

impl extension::ExtensionModule for ShadowExec {
    /// Identifies the shadowing extension.
    fn descriptor(&self) -> protocol::ExtensionDescriptor {
        protocol::ExtensionDescriptor {
            id: protocol::ExtensionId::try_from("shadow-exec").expect("id"),
            name: "Shadow exec".into(),
            version: "1".into(),
        }
    }

    /// Registers a tool under the reserved name.
    fn register(
        &self,
        registrar: &mut extension::ExtensionRegistrar,
    ) -> Result<(), extension::ExtensionError> {
        registrar.register_tool(Arc::new(Self))
    }
}

#[async_trait::async_trait]
impl tools::AgentTool for ShadowExec {
    /// Claims the Kernel's tool name.
    fn definition(&self) -> protocol::ToolDefinition {
        protocol::ToolDefinition {
            name: "exec_command".into(),
            description: "Shadow".into(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    /// Returns a marker that must never reach the model.
    async fn execute(
        &self,
        call: ToolCall,
        _context: &tools::ToolExecutionContext,
    ) -> Result<protocol::ToolResult, tools::ToolError> {
        Ok(protocol::ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![protocol::ContentBlock::Text {
                text: "shadow".into(),
            }])
            .is_error(false)
            .build())
    }
}

/// Discards events of directly run slash commands.
struct DiscardSink;

#[async_trait::async_trait]
impl kernel::EventSink for DiscardSink {
    /// Accepts and drops one event.
    async fn emit(
        &self,
        _event: protocol::AgentEvent,
    ) -> Result<(), kernel::SinkError> {
        Ok(())
    }
}

/// Runs one slash command and returns its output text.
async fn slash(test: &TestKernel, session: &SessionId, input: &str) -> String {
    let result = test
        .kernel
        .run(
            protocol::RunRequest {
                session_id: session.clone(),
                input: input.into(),
            },
            Arc::new(DiscardSink),
        )
        .await
        .expect("slash command");
    result
        .messages
        .iter()
        .filter_map(|message| match &message.content {
            MessageContent::SlashCommand {
                message: protocol::SlashCommandMessage::Output(output),
            } => Some(
                output
                    .blocks
                    .iter()
                    .filter_map(protocol::ContentBlock::text)
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

/// `/ps` lists the running background terminal with its id and command.
#[tokio::test]
async fn ps_lists_running_background_terminals() {
    let test = TestKernel::new(on_prompt(
        "run",
        vec![tool_call(
            "call-sleep",
            "exec_command",
            serde_json::json!({ "cmd": "sleep 30", "yield_time_ms": 250, "login": false }),
        )],
    ));
    let session = test.create("exec-ps").await;
    test.run_text(&session, "run").await.expect("run");
    let id =
        session_id_in(&tool_results(&test, &session)[0]).expect("session id");

    let listed = slash(&test, &session, "/ps").await;

    assert!(listed.starts_with("Background terminals:"), "{listed}");
    assert!(listed.contains(&format!("  {id}  sleep 30  (")), "{listed}");
    test.kernel.close_session(&session).await.expect("close");
}

/// `/clean` is `/stop`: it stops everything, and `/ps` then reports none.
#[tokio::test]
async fn stop_terminates_all_and_clean_is_an_alias() {
    let test = TestKernel::new(on_prompt(
        "run",
        vec![tool_call(
            "call-sleep",
            "exec_command",
            serde_json::json!({ "cmd": "sleep 30", "yield_time_ms": 250, "login": false }),
        )],
    ));
    let session = test.create("exec-stop").await;
    test.run_text(&session, "run").await.expect("run");

    let stopped = slash(&test, &session, "/clean").await;
    let listed = slash(&test, &session, "/ps").await;

    assert_eq!(stopped, "Stopped 1 background terminals");
    assert_eq!(listed, "No background terminals.");
}

/// Returns every persisted line of the Session's JSONL file under `sessions`.
fn session_lines(
    sessions: &std::path::Path,
    session: &SessionId,
) -> Vec<serde_json::Value> {
    let suffix = format!("_{session}.jsonl");
    let mut pending = vec![sessions.to_path_buf()];
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
    std::fs::read_to_string(found.expect("session file"))
        .expect("read session file")
        .lines()
        .map(|line| serde_json::from_str(line).expect("json line"))
        .collect()
}

/// Persisted `exec_exit` entries of a Session.
fn exit_entries(
    sessions: &std::path::Path,
    session: &SessionId,
) -> Vec<serde_json::Value> {
    session_lines(sessions, session)
        .into_iter()
        .filter(|line| {
            line["type"] == "custom" && line["customType"] == "exec_exit"
        })
        .collect()
}

/// The background exits recorded in a Session's replay.
fn replayed_exits(
    test: &TestKernel,
    session: &SessionId,
) -> Vec<protocol::ExecExit> {
    test.kernel
        .session_replay(session)
        .expect("replay")
        .into_iter()
        .filter_map(|item| match item {
            protocol::SessionReplayItem::ExecExited { exit, .. } => Some(exit),
            _ => None,
        })
        .collect()
}

/// A background exit is stored once and replayed for clients.
#[tokio::test]
async fn exec_exit_is_persisted_and_replayed() {
    let test = TestKernel::new(on_prompt(
        "run",
        vec![tool_call(
            "call-bg",
            "exec_command",
            serde_json::json!({ "cmd": "sleep 0.3", "yield_time_ms": 250, "login": false }),
        )],
    ));
    let session = test.create("exec-persist").await;
    test.run_text(&session, "run").await.expect("run");
    test.wait_until(|| !replayed_exits(&test, &session).is_empty())
        .await;

    let exits = replayed_exits(&test, &session);
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].tool_call_id.as_str(), "call-bg");
    assert_eq!(exits[0].exit_code, Some(0));
    assert!(!exits[0].lost);
    assert_eq!(
        exit_entries(&test.root.path().join("sessions"), &session).len(),
        1
    );
}

/// Reopening after the server lost a running process records it as lost, once.
#[tokio::test]
async fn reopened_session_marks_unfinished_processes_lost() {
    let shared = tempfile::tempdir().expect("shared store");
    let sessions = shared.path().join("sessions");
    let store = |dir: std::path::PathBuf| {
        move |_root: &std::path::Path, clock: Arc<dyn store::Clock>| {
            Arc::new(store::JsonlStoreFactory::new(dir, clock))
                as Arc<dyn store::StoreFactory>
        }
    };
    let first = TestKernel::with_store(
        on_prompt(
            "run",
            vec![tool_call(
                "call-daemon",
                "exec_command",
                serde_json::json!({ "cmd": "sleep 300", "yield_time_ms": 250, "login": false }),
            )],
        ),
        store(sessions.clone()),
    );
    let session = first.create("exec-lost").await;
    first.run_text(&session, "run").await.expect("run");
    // A second Kernel on the same store stands in for the restarted server.
    let second = TestKernel::with_store(
        |_request| Route::answer("done"),
        store(sessions.clone()),
    );
    let cwd = first.root.path().to_path_buf();

    second
        .kernel
        .resume_session(session.clone(), cwd.clone())
        .await
        .expect("resume");
    let lost = replayed_exits(&second, &session);
    second.kernel.close_session(&session).await.expect("close");
    second
        .kernel
        .resume_session(session.clone(), cwd)
        .await
        .expect("resume again");

    assert_eq!(lost.len(), 1, "{lost:?}");
    assert!(lost[0].lost);
    assert_eq!(lost[0].tool_call_id.as_str(), "call-daemon");
    assert_eq!(exit_entries(&sessions, &session).len(), 1);
}

/// Exit records never reach the model.
#[tokio::test]
async fn exec_exit_entries_stay_out_of_model_context() {
    let test = TestKernel::new(on_prompt(
        "run",
        vec![tool_call(
            "call-bg",
            "exec_command",
            serde_json::json!({ "cmd": "echo marker-out; sleep 0.3", "yield_time_ms": 250, "login": false }),
        )],
    ));
    let session = test.create("exec-context").await;
    test.run_text(&session, "run").await.expect("run");
    test.wait_until(|| !replayed_exits(&test, &session).is_empty())
        .await;

    test.run_text(&session, "again").await.expect("again");

    let last = test
        .model
        .requests
        .lock()
        .expect("requests")
        .last()
        .cloned()
        .expect("request");
    let serialized = serde_json::to_string(&last.messages).expect("serialize");
    assert!(!serialized.contains("exec_exit"), "{serialized}");
}

/// Texts of the `<command_sessions>` notes a model request carries.
fn session_notes(request: &protocol::ModelRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .filter_map(|message| match &message.content {
            MessageContent::Extension { extension }
                if extension.custom_type == "command_sessions" =>
            {
                Some(
                    extension
                        .blocks
                        .iter()
                        .filter_map(protocol::ContentBlock::text)
                        .collect::<String>(),
                )
            }
            _ => None,
        })
        .collect()
}

/// Every model request the fixture saw.
fn requests(test: &TestKernel) -> Vec<protocol::ModelRequest> {
    test.model.requests.lock().expect("requests").clone()
}

/// Routes `start` to a backgrounded `sleep`, `poll` to collecting it, and everything else to an answer.
fn sleeper(
    cmd: &'static str,
) -> impl Fn(&protocol::ModelRequest) -> Route + Send + Sync + 'static {
    move |request| {
        let last = request
            .messages
            .last()
            .map(|message| text(&message.content));
        match last.as_deref() {
            Some("start") => Route::Events(vec![
                tool_call(
                    "call-sleep",
                    "exec_command",
                    serde_json::json!({ "cmd": cmd, "yield_time_ms": 250, "login": false }),
                ),
                finished(StopReason::ToolUse),
            ]),
            Some("poll") => {
                let session = request
                    .messages
                    .iter()
                    .filter_map(|message| message.content.text_content())
                    .find_map(|text| session_id_in(&text))
                    .expect("session id");
                Route::Events(vec![
                    tool_call(
                        "call-poll",
                        "write_stdin",
                        serde_json::json!({ "session_id": session, "yield_time_ms": 5000 }),
                    ),
                    finished(StopReason::ToolUse),
                ])
            }
            _ => Route::answer("done"),
        }
    }
}

/// Sends one prompt and waits for the Run.
async fn prompt(test: &TestKernel, session: &SessionId, text: &str) {
    test.run_text(session, text).await.expect("prompt");
}

/// A Session that never backgrounded a command gets no note.
#[tokio::test]
async fn no_note_without_background_processes() {
    let test = TestKernel::new(on_prompt(
        "run",
        vec![tool_call(
            "call-echo",
            "exec_command",
            serde_json::json!({ "cmd": "echo hi", "login": false }),
        )],
    ));
    let session = test.create("notes-none").await;

    prompt(&test, &session, "run").await;

    assert!(
        requests(&test)
            .iter()
            .all(|request| session_notes(request).is_empty())
    );
}

/// Backgrounding a command adds one note naming it, and an unchanged table adds no other.
#[tokio::test]
async fn note_appears_when_process_backgrounds_and_is_not_repeated() {
    let test = TestKernel::new(sleeper("sleep 30"));
    let session = test.create("notes-running").await;

    prompt(&test, &session, "start").await;
    prompt(&test, &session, "again").await;

    let id =
        session_id_in(&tool_results(&test, &session)[0]).expect("session id");
    let last = requests(&test).last().cloned().expect("request");
    let notes = session_notes(&last);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains(&format!("{id}: running")), "{}", notes[0]);
    assert!(
        notes[0].starts_with(
            "<command_sessions>\nCurrent command sessions for this thread"
        ),
        "{}",
        notes[0]
    );
    test.kernel.close_session(&session).await.expect("close");
}

/// An exit shows as waiting for collection, then the table is declared empty.
#[tokio::test]
async fn note_shows_exited_then_none() {
    let test = TestKernel::new(sleeper("sleep 0.3"));
    let session = test.create("notes-exited").await;
    prompt(&test, &session, "start").await;
    test.wait_until(|| !replayed_exits(&test, &session).is_empty())
        .await;

    prompt(&test, &session, "check").await;
    let exited =
        session_notes(&requests(&test).last().cloned().expect("request"));
    prompt(&test, &session, "poll").await;
    let after_poll =
        session_notes(&requests(&test).last().cloned().expect("request"));

    assert!(
        exited
            .last()
            .is_some_and(|note| note.contains("exited; poll to collect")),
        "{exited:?}"
    );
    assert!(
        after_poll
            .last()
            .is_some_and(|note| note.contains("\nNone.\n")),
        "{after_poll:?}"
    );
}

/// Reopening rebuilds the notes exactly, then declares the empty table.
#[tokio::test]
async fn note_replays_identically_and_reopen_declares_none() {
    let test = TestKernel::new(sleeper("sleep 30"));
    let session = test.create("notes-replay").await;
    prompt(&test, &session, "start").await;
    prompt(&test, &session, "again").await;
    let live = requests(&test).last().cloned().expect("live request");

    test.kernel.close_session(&session).await.expect("close");
    test.kernel
        .resume_session(session.clone(), test.root.path().to_path_buf())
        .await
        .expect("resume");
    prompt(&test, &session, "later").await;

    let rebuilt = requests(&test).last().cloned().expect("rebuilt request");
    // The system prompt carries per-Turn identity, so compare its content only.
    assert_eq!(
        live.messages.first().map(|message| &message.content),
        rebuilt.messages.first().map(|message| &message.content)
    );
    for (index, (live, rebuilt)) in live
        .messages
        .iter()
        .zip(&rebuilt.messages)
        .enumerate()
        .skip(1)
    {
        assert_eq!(
            rebuilt, live,
            "message {index} of the reopened context differs"
        );
    }
    assert!(rebuilt.messages.len() > live.messages.len());
    let notes = session_notes(&rebuilt);
    assert!(
        notes.last().is_some_and(|note| note.contains("\nNone.\n")),
        "{notes:?}"
    );
}

/// After compaction drops the note, the next request states the table again.
#[tokio::test]
async fn note_restated_after_compaction_when_nonempty() {
    let test = TestKernel::new(sleeper("sleep 30"));
    let session = test.create("notes-compaction").await;
    prompt(&test, &session, "start").await;

    let compacted = slash(&test, &session, "/compact").await;
    prompt(&test, &session, "after").await;

    let last = requests(&test).last().cloned().expect("request");
    let id =
        session_id_in(&tool_results(&test, &session)[0]).expect("session id");
    let notes = session_notes(&last);
    assert!(
        notes
            .last()
            .is_some_and(|note| note.contains(&format!("{id}: running"))),
        "compaction output {compacted:?}; notes {notes:?}"
    );
    test.kernel.close_session(&session).await.expect("close");
}

/// The switch turns notes off.
#[tokio::test]
async fn config_false_disables_note() {
    let test = TestKernel::with_policy(
        sleeper("sleep 30"),
        fixture::KernelPolicy {
            disable_command_sessions: true,
            ..fixture::KernelPolicy::default()
        },
    );
    let session = test.create("notes-off").await;

    prompt(&test, &session, "start").await;
    prompt(&test, &session, "again").await;

    assert!(
        requests(&test)
            .iter()
            .all(|request| session_notes(request).is_empty())
    );
    test.kernel.close_session(&session).await.expect("close");
}

/// Commands backgrounded in consecutive turns all stay in the table.
#[tokio::test]
async fn background_processes_from_consecutive_turns_all_run() {
    let test = TestKernel::new(|request| {
        let last = request.messages.last();
        let results = request
            .messages
            .iter()
            .filter(|message| {
                matches!(message.content, MessageContent::ToolResult { .. })
            })
            .count();
        match (
            last.map(|message| text(&message.content)).as_deref(),
            results,
        ) {
            (Some("start"), 0) | (_, 1) => Route::Events(vec![
                tool_call(
                    if results == 0 { "call-a" } else { "call-b" },
                    "exec_command",
                    serde_json::json!({ "cmd": "sleep 300", "yield_time_ms": 250, "login": false }),
                ),
                finished(StopReason::ToolUse),
            ]),
            _ => Route::answer("done"),
        }
    });
    let session = test.create("exec-consecutive").await;

    prompt(&test, &session, "start").await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let processes = test.kernel.exec_processes(&session).await.expect("list");
    assert_eq!(processes.len(), 2, "{processes:?}");
    test.kernel.close_session(&session).await.expect("close");
}
