//! Trajectory pages over the history database: the active branch, page
//! boundaries, request numbering, and record assembly.

mod support;

use protocol::{
    ContentBlock, ContentHash, EntryId, ExtensionId, ExtensionMessage, LaneId,
    MessageContent, RequestOptions, RequestSnapshot, ThinkingLevel,
    TrajectoryCursor, TrajectoryPage, TrajectoryRecord, TrajectoryRecordKind,
};
use support::{AssistantFixture, Fixture, SessionLog};

/// A snapshot whose prompt and tools hash the given texts.
fn snapshot(prompt: &str, tools: &str) -> RequestSnapshot {
    RequestSnapshot::builder()
        .system_prompt(ContentHash::of(prompt.as_bytes()))
        .tools(ContentHash::of(tools.as_bytes()))
        .options(
            RequestOptions::builder()
                .provider_id("deepseek".into())
                .model_id("deepseek-flash".into())
                .thinking_level(ThinkingLevel::Off)
                .build(),
        )
        .message_count(1)
        .build()
}

/// An assistant attempt carrying the default snapshot.
fn request() -> AssistantFixture {
    AssistantFixture::builder()
        .request(snapshot("p", "[]"))
        .build()
}

/// Syncs the log, projects it, and reads one page.
async fn page(
    fixture: &Fixture,
    log: &mut SessionLog,
    cursor: TrajectoryCursor,
    limit: Option<u32>,
) -> TrajectoryPage {
    log.sync();
    fixture
        .runtime
        .refresh(&log.id(), &log.path())
        .await
        .expect("refresh");
    fixture
        .runtime
        .trajectory(&log.id(), cursor, limit)
        .await
        .expect("trajectory")
        .expect("known session")
}

/// The record ids of a page.
fn ids(page: &TrajectoryPage) -> Vec<&str> {
    page.records
        .iter()
        .map(|record| record.id.as_str())
        .collect()
}

/// Moves the `main` lane back to `entry`, so later appends branch from it.
fn branch_from(log: &mut SessionLog, entry: &EntryId) {
    log.store
        .move_lane(
            &LaneId::try_from("main").expect("lane"),
            Some(entry.clone()),
        )
        .expect("move lane");
}

/// The request records of a page.
fn requests(
    page: &TrajectoryPage,
) -> Vec<(&TrajectoryRecord, &protocol::RequestRecord)> {
    page.records
        .iter()
        .filter_map(|record| match &record.kind {
            TrajectoryRecordKind::Request(request) => Some((record, request)),
            _ => None,
        })
        .collect()
}

/// Unknown session has no trajectory.
#[tokio::test]
async fn unknown_session_has_no_trajectory() {
    let fixture = Fixture::new().await;
    let missing = protocol::SessionId::try_from("missing").expect("id");
    let page = fixture
        .runtime
        .trajectory(&missing, TrajectoryCursor::Tail, None)
        .await
        .expect("trajectory");
    assert!(page.is_none());
}

/// Tail page follows the active branch.
#[tokio::test]
async fn tail_page_follows_the_active_branch() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-a"));
    let entries: Vec<EntryId> =
        (1..=5).map(|n| log.user(&format!("message {n}"))).collect();
    branch_from(&mut log, &entries[1]);
    let side = log.user("side");

    let page = page(&fixture, &mut log, TrajectoryCursor::Tail, None).await;

    assert_eq!(
        ids(&page),
        [entries[0].as_str(), entries[1].as_str(), side.as_str()]
    );
    assert_eq!(page.leaf_id.as_ref(), Some(&side));
    assert!(!page.has_more_before);
}

/// Requests are numbered across the branch.
#[tokio::test]
async fn requests_are_numbered_across_the_branch() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-a"));
    log.run_started("run-1", "agent");
    log.user("first");
    log.assistant_with(request());
    log.assistant_with(request());
    log.run_finished("run-1", "completed", None);
    log.run_started("run-2", "agent");
    log.user("second");
    log.assistant_with(request());
    log.run_finished("run-2", "completed", None);

    let page = page(&fixture, &mut log, TrajectoryCursor::Tail, None).await;
    let numbered: Vec<(u32, u32)> = requests(&page)
        .iter()
        .map(|(record, request)| {
            (request.index, record.run.as_ref().expect("run").index)
        })
        .collect();

    assert_eq!(numbered, [(1, 1), (2, 1), (3, 2)]);
}

/// Page never starts with a tool result.
#[tokio::test]
async fn page_never_starts_with_a_tool_result() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-a"));
    let calling = log.assistant_with(
        AssistantFixture::builder()
            .request(snapshot("p", "[]"))
            .tool_calls(vec![("c1", "read"), ("c2", "grep")])
            .build(),
    );
    log.tool_result("c1");
    log.tool_result("c2");
    log.assistant_with(request());

    let page = page(&fixture, &mut log, TrajectoryCursor::Tail, Some(2)).await;

    assert_eq!(page.records[0].id, format!("request:{calling}"));
    let tools: Vec<_> = page
        .records
        .iter()
        .filter_map(|record| match &record.kind {
            TrajectoryRecordKind::Tool(tool) => Some(tool),
            _ => None,
        })
        .collect();
    assert_eq!(tools.len(), 2);
    assert!(tools.iter().all(|tool| tool.result_text.is_some()));
    assert!(!page.has_more_before);
}

/// Before and after cursors page the branch.
#[tokio::test]
async fn before_and_after_cursors_page_the_branch() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-a"));
    let mut all: Vec<EntryId> = (1..=12)
        .map(|n| log.user(&format!("message {n}")))
        .collect();

    let tail = page(&fixture, &mut log, TrajectoryCursor::Tail, Some(5)).await;
    let first = tail.first_sequence.expect("first");
    let middle =
        page(&fixture, &mut log, TrajectoryCursor::Before(first), Some(5))
            .await;
    let first = middle.first_sequence.expect("first");
    let head =
        page(&fixture, &mut log, TrajectoryCursor::Before(first), Some(5))
            .await;

    assert!(tail.has_more_before && middle.has_more_before);
    assert!(!head.has_more_before);
    let joined: Vec<&str> =
        [&head, &middle, &tail].into_iter().flat_map(ids).collect();
    let expected: Vec<&str> = all.iter().map(EntryId::as_str).collect();
    assert_eq!(joined, expected);

    let last = tail.last_sequence.expect("last");
    all.push(log.user("thirteen"));
    all.push(log.user("fourteen"));
    let newer =
        page(&fixture, &mut log, TrajectoryCursor::After(last), Some(5)).await;
    assert_eq!(ids(&newer), [all[12].as_str(), all[13].as_str()]);
    assert!(!newer.has_more_after);

    let capped =
        page(&fixture, &mut log, TrajectoryCursor::After(last), Some(1)).await;
    assert_eq!(ids(&capped), [all[12].as_str()]);
    assert!(capped.has_more_after);
}

/// After query off the branch returns the new tail.
#[tokio::test]
async fn after_query_off_the_branch_returns_the_new_tail() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-a"));
    let root = log.user("one");
    log.user("two");
    log.user("three");
    let before = page(&fixture, &mut log, TrajectoryCursor::Tail, None).await;

    branch_from(&mut log, &root);
    let side = log.user("side");
    let last = before.last_sequence.expect("last");
    let after =
        page(&fixture, &mut log, TrajectoryCursor::After(last), None).await;

    assert_eq!(after.leaf_id.as_ref(), Some(&side));
    assert_ne!(after.leaf_id, before.leaf_id);
    assert!(after.branch_changed);
    assert_eq!(ids(&after), [root.as_str(), side.as_str()]);
}

/// Appending moves the leaf on the same branch.
#[tokio::test]
async fn appending_moves_the_leaf_on_the_same_branch() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-a"));
    log.user("one");
    let before = page(&fixture, &mut log, TrajectoryCursor::Tail, None).await;
    let next = log.user("two");
    let last = before.last_sequence.expect("last");
    let after =
        page(&fixture, &mut log, TrajectoryCursor::After(last), None).await;

    assert_eq!(after.leaf_id.as_ref(), Some(&next));
    assert!(!after.branch_changed);
    assert_eq!(ids(&after), [next.as_str()]);
    assert_eq!(after.usage_before, protocol::ModelUsage::default());
}

/// Page counts images without their data.
#[tokio::test]
async fn page_counts_images_without_their_data() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-a"));
    let data = "A".repeat(100 * 1024);
    log.message(MessageContent::User {
        blocks: vec![
            ContentBlock::Text {
                text: "look".into(),
            },
            ContentBlock::Image {
                data: data.clone(),
                mime_type: "image/png".into(),
            },
        ],
    });

    let page = page(&fixture, &mut log, TrajectoryCursor::Tail, None).await;

    let TrajectoryRecordKind::User(user) = &page.records[0].kind else {
        panic!("user record expected");
    };
    assert_eq!(user.text, "look");
    assert_eq!(user.image_count, 1);
    assert!(!serde_json::to_string(&page).expect("json").contains(&data));
}

/// Assistant record carries timing and usage.
#[tokio::test]
async fn assistant_record_carries_timing_and_usage() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-a"));
    log.assistant_with(
        AssistantFixture::builder()
            .request(snapshot("p", "[]"))
            .timing((1000, 1800, 2600))
            .input(40)
            .output(7)
            .cache_read(3)
            .build(),
    );

    let page = page(&fixture, &mut log, TrajectoryCursor::Tail, None).await;
    let TrajectoryRecordKind::Assistant(assistant) = &page.records[1].kind
    else {
        panic!("assistant record expected");
    };

    assert_eq!(assistant.request_index, 1);
    assert_eq!(assistant.requested_at_ms.get(), 1000);
    assert_eq!(assistant.first_token_at_ms.get(), 1800);
    assert_eq!(assistant.last_token_at_ms.get(), 2600);
    assert_eq!(assistant.usage.as_ref().output_tokens, Some(7));
    assert_eq!(assistant.usage.as_ref().cached_input_tokens, Some(3));
    assert_eq!(
        page.records[1]
            .started_at_ms
            .map(protocol::TimestampMs::get),
        Some(1000)
    );
    assert_eq!(
        page.records[1].ended_at_ms.map(protocol::TimestampMs::get),
        Some(2600)
    );
}

/// Entries outside runs have no run.
#[tokio::test]
async fn entries_outside_runs_have_no_run() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-a"));
    log.message(MessageContent::Extension {
        extension: ExtensionMessage {
            extension_id: ExtensionId::try_from("notes").expect("id"),
            custom_type: "note".into(),
            blocks: vec![ContentBlock::Text {
                text: "remember".into(),
            }],
            display: true,
            include_in_context: false,
            details: None,
        },
    });
    log.run_started("run-1", "agent");
    log.user("hello");

    let page = page(&fixture, &mut log, TrajectoryCursor::Tail, None).await;

    let TrajectoryRecordKind::Extension(extension) = &page.records[0].kind
    else {
        panic!("extension record expected");
    };
    assert_eq!(extension.text, "remember");
    assert_eq!(page.records[0].run, None);
    assert_eq!(page.records[1].run.as_ref().map(|run| run.index), Some(1));
}

/// Tool definitions JSON for `names`, as `ModelRequest::capture` writes it.
fn tools(names: &[&str]) -> String {
    let definitions: Vec<protocol::ToolDefinition> = names
        .iter()
        .map(|name| protocol::ToolDefinition {
            name: (*name).into(),
            description: String::new(),
            parameters: serde_json::json!({}),
        })
        .collect();
    serde_json::to_string(&definitions).expect("tools json")
}

/// Writes one request: its contents (once per hash) and the assistant entry.
fn request_with(log: &mut SessionLog, prompt: &str, tools: &str) {
    for (kind, content) in [
        (protocol::RequestContentKind::SystemPrompt, prompt),
        (protocol::RequestContentKind::Tools, tools),
    ] {
        log.request_content(
            "run-1",
            &protocol::RequestContent {
                hash: ContentHash::of(content.as_bytes()),
                kind,
                content: content.into(),
            },
        );
    }
    log.assistant_with(
        AssistantFixture::builder()
            .request(snapshot(prompt, tools))
            .build(),
    );
}

/// The three requests of the comparison tests.
fn three_requests(log: &mut SessionLog) {
    log.run_started("run-1", "agent");
    log.user("go");
    request_with(log, "prompt one", &tools(&["read", "write"]));
    request_with(log, "prompt two", &tools(&["read", "write"]));
    request_with(log, "prompt two", &tools(&["read", "grep"]));
}

/// `(first, prompt_changed, added, removed)` of each request record.
fn changes(
    page: &TrajectoryPage,
) -> Vec<(bool, bool, Vec<String>, Vec<String>)> {
    requests(page)
        .into_iter()
        .map(|(_, request)| {
            (
                request.first,
                request.prompt_changed,
                request.tools_added.clone(),
                request.tools_removed.clone(),
            )
        })
        .collect()
}

/// Prompt and tool changes are reported.
#[tokio::test]
async fn prompt_and_tool_changes_are_reported() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-a"));
    three_requests(&mut log);

    let page = page(&fixture, &mut log, TrajectoryCursor::Tail, None).await;

    assert_eq!(
        changes(&page),
        [
            (true, false, vec![], vec![]),
            (false, true, vec![], vec![]),
            (
                false,
                false,
                vec!["grep".to_string()],
                vec!["write".to_string()]
            ),
        ]
    );
}

/// Comparison crosses the page boundary.
#[tokio::test]
async fn comparison_crosses_the_page_boundary() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-a"));
    three_requests(&mut log);

    let page = page(&fixture, &mut log, TrajectoryCursor::Tail, Some(1)).await;

    assert_eq!(
        changes(&page),
        [(
            false,
            false,
            vec!["grep".to_string()],
            vec!["write".to_string()]
        )]
    );
}

/// Queued messages are marked.
#[tokio::test]
async fn queued_messages_are_marked() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-a"));
    log.run_started("run-1", "agent");
    log.user("typed");
    let steering = log.user("steer");
    log.queue_enqueued("run-1", &steering, "steering");
    let follow_up = log.user("later");
    log.queue_enqueued("run-1", &follow_up, "followUp");

    let page = page(&fixture, &mut log, TrajectoryCursor::Tail, None).await;
    let origins: Vec<protocol::UserOrigin> = page
        .records
        .iter()
        .filter_map(|record| match &record.kind {
            TrajectoryRecordKind::User(user) => Some(user.origin),
            _ => None,
        })
        .collect();

    assert_eq!(
        origins,
        [
            protocol::UserOrigin::Input,
            protocol::UserOrigin::Steering,
            protocol::UserOrigin::FollowUp
        ]
    );
}

/// Failed attempt and retry are two requests.
#[tokio::test]
async fn failed_attempt_and_retry_are_two_requests() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-a"));
    log.run_started("run-1", "agent");
    log.user("go");
    log.assistant_with(
        AssistantFixture::builder()
            .request(snapshot("p", "[]"))
            .error("service unavailable")
            .build(),
    );
    log.assistant_with(request());

    let page = page(&fixture, &mut log, TrajectoryCursor::Tail, None).await;
    let assistants: Vec<&protocol::AssistantRecord> = page
        .records
        .iter()
        .filter_map(|record| match &record.kind {
            TrajectoryRecordKind::Assistant(assistant) => Some(assistant),
            _ => None,
        })
        .collect();

    assert_eq!(
        requests(&page)
            .iter()
            .map(|(_, r)| r.index)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert!(assistants[0].error.is_some());
    assert!(assistants[1].error.is_none());
    assert_eq!(changes(&page)[1], (false, false, vec![], vec![]));
}

/// Usage before sums earlier requests.
#[tokio::test]
async fn usage_before_sums_earlier_requests() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-a"));
    for _ in 0..5 {
        log.assistant_with(
            AssistantFixture::builder()
                .request(snapshot("p", "[]"))
                .input(100)
                .output(10)
                .build(),
        );
    }

    let page = page(&fixture, &mut log, TrajectoryCursor::Tail, Some(2)).await;

    assert_eq!(requests(&page)[0].1.index, 4);
    let usage = page.usage_before.as_ref();
    assert_eq!(usage.input_tokens, Some(300));
    assert_eq!(usage.output_tokens, Some(30));
}

/// Request content reads by hash.
#[tokio::test]
async fn request_content_reads_by_hash() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-a"));
    log.run_started("run-1", "agent");
    request_with(&mut log, "the prompt", "[]");
    page(&fixture, &mut log, TrajectoryCursor::Tail, None).await;

    let found = fixture
        .runtime
        .request_content(&log.id(), &ContentHash::of(b"the prompt"))
        .await
        .expect("read")
        .expect("indexed")
        .expect("content");
    assert_eq!(found.kind, protocol::RequestContentKind::SystemPrompt);
    assert_eq!(found.content, "the prompt");
    let missing = fixture
        .runtime
        .request_content(&log.id(), &ContentHash::of(b"other"))
        .await
        .expect("read")
        .expect("indexed");
    assert!(missing.is_none());
}

/// Compaction and settings become records.
#[tokio::test]
async fn compaction_and_settings_become_records() {
    let fixture = Fixture::new().await;
    let mut log = fixture.session(fixture.options("session-a"));
    log.user("before");
    let compaction = protocol::CompactionData::builder()
        .summary("the story so far".into())
        .retained_tail(Vec::new())
        .tokens_before(1_200)
        .details(Some(
            protocol::CompactionDetails::builder()
                .reason(protocol::CompactionReason::Manual)
                .run_id(protocol::RunId::try_from("run-c").expect("run"))
                .turn_id(protocol::TurnId::try_from("turn-c").expect("turn"))
                .started_at_ms(protocol::TimestampMs::from(1))
                .ended_at_ms(protocol::TimestampMs::from(2))
                .build(),
        ))
        .build();
    log.entry(
        store::EntryKind::Compaction,
        serde_json::to_value(compaction).expect("compaction"),
    );
    log.model_change("deepseek", "deepseek-pro");
    log.thinking("high");

    let page = page(&fixture, &mut log, TrajectoryCursor::Tail, None).await;
    let kinds: Vec<&TrajectoryRecordKind> = page
        .records
        .iter()
        .skip(1)
        .map(|record| &record.kind)
        .collect();

    assert_eq!(
        kinds,
        [
            &TrajectoryRecordKind::Compaction {
                summary: "the story so far".into(),
                tokens_before: Some(1_200),
            },
            &TrajectoryRecordKind::Setting(protocol::SettingChange::Model {
                provider: "deepseek".into(),
                model: "deepseek-pro".into(),
            }),
            &TrajectoryRecordKind::Setting(
                protocol::SettingChange::ThinkingLevel {
                    level: ThinkingLevel::High,
                }
            ),
        ]
    );
}
