mod support;

use std::num::NonZeroU32;

use protocol::{LaneId, RunId, SessionId, SessionTreeEntry};
use state::{SessionFilter, SessionScope, SortKey};
use store::SessionEntry;
use support::{Fixture, SessionLog};

/// Refreshes one log into the fixture's databases.
async fn project(fixture: &Fixture, log: &SessionLog) {
    fixture
        .runtime
        .refresh(&log.id(), &log.path())
        .await
        .expect("refresh");
}

/// Seeds five Sessions whose three sort orders all differ (see each assertion).
async fn seeded() -> Fixture {
    let fixture = Fixture::new().await;
    let mut s1 = SessionLog::new(
        store::StoreFactory::create(
            fixture.factory.as_ref(),
            options(&fixture, "s1", "alpha"),
        )
        .expect("s1"),
    );
    s1.user("first");
    s1.assistant("p-a", "m", 1);
    s1.sync();
    let mut s2 = SessionLog::new(
        store::StoreFactory::create(
            fixture.factory.as_ref(),
            options(&fixture, "s2", "alpha"),
        )
        .expect("s2"),
    );
    s2.user("second");
    s2.assistant("p-b", "m", 1);
    s2.sync();
    let mut s3 = SessionLog::new(
        store::StoreFactory::create(
            fixture.factory.as_ref(),
            options(&fixture, "s3", "beta"),
        )
        .expect("s3"),
    );
    s3.user("third");
    s3.assistant("p-a", "m", 1);
    s3.sync();
    let mut s4 = fixture.owned_session(
        options(&fixture, "s4", "beta"),
        "s1",
        "worker",
        "Ada",
    );
    s4.user("child");
    s4.assistant("p-c", "m", 1);
    s4.sync();
    let mut s5 = SessionLog::new(
        store::StoreFactory::create(
            fixture.factory.as_ref(),
            options(&fixture, "s5", "beta"),
        )
        .expect("s5"),
    );
    s5.thinking("low");
    s5.sync();
    s2.assistant("p-b", "m", 1);
    s2.sync();
    s3.user("third again");
    s3.sync();
    for log in [&s1, &s2, &s3, &s4, &s5] {
        project(&fixture, log).await;
    }
    fixture
}

/// Creation options for `id` in the named cwd of the fixture.
fn options(
    fixture: &Fixture,
    id: &str,
    cwd: &str,
) -> store::SessionCreateOptions {
    store::SessionCreateOptions::builder()
        .session_id(SessionId::try_from(id).expect("id"))
        .cwd(fixture.cwd(cwd))
        .build()
}

/// Lists ids for one filter.
async fn ids(fixture: &Fixture, filter: SessionFilter<'_>) -> Vec<String> {
    fixture
        .runtime
        .list_sessions(filter)
        .await
        .expect("list")
        .sessions
        .into_iter()
        .map(|row| row.id.to_string())
        .collect()
}

/// Each sort key orders newest first with its own timestamp.
#[tokio::test]
async fn list_orders_by_each_sort_key() {
    let fixture = seeded().await;
    let by = |key| SessionFilter::builder().sort_key(key).build();
    assert_eq!(
        ids(&fixture, by(SortKey::CreatedAt)).await,
        ["s5", "s4", "s3", "s2", "s1"]
    );
    assert_eq!(
        ids(&fixture, by(SortKey::UpdatedAt)).await,
        ["s3", "s2", "s5", "s4", "s1"]
    );
    assert_eq!(
        ids(&fixture, by(SortKey::RecencyAt)).await,
        ["s3", "s5", "s4", "s2", "s1"]
    );
}

/// Filters narrow by workspace, visibility, subagent ownership, and provider.
#[tokio::test]
async fn list_filters_by_cwd_scope_and_provider() {
    let fixture = seeded().await;
    let alpha = fixture.cwd("alpha");
    assert_eq!(
        ids(
            &fixture,
            SessionFilter::builder().cwd(Some(alpha.as_path())).build()
        )
        .await,
        ["s2", "s1"]
    );
    assert_eq!(
        ids(
            &fixture,
            SessionFilter::builder()
                .scope(SessionScope::Visible)
                .build()
        )
        .await,
        ["s3", "s2", "s4", "s1"]
    );
    assert_eq!(
        ids(
            &fixture,
            SessionFilter::builder()
                .scope(SessionScope::Interactive)
                .build()
        )
        .await,
        ["s3", "s2", "s1"]
    );
    assert_eq!(
        ids(
            &fixture,
            SessionFilter::builder().provider_id(Some("p-a")).build()
        )
        .await,
        ["s3", "s1"]
    );
}

/// Keyset pages walk every row once and end without an anchor.
#[tokio::test]
async fn list_pages_with_anchor() {
    let fixture = seeded().await;
    let mut anchor = None;
    let mut seen = Vec::new();
    let mut pages = 0;
    loop {
        let page = fixture
            .runtime
            .list_sessions(
                SessionFilter::builder()
                    .limit(Some(NonZeroU32::new(2).expect("limit")))
                    .anchor(anchor.take())
                    .build(),
            )
            .await
            .expect("page");
        pages += 1;
        seen.extend(page.sessions.into_iter().map(|row| row.id.to_string()));
        match page.next_anchor {
            Some(next) => anchor = Some(next),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen, ["s3", "s2", "s5", "s4", "s1"]);
}

/// LIKE wildcards in a search term match only themselves.
#[tokio::test]
async fn search_matches_percent_and_underscore_literally() {
    let fixture = Fixture::new().await;
    for (id, name) in [("s-literal", "100%_done"), ("s-wild", "100xydone")] {
        let mut log = fixture.session(fixture.options(id));
        log.name(Some(name));
        log.sync();
        project(&fixture, &log).await;
    }
    assert_eq!(
        ids(
            &fixture,
            SessionFilter::builder().search_term(Some("0%_")).build()
        )
        .await,
        ["s-literal"]
    );
}

/// Children of one owner come back ordered by agent name.
#[tokio::test]
async fn children_lists_owned_sessions_by_agent_name() {
    let fixture = Fixture::new().await;
    for (id, agent) in [("c-zed", "zed"), ("c-amy", "amy")] {
        let mut log =
            fixture.owned_session(fixture.options(id), "owner", agent, agent);
        log.sync();
        project(&fixture, &log).await;
    }
    let children = fixture
        .runtime
        .children(&SessionId::try_from("owner").expect("owner"))
        .await
        .expect("children");
    let names = children
        .iter()
        .map(|row| row.ownership.as_ref().expect("edge").name.to_string())
        .collect::<Vec<_>>();
    assert_eq!(names, ["amy", "zed"]);
}

/// Deleting a Session clears every row in both databases.
#[tokio::test]
async fn delete_session_clears_both_databases() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("s-gone"));
    log.run_started("run-1", "agent");
    log.user("bye");
    log.sync();
    project(&fixture, &log).await;

    fixture
        .runtime
        .delete_session(&log.id())
        .await
        .expect("delete");

    let state = fixture.state_pool().await;
    let history = fixture.history_pool().await;
    for (pool, table) in [
        (&state, "sessions"),
        (&state, "projection_state"),
        (&history, "projection_state"),
        (&history, "runs"),
        (&history, "entries"),
        (&history, "lanes"),
    ] {
        let sql = match table {
            "sessions" => "SELECT COUNT(*) FROM sessions",
            "projection_state" => "SELECT COUNT(*) FROM projection_state",
            "runs" => "SELECT COUNT(*) FROM runs",
            "entries" => "SELECT COUNT(*) FROM entries",
            _ => "SELECT COUNT(*) FROM lanes",
        };
        let count: i64 = sqlx::query_scalar(sql)
            .fetch_one(pool)
            .await
            .expect("count");
        assert_eq!(count, 0, "{table} still has rows");
    }
    assert!(
        fixture
            .runtime
            .session(&log.id())
            .await
            .expect("lookup")
            .is_none()
    );
}

/// Maps a stored entry the way the kernel's tree snapshot does.
fn tree_entry(entry: SessionEntry) -> SessionTreeEntry {
    SessionTreeEntry::builder()
        .entry_id(entry.id)
        .parent_id(entry.parent_id)
        .kind(entry.kind.as_str().to_string())
        .timestamp_ms(entry.timestamp_ms)
        .payload(serde_json::Value::Object(entry.payload))
        .build()
}

/// The history database reproduces the kernel's Session tree snapshot.
#[tokio::test]
async fn tree_snapshot_matches_store() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("s-tree"));
    log.user("root");
    log.assistant("p", "m", 1);
    log.on_lane("side").user("branch");
    log.on_lane("main").user("main again");
    log.name(Some("Tree"));
    log.sync();
    project(&fixture, &log).await;
    let main = LaneId::try_from("main").expect("lane");

    let snapshot = fixture
        .runtime
        .tree_snapshot(&log.id(), &main)
        .await
        .expect("snapshot")
        .expect("known session");

    assert_eq!(snapshot.session_id, log.id());
    assert_eq!(snapshot.lane, main);
    assert_eq!(snapshot.leaf_id.as_ref(), log.store.lane(&main));
    assert_eq!(snapshot.name.as_deref(), Some("Tree"));
    assert_eq!(
        snapshot.entries,
        log.store
            .entries()
            .into_iter()
            .map(tree_entry)
            .collect::<Vec<_>>()
    );
}

/// A branch is returned from the root down to the requested leaf.
#[tokio::test]
async fn branch_walks_root_to_leaf() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("s-branch"));
    log.user("root");
    log.assistant("p", "m", 1);
    let leaf = log.on_lane("side").user("branch");
    log.on_lane("main").user("main again");
    log.sync();
    project(&fixture, &log).await;

    let branch = fixture
        .runtime
        .branch(&log.id(), &leaf)
        .await
        .expect("branch");

    let expected = log.store.branch(&leaf).expect("store branch");
    assert_eq!(
        branch,
        expected.into_iter().map(tree_entry).collect::<Vec<_>>()
    );
}

/// Older indexes with a broken parent report the offending entry instead of a partial branch.
#[tokio::test]
async fn branch_requires_a_root_in_an_existing_index() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("old-index"));
    let root = log.user("root");
    let leaf = log.user("leaf");
    log.sync();
    project(&fixture, &log).await;
    sqlx::query("UPDATE entries SET parent_id = 'missing' WHERE entry_id = ?")
        .bind(root.as_str())
        .execute(&fixture.history_pool().await)
        .await
        .expect("old index");
    assert!(matches!(fixture.runtime.branch(&log.id(), &leaf).await,
        Err(state::StateError::InvalidParent { entry_id, .. }) if entry_id == root));
}

/// Runs page by start position, and one Run's entries come back in log order.
#[tokio::test]
async fn runs_and_run_entries_page_in_log_order() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("s-runs"));
    log.run_started("run-1", "agent");
    let first = log.user("one");
    let reply = log.assistant("p", "m", 1);
    log.run_finished("run-1", "failed", Some("boom"));
    log.run_started("run-2", "compaction");
    log.sync();
    project(&fixture, &log).await;

    let runs = fixture
        .runtime
        .runs(&log.id(), None, 10)
        .await
        .expect("runs");
    let names = runs
        .iter()
        .map(|run| run.run_id.to_string())
        .collect::<Vec<_>>();
    assert_eq!(names, ["run-1", "run-2"]);
    assert_eq!(runs[0].status, "failed");
    assert_eq!(runs[0].error_json.as_deref(), Some("\"boom\""));
    assert_eq!(runs[1].operation, "compaction");
    assert_eq!(runs[1].status, "in_progress");
    let later = fixture
        .runtime
        .runs(&log.id(), Some(runs[0].start_sequence), 10)
        .await
        .expect("later runs");
    assert_eq!(later.len(), 1);
    assert_eq!(later[0].run_id.as_str(), "run-2");

    let entries = fixture
        .runtime
        .run_entries(&log.id(), &RunId::try_from("run-1").expect("run"))
        .await
        .expect("run entries");
    let ids = entries
        .iter()
        .map(|entry| entry.entry_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(ids, [first.clone(), reply]);
    let users = fixture
        .runtime
        .user_entries(&log.id())
        .await
        .expect("users");
    assert_eq!(
        users
            .iter()
            .map(|entry| entry.entry_id.clone())
            .collect::<Vec<_>>(),
        [first]
    );
}

/// History reads catch up with the log before answering.
#[tokio::test]
async fn history_reads_refresh_first() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("s-fresh"));
    log.user("one");
    log.sync();
    project(&fixture, &log).await;
    let late = log.user("two");
    log.name(Some("Offline title"));
    log.sync();

    let snapshot = fixture
        .runtime
        .tree_snapshot(&log.id(), &LaneId::try_from("main").expect("lane"))
        .await
        .expect("snapshot")
        .expect("known session");

    assert_eq!(
        snapshot.entries.last().map(|entry| &entry.entry_id),
        Some(&late)
    );
    assert_eq!(snapshot.name.as_deref(), Some("Offline title"));
}

/// An idle history reader proceeds while external connections hold both writer locks.
#[tokio::test]
async fn caught_up_history_needs_no_writer_lock() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("idle"));
    log.user("projected");
    log.sync();
    project(&fixture, &log).await;
    let pool = fixture.state_pool().await;
    let held = pool.begin_with("BEGIN IMMEDIATE").await.expect("writer");
    let history = fixture.history_pool().await;
    let held_history = history
        .begin_with("BEGIN IMMEDIATE")
        .await
        .expect("history writer");
    let rows = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        fixture.runtime.user_entries(&log.id()),
    )
    .await;
    held.rollback().await.expect("release writer");
    held_history
        .rollback()
        .await
        .expect("release history writer");
    assert_eq!(
        rows.expect("read while writer is held")
            .expect("history")
            .len(),
        1
    );
}

/// An empty text preview still represents an interactive user message.
#[tokio::test]
async fn user_message_without_preview_is_visible() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("empty"));
    log.message(protocol::MessageContent::User {
        blocks: vec![protocol::ContentBlock::Image {
            data: "AA==".into(),
            mime_type: "image/png".into(),
        }],
    });
    log.sync();
    project(&fixture, &log).await;
    for scope in [SessionScope::Visible, SessionScope::Interactive] {
        assert_eq!(
            ids(&fixture, SessionFilter::builder().scope(scope).build()).await,
            ["empty"]
        );
    }
}
