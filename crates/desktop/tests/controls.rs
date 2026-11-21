//! Session controls: configuration, commands, usage, plan, retries, and questions.

mod support;

use agent_client_protocol::v2::schema as wire;
use desktop::backend::QuestionKey;
use desktop::projection::{RetryState, SessionProjection, TodoStatus};
use serde_json::{Value, json};
use support::ScriptedBackend;

/// Parses one wire update from its JSON form.
fn update(value: Value) -> wire::SessionUpdate {
    serde_json::from_value(value).expect("valid session update")
}

/// Applies every update to a fresh projection.
fn project(updates: &[wire::SessionUpdate]) -> SessionProjection {
    let mut projection = SessionProjection::default();
    for update in updates {
        projection.apply(update);
    }
    projection
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

/// Whether the update is the session going idle.
fn is_idle(update: &wire::SessionUpdate) -> bool {
    matches!(
        update,
        wire::SessionUpdate::StateUpdate(wire::StateUpdate::Idle(_))
    )
}

#[test]
fn config_options_come_from_session_start_and_updates() {
    let mut backend = ScriptedBackend::start();
    let start = backend
        .block_on(backend.client.new_session(backend.workspace.path()))
        .expect("new session");
    let mut projection = SessionProjection::default();

    projection.controls_mut().set_config(start.config_options);

    let controls = projection.controls();
    let model = controls.config("model").expect("model option");
    assert!(!model.choices.is_empty());
    assert_eq!(controls.config("mode").expect("mode").current, "default");
    backend
        .block_on(backend.client.set_config_option(
            &start.id,
            "thinking_level",
            "high",
        ))
        .expect("set");
    let updates = backend.wait_for(&start.id, |update| {
        matches!(update, wire::SessionUpdate::ConfigOptionUpdate(_))
    });
    for update in &updates {
        projection.apply(update);
    }
    assert_eq!(
        projection
            .controls()
            .config("thinking_level")
            .expect("thinking")
            .current,
        "high"
    );
}

#[test]
fn commands_snapshot_replaces_the_list() {
    let mut projection = SessionProjection::default();
    let snapshot = |names: &[&str]| {
        let commands: Vec<Value> = names
            .iter()
            .map(|name| {
                json!({
                    "type": "command", "name": name, "description": format!("{name} command"),
                    "_meta": { "clawcode": { "slashCommand": { "source": "builtin", "aliasKind": "canonical" } } }
                })
            })
            .collect();
        update(json!({
            "sessionUpdate": "available_commands_update",
            "availableCommands": commands
        }))
    };

    let first = projection.apply(&snapshot(&["compact", "name"]));
    projection.apply(&snapshot(&["session"]));

    assert!(first.controls);
    let names: Vec<&str> = projection
        .controls()
        .commands()
        .iter()
        .map(|command| command.name.as_str())
        .collect();
    assert_eq!(names, ["session"]);
}

#[test]
fn usage_update_sets_context_and_percent() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    say(&backend, &id, "hello");

    let projection = project(&backend.wait_idle(&id));

    let context = projection.controls().context().expect("context usage");
    assert!(context.size > 0);
    assert!(context.percent_tenths().is_some());
}

#[test]
fn malformed_usage_counter_drops_the_whole_usage() {
    let mut projection = SessionProjection::default();

    projection.apply(&update(json!({
        "sessionUpdate": "usage_update", "used": 10, "size": 100,
        "_meta": { "clawcode": {
            "usage": { "input_tokens": "x", "output_tokens": "2" },
            "contextWindow": "100", "timestampMs": "1", "sequence": 1
        } }
    })));

    let context = projection.controls().context().expect("context");
    assert_eq!(context.size, 100);
    assert!(context.exact.is_none());
    assert!(context.used.is_none());
}

#[test]
fn todos_and_open_plan_follow_plan_updates() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    say(&backend, &id, "todo");
    let updates = backend.wait_idle(&id);
    let (idle, live): (Vec<_>, Vec<_>) = updates.into_iter().partition(is_idle);

    let mut projection = project(&live);

    let todos = projection.controls().todos();
    assert_eq!(todos.len(), 2);
    assert_eq!(todos[0].status, TodoStatus::InProgress);
    assert_eq!(todos[1].status, TodoStatus::Pending);
    assert!(projection.controls().plan_open());
    for update in &idle {
        projection.apply(update);
    }
    assert!(!projection.controls().plan_open());
}

#[test]
fn asked_questions_open_and_settle() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    say(&backend, &id, "ask");
    let mut projection = project(&backend.wait_idle(&id));
    let open = projection.controls().open_questions();
    assert_eq!(open.len(), 2);
    assert_eq!(open[0].title, "Pick one");
    assert_eq!(open[0].options, ["a", "b"]);
    let first = open[0].key.clone();
    let second: QuestionKey = open[1].key.clone();

    backend
        .block_on(backend.client.answer_question(&id, &first, "a"))
        .expect("answer");
    for update in &backend.wait_idle(&id) {
        projection.apply(update);
    }
    assert_eq!(projection.controls().open_questions().len(), 1);

    backend
        .block_on(backend.client.dismiss_questions(&id, &[second]))
        .expect("dismiss");
    let settled = backend.wait_for(&id, |update| {
        serde_json::to_string(update)
            .is_ok_and(|text| text.contains("question_reply"))
    });
    for update in &settled {
        projection.apply(update);
    }
    assert!(projection.controls().open_questions().is_empty());
}

#[test]
fn questions_reopen_after_replay() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    say(&backend, &id, "ask");
    backend.wait_idle(&id);

    backend
        .block_on(
            backend
                .client
                .resume_with_replay(&id, backend.workspace.path()),
        )
        .expect("resume");
    let projection = project(&backend.wait_replay_complete(&id));

    assert_eq!(projection.controls().open_questions().len(), 2);
}

#[test]
fn compaction_clears_context_usage() {
    let mut backend = ScriptedBackend::start();
    let id = backend.new_session();
    say(&backend, &id, "hello");
    let mut projection = project(&backend.wait_idle(&id));
    assert!(projection.controls().context().is_some());

    backend
        .block_on(backend.client.compact(&id))
        .expect("compact");
    let updates = backend.wait_for(&id, |update| {
        matches!(update, wire::SessionUpdate::CompactionUpdate(_))
            && serde_json::to_string(update)
                .is_ok_and(|text| text.contains("\"completed\""))
    });
    for update in &updates {
        projection.apply(update);
    }

    assert!(projection.controls().context().is_none());
    assert!(projection.controls().compaction().is_none());
}

#[test]
fn retry_countdown_state() {
    let mut projection = SessionProjection::default();

    projection.apply(&update(json!({
        "sessionUpdate": "_clawcode/event",
        "payload": { "event": "retry_scheduled", "run_id": "r1", "attempt": 1,
                     "max_attempts": 3, "delay_ms": 1200, "error": "overloaded" },
        "_meta": { "clawcode": { "turnId": "t", "timestampMs": "1000", "sequence": 1 } }
    })));

    assert_eq!(
        projection.controls().retry(),
        Some(&RetryState::Waiting {
            attempt: 1,
            max: 3,
            due_ms: 2200,
            error: "overloaded".to_string()
        })
    );
}

#[test]
fn failed_retry_and_compaction_clear_when_the_next_run_starts() {
    let mut projection = SessionProjection::default();
    projection.apply(&update(json!({
        "sessionUpdate": "_clawcode/event",
        "payload": { "event": "retry_end", "run_id": "r1", "attempt": 3,
                     "success": false, "final_error": "overloaded" },
        "_meta": { "clawcode": { "turnId": "t", "timestampMs": "1000", "sequence": 1 } }
    })));
    projection.apply(&update(json!({
        "sessionUpdate": "compaction_update", "compactionId": "c1", "status": "failed", "error": "too small"
    })));
    projection.apply(&update(
        json!({ "sessionUpdate": "state_update", "state": "idle" }),
    ));
    assert!(matches!(
        projection.controls().retry(),
        Some(RetryState::Failed { .. })
    ));
    assert!(projection.controls().compaction().is_some());

    // A new run is a fresh start: the last run's failures stop showing.
    let change = projection.apply(&update(
        json!({ "sessionUpdate": "state_update", "state": "running" }),
    ));

    assert!(change.controls);
    assert!(projection.controls().retry().is_none());
    assert!(projection.controls().compaction().is_none());
}
