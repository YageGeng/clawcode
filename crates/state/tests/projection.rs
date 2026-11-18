mod support;

use std::io::Write;

use protocol::{EntryId, LaneId, SessionGitInfo, SessionProvenance};
use sqlx::Row;
use state::StateError;
use store::{SessionEntry, StoreFactory};
use support::{Fixture, SessionLog};

/// Writes the canonical owned Session used by the metadata and history tests.
fn canonical(fixture: &Fixture) -> (SessionLog, [EntryId; 3]) {
    let options = store::SessionCreateOptions::builder()
        .session_id(protocol::SessionId::try_from("session-a").expect("id"))
        .cwd(fixture.cwd("project"))
        .provenance(SessionProvenance {
            git: Some(SessionGitInfo {
                sha: Some("abc123".into()),
                branch: Some("main".into()),
                origin_url: Some("git@example.com:o/r.git".into()),
            }),
            app_version: "9.9.9".into(),
        })
        .build();
    let mut log =
        fixture.owned_session(options, "session-owner", "worker", "Ada");
    log.run_started("run-1", "agent");
    let user = log.user("hello\nworld");
    log.assistant("deepseek", "deepseek-flash", 120);
    log.tool_result("call-1");
    let final_assistant = log.assistant("deepseek", "deepseek-flash", 30);
    log.run_finished("run-1", "completed", None);
    log.thinking("high");
    let mode = log.custom(
        "clawcode",
        "session_mode",
        serde_json::json!({"mode": "plan", "turnId": "turn-x"}),
    );
    log.name(Some("Demo"));
    log.sync();
    (log, [user, final_assistant, mode])
}

/// Reads one `sessions` row through an external pool.
async fn session_row(fixture: &Fixture, id: &str) -> sqlx::sqlite::SqliteRow {
    sqlx::query("SELECT * FROM sessions WHERE id = ?")
        .bind(id)
        .fetch_one(&fixture.state_pool().await)
        .await
        .expect("session row")
}

/// Reads one cursor column from the metadata database.
async fn state_cursor(fixture: &Fixture, id: &str) -> (i64, i64) {
    let row = sqlx::query("SELECT next_byte_offset, next_sequence FROM projection_state WHERE session_id = ?")
        .bind(id)
        .fetch_one(&fixture.state_pool().await)
        .await
        .expect("cursor");
    (row.get(0), row.get(1))
}

/// Counts history entries of one Session.
async fn entry_count(fixture: &Fixture, id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM entries WHERE session_id = ?")
        .bind(id)
        .fetch_one(&fixture.history_pool().await)
        .await
        .expect("entry count")
}

/// Reads `tokens_used` of one Session.
async fn tokens(fixture: &Fixture, id: &str) -> i64 {
    session_row(fixture, id).await.get("tokens_used")
}

/// Returns the timestamp of one stored entry as an SQLite integer.
fn timestamp(log: &SessionLog, id: &EntryId) -> i64 {
    i64::try_from(log.store.get_entry(id).expect("entry").timestamp_ms.get())
        .expect("ts")
}

/// Every Codex-mapped metadata column is derived from the log.
#[tokio::test]
async fn refresh_projects_session_metadata() {
    let fixture = Fixture::new().await;
    let (log, [user, _, mode]) = canonical(&fixture);
    fixture
        .runtime
        .refresh(&log.id(), &log.path())
        .await
        .expect("refresh");

    let row = session_row(&fixture, "session-a").await;
    assert_eq!(
        row.get::<Option<String>, _>("name").as_deref(),
        Some("Demo")
    );
    assert_eq!(row.get::<String, _>("preview"), "hello\nworld");
    assert_eq!(row.get::<i64, _>("has_user_message"), 1);
    assert_eq!(
        row.get::<Option<String>, _>("provider_id").as_deref(),
        Some("deepseek")
    );
    assert_eq!(
        row.get::<Option<String>, _>("model_id").as_deref(),
        Some("deepseek-flash")
    );
    assert_eq!(
        row.get::<Option<String>, _>("thinking_level").as_deref(),
        Some("high")
    );
    assert_eq!(row.get::<String, _>("session_mode"), "plan");
    assert_eq!(row.get::<i64, _>("tokens_used"), 150);
    assert_eq!(row.get::<i64, _>("recency_at_ms"), timestamp(&log, &user));
    // The custom entry is the last mutation that carries a timestamp; the name fact has none.
    assert_eq!(row.get::<i64, _>("updated_at_ms"), timestamp(&log, &mode));
    assert_eq!(
        row.get::<Option<String>, _>("owner_session_id").as_deref(),
        Some("session-owner")
    );
    assert_eq!(
        row.get::<Option<String>, _>("agent_name").as_deref(),
        Some("worker")
    );
    assert_eq!(
        row.get::<Option<String>, _>("agent_nickname").as_deref(),
        Some("Ada")
    );
    assert_eq!(
        row.get::<Option<String>, _>("git_sha").as_deref(),
        Some("abc123")
    );
    assert_eq!(
        row.get::<Option<String>, _>("git_branch").as_deref(),
        Some("main")
    );
    assert_eq!(
        row.get::<Option<String>, _>("git_origin_url").as_deref(),
        Some("git@example.com:o/r.git")
    );
    assert_eq!(
        row.get::<Option<String>, _>("app_version").as_deref(),
        Some("9.9.9")
    );
    assert_eq!(
        row.get::<String, _>("path"),
        log.path().to_str().expect("utf8")
    );
}

/// History rows mirror the store's entries, lane leaf, and Run lifecycle.
#[tokio::test]
async fn refresh_projects_history_matching_store() {
    let fixture = Fixture::new().await;
    let (log, [user, final_assistant, _]) = canonical(&fixture);
    fixture
        .runtime
        .refresh(&log.id(), &log.path())
        .await
        .expect("refresh");
    let history = fixture.history_pool().await;

    let rows = sqlx::query(
        "SELECT entry_id, parent_id, kind, message_type, run_id, entry_json FROM entries \
         WHERE session_id = 'session-a' ORDER BY sequence",
    )
    .fetch_all(&history)
    .await
    .expect("entries");
    let expected = log.store.entries();
    assert_eq!(rows.len(), expected.len());
    for (row, entry) in rows.iter().zip(&expected) {
        assert_eq!(row.get::<String, _>("entry_id"), entry.id.as_str());
        assert_eq!(
            row.get::<Option<String>, _>("parent_id"),
            entry.parent_id.as_ref().map(|id| id.as_str().to_owned())
        );
        assert_eq!(row.get::<String, _>("kind"), entry.kind.as_str());
        let decoded: SessionEntry =
            serde_json::from_str(&row.get::<String, _>("entry_json"))
                .expect("entry json");
        assert_eq!(&decoded, entry);
    }
    let user_row = rows
        .iter()
        .find(|row| row.get::<String, _>("entry_id") == user.as_str())
        .expect("user row");
    assert_eq!(
        user_row.get::<Option<String>, _>("message_type").as_deref(),
        Some("user")
    );
    let in_run = rows
        .iter()
        .filter(|row| {
            row.get::<Option<String>, _>("run_id").as_deref() == Some("run-1")
        })
        .count();
    assert_eq!(
        in_run, 4,
        "user, two assistants, and the tool result ran inside run-1"
    );

    let lanes = sqlx::query(
        "SELECT lane, leaf_id FROM lanes WHERE session_id = 'session-a'",
    )
    .fetch_all(&history)
    .await
    .expect("lanes");
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0].get::<String, _>("lane"), "main");
    let main = LaneId::try_from("main").expect("lane");
    assert_eq!(
        lanes[0].get::<Option<String>, _>("leaf_id"),
        log.store.lane(&main).map(|id| id.as_str().to_owned())
    );

    let run = sqlx::query("SELECT * FROM runs WHERE session_id = 'session-a'")
        .fetch_one(&history)
        .await
        .expect("run");
    assert_eq!(run.get::<String, _>("run_id"), "run-1");
    assert_eq!(run.get::<String, _>("operation"), "agent");
    assert_eq!(run.get::<String, _>("status"), "completed");
    assert_eq!(
        run.get::<Option<String>, _>("first_user_entry_id")
            .as_deref(),
        Some(user.as_str())
    );
    assert_eq!(
        run.get::<Option<String>, _>("final_assistant_entry_id")
            .as_deref(),
        Some(final_assistant.as_str())
    );
    let started: i64 = run.get("started_at_ms");
    let completed: i64 = run.get("completed_at_ms");
    assert!(completed > started);
    assert_eq!(run.get::<i64, _>("duration_ms"), completed - started);
}

/// Repeated refreshes apply each line once; new lines are applied incrementally.
#[tokio::test]
async fn refresh_is_incremental_and_idempotent() {
    let fixture = Fixture::new().await;
    let (mut log, _) = canonical(&fixture);
    fixture
        .runtime
        .refresh(&log.id(), &log.path())
        .await
        .expect("first");
    fixture
        .runtime
        .refresh(&log.id(), &log.path())
        .await
        .expect("second");
    assert_eq!(tokens(&fixture, "session-a").await, 150);
    let before = entry_count(&fixture, "session-a").await;

    log.assistant("deepseek", "deepseek-flash", 7);
    log.sync();
    fixture
        .runtime
        .refresh(&log.id(), &log.path())
        .await
        .expect("third");

    assert_eq!(tokens(&fixture, "session-a").await, 157);
    assert_eq!(entry_count(&fixture, "session-a").await, before + 1);
}

/// A torn tail waits until it is a complete line.
#[tokio::test]
async fn refresh_ignores_torn_tail_until_complete() {
    let fixture = Fixture::new().await;
    let (log, _) = canonical(&fixture);
    let path = log.path();
    let complete = i64::try_from(std::fs::metadata(&path).expect("meta").len())
        .expect("len");
    drop(log);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append")
        .write_all(b"{\"kind\":\"entry\"")
        .expect("torn tail");
    let id = protocol::SessionId::try_from("session-a").expect("id");

    fixture.runtime.refresh(&id, &path).await.expect("refresh");
    assert_eq!(state_cursor(&fixture, "session-a").await.0, complete);
    let before = entry_count(&fixture, "session-a").await;

    // Reopening repairs the torn tail; the next append is a complete line.
    let mut log = SessionLog::new(fixture.factory.open(&path).expect("reopen"));
    log.user("after repair");
    log.sync();
    fixture
        .runtime
        .refresh(&id, &path)
        .await
        .expect("refresh after repair");
    assert_eq!(entry_count(&fixture, "session-a").await, before + 1);
}

/// A log rewritten shorter for the same identity is reprojected from scratch.
#[tokio::test]
async fn refresh_reprojects_rewritten_file() {
    let fixture = Fixture::new().await;
    let (log, _) = canonical(&fixture);
    let path = log.path();
    fixture
        .runtime
        .refresh(&log.id(), &path)
        .await
        .expect("refresh");

    let other = Fixture::new().await;
    let mut replacement = other.session(other.options("session-a"));
    replacement.user("replacement");
    replacement.sync();
    std::fs::copy(replacement.path(), &path).expect("overwrite");
    fixture
        .runtime
        .refresh(&log.id(), &path)
        .await
        .expect("reproject");

    let row = session_row(&fixture, "session-a").await;
    assert_eq!(row.get::<String, _>("preview"), "replacement");
    assert_eq!(row.get::<i64, _>("tokens_used"), 0);
    assert!(row.get::<Option<String>, _>("owner_session_id").is_none());
    assert_eq!(entry_count(&fixture, "session-a").await, 1);
    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
        .fetch_one(&fixture.history_pool().await)
        .await
        .expect("runs");
    assert_eq!(runs, 0);
}

/// A log rewritten longer so the cursor lands inside a line is reprojected.
#[tokio::test]
async fn refresh_reprojects_when_cursor_lands_mid_line() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-m"));
    log.user("short");
    log.sync();
    let path = log.path();
    fixture
        .runtime
        .refresh(&log.id(), &path)
        .await
        .expect("refresh");

    let other = Fixture::new().await;
    let mut replacement = other.session(other.options("session-m"));
    replacement.user(&"long ".repeat(400));
    replacement.user("after");
    replacement.sync();
    std::fs::copy(replacement.path(), &path).expect("overwrite");
    fixture
        .runtime
        .refresh(&log.id(), &path)
        .await
        .expect("reproject");

    assert_eq!(entry_count(&fixture, "session-m").await, 2);
}

/// A log whose header names another Session is refused once, without rows.
#[tokio::test]
async fn foreign_header_is_refused() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-b"));
    log.sync();
    let id = protocol::SessionId::try_from("session-a").expect("id");

    let error = fixture
        .runtime
        .refresh(&id, &log.path())
        .await
        .expect_err("foreign header");

    match error {
        StateError::Projection { offset, reason, .. } => {
            assert_eq!(offset, 0);
            assert_eq!(reason, "log header belongs to session session-b");
        }
        other => panic!("unexpected error: {other}"),
    }
    assert!(
        fixture
            .runtime
            .session(&id)
            .await
            .expect("lookup")
            .is_none()
    );
}

/// Two runtimes refreshing the same Session at once apply each line once.
#[tokio::test]
async fn concurrent_refresh_counts_tokens_once() {
    let fixture = Fixture::new().await;
    let (log, _) = canonical(&fixture);
    let second = state::StateRuntime::init(&fixture.config)
        .await
        .expect("second runtime");
    let id = log.id();
    let path = log.path();

    let (first, other) = tokio::join!(
        fixture.runtime.refresh(&id, &path),
        second.refresh(&id, &path)
    );

    first.expect("first refresh");
    other.expect("second refresh");
    assert_eq!(tokens(&fixture, "session-a").await, 150);
}

/// Only clawcode's own session_mode entries change the mode.
#[tokio::test]
async fn third_party_session_mode_is_ignored() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-m"));
    log.custom(
        "other-ext",
        "session_mode",
        serde_json::json!({"mode": "plan"}),
    );
    log.sync();
    fixture
        .runtime
        .refresh(&log.id(), &log.path())
        .await
        .expect("refresh");
    assert_eq!(
        session_row(&fixture, "session-m")
            .await
            .get::<String, _>("session_mode"),
        "default"
    );
}

/// A Run belongs to its own lane only.
#[tokio::test]
async fn entry_on_other_lane_has_no_run() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-l"));
    log.user("root");
    log.run_started("run-1", "agent");
    let side = log.on_lane("side").user("side question");
    log.sync();
    fixture
        .runtime
        .refresh(&log.id(), &log.path())
        .await
        .expect("refresh");
    let run_id: Option<String> =
        sqlx::query_scalar("SELECT run_id FROM entries WHERE entry_id = ?")
            .bind(side.as_str())
            .fetch_one(&fixture.history_pool().await)
            .await
            .expect("side entry");
    assert!(run_id.is_none());
}

/// Compaction Runs keep the operation and outcome the kernel records for them.
#[tokio::test]
async fn compaction_run_records_operation_and_outcome() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-c"));
    log.user("root");
    log.compaction_started("run-c");
    log.compaction_finished(
        "run-c",
        protocol::CompactionOperationOutcome::Aborted,
    );
    log.sync();
    fixture
        .runtime
        .refresh(&log.id(), &log.path())
        .await
        .expect("refresh");
    let (operation, status): (String, String) = sqlx::query_as(
        "SELECT operation, status FROM runs WHERE run_id = 'run-c'",
    )
    .fetch_one(&fixture.history_pool().await)
    .await
    .expect("run");
    assert_eq!(
        (operation.as_str(), status.as_str()),
        ("compaction", "aborted")
    );
}

/// A header-only Session gets usable initial timestamps.
#[tokio::test]
async fn header_only_session_gets_initial_timestamps() {
    let fixture = Fixture::new().await;
    let log = fixture.session(fixture.options("session-h"));
    fixture
        .runtime
        .refresh(&log.id(), &log.path())
        .await
        .expect("refresh");
    let row = session_row(&fixture, "session-h").await;
    let created: i64 = row.get("created_at_ms");
    assert_eq!(row.get::<i64, _>("updated_at_ms"), created);
    assert_eq!(row.get::<i64, _>("recency_at_ms"), created);
    assert_eq!(row.get::<String, _>("preview"), "");
    assert_eq!(row.get::<i64, _>("has_user_message"), 0);
    assert_eq!(state_cursor(&fixture, "session-h").await.1, 1);
}

/// Preview truncation counts characters and never splits one.
#[tokio::test]
async fn preview_keeps_200_chars_on_char_boundaries() {
    let fixture = Fixture::new().await;
    let text = "汉😀".repeat(150);
    let mut log = fixture.session(fixture.options("session-p"));
    log.user(&text);
    log.sync();
    fixture
        .runtime
        .refresh(&log.id(), &log.path())
        .await
        .expect("refresh");
    let preview: String =
        session_row(&fixture, "session-p").await.get("preview");
    assert_eq!(preview.chars().count(), 200);
    assert_eq!(preview, text.chars().take(200).collect::<String>());
}

/// Projection stops at the first line it cannot apply and keeps everything before it.
#[tokio::test]
async fn invalid_line_stops_at_last_good_line() {
    let fixture = Fixture::new().await;
    let (log, _) = canonical(&fixture);
    let path = log.path();
    let bad_offset = std::fs::metadata(&path).expect("meta").len();
    drop(log);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append")
        .write_all(b"{\"kind\":\"bogus\",\"seq\":99}\n")
        .expect("bad line");
    let id = protocol::SessionId::try_from("session-a").expect("id");

    let error = fixture
        .runtime
        .refresh(&id, &path)
        .await
        .expect_err("bad line");

    assert!(
        matches!(error, StateError::Projection { offset, .. } if offset == bad_offset)
    );
    assert_eq!(tokens(&fixture, "session-a").await, 150);
    assert_eq!(
        state_cursor(&fixture, "session-a").await.0,
        i64::try_from(bad_offset).expect("offset")
    );
}

/// A line that fails after a partial update rolls that update back.
#[tokio::test]
async fn failed_line_rolls_back_its_partial_updates() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-rollback"));
    log.user("before");
    log.sync();
    fixture
        .runtime
        .refresh(&log.id(), &log.path())
        .await
        .expect("refresh");
    let before: i64 = session_row(&fixture, "session-rollback")
        .await
        .get("updated_at_ms");

    // `touch` runs before message decoding fails on this payload.
    log.entry(
        store::EntryKind::Message,
        serde_json::json!({"not": "a message"}),
    );
    log.sync();
    let error = fixture.runtime.refresh(&log.id(), &log.path()).await;

    assert!(matches!(error, Err(StateError::Projection { .. })));
    let after: i64 = session_row(&fixture, "session-rollback")
        .await
        .get("updated_at_ms");
    assert_eq!(after, before);
}
