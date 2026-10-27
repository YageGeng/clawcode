use protocol::{ContentBlock, ToolCallId, ToolOutputPolicy, ToolResult};
use tokio_util::sync::CancellationToken;
use tools::{ToolContentText, ToolResultProjection};

/// Builds a normalized result without introducing transport metadata.
fn result(blocks: Vec<ContentBlock>, is_error: bool) -> ToolResult {
    ToolResult::builder()
        .tool_call_id(ToolCallId::try_from("output").expect("id"))
        .blocks(blocks)
        .is_error(is_error)
        .build()
}

/// Counts the actual shared Provider projection rather than trusting diagnostics.
fn visible_bytes(result: &ToolResult) -> usize {
    result
        .blocks
        .iter()
        .filter_map(|block| ToolContentText::try_from(block).ok())
        .map(|view| {
            let mut text = Vec::new();
            view.write_to(&mut text).expect("projection");
            String::from_utf8(text).expect("valid UTF-8")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .len()
}

/// Aggregate text includes structured JSON and preserves a complete normalized file.
#[test]
fn aggregate_budget_preserves_original_and_utf8() {
    let directory = tempfile::tempdir().expect("directory");
    let mut output = result(
        vec![
            ContentBlock::Text {
                text: "中文".repeat(5000),
            },
            ContentBlock::Structured {
                value: serde_json::json!({"tail":"last"}),
            },
        ],
        false,
    );
    let original = output.clone();
    ToolResultProjection::new(
        &mut output,
        ToolOutputPolicy::Bounded,
        &CancellationToken::new(),
        directory.path(),
    )
    .apply()
    .expect("projection");
    assert!(visible_bytes(&output) <= 20 * 1024);
    assert_eq!(
        visible_bytes(&output),
        output
            .output
            .as_ref()
            .expect("diagnostic")
            .visible_text_bytes
    );
    let path = output
        .output
        .as_ref()
        .and_then(|info| info.full_output_path.as_ref())
        .expect("complete output");
    let saved: ToolResult =
        serde_json::from_slice(&std::fs::read(path).expect("file"))
            .expect("normalized JSON");
    assert_eq!(saved, original);
    assert!(!output.is_error);
}

/// Whole media blocks obey both budgets and small structured results remain structured.
#[test]
fn media_is_whole_and_small_results_need_no_file() {
    let directory = tempfile::tempdir().expect("directory");
    let token = CancellationToken::new();
    let mut small = result(
        vec![ContentBlock::Structured {
            value: serde_json::json!({"ok":true}),
        }],
        false,
    );
    let original = small.clone();
    ToolResultProjection::new(
        &mut small,
        ToolOutputPolicy::Bounded,
        &token,
        directory.path(),
    )
    .apply()
    .expect("small");
    assert_eq!(small, original);
    for length in [20 * 1024, 20 * 1024 + 1] {
        let mut boundary = result(
            vec![ContentBlock::Text {
                text: "x".repeat(length),
            }],
            false,
        );
        ToolResultProjection::new(
            &mut boundary,
            ToolOutputPolicy::Bounded,
            &token,
            directory.path(),
        )
        .apply()
        .expect("text boundary");
        assert_eq!(boundary.output.is_some(), length > 20 * 1024);
        assert!(visible_bytes(&boundary) <= 20 * 1024);
    }
    let mut media = result(
        (0..6)
            .map(|_| ContentBlock::Image {
                data: "x".repeat(3 * 1024 * 1024),
                mime_type: "image/png".into(),
            })
            .collect(),
        false,
    );
    ToolResultProjection::new(
        &mut media,
        ToolOutputPolicy::Bounded,
        &token,
        directory.path(),
    )
    .apply()
    .expect("media");
    let images: Vec<_> = media
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Image { data, .. } => Some(data),
            _ => None,
        })
        .collect();
    assert_eq!(images.len(), 2);
    assert!(images.iter().all(|data| data.len() == 3 * 1024 * 1024));
    assert_eq!(media.output.as_ref().expect("diagnostic").omitted_media, 4);
    assert!(visible_bytes(&media) <= 20 * 1024);

    let mut mixed = result(
        vec![
            ContentBlock::Audio {
                data: "encoded".into(),
                mime_type: "audio/wav".into(),
            },
            ContentBlock::Text {
                text: "x".repeat(30_000),
            },
        ],
        false,
    );
    ToolResultProjection::new(
        &mut mixed,
        ToolOutputPolicy::Bounded,
        &token,
        directory.path(),
    )
    .apply()
    .expect("mixed media");
    assert!(
        matches!(mixed.blocks.first(), Some(ContentBlock::Audio { data, .. }) if data == "encoded")
    );
    assert_eq!(mixed.output.as_ref().expect("info").omitted_media, 0);
    assert!(visible_bytes(&mixed) <= 20 * 1024);
}

/// File errors and cancellation never turn a completed remote operation into a retry.
#[test]
fn spool_failure_preserves_remote_status() {
    let directory = tempfile::tempdir().expect("directory");
    for is_error in [true, false] {
        let mut output = result(
            vec![ContentBlock::Text {
                text: "x".repeat(30_000),
            }],
            is_error,
        );
        ToolResultProjection::new(
            &mut output,
            ToolOutputPolicy::Bounded,
            &CancellationToken::new(),
            &directory.path().join("missing"),
        )
        .apply()
        .expect("bounded despite IO failure");
        assert_eq!(output.is_error, is_error);
        assert_eq!(
            output.output.as_ref().expect("diagnostic").save_failure,
            Some(protocol::ToolOutputSaveFailure::Io)
        );
        assert!(visible_bytes(&output) <= 20 * 1024);
    }
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let mut output = result(
        vec![ContentBlock::Text {
            text: "x".repeat(30_000),
        }],
        false,
    );
    ToolResultProjection::new(
        &mut output,
        ToolOutputPolicy::Bounded,
        &cancellation,
        directory.path(),
    )
    .apply()
    .expect("cancelled save");
    assert_eq!(
        output.output.as_ref().expect("diagnostic").save_failure,
        Some(protocol::ToolOutputSaveFailure::Cancelled)
    );
    assert_eq!(
        std::fs::read_dir(directory.path())
            .expect("directory")
            .count(),
        0
    );
}

/// A complete file is never published when serialized bytes exceed the spool cap.
#[test]
fn oversized_spool_is_not_published() {
    let directory = tempfile::tempdir().expect("directory");
    let mut output = result(
        vec![ContentBlock::Text {
            text: "x".repeat(64 * 1024 * 1024),
        }],
        false,
    );
    ToolResultProjection::new(
        &mut output,
        ToolOutputPolicy::Bounded,
        &CancellationToken::new(),
        directory.path(),
    )
    .apply()
    .expect("bounded");
    let info = output.output.expect("diagnostic");
    assert!(info.full_output_path.is_none());
    assert_eq!(
        info.save_failure,
        Some(protocol::ToolOutputSaveFailure::TooLarge)
    );
    assert_eq!(
        std::fs::read_dir(directory.path())
            .expect("directory")
            .count(),
        0
    );
}

/// Hook-expanded pages fail as complete bounded errors instead of exposing a broken cursor.
#[test]
fn resource_pages_remain_valid_after_hooks() {
    let directory = tempfile::tempdir().expect("directory");
    let token = CancellationToken::new();
    for text in [
        "not JSON".to_string(),
        serde_json::json!({"resources":[],"nextCursor":"cursor"}).to_string(),
        "x".repeat(21 * 1024),
    ] {
        let mut output = result(vec![ContentBlock::Text { text }], false);
        ToolResultProjection::new(
            &mut output,
            ToolOutputPolicy::ResourcePage(
                protocol::McpResourcePageKind::Resources,
            ),
            &token,
            directory.path(),
        )
        .apply()
        .expect("page validation");
        assert!(output.is_error);
        assert!(visible_bytes(&output) <= 20 * 1024);
        assert!(
            !output
                .blocks
                .first()
                .and_then(ContentBlock::text)
                .expect("error")
                .contains("nextCursor")
        );
    }
}
