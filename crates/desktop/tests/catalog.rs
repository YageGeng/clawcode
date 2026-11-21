//! Session catalog: summaries from `session/list`, grouping, ordering, search, and ages.

use std::path::{Path, PathBuf};

use agent_client_protocol::v2::schema as wire;
use desktop::catalog::{
    RelativeAge, SessionCatalog, SessionSort, SessionSummary,
};

/// Builds a listed session with optional title and product metadata.
fn info(
    id: &str,
    cwd: &str,
    title: Option<&str>,
    meta: serde_json::Value,
) -> wire::SessionInfo {
    let info = wire::SessionInfo::new(id.to_string(), PathBuf::from(cwd))
        .meta(wire::Meta::from_iter([("clawcode".to_string(), meta)]));
    match title {
        Some(title) => info.title(title.to_string()),
        None => info,
    }
}

/// Builds a top-level summary directly.
fn summary(
    id: &str,
    cwd: &str,
    title: &str,
    modified: Option<u64>,
) -> SessionSummary {
    SessionSummary::builder()
        .session_id(wire::SessionId::from(id.to_string()))
        .cwd(PathBuf::from(cwd))
        .title(Some(title.to_string()))
        .modified_at_ms(modified)
        .build()
}

/// Returns the ids of a query result in order.
fn ids(sessions: Vec<&SessionSummary>) -> Vec<String> {
    sessions
        .iter()
        .map(|session| session.session_id.to_string())
        .collect()
}

#[test]
fn summary_reads_product_meta() {
    let listed = info(
        "s1",
        "/w/a/",
        Some("Refactor"),
        serde_json::json!({
            "createdAtMs": "10",
            "modifiedAtMs": "20",
            "ownership": { "sessionId": "p", "name": "task_1", "nickname": "Chandrasekhar" }
        }),
    );

    let summary = SessionSummary::from(&listed);

    assert_eq!(summary.session_id.to_string(), "s1");
    assert_eq!(summary.cwd, PathBuf::from("/w/a"));
    assert_eq!(summary.title.as_deref(), Some("Refactor"));
    assert_eq!(summary.created_at_ms, Some(10));
    assert_eq!(summary.modified_at_ms, Some(20));
    // A spawned agent is owned by the session that spawned it.
    assert_eq!(
        summary.parent_session_id.map(|id| id.to_string()),
        Some("p".to_string())
    );
}

#[test]
fn forks_are_user_sessions_not_subagents() {
    // `parentSessionId` records where a fork came from; the fork itself is the user's session.
    let fork = info(
        "f1",
        "/w/a",
        Some("Fork"),
        serde_json::json!({ "parentSessionId": "p" }),
    );
    let summary = SessionSummary::from(&fork);
    assert_eq!(summary.parent_session_id, None);
    let catalog: SessionCatalog = [summary].into_iter().collect();
    assert_eq!(ids(catalog.recent(10, SessionSort::Modified)), ["f1"]);
}

#[test]
fn blank_title_and_malformed_meta_fall_back() {
    let listed = info(
        "s1",
        "/w/a",
        Some("   "),
        serde_json::json!({ "modifiedAtMs": "abc", "createdAtMs": 5 }),
    );

    let summary = SessionSummary::from(&listed);

    assert_eq!(summary.title, None);
    assert_eq!(summary.modified_at_ms, None);
    assert_eq!(summary.created_at_ms, None);
    assert_eq!(summary.parent_session_id, None);
}

#[test]
fn workspace_sessions_exclude_children_and_sort_by_recency() {
    let mut child = summary("c", "/w", "child", Some(99));
    child.parent_session_id = Some(wire::SessionId::from("s2".to_string()));
    let catalog: SessionCatalog = [
        summary("s1", "/w", "one", Some(5)),
        summary("s3", "/w", "three", None),
        summary("s2", "/w", "two", Some(9)),
        child,
        summary("other", "/x", "elsewhere", Some(50)),
    ]
    .into_iter()
    .collect();

    assert_eq!(
        ids(catalog.for_workspace(Path::new("/w/"), SessionSort::Modified)),
        ["s2", "s1", "s3"]
    );
}

#[test]
fn recent_spans_workspaces_and_respects_limit() {
    let catalog: SessionCatalog = (0..12)
        .map(|n| {
            summary(
                &format!("s{n:02}"),
                if n % 2 == 0 { "/a" } else { "/b" },
                "t",
                Some(n),
            )
        })
        .collect();

    let recent = ids(catalog.recent(10, SessionSort::Modified));

    assert_eq!(recent.len(), 10);
    assert_eq!(recent[0], "s11");
    assert_eq!(recent[9], "s02");
}

#[test]
fn search_matches_title_and_path_case_insensitively() {
    let catalog: SessionCatalog = [
        summary("k", "/x/other", "refactor kernel", Some(2)),
        summary("c", "/x/clawcode", "unrelated", Some(1)),
        summary("n", "/x/none", "nothing", Some(3)),
    ]
    .into_iter()
    .collect();

    assert_eq!(ids(catalog.search("KERNEL")), ["k"]);
    assert_eq!(ids(catalog.search("clawcode")), ["c"]);
    assert_eq!(ids(catalog.search("   ")), ["n", "k", "c"]);
}

#[test]
fn cwds_are_distinct_and_most_recent_first() {
    let catalog: SessionCatalog = [
        summary("1", "/a", "t", Some(1)),
        summary("2", "/b", "t", Some(5)),
        summary("3", "/a/", "t", Some(9)),
    ]
    .into_iter()
    .collect();

    assert_eq!(catalog.cwds(), [Path::new("/a"), Path::new("/b")]);
}

#[test]
fn upsert_replaces_and_remove_drops() {
    let mut catalog: SessionCatalog =
        [summary("1", "/a", "old", Some(1))].into_iter().collect();
    let id = wire::SessionId::from("1".to_string());

    catalog.upsert(summary("1", "/a", "new", Some(2)));
    assert_eq!(
        catalog
            .get(&id)
            .and_then(|session| session.title.as_deref()),
        Some("new")
    );

    catalog.remove(&id);
    assert!(catalog.get(&id).is_none());
}

#[test]
fn relative_age_buckets() {
    use RelativeAge::*;
    let minute = 60_000;
    let hour = 60 * minute;
    let day = 24 * hour;
    let base = 1_000_000_000;
    for (delta, expected) in [
        (0, Now),
        (minute - 1, Now),
        (minute, Minutes(1)),
        (hour - 1, Minutes(59)),
        (hour, Hours(1)),
        (day, Days(1)),
        (6 * day, Days(6)),
        (7 * day, Weeks(1)),
    ] {
        assert_eq!(
            RelativeAge::between(base + delta, base),
            expected,
            "delta {delta}"
        );
    }
    assert_eq!(RelativeAge::between(5, 10), Now);
}

#[test]
fn sort_by_created_orders_and_puts_unknown_last() {
    let created = |id: &str, at: Option<u64>| {
        SessionSummary::builder()
            .session_id(wire::SessionId::from(id.to_string()))
            .cwd(PathBuf::from("/w"))
            .created_at_ms(at)
            .modified_at_ms(Some(1))
            .build()
    };
    let catalog: SessionCatalog = [
        created("c", Some(5)),
        created("e", None),
        created("d", Some(9)),
    ]
    .into_iter()
    .collect();

    assert_eq!(
        ids(catalog.for_workspace(Path::new("/w"), SessionSort::Created)),
        ["d", "c", "e"]
    );
}

#[test]
fn now_ms_reads_the_wall_clock() {
    let expected = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_millis();
    let now = u128::from(desktop::now_ms());
    assert!(now.abs_diff(expected) < 60_000, "{now} vs {expected}");
}

#[test]
fn children_and_listed_ancestor() {
    let session_id = |id: &str| wire::SessionId::from(id.to_string());
    let child = |id: &str, parent: &str, modified: u64| {
        let mut summary = summary(id, "/w", id, Some(modified));
        summary.parent_session_id = Some(session_id(parent));
        summary
    };
    let mut catalog: SessionCatalog = [
        summary("a", "/w", "a", Some(1)),
        child("b", "a", 5),
        child("b2", "a", 9),
        child("c", "b", 2),
        child("orphan", "gone", 3),
        child("x", "y", 4),
        child("y", "x", 4),
    ]
    .into_iter()
    .collect();
    assert_eq!(ids(catalog.children(&session_id("a"))), ["b2", "b"]);
    assert_eq!(
        catalog.listed_ancestor(&session_id("c")),
        Some(session_id("a"))
    );
    assert_eq!(
        catalog.listed_ancestor(&session_id("a")),
        Some(session_id("a"))
    );
    assert_eq!(catalog.listed_ancestor(&session_id("orphan")), None);
    // A cycle ends without an ancestor instead of looping forever.
    assert_eq!(catalog.listed_ancestor(&session_id("x")), None);
    catalog.remove(&session_id("a"));
    assert_eq!(catalog.listed_ancestor(&session_id("c")), None);
}

#[test]
fn a_session_changed_since_a_run_started() {
    let session = summary("c", "/w", "c", Some(500));
    assert!(session.changed_since(Some(400)));
    assert!(session.changed_since(Some(500)));
    assert!(!session.changed_since(Some(600)));
    assert!(!session.changed_since(None));
    assert!(!summary("d", "/w", "d", None).changed_since(Some(1)));
}
