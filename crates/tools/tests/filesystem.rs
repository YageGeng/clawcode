use std::path::Path;
use std::sync::Arc;

use base64::Engine as _;
use image::{ImageBuffer, ImageFormat, Rgba};
use protocol::{
    ContentBlock, SessionId, ToolCall, ToolCallId, ToolResultDetails, TurnId,
};
use tokio_util::sync::CancellationToken;
use tools::{
    BuiltinToolFactory, DiscardToolUpdates, ToolExecutionContext, ToolFactory,
};

/// Builds one direct tool context rooted at a real temporary directory.
fn execution_context(cwd: &Path) -> ToolExecutionContext {
    ToolExecutionContext::builder()
        .session_id(SessionId::try_from("session-fs").expect("session id"))
        .turn_id(TurnId::try_from("turn-fs").expect("turn id"))
        .trace_id(protocol::TraceId::try_from("trace-fs").expect("trace id"))
        .cwd(cwd.to_path_buf())
        .cancellation(CancellationToken::new())
        .updates(Arc::new(DiscardToolUpdates))
        .build()
}

/// Builds one correlated call without hiding tool-specific argument JSON.
fn tool_call(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        tool_call_id: ToolCallId::try_from(id).expect("tool call id"),
        name: name.to_string(),
        arguments,
    }
}

/// Returns the default registry used by all filesystem behavior tests.
fn registry() -> tools::ToolRegistry {
    BuiltinToolFactory::new()
        .create()
        .expect("built-in registry")
}

/// Creates the same valid 1x1 24bpp BMP shape used by pi's regression tests.
fn tiny_bmp() -> Vec<u8> {
    let image = image::DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
        1,
        1,
        Rgba([255_u8, 0_u8, 0_u8, 255_u8]),
    ));
    let mut output = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut output, ImageFormat::Bmp)
        .expect("encode bmp fixture");
    output.into_inner()
}

/// Read exposes pi's number-based offset and limit schema.
#[test]
fn read_definition_matches_pi_schema() {
    let definition = registry()
        .definitions()
        .into_iter()
        .find(|definition| definition.name == "read")
        .expect("read definition");

    assert_eq!(
        definition.parameters["required"],
        serde_json::json!(["path"])
    );
    assert_eq!(
        definition.parameters["properties"]["offset"]["type"],
        "number"
    );
    assert_eq!(
        definition.parameters["properties"]["limit"]["type"],
        "number"
    );
}

/// Read applies 1-based pagination and emits pi's continuation notice.
#[tokio::test]
async fn read_supports_offset_and_limit() {
    let root = tempfile::tempdir().expect("temporary directory");
    let lines = (1..=100)
        .map(|line| format!("Line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(root.path().join("page.txt"), lines).expect("write fixture");

    let result = registry()
        .execute(
            tool_call(
                "read-page",
                "read",
                serde_json::json!({ "path": "@page.txt", "offset": 41, "limit": 20 }),
            ),
            &execution_context(root.path()),
        )
        .await
        .expect("read page");
    let text = result.blocks[0].text().expect("text result");

    assert!(text.starts_with("Line 41\n"));
    assert!(text.contains("Line 60"));
    assert!(!text.contains("Line 61"));
    assert!(
        text.ends_with("[40 more lines in file. Use offset=61 to continue.]")
    );
    assert_eq!(result.details, None);
}

/// Read returns typed details and a continuation offset after pi's 2000-line limit.
#[tokio::test]
async fn read_truncates_large_text_from_the_head() {
    let root = tempfile::tempdir().expect("temporary directory");
    let lines = (1..=2_500)
        .map(|line| format!("Line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(root.path().join("large.txt"), lines)
        .expect("write fixture");

    let result = registry()
        .execute(
            tool_call(
                "read-large",
                "read",
                serde_json::json!({ "path": "large.txt" }),
            ),
            &execution_context(root.path()),
        )
        .await
        .expect("read large file");
    let text = result.blocks[0].text().expect("text result");

    assert!(text.contains("Line 2000"));
    assert!(!text.contains("Line 2001"));
    assert!(text.ends_with(
        "[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]"
    ));
    let Some(ToolResultDetails::Read { truncation }) = result.details else {
        panic!("read truncation details");
    };
    assert_eq!(truncation.output_lines, 2_000);
}

/// Image detection uses file magic and returns a native image block.
#[tokio::test]
async fn read_detects_png_content_without_relying_on_extension() {
    let root = tempfile::tempdir().expect("temporary directory");
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAX+XDSwAAAABJRU5ErkJggg==")
        .expect("png fixture");
    std::fs::write(root.path().join("image.txt"), png).expect("write image");

    let result = registry()
        .execute(
            tool_call(
                "read-image",
                "read",
                serde_json::json!({ "path": "image.txt" }),
            ),
            &execution_context(root.path()),
        )
        .await
        .expect("read image");

    assert_eq!(result.blocks[0].text(), Some("Read image file [image/png]"));
    assert!(matches!(
        result.blocks.get(1),
        Some(ContentBlock::Image { mime_type, data })
            if mime_type == "image/png" && !data.is_empty()
    ));
}

/// BMP input is normalized to a provider-supported PNG attachment.
#[tokio::test]
async fn read_converts_bmp_to_png() {
    let root = tempfile::tempdir().expect("temporary directory");
    std::fs::write(root.path().join("image.bmp"), tiny_bmp())
        .expect("write bmp");

    let result = registry()
        .execute(
            tool_call(
                "read-bmp",
                "read",
                serde_json::json!({ "path": "image.bmp" }),
            ),
            &execution_context(root.path()),
        )
        .await
        .expect("read bmp");
    let text = result.blocks[0].text().expect("image note");
    let Some(ContentBlock::Image { mime_type, data }) = result.blocks.get(1)
    else {
        panic!("image block");
    };
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(data)
        .expect("decode png");

    assert_eq!(mime_type, "image/png");
    assert!(text.contains("Read image file [image/png]"));
    assert!(text.contains("[Image converted from image/bmp to image/png.]"));
    assert!(decoded.starts_with(&[0x89, b'P', b'N', b'G']));
}

/// A BM text prefix alone is not enough to classify a file as a valid BMP.
#[tokio::test]
async fn read_rejects_invalid_bmp_header_as_image() {
    let root = tempfile::tempdir().expect("temporary directory");
    std::fs::write(root.path().join("not-image.txt"), b"BM plain text")
        .expect("write text fixture");

    let result = registry()
        .execute(
            tool_call(
                "read-invalid-bmp",
                "read",
                serde_json::json!({ "path": "not-image.txt" }),
            ),
            &execution_context(root.path()),
        )
        .await
        .expect("read text");

    assert_eq!(result.blocks.len(), 1);
    assert_eq!(result.blocks[0].text(), Some("BM plain text"));
}

/// Images wider than pi's inline limit are resized and include coordinate guidance.
#[tokio::test]
async fn read_resizes_images_larger_than_2000_pixels() {
    let root = tempfile::tempdir().expect("temporary directory");
    let image =
        ImageBuffer::from_pixel(2_001, 1, Rgba([255_u8, 0_u8, 0_u8, 255_u8]));
    image
        .save_with_format(root.path().join("wide.png"), ImageFormat::Png)
        .expect("write wide png");

    let result = registry()
        .execute(
            tool_call(
                "read-wide",
                "read",
                serde_json::json!({ "path": "wide.png" }),
            ),
            &execution_context(root.path()),
        )
        .await
        .expect("read wide image");
    let text = result.blocks[0].text().expect("image note");

    assert!(text.contains("original 2001x1, displayed at 2000x1"));
    assert!(text.contains("Multiply coordinates by 1.00"));
}

/// Write creates missing parent directories and reports pi's success text.
#[tokio::test]
async fn write_creates_parent_directories() {
    let root = tempfile::tempdir().expect("temporary directory");
    let result = registry()
        .execute(
            tool_call(
                "write-nested",
                "write",
                serde_json::json!({
                    "path": "nested/dir/file.txt",
                    "content": "hello world!"
                }),
            ),
            &execution_context(root.path()),
        )
        .await
        .expect("write file");

    assert_eq!(
        std::fs::read_to_string(root.path().join("nested/dir/file.txt"))
            .expect("read written file"),
        "hello world!"
    );
    assert_eq!(
        result.blocks[0].text(),
        Some("Successfully wrote 12 bytes to nested/dir/file.txt")
    );
}

/// Edit applies disjoint replacements against one original snapshot and preserves CRLF/BOM.
#[tokio::test]
async fn edit_applies_multiple_blocks_and_preserves_encoding_markers() {
    let root = tempfile::tempdir().expect("temporary directory");
    std::fs::write(
        root.path().join("edit.txt"),
        "\u{feff}alpha\r\nbeta\r\ngamma\r\n",
    )
    .expect("write edit fixture");

    let result = registry()
        .execute(
            tool_call(
                "edit-multiple",
                "edit",
                serde_json::json!({
                    "path": "edit.txt",
                    "edits": [
                        { "oldText": "alpha\n", "newText": "ALPHA\n" },
                        { "oldText": "gamma\n", "newText": "GAMMA\n" }
                    ]
                }),
            ),
            &execution_context(root.path()),
        )
        .await
        .expect("edit file");

    assert_eq!(
        std::fs::read_to_string(root.path().join("edit.txt"))
            .expect("read edited file"),
        "\u{feff}ALPHA\r\nbeta\r\nGAMMA\r\n"
    );
    assert_eq!(
        result.blocks[0].text(),
        Some("Successfully replaced 2 block(s) in edit.txt.")
    );
    let Some(ToolResultDetails::Edit { diff, patch, .. }) = result.details
    else {
        panic!("edit details");
    };
    assert!(diff.contains("-1 alpha"));
    assert!(diff.contains("+1 ALPHA"));
    assert!(!diff.starts_with("--- "));
    assert!(patch.contains("-alpha"));
    assert!(patch.contains("+ALPHA"));
}

/// Fuzzy punctuation or whitespace matching preserves untouched original lines byte-for-byte.
#[tokio::test]
async fn edit_fuzzy_match_preserves_unchanged_lines() {
    let root = tempfile::tempdir().expect("temporary directory");
    let path = root.path().join("fuzzy.txt");
    std::fs::write(&path, "alpha   \nkeep   \nomega\n").expect("write fixture");

    registry()
        .execute(
            tool_call(
                "edit-fuzzy",
                "edit",
                serde_json::json!({
                    "path": "fuzzy.txt",
                    "edits": [{ "oldText": "alpha\n", "newText": "ALPHA\n" }]
                }),
            ),
            &execution_context(root.path()),
        )
        .await
        .expect("fuzzy edit");

    assert_eq!(
        std::fs::read_to_string(path).expect("read edited file"),
        "ALPHA\nkeep   \nomega\n"
    );
}

/// Legacy top-level replacements and JSON-string edits normalize into the public array shape.
#[tokio::test]
async fn edit_accepts_pi_legacy_and_stringified_arguments() {
    let root = tempfile::tempdir().expect("temporary directory");
    let legacy = root.path().join("legacy.txt");
    let stringified = root.path().join("stringified.txt");
    std::fs::write(&legacy, "before\n").expect("write legacy fixture");
    std::fs::write(&stringified, "left\n").expect("write string fixture");
    let registry = registry();
    let context = execution_context(root.path());

    registry
        .execute(
            tool_call(
                "edit-legacy",
                "edit",
                serde_json::json!({
                    "path": "legacy.txt",
                    "oldText": "before",
                    "newText": "after"
                }),
            ),
            &context,
        )
        .await
        .expect("legacy edit");
    registry
        .execute(
            tool_call(
                "edit-stringified",
                "edit",
                serde_json::json!({
                    "path": "stringified.txt",
                    "edits": "[{\"oldText\":\"left\",\"newText\":\"right\"}]"
                }),
            ),
            &context,
        )
        .await
        .expect("stringified edit");

    assert_eq!(
        std::fs::read_to_string(legacy).expect("legacy result"),
        "after\n"
    );
    assert_eq!(
        std::fs::read_to_string(stringified).expect("string result"),
        "right\n"
    );
}

/// Edit rejects duplicate targets without mutating the file.
#[tokio::test]
async fn edit_requires_each_old_text_to_be_unique() {
    let root = tempfile::tempdir().expect("temporary directory");
    let path = root.path().join("duplicate.txt");
    std::fs::write(&path, "foo foo foo").expect("write fixture");

    let error = registry()
        .execute(
            tool_call(
                "edit-duplicate",
                "edit",
                serde_json::json!({
                    "path": "duplicate.txt",
                    "edits": [{ "oldText": "foo", "newText": "bar" }]
                }),
            ),
            &execution_context(root.path()),
        )
        .await
        .expect_err("duplicate edit should fail");

    assert!(error.to_string().contains("Found 3 occurrences"));
    assert_eq!(
        std::fs::read_to_string(path).expect("read fixture"),
        "foo foo foo"
    );
}
