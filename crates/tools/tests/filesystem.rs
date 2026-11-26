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
    ToolCall::builder()
        .tool_call_id(ToolCallId::try_from(id).expect("tool call id"))
        .name(name.to_string())
        .arguments(arguments)
        .build()
}

/// Returns the default registry used by all filesystem behavior tests.
fn registry() -> tools::ToolRegistry {
    BuiltinToolFactory::new()
        .create()
        .expect("built-in registry")
}

/// Delivers the canonical path to a test client and waits for its one-shot decision.
struct ApprovalClient {
    requests: tokio::sync::mpsc::UnboundedSender<(
        std::path::PathBuf,
        tokio::sync::oneshot::Sender<bool>,
    )>,
}

#[async_trait::async_trait]
impl tools::FileMutationApprover for ApprovalClient {
    /// Holds execution before filesystem changes until the test client answers.
    async fn approve(
        &self,
        call: &ToolCall,
        target: &Path,
    ) -> Result<(), tools::ToolError> {
        let (answer, decision) = tokio::sync::oneshot::channel();
        self.requests
            .send((target.to_owned(), answer))
            .expect("report approval");
        if decision.await.unwrap_or(false) {
            Ok(())
        } else {
            Err(tools::ToolError::Execution {
                tool: call.name.clone(),
                message: "Denied by user".into(),
            })
        }
    }
}

/// External requests wait without creating parent directories or modifying existing bytes.
#[tokio::test]
async fn file_approval_blocks_external_writes_and_edits_before_side_effects() {
    let root = tempfile::tempdir().expect("temporary root");
    let workspace = root.path().join("work");
    std::fs::create_dir(&workspace).expect("create workspace");
    let outside = root.path().join("work-other");
    std::fs::create_dir(&outside).expect("create sibling");
    std::fs::write(outside.join("existing"), "before").expect("original file");
    let (requests, mut incoming) = tokio::sync::mpsc::unbounded_channel();
    let mut context = execution_context(&workspace);
    context.file_mutation_approver =
        Some(Arc::new(ApprovalClient { requests }));
    let registry = registry();
    // Prefix-sharing sibling directories are distinct workspace roots.
    for (tool, path, args) in [
        (
            "write",
            "new/file",
            serde_json::json!({"path": "../work-other/new/file", "content": "after"}),
        ),
        (
            "edit",
            "existing",
            serde_json::json!({"path": "../work-other/existing", "edits": [{"oldText": "before", "newText": "after"}]}),
        ),
    ] {
        let mut execution =
            Box::pin(registry.execute(tool_call("call", tool, args), &context));
        let (target, answer) = tokio::select! {
            result = &mut execution => panic!("external {tool} completed without approval: {result:?}"),
            request = incoming.recv() => request.expect("approval request"),
        };
        assert_eq!(
            target,
            outside
                .canonicalize()
                .expect("physical sibling path")
                .join(path)
        );
        assert!(!outside.join("new").exists());
        assert_eq!(
            std::fs::read(outside.join("existing")).expect("read original"),
            b"before"
        );
        answer.send(false).expect("reject");
        execution.await.expect_err("mutation must be blocked");
        assert!(!outside.join("new").exists());
        assert_eq!(
            std::fs::read(outside.join("existing")).expect("read original"),
            b"before"
        );
    }
    let inside = url::Url::from_file_path(workspace.join("new/file"))
        .expect("local file URL");
    let call = tool_call(
        "inside",
        "write",
        serde_json::json!({"path": inside.as_str(), "content":"ok"}),
    );
    tokio::select! {
        result = registry.execute(call, &context) => { result.expect("inside write"); }
        request = incoming.recv() => panic!("unexpected approval for workspace target: {request:?}"),
    }
    assert_eq!(
        std::fs::read(workspace.join("new/file")).expect("read inside write"),
        b"ok"
    );
    // A directory-only spelling must never create a regular file when the leaf is missing.
    let invalid = tool_call(
        "directory",
        "write",
        serde_json::json!({"path":"directory/", "content":"wrong"}),
    );
    registry
        .execute(invalid, &context)
        .await
        .expect_err("directory spelling");
    assert!(!workspace.join("directory").is_file());
}

/// Each guarded file save updates only its named workspace entry and preserves its mode.
#[tokio::test]
async fn file_approval_preserves_other_names_of_workspace_files() {
    let root = tempfile::tempdir().expect("project and backups");
    let workspace = root.path().join("work");
    let backups = root.path().join("backups");
    std::fs::create_dir(&workspace).expect("workspace");
    std::fs::create_dir(&backups).expect("backups");
    let (requests, incoming) = tokio::sync::mpsc::unbounded_channel();
    drop(incoming);
    let mut context = execution_context(&workspace);
    context.file_mutation_approver =
        Some(Arc::new(ApprovalClient { requests }));
    let tools = registry();
    for (tool, args) in [
        (
            "write",
            serde_json::json!({"path":"write", "content":"version two"}),
        ),
        (
            "edit",
            serde_json::json!({"path":"edit", "oldText":"one", "newText":"two"}),
        ),
    ] {
        let working_copy = workspace.join(tool);
        std::fs::write(&working_copy, "version one")
            .expect("original document");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                &working_copy,
                std::fs::Permissions::from_mode(0o750),
            )
            .expect("set document permissions");
        }
        let backup = backups.join(tool);
        std::fs::hard_link(&working_copy, &backup)
            .expect("back up the working copy");
        let result = tools.execute(tool_call(tool, tool, args), &context).await;
        #[cfg(target_os = "linux")]
        {
            result.expect("save working copy");
            assert_eq!(
                std::fs::read(&working_copy).expect("updated copy"),
                b"version two"
            );
        }
        #[cfg(not(target_os = "linux"))]
        result.expect_err("shared file requires an isolated replacement");
        assert_eq!(std::fs::read(&backup).expect("backup"), b"version one");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&working_copy)
                    .expect("document metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o750
            );
        }
    }
    assert_eq!(
        std::fs::read_dir(&workspace)
            .expect("workspace entries")
            .count(),
        2
    );
}

/// Guarded replacement preserves a document's user metadata and rejects privileged mode bits.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn file_approval_preserves_metadata_and_refuses_privileged_files() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir().expect("workspace");
    let document = root.path().join("notes");
    std::fs::write(&document, "draft").expect("write document");
    xattr::set(&document, "user.project", b"journal").expect("tag document");
    // Linux encodes a POSIX ACL as a version and entries for owner, group, mask and others.
    let mut acl = 2u32.to_le_bytes().to_vec();
    for (tag, permissions) in [(1u16, 6u16), (4, 4), (16, 0), (32, 0)] {
        acl.extend(tag.to_le_bytes());
        acl.extend(permissions.to_le_bytes());
        acl.extend(u32::MAX.to_le_bytes());
    }
    xattr::set(&document, "system.posix_acl_access", &acl)
        .expect("set document ACL");
    let (requests, incoming) = tokio::sync::mpsc::unbounded_channel();
    drop(incoming);
    let mut context = execution_context(root.path());
    context.file_mutation_approver =
        Some(Arc::new(ApprovalClient { requests }));
    let tools = registry();
    let mut call = tool_call(
        "write-note",
        "write",
        serde_json::json!({"path":"notes", "content":"final"}),
    );
    tools
        .execute(call.clone(), &context)
        .await
        .expect("save document");
    assert_eq!(
        xattr::get(&document, "user.project")
            .expect("document tag")
            .as_deref(),
        Some(b"journal".as_slice())
    );
    assert_eq!(
        xattr::get(&document, "system.posix_acl_access").expect("document ACL"),
        Some(acl)
    );
    std::fs::set_permissions(
        &document,
        std::fs::Permissions::from_mode(0o4750),
    )
    .expect("set special mode");
    call.arguments["content"] = serde_json::json!("revised");
    tools
        .execute(call, &context)
        .await
        .expect_err("special mode must be rejected");
    assert_eq!(
        std::fs::read(&document).expect("previous document"),
        b"final"
    );
    assert_eq!(
        std::fs::read_dir(root.path())
            .expect("workspace entries")
            .count(),
        1
    );
}

/// Editing requires every directory in the supplied path to exist before resolving parents.
#[tokio::test]
async fn file_approval_edit_requires_existing_intermediate_directories() {
    let root = tempfile::tempdir().expect("workspace");
    let document = root.path().join("notes");
    std::fs::write(&document, "draft").expect("write document");
    let (requests, incoming) = tokio::sync::mpsc::unbounded_channel();
    drop(incoming);
    let mut context = execution_context(root.path());
    context.file_mutation_approver =
        Some(Arc::new(ApprovalClient { requests }));
    let call = tool_call(
        "edit-note",
        "edit",
        serde_json::json!({
            "path": "archive/../notes", "oldText": "draft", "newText": "final"
        }),
    );
    registry()
        .execute(call, &context)
        .await
        .expect_err("missing archive directory");
    assert_eq!(
        std::fs::read(&document).expect("original document"),
        b"draft"
    );
    assert!(!root.path().join("archive").exists());
}

/// A dangling link preserves whether its destination names a file or requires a directory.
#[cfg(unix)]
#[tokio::test]
async fn file_approval_preserves_dangling_link_directory_suffixes() {
    let root = tempfile::tempdir().expect("workspace");
    let (requests, incoming) = tokio::sync::mpsc::unbounded_channel();
    drop(incoming); // Every destination in this test belongs to the workspace.
    let mut context = execution_context(root.path());
    context.file_mutation_approver =
        Some(Arc::new(ApprovalClient { requests }));
    let tools = registry();
    for (name, destination) in [
        ("slash", "missing/"),
        ("dot", "missing/."),
        ("file", "plain"),
    ] {
        std::os::unix::fs::symlink(destination, root.path().join(name))
            .expect("dangling link");
        let call = tool_call(
            name,
            "write",
            serde_json::json!({"path": name, "content": "saved"}),
        );
        let result = tools.execute(call, &context).await;
        if name == "file" {
            result.expect("create a file through its link");
            assert_eq!(
                std::fs::read(root.path().join("plain")).expect("read file"),
                b"saved"
            );
        } else {
            result.expect_err("directory spelling");
            assert!(!root.path().join("missing").exists());
        }
    }
}

/// An external path resolved through multiple links stays bound to the approved name.
#[cfg(unix)]
#[tokio::test]
async fn file_approval_pins_dangling_symlinks_and_normalizes_parent_components()
{
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().expect("temporary root");
    let workspace = root.path().join("project");
    let external = root.path().join("external");
    let other = root.path().join("other");
    std::fs::create_dir(&workspace).expect("create workspace");
    std::fs::create_dir_all(external.join("nested"))
        .expect("create external target");
    std::fs::create_dir(&other).expect("create other directory");
    symlink("../external/nested", workspace.join("alias"))
        .expect("workspace link");
    symlink("missing/file", external.join("destination"))
        .expect("dangling link");
    let (requests, mut incoming) = tokio::sync::mpsc::unbounded_channel();
    let mut context = execution_context(&workspace);
    context.file_mutation_approver =
        Some(Arc::new(ApprovalClient { requests }));
    let tools = registry();
    let call = tool_call(
        "write-link",
        "write",
        serde_json::json!({
            "path":"@alias/../destination", "content":"approved"
        }),
    );
    let mut execution = Box::pin(tools.execute(call, &context));
    let (target, answer) = tokio::select! {
        result = &mut execution => panic!("linked destination completed without approval: {result:?}"),
        request = incoming.recv() => request.expect("approval request"),
    };
    assert_eq!(
        target,
        external
            .canonicalize()
            .expect("physical external directory")
            .join("missing/file")
    );
    assert!(!external.join("missing").exists());
    // A formerly missing canonical component now points somewhere else.
    symlink(&other, external.join("missing")).expect("replacement link");
    answer.send(true).expect("allow");
    execution.await.expect_err("mutation must be blocked");
    assert!(!other.join("file").exists());
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
