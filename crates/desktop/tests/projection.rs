//! Session projection: ACP updates folded into ordered transcript rows.

mod support;

use std::collections::HashSet;

use agent_client_protocol::v2::schema as wire;
use desktop::projection::{
    CommandKind, CommandStatus, CompactionReason, MessageRow, ProjectionChange,
    Role, RowKey, RunState, SessionProjection, ToolCategory, ToolRow,
    ToolStatus, TranscriptRow,
};
use serde_json::{Value, json};
use support::ScriptedBackend;

/// Product metadata carrying one kernel sequence number.
fn meta(sequence: u64) -> Value {
    json!({ "clawcode": { "turnId": "t1", "timestampMs": "1000", "sequence": sequence } })
}

/// Parses one wire update from its JSON form.
fn update(value: Value) -> wire::SessionUpdate {
    serde_json::from_value(value).expect("valid session update")
}

/// Applies one JSON update and returns the change.
fn apply(projection: &mut SessionProjection, value: Value) -> ProjectionChange {
    projection.apply(&update(value))
}

/// Returns the message row at `index`.
fn message(projection: &SessionProjection, index: usize) -> &MessageRow {
    projection
        .row(index)
        .and_then(|row| match row {
            TranscriptRow::Message(row) => Some(row),
            _ => None,
        })
        .expect("message row")
}

/// Returns the tool row at `index`.
fn tool(projection: &SessionProjection, index: usize) -> &ToolRow {
    projection
        .row(index)
        .and_then(|row| match row {
            TranscriptRow::Tool(row) => Some(row),
            _ => None,
        })
        .expect("tool row")
}

/// Key of the message with this id.
fn message_key(id: &str) -> RowKey {
    RowKey::Message(wire::MessageId::from(id.to_string()))
}

#[test]
fn agent_chunks_accumulate_into_one_streaming_row() {
    let mut projection = SessionProjection::default();
    let chunk = |text: &str| {
        json!({
            "sessionUpdate": "agent_message_chunk", "messageId": "m1",
            "content": { "type": "text", "text": text }, "_meta": meta(1)
        })
    };

    let first = apply(&mut projection, chunk("Hi "));
    let second = apply(&mut projection, chunk("there"));

    assert_eq!(projection.len(), 1);
    let row = message(&projection, 0);
    assert_eq!(row.role, Role::Assistant);
    assert_eq!(row.text, "Hi there");
    assert!(row.streaming);
    assert_eq!(first.inserted, HashSet::from([message_key("m1")]));
    assert_eq!(second.updated, HashSet::from([message_key("m1")]));
    assert!(second.inserted.is_empty());
    // A new row moves the layout; more text in a visible row does not.
    assert!(first.structural);
    assert!(!second.structural);
}

#[test]
fn layout_relevant_updates_are_structural() {
    let mut projection = SessionProjection::default();
    let chunk = json!({
        "sessionUpdate": "agent_message_chunk", "messageId": "m1",
        "content": { "type": "text", "text": "" }, "_meta": meta(1)
    });
    apply(&mut projection, chunk.clone());
    // Settled and empty, the message is hidden.
    apply(
        &mut projection,
        json!({ "sessionUpdate": "state_update", "state": "idle" }),
    );
    assert!(!message(&projection, 0).is_visible());

    // Streaming again shows it: that moves the rows below it.
    assert!(apply(&mut projection, chunk).structural);
    let tool = json!({
        "sessionUpdate": "tool_call_update", "toolCallId": "c1",
        "title": "Read", "status": "in_progress", "_meta": meta(2)
    });
    assert!(apply(&mut projection, tool).structural);
    let output = json!({
        "sessionUpdate": "tool_call_content_chunk", "toolCallId": "c1",
        "content": { "type": "content", "content": { "type": "text", "text": "x" } }
    });
    assert!(!apply(&mut projection, output).structural);
    let status = json!({
        "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed"
    });
    assert!(apply(&mut projection, status).structural);
    let title =
        json!({ "sessionUpdate": "session_info_update", "title": "Plan" });
    assert!(!apply(&mut projection, title).structural);
}

#[test]
fn final_message_replaces_streamed_text_and_stops_streaming() {
    let mut projection = SessionProjection::default();
    apply(
        &mut projection,
        json!({
            "sessionUpdate": "agent_message_chunk", "messageId": "m1",
            "content": { "type": "text", "text": "Hi" }, "_meta": meta(1)
        }),
    );

    apply(
        &mut projection,
        json!({
            "sessionUpdate": "agent_message", "messageId": "m1",
            "content": [{ "type": "text", "text": "Hi " }, { "type": "text", "text": "there!" }],
            "_meta": meta(2)
        }),
    );

    let row = message(&projection, 0);
    assert_eq!(row.text, "Hi there!");
    assert!(!row.streaming);
    assert_eq!(row.timestamp_ms, Some(1000));
    assert_eq!(row.turn_id.as_deref(), Some("t1"));
}

#[test]
fn user_message_keeps_images() {
    let mut projection = SessionProjection::default();

    apply(
        &mut projection,
        json!({
            "sessionUpdate": "user_message", "messageId": "u1",
            "content": [
                { "type": "text", "text": "look" },
                { "type": "image", "data": "aGk=", "mimeType": "image/png" }
            ],
            "_meta": meta(1)
        }),
    );

    let row = message(&projection, 0);
    assert_eq!(row.role, Role::User);
    assert_eq!(row.text, "look");
    assert_eq!(row.images.len(), 1);
    assert_eq!(row.images[0].mime_type, "image/png");
    assert_eq!(row.images[0].data, "aGk=");
}

#[test]
fn thought_chunks_attach_to_the_assistant_row() {
    let mut projection = SessionProjection::default();
    let thought = |text: &str| {
        json!({
            "sessionUpdate": "agent_thought_chunk", "messageId": "m1",
            "content": { "type": "text", "text": text }, "_meta": meta(1)
        })
    };

    apply(&mut projection, thought("a"));
    apply(&mut projection, thought("b"));
    apply(
        &mut projection,
        json!({
            "sessionUpdate": "agent_message_chunk", "messageId": "m1",
            "content": { "type": "text", "text": "answer" }, "_meta": meta(2)
        }),
    );

    assert_eq!(projection.len(), 1);
    let row = message(&projection, 0);
    assert_eq!(row.reasoning, "ab");
    assert_eq!(row.text, "answer");
}

#[test]
fn tool_updates_merge_fields_and_map_status() {
    let mut projection = SessionProjection::default();

    apply(
        &mut projection,
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "c1",
            "title": "Read", "kind": "read", "status": "pending", "_meta": meta(1)
        }),
    );
    assert_eq!(tool(&projection, 0).status, ToolStatus::Pending);
    apply(
        &mut projection,
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "in_progress"
        }),
    );
    assert_eq!(tool(&projection, 0).status, ToolStatus::Running);
    apply(
        &mut projection,
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed",
            "content": [{ "type": "content", "content": { "type": "text", "text": "42 lines" } }]
        }),
    );

    let row = tool(&projection, 0);
    assert_eq!(row.title, "Read");
    assert_eq!(row.kind, Some(wire::ToolKind::Read));
    assert_eq!(row.status, ToolStatus::Completed);
    assert_eq!(row.output, "42 lines");
}

#[test]
fn blocked_failure_is_reported_as_blocked() {
    let mut projection = SessionProjection::default();

    apply(
        &mut projection,
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "c1", "title": "exec", "status": "failed",
            "_meta": { "clawcode": { "sequence": 1, "disposition": { "type": "blocked" } } }
        }),
    );
    apply(
        &mut projection,
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "c2", "title": "exec", "status": "failed",
            "rawOutput": { "details": { "type": "blocked" } }
        }),
    );
    apply(
        &mut projection,
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "c3", "title": "exec", "status": "failed"
        }),
    );

    assert_eq!(tool(&projection, 0).status, ToolStatus::Blocked);
    assert_eq!(tool(&projection, 1).status, ToolStatus::Blocked);
    assert_eq!(tool(&projection, 2).status, ToolStatus::Failed);
}

#[test]
fn tool_content_chunks_append_output_even_before_the_tool_row() {
    let mut projection = SessionProjection::default();
    let chunk = |text: &str| {
        json!({
            "sessionUpdate": "tool_call_content_chunk", "toolCallId": "c9",
            "content": { "type": "content", "content": { "type": "text", "text": text } }
        })
    };

    let change = apply(&mut projection, chunk("line 1\n"));
    apply(&mut projection, chunk("line 2\n"));

    let row = tool(&projection, 0);
    assert_eq!(row.status, ToolStatus::Running);
    assert_eq!(row.title, "c9");
    assert_eq!(row.output, "line 1\nline 2\n");
    assert_eq!(
        change.inserted,
        HashSet::from([RowKey::Tool(wire::ToolCallId::from("c9".to_string()))])
    );
}

#[test]
fn rows_order_by_kernel_sequence_even_when_out_of_order() {
    let mut projection = SessionProjection::default();
    let message_at = |id: &str, sequence: u64| {
        json!({
            "sessionUpdate": "agent_message", "messageId": id,
            "content": [{ "type": "text", "text": id }], "_meta": meta(sequence)
        })
    };

    apply(&mut projection, message_at("second", 2));
    apply(&mut projection, message_at("first", 1));

    assert_eq!(message(&projection, 0).text, "first");
    assert_eq!(message(&projection, 1).text, "second");
    assert_eq!(projection.index_of(&message_key("first")), Some(0));
    assert_eq!(projection.index_of(&message_key("second")), Some(1));
}

#[test]
fn rows_without_meta_follow_existing_rows() {
    let mut projection = SessionProjection::default();
    apply(
        &mut projection,
        json!({
            "sessionUpdate": "agent_message", "messageId": "a",
            "content": [{ "type": "text", "text": "a" }], "_meta": meta(5)
        }),
    );

    apply(
        &mut projection,
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "c1", "title": "Read", "status": "pending"
        }),
    );

    assert_eq!(
        projection
            .index_of(&RowKey::Tool(wire::ToolCallId::from("c1".to_string()))),
        Some(1)
    );
}

#[test]
fn extend_keeps_inserted_keys_out_of_updated() {
    let (k1, k2) = (message_key("k1"), message_key("k2"));
    let mut change = ProjectionChange::builder()
        .updated(HashSet::from([k2.clone()]))
        .build();
    // A later insertion supersedes an earlier update of the same row.
    change.mark_inserted(k1.clone());
    change.extend(
        ProjectionChange::builder()
            .updated(HashSet::from([k1.clone(), k2.clone()]))
            .run_state(true)
            .build(),
    );

    assert_eq!(change.inserted, HashSet::from([k1]));
    assert_eq!(change.updated, HashSet::from([k2]));
    assert!(change.run_state);
    assert!(change.structural, "an insertion is structural");
}

#[test]
fn idle_state_clears_streaming_flags_and_reports_run_state() {
    let mut projection = SessionProjection::default();

    let running = apply(
        &mut projection,
        json!({ "sessionUpdate": "state_update", "state": "running" }),
    );
    assert_eq!(projection.run_state(), RunState::Running);
    assert!(running.run_state);
    apply(
        &mut projection,
        json!({
            "sessionUpdate": "agent_message_chunk", "messageId": "m1",
            "content": { "type": "text", "text": "partial" }, "_meta": meta(1)
        }),
    );
    let idle = apply(
        &mut projection,
        json!({ "sessionUpdate": "state_update", "state": "idle" }),
    );

    assert_eq!(projection.run_state(), RunState::Idle);
    assert!(idle.run_state);
    assert!(!message(&projection, 0).streaming);
    assert_eq!(idle.updated, HashSet::from([message_key("m1")]));
}

#[test]
fn title_update_sets_and_clears_title() {
    let mut projection = SessionProjection::default();

    let change = apply(
        &mut projection,
        json!({ "sessionUpdate": "session_info_update", "title": "  Plan  " }),
    );
    assert_eq!(projection.title(), Some("Plan"));
    assert!(change.title);
    apply(
        &mut projection,
        json!({ "sessionUpdate": "session_info_update", "title": "   " }),
    );
    assert_eq!(projection.title(), None);
    apply(
        &mut projection,
        json!({ "sessionUpdate": "session_info_update", "title": "Again" }),
    );
    apply(
        &mut projection,
        json!({ "sessionUpdate": "session_info_update", "title": null }),
    );
    assert_eq!(projection.title(), None);
}

#[test]
fn unrelated_updates_change_nothing() {
    let mut projection = SessionProjection::default();

    let change = apply(
        &mut projection,
        json!({ "sessionUpdate": "replay_complete" }),
    );

    assert!(change.is_empty());
    assert_eq!(projection.len(), 0);
}

#[test]
fn clear_drops_rows_but_keeps_run_state_and_title() {
    let mut projection = SessionProjection::default();
    apply(
        &mut projection,
        json!({ "sessionUpdate": "state_update", "state": "running" }),
    );
    apply(
        &mut projection,
        json!({ "sessionUpdate": "session_info_update", "title": "Plan" }),
    );
    apply(
        &mut projection,
        json!({
            "sessionUpdate": "agent_message", "messageId": "m1",
            "content": [{ "type": "text", "text": "x" }], "_meta": meta(1)
        }),
    );

    projection.clear();

    assert_eq!(projection.len(), 0);
    assert_eq!(projection.index_of(&message_key("m1")), None);
    assert_eq!(projection.run_state(), RunState::Running);
    assert_eq!(projection.title(), Some("Plan"));
}

/// Summarizes message rows as (role, text, reasoning) for comparison.
fn messages(projection: &SessionProjection) -> Vec<(Role, String, String)> {
    (0..projection.len())
        .filter_map(|index| match projection.row(index) {
            Some(TranscriptRow::Message(row)) => {
                Some((row.role, row.text.clone(), row.reasoning.clone()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn replayed_scripted_run_rebuilds_the_same_transcript() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    backend
        .block_on(backend.client.prompt(&id, vec!["think".into()]))
        .expect("prompt");
    let mut live = SessionProjection::default();
    for update in backend.wait_idle(&id) {
        live.apply(&update);
    }
    backend
        .block_on(backend.client.close_session(&id))
        .expect("close");
    backend.drain(&id);

    backend
        .block_on(
            backend
                .client
                .resume_with_replay(&id, backend.workspace.path()),
        )
        .expect("resume");
    let mut replayed = SessionProjection::default();
    for update in backend.wait_replay_complete(&id) {
        replayed.apply(&update);
    }

    let expected = vec![
        (Role::User, "think".to_string(), String::new()),
        (Role::Assistant, "done".to_string(), "pondering".to_string()),
    ];
    assert_eq!(messages(&live), expected);
    assert_eq!(messages(&replayed), expected);
}

/// Runs one scripted prompt to idle and projects every update it produced.
fn run_prompt(text: &str) -> SessionProjection {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    backend
        .block_on(backend.client.prompt(&id, vec![text.into()]))
        .expect("prompt");
    let mut projection = SessionProjection::default();
    for update in backend.wait_idle(&id) {
        projection.apply(&update);
    }
    projection
}

/// Finds the tool row with this id.
fn tool_by_id<'a>(projection: &'a SessionProjection, id: &str) -> &'a ToolRow {
    let key = RowKey::Tool(wire::ToolCallId::from(id.to_string()));
    tool(
        projection,
        projection.index_of(&key).expect("tool row present"),
    )
}

#[test]
fn read_tool_reports_target_and_line_count() {
    let projection = run_prompt("read");
    let row = tool_by_id(&projection, "read-1");
    assert_eq!(row.category(), ToolCategory::Read);
    assert_eq!(row.target(), Some("notes.txt"));
    assert_eq!(row.status, ToolStatus::Completed);
    assert!(row.line_count() >= 1);
}

#[test]
fn edit_tool_carries_patch_and_stats() {
    let projection = run_prompt("edit");
    let row = tool_by_id(&projection, "edit-1");
    assert_eq!(row.category(), ToolCategory::Edit);
    assert_eq!(row.diff_stats(), Some((1, 1)));
    assert!(row.patch.as_deref().unwrap_or_default().contains("+beta"));
}

#[test]
fn exec_output_follows_terminal_snapshots() {
    let projection = run_prompt("exec");
    let row = tool_by_id(&projection, "exec-1");
    assert_eq!(row.category(), ToolCategory::Execute);
    assert_eq!(row.target(), Some("printf 'one\\ntwo\\n'"));
    assert!(row.output.contains("one\ntwo"), "output: {:?}", row.output);
}

#[test]
fn terminal_update_for_unknown_tool_is_ignored() {
    let mut projection = SessionProjection::default();
    let change = apply(
        &mut projection,
        json!({
            "sessionUpdate": "terminal_update", "terminalId": "nope",
            "output": { "data": "aGk=" }
        }),
    );
    assert!(change.is_empty());
}

#[test]
fn terminal_snapshot_replaces_tool_output() {
    let mut projection = SessionProjection::default();
    apply(
        &mut projection,
        json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "title": "x" }),
    );
    for data in ["aGk=", "aGkgdGhlcmU="] {
        apply(
            &mut projection,
            json!({
                "sessionUpdate": "terminal_update", "terminalId": "t1",
                "output": { "data": data }
            }),
        );
    }
    assert_eq!(tool_by_id(&projection, "t1").output, "hi there");
}

#[test]
fn assistant_message_records_its_duration() {
    let projection = run_prompt("think");
    let found = (0..projection.len()).any(|index| {
        matches!(
            projection.row(index),
            Some(TranscriptRow::Message(row))
                if row.role == Role::Assistant && row.duration_ms.is_some()
        )
    });
    assert!(found, "assistant row should carry a duration");
}

#[test]
fn queue_snapshot_replaces_pending() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    backend
        .block_on(backend.client.prompt(&id, vec!["slow".into()]))
        .expect("prompt");
    backend.wait_for(&id, |update| {
        matches!(
            update,
            wire::SessionUpdate::StateUpdate(wire::StateUpdate::Running(_))
        )
    });
    backend
        .block_on(backend.client.prompt(&id, vec!["hello".into()]))
        .expect("queued prompt");
    let mut projection = SessionProjection::default();
    let mut queued = false;
    // The first run may report idle before the follow-up leaves the queue,
    // so read up to the follow-up run's idle when the queue is not drained yet.
    for _ in 0..2 {
        let updates = backend.wait_for(&id, |update| {
            matches!(
                update,
                wire::SessionUpdate::StateUpdate(wire::StateUpdate::Idle(_))
            )
        });
        for update in &updates {
            let change = projection.apply(update);
            if change.queue && projection.pending().follow_up.len() == 1 {
                queued = true;
            }
        }
        if queued && projection.pending().follow_up.is_empty() {
            break;
        }
    }
    assert!(queued, "a snapshot with one follow-up should arrive");
    assert!(projection.pending().follow_up.is_empty());
}

#[test]
fn run_start_is_recorded_and_cleared() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    backend
        .block_on(backend.client.prompt(&id, vec!["slow".into()]))
        .expect("prompt");
    let mut projection = SessionProjection::default();
    for update in backend.wait_for(&id, |update| {
        matches!(
            update,
            wire::SessionUpdate::StateUpdate(wire::StateUpdate::Running(_))
        )
    }) {
        projection.apply(&update);
    }
    assert!(projection.run_started_ms().is_some());
    for update in backend.wait_for(&id, |update| {
        matches!(
            update,
            wire::SessionUpdate::StateUpdate(wire::StateUpdate::Idle(_))
        )
    }) {
        projection.apply(&update);
    }
    assert_eq!(projection.run_started_ms(), None);
}

#[test]
fn answering_an_approval_keeps_the_run_start() {
    let mut projection = SessionProjection::default();
    let state = |state: &str, at: &str| {
        json!({
            "sessionUpdate": "state_update", "state": state,
            "_meta": { "clawcode": { "timestampMs": at } }
        })
    };
    apply(&mut projection, state("running", "1000"));
    assert_eq!(projection.run_started_ms(), Some(1000));

    apply(&mut projection, state("requires_action", "2000"));
    apply(&mut projection, state("running", "3000"));
    assert_eq!(projection.run_started_ms(), Some(1000), "same run");

    apply(&mut projection, state("idle", "4000"));
    apply(&mut projection, state("running", "5000"));
    assert_eq!(projection.run_started_ms(), Some(5000), "a new run");
}

#[test]
fn malformed_message_timing_records_no_duration() {
    let mut projection = SessionProjection::default();
    let upsert = |id: &str, timing: Value| {
        json!({
            "sessionUpdate": "agent_message", "messageId": id,
            "content": [{ "type": "text", "text": id }],
            "_meta": { "clawcode": { "sequence": 1, "messageTiming": timing } }
        })
    };
    // The server's `protocol::MessageTiming`: decimal strings, snake_case keys.
    apply(
        &mut projection,
        upsert(
            "ok",
            json!({ "timestamp_ms": "1000", "started_at_ms": "1000", "ended_at_ms": "1500" }),
        ),
    );
    apply(
        &mut projection,
        upsert("bad", json!({ "started_at_ms": "x", "ended_at_ms": 7 })),
    );

    assert_eq!(message(&projection, 0).duration_ms, Some(500));
    assert_eq!(message(&projection, 1).duration_ms, None);
}

#[test]
fn attaching_mid_run_falls_back_to_the_last_prompt_time() {
    let mut projection = SessionProjection::default();
    projection.set_run_state(RunState::Running);
    apply(
        &mut projection,
        json!({
            "sessionUpdate": "user_message", "messageId": "u1",
            "content": [{ "type": "text", "text": "go" }], "_meta": meta(1)
        }),
    );
    assert_eq!(projection.run_started_ms(), Some(1000));
}

#[test]
fn malformed_queue_payload_is_ignored() {
    let mut projection = SessionProjection::default();
    let change = apply(
        &mut projection,
        json!({
            "sessionUpdate": "_clawcode/event",
            "payload": { "event": "queue_changed", "pending": 42 }
        }),
    );
    assert!(!change.queue);
}

#[test]
fn exec_footer_keeps_the_terminal_output() {
    let mut projection = SessionProjection::default();
    apply(
        &mut projection,
        json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1",
            "title": "x", "kind": "execute" }),
    );
    apply(
        &mut projection,
        json!({
            "sessionUpdate": "terminal_update", "terminalId": "t1",
            "output": { "data": "aGkgdGhlcmU=" }
        }),
    );
    apply(
        &mut projection,
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed",
            "content": [
                { "type": "terminal", "terminalId": "t1" },
                { "type": "content", "content": { "type": "text", "text": "Full output: /tmp/out.log" } }
            ]
        }),
    );
    let row = tool_by_id(&projection, "t1");
    assert_eq!(row.output, "hi there");
    assert_eq!(row.footer.as_deref(), Some("Full output: /tmp/out.log"));
}

/// Sends one text prompt.
fn say(backend: &ScriptedBackend, id: &wire::SessionId, text: &str) {
    backend
        .block_on(backend.client.prompt(
            id,
            vec![wire::ContentBlock::Text(wire::TextContent::new(text))],
        ))
        .expect("prompt");
}

/// Applies every update to a fresh projection.
fn project(updates: &[wire::SessionUpdate]) -> SessionProjection {
    let mut projection = SessionProjection::default();
    for update in updates {
        projection.apply(update);
    }
    projection
}

/// Every row of a projection.
fn rows(projection: &SessionProjection) -> Vec<&TranscriptRow> {
    (0..projection.len())
        .filter_map(|index| projection.row(index))
        .collect()
}

#[test]
fn compact_inserts_a_compaction_row() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    say(&backend, &id, "hello");
    let mut updates = backend.wait_idle(&id);
    backend
        .block_on(backend.client.compact(&id))
        .expect("compact");
    updates.extend(backend.wait_for(&id, |update| {
        matches!(update, wire::SessionUpdate::CompactionUpdate(_))
            && serde_json::to_string(update)
                .is_ok_and(|text| text.contains("\"completed\""))
    }));

    let projection = project(&updates);

    let compaction = rows(&projection)
        .into_iter()
        .find_map(|row| match row {
            TranscriptRow::Compaction(row) => Some(row.clone()),
            _ => None,
        })
        .expect("compaction row");
    assert!(compaction.tokens_before.is_some());
    assert_eq!(compaction.reason, CompactionReason::Manual);
    assert!(
        projection
            .index_of(&RowKey::Compaction(compaction.entry_id.clone()))
            .is_some()
    );
}

#[test]
fn slash_invocation_and_output_carry_command_meta() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    say(&backend, &id, "hello");
    let mut updates = backend.wait_idle(&id);
    say(&backend, &id, "/name");
    updates.extend(backend.wait_idle(&id));

    let projection = project(&updates);

    let commands: Vec<(Role, CommandKind, Option<CommandStatus>)> =
        rows(&projection)
            .into_iter()
            .filter_map(|row| match row {
                TranscriptRow::Message(row) => {
                    row.command.as_ref().map(|command| {
                        assert_eq!(command.name, "name");
                        (row.role, command.kind, command.status)
                    })
                }
                _ => None,
            })
            .collect();
    assert!(commands.contains(&(Role::User, CommandKind::Invocation, None)));
    assert!(
        commands
            .iter()
            .any(|(role, kind, status)| *role == Role::Assistant
                && *kind == CommandKind::Output
                && *status == Some(CommandStatus::Succeeded))
    );
}

#[test]
fn assistant_meta_and_error_are_kept() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    say(&backend, &id, "hello");
    let mut projection = project(&backend.wait_idle(&id));
    let reply = rows(&projection)
        .into_iter()
        .find_map(|row| match row {
            TranscriptRow::Message(row) if row.role == Role::Assistant => {
                Some(row.clone())
            }
            _ => None,
        })
        .expect("reply");
    let assistant = reply.assistant.expect("assistant meta");
    assert!(assistant.usage.is_some());
    assert_eq!(assistant.model_id, "scripted");

    apply(
        &mut projection,
        json!({ "sessionUpdate": "agent_message", "messageId": "failed",
            "content": [],
            "_meta": { "clawcode": { "sequence": 999, "timestampMs": "5",
                "assistant": { "provider_id": "p", "model_id": "m",
                    "stop_reason": "error", "error": "overloaded" } } } }),
    );

    let failed = projection
        .index_of(&message_key("failed"))
        .and_then(|index| projection.row(index));
    let Some(TranscriptRow::Message(failed)) = failed else {
        panic!("failed message row");
    };
    assert!(failed.is_visible());
    assert_eq!(
        failed.assistant.as_ref().and_then(|a| a.error.as_deref()),
        Some("overloaded")
    );
}

#[test]
fn handler_failure_becomes_an_error_row() {
    let mut projection = SessionProjection::default();

    let change = apply(
        &mut projection,
        json!({
            "sessionUpdate": "_clawcode/event",
            "payload": { "event": "extension_handler_failed",
                "extension_id": "audit", "point": "before_tool", "message": "boom" },
            "_meta": { "clawcode": { "turnId": "t9", "timestampMs": "1", "sequence": 4 } }
        }),
    );

    let key = RowKey::Error("t9:4".to_string());
    assert!(change.inserted.contains(&key));
    let Some(TranscriptRow::Error(row)) = projection
        .index_of(&key)
        .and_then(|index| projection.row(index))
    else {
        panic!("error row");
    };
    assert_eq!(row.source, "audit");
    assert_eq!(row.message, "boom");
    assert_eq!(key.to_string(), "error:t9:4");
}

#[test]
fn replayed_handler_error_keeps_its_message() {
    let mut projection = SessionProjection::default();

    // The kernel persists the error text as the extension message's text
    // block; its details hold only the kind and the hook point.
    apply(
        &mut projection,
        json!({
            "sessionUpdate": "_clawcode/event",
            "payload": { "event": "message_end", "message": {
                "message_id": "m7", "turn_id": "t9",
                "timestamp_ms": "1", "started_at_ms": "1", "ended_at_ms": "1",
                "content": { "type": "extension", "extension": {
                    "extension_id": "audit", "custom_type": "before_tool",
                    "blocks": [{ "type": "text", "text": "boom" }],
                    "display": true, "include_in_context": false,
                    "details": { "kind": "handler_error", "point": "before_tool" }
                } }
            } },
            "_meta": { "clawcode": { "turnId": "t9", "timestampMs": "1", "sequence": 5 } }
        }),
    );

    let key = RowKey::Error("m7".to_string());
    let Some(TranscriptRow::Error(row)) = projection
        .index_of(&key)
        .and_then(|index| projection.row(index))
    else {
        panic!("error row");
    };
    assert_eq!(row.source, "audit");
    assert_eq!(row.message, "boom");
}

#[test]
fn agent_mail_becomes_a_mail_row() {
    let mut projection = SessionProjection::default();

    // A spawned agent's task arrives as a visible `agent_message` extension message.
    apply(
        &mut projection,
        json!({
            "sessionUpdate": "_clawcode/event",
            "payload": { "event": "message_end", "message": {
                "message_id": "m1", "turn_id": "t1",
                "timestamp_ms": "1", "started_at_ms": "1", "ended_at_ms": "1",
                "content": { "type": "extension", "extension": {
                    "extension_id": "clawcode", "custom_type": "agent_message",
                    "blocks": [{ "type": "text", "text":
                        "Message Type: new_task\nTask name: /root/task_1\nSender: /root\nPayload:\nreply with pong\nplease" }],
                    "display": true, "include_in_context": true,
                    "details": { "kind": "new_task", "author": "/root", "recipient": "/root/task_1" }
                } }
            } },
            "_meta": { "clawcode": { "turnId": "t1", "timestampMs": "1", "sequence": 1 } }
        }),
    );

    let key = RowKey::Mail("m1".to_string());
    let Some(TranscriptRow::Mail(row)) = projection
        .index_of(&key)
        .and_then(|index| projection.row(index))
    else {
        panic!("mail row");
    };
    assert_eq!(row.author, "/root");
    assert_eq!(row.kind, "new_task");
    assert_eq!(row.payload, "reply with pong\nplease");
    assert_eq!(key.to_string(), "mail:m1");
}

#[test]
fn hidden_extension_messages_stay_out_of_the_transcript() {
    let mut projection = SessionProjection::default();
    apply(
        &mut projection,
        json!({
            "sessionUpdate": "_clawcode/event",
            "payload": { "event": "message_end", "message": {
                "message_id": "m2", "turn_id": "t1",
                "timestamp_ms": "1", "started_at_ms": "1", "ended_at_ms": "1",
                "content": { "type": "extension", "extension": {
                    "extension_id": "clawcode", "custom_type": "agent_message",
                    "blocks": [{ "type": "text", "text": "Payload:\nsecret" }],
                    "display": false, "include_in_context": true,
                    "details": { "kind": "message", "author": "/root", "recipient": "/root/a" }
                } }
            } },
            "_meta": { "clawcode": { "turnId": "t1", "timestampMs": "1", "sequence": 1 } }
        }),
    );
    assert_eq!(projection.len(), 0);
}
