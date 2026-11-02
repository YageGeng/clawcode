use std::path::Path;
use std::sync::Arc;

use protocol::{SessionId, ToolCall, ToolCallId, TurnId};
use tokio_util::sync::CancellationToken;
use tools::{
    BuiltinToolFactory, DiscardToolUpdates, ToolError, ToolExecutionContext,
    ToolFactory,
};

/// Builds one direct tool context rooted at a real temporary directory.
fn execution_context(cwd: &Path) -> ToolExecutionContext {
    ToolExecutionContext::builder()
        .session_id(SessionId::try_from("session-grep").expect("session id"))
        .turn_id(TurnId::try_from("turn-grep").expect("turn id"))
        .trace_id(protocol::TraceId::try_from("trace-grep").expect("trace id"))
        .cwd(cwd.to_path_buf())
        .cancellation(CancellationToken::new())
        .updates(Arc::new(DiscardToolUpdates))
        .build()
}

/// Runs one grep call against the default built-in registry.
async fn grep(
    cwd: &Path,
    arguments: serde_json::Value,
) -> Result<String, ToolError> {
    let call = ToolCall::builder()
        .tool_call_id(ToolCallId::try_from("grep-call").expect("tool call id"))
        .name("grep".to_string())
        .arguments(arguments)
        .build();
    let result = BuiltinToolFactory::new()
        .create()
        .expect("built-in registry")
        .execute(call, &execution_context(cwd))
        .await?;
    Ok(result
        .blocks
        .first()
        .and_then(protocol::ContentBlock::text)
        .expect("text result")
        .to_string())
}

/// Writes one fixture file, creating its parent directories.
fn write(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().expect("fixture parent"))
        .expect("create fixture directory");
    std::fs::write(path, content).expect("write fixture");
}

/// Grep exposes pi's argument schema with only the pattern required.
#[test]
fn grep_definition_matches_pi_schema() {
    let definition = BuiltinToolFactory::new()
        .create()
        .expect("built-in registry")
        .definitions()
        .into_iter()
        .find(|definition| definition.name == "grep")
        .expect("grep definition");

    assert_eq!(
        definition.parameters["required"],
        serde_json::json!(["pattern"])
    );
    for (name, kind) in [
        ("pattern", "string"),
        ("path", "string"),
        ("glob", "string"),
        ("ignoreCase", "boolean"),
        ("literal", "boolean"),
        ("context", "number"),
        ("limit", "number"),
    ] {
        assert_eq!(definition.parameters["properties"][name]["type"], kind);
    }
}

/// Directory searches print paths relative to the search root; file searches print the basename.
#[tokio::test]
async fn grep_reports_relative_paths_and_line_numbers() {
    let root = tempfile::tempdir().expect("temporary directory");
    write(root.path(), "src/main.rs", "use std::io;\nfn main() {}\n");
    write(root.path(), "notes.txt", "no functions here\n");

    let text = grep(root.path(), serde_json::json!({ "pattern": "fn \\w+" }))
        .await
        .expect("grep directory");
    assert_eq!(text, "src/main.rs:2: fn main() {}");

    let text = grep(
        root.path(),
        serde_json::json!({ "pattern": "use", "path": "src/main.rs" }),
    )
    .await
    .expect("grep file");
    assert_eq!(text, "main.rs:1: use std::io;");
}

/// Case-insensitive and literal switches change how the pattern matches.
#[tokio::test]
async fn grep_supports_ignore_case_and_literal() {
    let root = tempfile::tempdir().expect("temporary directory");
    write(root.path(), "a.txt", "Hello World\naxb\na.b\n");

    let text = grep(root.path(), serde_json::json!({ "pattern": "hello" }))
        .await
        .expect("case-sensitive grep");
    assert_eq!(text, "No matches found");

    let text = grep(
        root.path(),
        serde_json::json!({ "pattern": "hello", "ignoreCase": true }),
    )
    .await
    .expect("case-insensitive grep");
    assert_eq!(text, "a.txt:1: Hello World");

    let text = grep(
        root.path(),
        serde_json::json!({ "pattern": "a.b", "literal": true }),
    )
    .await
    .expect("literal grep");
    assert_eq!(text, "a.txt:3: a.b");
}

/// Glob filters files, gitignore rules apply, hidden files are searched, and `.git` is skipped.
#[tokio::test]
async fn grep_filters_files_like_ripgrep() {
    let root = tempfile::tempdir().expect("temporary directory");
    write(root.path(), ".git/HEAD", "needle in git metadata\n");
    write(root.path(), ".gitignore", "ignored.txt\n");
    write(root.path(), "ignored.txt", "needle\n");
    write(root.path(), ".hidden.txt", "needle\n");
    write(root.path(), "src/lib.rs", "needle\n");
    write(root.path(), "binary.bin", "needle\0binary\n");

    let text = grep(root.path(), serde_json::json!({ "pattern": "needle" }))
        .await
        .expect("grep all");
    assert_eq!(text, ".hidden.txt:1: needle\nsrc/lib.rs:1: needle");

    let text = grep(
        root.path(),
        serde_json::json!({ "pattern": "needle", "glob": "*.rs" }),
    )
    .await
    .expect("grep glob");
    assert_eq!(text, "src/lib.rs:1: needle");

    // Directory-anchored globs resolve against the default `.` search root.
    let text = grep(
        root.path(),
        serde_json::json!({ "pattern": "needle", "glob": "src/*.rs" }),
    )
    .await
    .expect("grep anchored glob");
    assert_eq!(text, "src/lib.rs:1: needle");
}

/// Context lines use pi's `path-N- text` form and nearby matches share their context.
#[tokio::test]
async fn grep_renders_context_lines() {
    let root = tempfile::tempdir().expect("temporary directory");
    write(
        root.path(),
        "f.txt",
        "one\nmatch a\ntwo\nmatch b\nthree\nfour\n",
    );

    let text = grep(
        root.path(),
        serde_json::json!({ "pattern": "match", "context": 1 }),
    )
    .await
    .expect("grep context");
    assert_eq!(
        text,
        "f.txt-1- one\nf.txt:2: match a\nf.txt-3- two\nf.txt:4: match b\nf.txt-5- three"
    );
}

/// The match limit stops the search and tells the model how to continue.
#[tokio::test]
async fn grep_stops_at_match_limit() {
    let root = tempfile::tempdir().expect("temporary directory");
    write(root.path(), "a.txt", "hit 1\nhit 2\nhit 3\n");
    write(root.path(), "b.txt", "hit 4\n");

    let text = grep(
        root.path(),
        serde_json::json!({ "pattern": "hit", "limit": 2 }),
    )
    .await
    .expect("grep limit");
    assert_eq!(
        text,
        "a.txt:1: hit 1\na.txt:2: hit 2\n\n[2 matches limit reached. Use limit=4 for more, or refine pattern]"
    );
}

/// The final allowed match keeps its trailing context but not the next match's leading context.
#[tokio::test]
async fn grep_keeps_trailing_context_at_match_limit() {
    let root = tempfile::tempdir().expect("temporary directory");
    write(root.path(), "f.txt", "a\nhit1\nb\nc\nhit2\n");

    let text = grep(
        root.path(),
        serde_json::json!({ "pattern": "hit", "limit": 1, "context": 1 }),
    )
    .await
    .expect("grep limit with context");
    assert_eq!(
        text,
        "f.txt-1- a\nf.txt:2: hit1\nf.txt-3- b\n\n[1 matches limit reached. Use limit=2 for more, or refine pattern]"
    );
}

/// Matches inside the final allowed match's trailing window are shown as context lines.
#[tokio::test]
async fn grep_renders_later_matches_as_trailing_context() {
    let root = tempfile::tempdir().expect("temporary directory");
    write(root.path(), "f.txt", "hit1\nhit2\nx\ny\n");

    let text = grep(
        root.path(),
        serde_json::json!({ "pattern": "hit", "limit": 1, "context": 2 }),
    )
    .await
    .expect("grep adjacent matches past the limit");
    assert_eq!(
        text,
        "f.txt:1: hit1\nf.txt-2- hit2\nf.txt-3- x\n\n[1 matches limit reached. Use limit=2 for more, or refine pattern]"
    );
}

/// Oversized context is clamped to 2000 lines so the searcher cannot buffer a whole file.
#[tokio::test]
async fn grep_clamps_context_to_2000_lines() {
    let root = tempfile::tempdir().expect("temporary directory");
    write(
        root.path(),
        "f.txt",
        &format!("{}needle\n", "x\n".repeat(2_500)),
    );

    let text = grep(
        root.path(),
        serde_json::json!({ "pattern": "needle", "context": 1e9 }),
    )
    .await
    .expect("grep huge context");
    assert!(text.starts_with("f.txt-501- x\n"));
    assert!(text.ends_with("f.txt:2501: needle"));
}

/// Invalid globs are rejected during preflight, like invalid patterns.
#[test]
fn grep_validate_rejects_invalid_glob() {
    let call = ToolCall::builder()
        .tool_call_id(ToolCallId::try_from("grep-call").expect("tool call id"))
        .name("grep".to_string())
        .arguments(serde_json::json!({ "pattern": "needle", "glob": "[" }))
        .build();

    BuiltinToolFactory::new()
        .create()
        .expect("built-in registry")
        .validate(&call)
        .expect_err("invalid glob");
}

/// Output beyond 50KB is cut at a complete line and reported, even under a huge match limit.
#[tokio::test]
async fn grep_caps_output_at_50kb() {
    let root = tempfile::tempdir().expect("temporary directory");
    let line = format!("needle{}", "x".repeat(94));
    write(root.path(), "big.txt", &format!("{line}\n").repeat(2_000));

    let text = grep(
        root.path(),
        serde_json::json!({ "pattern": "needle", "limit": 1_000_000 }),
    )
    .await
    .expect("grep large output");
    let (content, notice) =
        text.rsplit_once("\n\n").expect("output with notice");
    assert_eq!(notice, "[50.0KB limit reached]");
    assert!(content.len() <= 50 * 1_024);
    assert!(content.ends_with(&line));
}

/// Lines over 500 characters are shortened with pi's marker and notice.
#[tokio::test]
async fn grep_truncates_long_lines() {
    let root = tempfile::tempdir().expect("temporary directory");
    write(
        root.path(),
        "long.txt",
        &format!("needle{}\n", "x".repeat(600)),
    );

    let text = grep(root.path(), serde_json::json!({ "pattern": "needle" }))
        .await
        .expect("grep long line");
    let expected_line =
        format!("long.txt:1: needle{}... [truncated]", "x".repeat(494));
    assert_eq!(
        text,
        format!(
            "{expected_line}\n\n[Some lines truncated to 500 chars. Use read tool to see full lines]"
        )
    );
}

/// Missing paths and invalid regular expressions fail the call.
#[tokio::test]
async fn grep_rejects_missing_paths_and_invalid_patterns() {
    let root = tempfile::tempdir().expect("temporary directory");

    let error = grep(
        root.path(),
        serde_json::json!({ "pattern": "x", "path": "missing" }),
    )
    .await
    .expect_err("missing path");
    assert_eq!(
        error.to_string(),
        format!("Path not found: {}", root.path().join("missing").display())
    );

    grep(root.path(), serde_json::json!({ "pattern": "(" }))
        .await
        .expect_err("invalid regex");
}
