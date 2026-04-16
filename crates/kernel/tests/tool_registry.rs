use kernel::{
    session::{SessionId, ThreadId},
    tools::{
        Tool, ToolCallRequest, ToolContext,
        builtin::default_read_only_tools,
        builtin::{
            FsReadTextFileTool, FsWriteTextFileTool, ReadFileTool, ReadTextFileTool, WriteFileTool,
            WriteTextFileTool, default_file_tools_with_root,
        },
        executor::ToolExecutor,
        registry::ToolRegistry,
    },
};

use std::{
    fs,
    path::PathBuf,
    process::id,
    sync::OnceLock,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::symlink;

use tokio::sync::Mutex as TokioMutex;

/// Serializes tests that temporarily switch the process working directory.
static CWD_MUTEX: OnceLock<TokioMutex<()>> = OnceLock::new();

/// Guard that restores the original working directory and removes the temp cwd on drop.
struct TempCwdGuard {
    _lock: tokio::sync::MutexGuard<'static, ()>,
    original_cwd: PathBuf,
    temp_cwd: PathBuf,
}

impl Drop for TempCwdGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.original_cwd).unwrap();
        let _ = fs::remove_dir_all(&self.temp_cwd);
    }
}

/// Switches the process cwd to an isolated temporary directory for `.`-root tests.
async fn enter_temp_cwd(prefix: &str) -> TempCwdGuard {
    let lock = CWD_MUTEX.get_or_init(|| TokioMutex::new(())).lock().await;
    let original_cwd = std::env::current_dir().unwrap();
    let temp_cwd = temp_root(prefix);
    std::env::set_current_dir(&temp_cwd).unwrap();

    TempCwdGuard {
        _lock: lock,
        original_cwd,
        temp_cwd,
    }
}

#[tokio::test]
async fn default_read_only_tools_is_empty() {
    let registry = ToolRegistry::default();
    for tool in default_read_only_tools() {
        registry.register_arc(tool).await;
    }

    let definitions = registry.definitions().await;
    assert!(definitions.is_empty());
}

/// Creates a temporary directory with a stable unique suffix under the OS temp dir.
fn temp_root(prefix: &str) -> PathBuf {
    let mut root = std::env::temp_dir();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_nanos();
    root.push(format!(
        "clawcode-kernel-test-{prefix}-{pid}-{ts}",
        pid = id(),
        ts = timestamp
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

/// Returns a stable session id pair for passing namespaced session correlation checks.
fn namespaced_session_pair() -> (SessionId, String) {
    let id = SessionId::new();
    (id.clone(), id.to_string())
}

#[tokio::test]
async fn default_file_tools_register_read_and_write() {
    let registry = ToolRegistry::default();
    for tool in default_file_tools_with_root(".") {
        registry.register_arc(tool).await;
    }

    let definitions = registry.definitions().await;
    let names = definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"file_read"));
    assert!(names.contains(&"file_write"));
    assert!(names.contains(&"read_text_file"));
    assert!(names.contains(&"write_text_file"));
    assert!(names.contains(&"fs/read_text_file"));
    assert!(names.contains(&"fs/write_text_file"));
}

/// Verifies namespaced tools can be looked up by sanitized aliases.
#[tokio::test]
async fn get_resolves_sanitized_namespace_alias() {
    let registry = ToolRegistry::default();
    registry
        .register_arc(Arc::new(FsReadTextFileTool::new("/")))
        .await;

    let tool = registry
        .get("fs_read_text_file")
        .await
        .expect("expected registry to resolve sanitized namespace tool name");

    assert_eq!(tool.name(), "fs/read_text_file");
}

#[test]
// Verifies namespaced read schema advertises canonical session_id only.
fn fs_read_text_file_tool_schema_requires_canonical_session_id() {
    let tool = FsReadTextFileTool::new("/");
    let params = tool.parameters();

    assert_eq!(
        params["required"],
        serde_json::json!(["path", "session_id"])
    );
    assert!(params.get("anyOf").is_none());
    let properties = params["properties"]
        .as_object()
        .expect("expected properties object");
    assert!(properties.contains_key("session_id"));
    assert!(!properties.contains_key("sessionId"));
    assert!(!properties.contains_key("_meta"));
}

#[test]
// Verifies namespaced write schema advertises canonical session_id only.
fn fs_write_text_file_tool_schema_requires_canonical_session_id() {
    let tool = FsWriteTextFileTool::new("/");
    let params = tool.parameters();

    assert_eq!(
        params["required"],
        serde_json::json!(["path", "content", "session_id"])
    );
    assert!(params.get("anyOf").is_none());
    let properties = params["properties"]
        .as_object()
        .expect("expected properties object");
    assert!(properties.contains_key("session_id"));
    assert!(!properties.contains_key("sessionId"));
    assert!(!properties.contains_key("_meta"));
}

#[tokio::test]
async fn fs_read_text_file_tool_supports_line_and_limit() {
    let root = temp_root("read-text-namespaced-range");
    let target = root.join("notes.txt");
    fs::write(&target, "alpha\nbeta\ngamma\n").unwrap();

    let tool = FsReadTextFileTool::new(&root);
    let (session_id, session_id_str) = namespaced_session_pair();
    let meta = serde_json::json!({"trace": "read-range"});
    let output = tool
        .execute(
            serde_json::json!({
                "path": target.to_string_lossy(),
                "line": 2,
                "limit": 2,
                "sessionId": session_id_str,
                "_meta": meta,
            }),
            ToolContext::new(session_id.clone(), ThreadId::new()),
        )
        .await
        .unwrap();

    assert_eq!(output.text, "beta\ngamma");
    assert_eq!(
        output.structured["status"],
        serde_json::Value::String("ok".to_string())
    );
    assert_eq!(
        output.structured["session_id"],
        serde_json::Value::String(session_id.to_string())
    );
    assert_eq!(
        output.structured["_meta"],
        serde_json::json!({"trace": "read-range"})
    );

    let _ = fs::remove_dir_all(&root);
}

/// Verifies namespaced file reads accept `session_id` in snake_case.
#[tokio::test]
async fn fs_read_text_file_tool_supports_snake_case_session_id() {
    let root = temp_root("read-text-namespaced-snake-session");
    let target = root.join("notes.txt");
    fs::write(&target, "alpha\nbeta\ngamma\n").unwrap();

    let tool = FsReadTextFileTool::new(&root);
    let (session_id, session_id_str) = namespaced_session_pair();
    let output = tool
        .execute(
            serde_json::json!({
                "path": target.to_string_lossy(),
                "line": 2,
                "limit": 1,
                "session_id": session_id_str,
            }),
            ToolContext::new(session_id.clone(), ThreadId::new()),
        )
        .await
        .unwrap();

    assert_eq!(output.text, "beta");
    assert_eq!(
        output.structured["session_id"],
        serde_json::Value::String(session_id.to_string())
    );

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn fs_read_text_file_tool_rejects_session_id_mismatch() {
    let root = temp_root("read-text-namespaced-mismatch");
    let target = root.join("notes.txt");
    fs::write(&target, "alpha\nbeta\n").unwrap();

    let tool = FsReadTextFileTool::new(&root);
    let (runtime_session_id, _) = namespaced_session_pair();
    let (_, requested_session_id) = namespaced_session_pair();

    let err = tool
        .execute(
            serde_json::json!({
                "path": target.to_string_lossy(),
                "line": 1,
                "sessionId": requested_session_id,
            }),
            ToolContext::new(runtime_session_id, ThreadId::new()),
        )
        .await
        .expect_err("expected runtime session mismatch to be rejected");

    assert!(
        err.to_string()
            .contains("sessionId does not match runtime session context")
    );

    let _ = fs::remove_dir_all(&root);
}

/// Verifies namespaced reads reject duplicate session fields (serde-level duplicate alias).
#[tokio::test]
async fn fs_read_text_file_tool_rejects_duplicated_session_aliases() {
    let root = temp_root("read-text-namespaced-both-session");
    let target = root.join("notes.txt");
    fs::write(&target, "alpha\nbeta\n").unwrap();

    let tool = FsReadTextFileTool::new(&root);
    let (session_id, session_id_str) = namespaced_session_pair();
    let err = tool
        .execute(
            serde_json::json!({
                "path": target.to_string_lossy(),
                "sessionId": session_id_str,
                "session_id": session_id_str,
            }),
            ToolContext::new(session_id, ThreadId::new()),
        )
        .await
        .expect_err("expected duplicate session fields to be rejected");

    assert!(err.to_string().contains("duplicate field `session_id`"));

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn fs_read_text_file_tool_rejects_relative_path() {
    let root = temp_root("read-text-namespaced-relative");
    let target = root.join("notes.txt");
    fs::write(&target, "alpha\nbeta\n").unwrap();

    let tool = FsReadTextFileTool::new(&root);
    let (session_id, session_id_str) = namespaced_session_pair();
    let err = tool
        .execute(
            serde_json::json!({
                "path": "notes.txt",
                "line": 1,
                "limit": 1,
                "sessionId": session_id_str,
            }),
            ToolContext::new(session_id, ThreadId::new()),
        )
        .await;

    assert!(err.is_err());
    let message = err.unwrap_err().to_string();
    assert!(message.contains("path must be an absolute filesystem path"));

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn fs_read_text_file_tool_rejects_missing_session_id() {
    // Session correlation is required by the namespaced read tool at execution time.
    let root = temp_root("read-text-namespaced-no-session");
    let target = root.join("notes.txt");
    fs::write(&target, "alpha\n").unwrap();

    let tool = FsReadTextFileTool::new(&root);
    let err = tool
        .execute(
            serde_json::json!({ "path": target.to_string_lossy(), "line": 1, "limit": 1 }),
            ToolContext::new(namespaced_session_pair().0, ThreadId::new()),
        )
        .await;

    assert!(err.is_err());
    assert!(
        err.unwrap_err()
            .to_string()
            .contains("sessionId is required")
    );

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
// Ensures session identifiers cannot be omitted or blank whitespace.
async fn fs_read_text_file_tool_rejects_blank_session_id() {
    let root = temp_root("read-text-namespaced-blank-session");
    let target = root.join("notes.txt");
    fs::write(&target, "alpha\n").unwrap();

    let tool = FsReadTextFileTool::new(&root);
    let err = tool
        .execute(
            serde_json::json!({ "path": target.to_string_lossy(), "sessionId": "   " }),
            ToolContext::new(namespaced_session_pair().0, ThreadId::new()),
        )
        .await;

    assert!(err.is_err());
    assert!(
        err.unwrap_err()
            .to_string()
            .contains("sessionId must not be empty")
    );

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn fs_write_text_file_tool_supports_session_id_output_and_write() {
    let root = temp_root("write-text-namespaced-session");
    let target = root.join("out.txt");
    let tool = FsWriteTextFileTool::new(&root);
    let (session_id, session_id_str) = namespaced_session_pair();
    let meta = serde_json::json!({"trace": "write-session"});

    let output = tool
        .execute(
            serde_json::json!({
                "path": target.to_string_lossy(),
                "content": "hello",
                "sessionId": session_id_str,
                "_meta": meta,
            }),
            ToolContext::new(session_id.clone(), ThreadId::new()),
        )
        .await
        .unwrap();

    assert_eq!(
        output.structured["path"],
        serde_json::Value::String(target.to_string_lossy().to_string())
    );
    assert_eq!(output.structured["bytes_written"], serde_json::json!(5));
    assert_eq!(
        output.structured["session_id"],
        serde_json::Value::String(session_id.to_string())
    );
    assert_eq!(
        output.structured["_meta"],
        serde_json::json!({"trace": "write-session"})
    );
    assert_eq!(fs::read_to_string(target).unwrap(), "hello");

    let _ = fs::remove_dir_all(&root);
}

/// Verifies namespaced file writes accept `session_id` in snake_case.
#[tokio::test]
async fn fs_write_text_file_tool_supports_snake_case_session_id() {
    let root = temp_root("write-text-namespaced-snake-session");
    let target = root.join("out.txt");
    let tool = FsWriteTextFileTool::new(&root);
    let (session_id, session_id_str) = namespaced_session_pair();

    let output = tool
        .execute(
            serde_json::json!({
                "path": target.to_string_lossy(),
                "content": "hello",
                "session_id": session_id_str,
            }),
            ToolContext::new(session_id.clone(), ThreadId::new()),
        )
        .await
        .unwrap();

    assert_eq!(
        output.structured["session_id"],
        serde_json::Value::String(session_id.to_string())
    );
    assert_eq!(fs::read_to_string(target).unwrap(), "hello");

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn fs_write_text_file_tool_rejects_session_id_mismatch() {
    let root = temp_root("write-text-namespaced-mismatch");
    let target = root.join("out.txt");
    let tool = FsWriteTextFileTool::new(&root);
    let (runtime_session_id, _) = namespaced_session_pair();
    let (_, requested_session_id) = namespaced_session_pair();

    let err = tool
        .execute(
            serde_json::json!({
                "path": target.to_string_lossy(),
                "content": "hello",
                "sessionId": requested_session_id,
            }),
            ToolContext::new(runtime_session_id, ThreadId::new()),
        )
        .await
        .expect_err("expected runtime session mismatch to be rejected");

    assert!(
        err.to_string()
            .contains("sessionId does not match runtime session context")
    );

    let _ = fs::remove_dir_all(&root);
}

/// Verifies namespaced writes reject duplicate session fields (serde-level duplicate alias).
#[tokio::test]
async fn fs_write_text_file_tool_rejects_duplicated_session_aliases() {
    let root = temp_root("write-text-namespaced-both-session");
    let target = root.join("out.txt");
    let tool = FsWriteTextFileTool::new(&root);
    let (session_id, session_id_str) = namespaced_session_pair();

    let err = tool
        .execute(
            serde_json::json!({
                "path": target.to_string_lossy(),
                "content": "hello",
                "sessionId": session_id_str,
                "session_id": session_id_str,
            }),
            ToolContext::new(session_id, ThreadId::new()),
        )
        .await
        .expect_err("expected duplicate session fields to be rejected");

    assert!(err.to_string().contains("duplicate field `session_id`"));

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn fs_write_text_file_tool_rejects_relative_path() {
    let root = temp_root("write-text-namespaced-relative");
    let target = root.join("out.txt");
    let tool = FsWriteTextFileTool::new(&root);
    let (session_id, session_id_str) = namespaced_session_pair();

    let err = tool
        .execute(
            serde_json::json!({
                "path": "out.txt",
                "content": "hello",
                "sessionId": session_id_str,
            }),
            ToolContext::new(session_id, ThreadId::new()),
        )
        .await;

    assert!(err.is_err());
    let message = err.unwrap_err().to_string();
    assert!(message.contains("path must be an absolute filesystem path"));

    let _ = fs::remove_dir_all(&root);
    assert!(!target.exists());
}

#[tokio::test]
async fn fs_write_text_file_tool_rejects_missing_session_id() {
    // Write operations for namespaced ACP aliases must include a valid session id.
    let root = temp_root("write-text-namespaced-no-session");
    let target = root.join("out.txt");
    let tool = FsWriteTextFileTool::new(&root);

    let err = tool
        .execute(
            serde_json::json!({"path": target.to_string_lossy(), "content": "hello"}),
            ToolContext::new(namespaced_session_pair().0, ThreadId::new()),
        )
        .await;

    assert!(err.is_err());
    assert!(
        err.unwrap_err()
            .to_string()
            .contains("sessionId is required")
    );

    let _ = fs::remove_dir_all(&root);
    assert!(!target.exists());
}

#[tokio::test]
// Ensures namespaced write requests reject empty session identifiers.
async fn fs_write_text_file_tool_rejects_blank_session_id() {
    let root = temp_root("write-text-namespaced-blank-session");
    let target = root.join("out.txt");
    let tool = FsWriteTextFileTool::new(&root);

    let err = tool
        .execute(
            serde_json::json!({"path": target.to_string_lossy(), "content": "hello", "sessionId": ""}),
            ToolContext::new(namespaced_session_pair().0, ThreadId::new()),
        )
        .await;

    assert!(err.is_err());
    assert!(
        err.unwrap_err()
            .to_string()
            .contains("sessionId must not be empty")
    );
    assert!(!target.exists());

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn read_file_tool_reads_existing_content() {
    let root = temp_root("read");
    let target = root.join("notes.txt");
    fs::write(&target, "hello\nclawcode\n").unwrap();

    let tool = ReadTextFileTool::new(&root);
    let output = tool
        .execute(
            serde_json::json!({ "path": "notes.txt" }),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await
        .unwrap();

    assert_eq!(output.text, "hello\nclawcode\n");
    assert_eq!(
        output.structured["path"],
        serde_json::Value::String(target.to_string_lossy().to_string())
    );
    assert_eq!(output.structured["bytes"], serde_json::json!(15));
    assert_eq!(
        output.structured["status"],
        serde_json::Value::String("ok".to_string())
    );

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
/// Verifies read paths with `.` segments stay inside a normal configured root.
async fn read_file_tool_accepts_dot_relative_path_inside_root() {
    let root = temp_root("read-dot-relative");
    let target = root.join("poem.txt");
    fs::write(&target, "first line\nsecond line\n").unwrap();

    let tool = ReadFileTool::new(&root);
    let output = tool
        .execute(
            serde_json::json!({"path": "./poem.txt"}),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await
        .unwrap();

    assert_eq!(output.text, "first line\nsecond line\n");
    assert_eq!(
        output.structured["path"],
        serde_json::json!(target.to_string_lossy().to_string())
    );

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
/// Verifies the default CLI-style root `.` can read `./poem.txt` inside the workspace.
async fn read_file_tool_accepts_dot_relative_path_with_dot_root() {
    let _cwd_guard = enter_temp_cwd("read-dot-root").await;
    let target = std::env::current_dir().unwrap().join("poem.txt");
    fs::write(&target, "first line\nsecond line\n").unwrap();

    let tool = ReadFileTool::new(".");
    let output = tool
        .execute(
            serde_json::json!({"path": "./poem.txt"}),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await
        .unwrap();

    assert_eq!(output.text, "first line\nsecond line\n");
    assert_eq!(
        output.structured["path"],
        serde_json::json!(target.to_string_lossy().to_string())
    );
}

#[tokio::test]
async fn read_text_file_tool_supports_line_and_limit() {
    let root = temp_root("read-text-range");
    let target = root.join("notes.txt");
    fs::write(&target, "alpha\nbeta\ngamma\ndelta\n").unwrap();

    let tool = ReadTextFileTool::new(&root);
    let output = tool
        .execute(
            serde_json::json!({ "path": "notes.txt", "line": 2, "limit": 2 }),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await
        .unwrap();

    assert_eq!(output.text, "beta\ngamma");
    assert_eq!(
        output.structured["line"],
        serde_json::Value::Number(serde_json::Number::from(2)),
    );
    assert_eq!(
        output.structured["limit"],
        serde_json::Value::Number(serde_json::Number::from(2)),
    );
    assert_eq!(output.structured["session_id"], serde_json::Value::Null,);

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn read_text_file_tool_supports_limit_without_line_offset() {
    let root = temp_root("read-text-limit-only");
    let target = root.join("notes.txt");
    fs::write(&target, "alpha\nbeta\ngamma\ndelta\n").unwrap();

    let tool = ReadTextFileTool::new(&root);
    let output = tool
        .execute(
            serde_json::json!({ "path": "notes.txt", "limit": 2 }),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await
        .unwrap();

    assert_eq!(output.text, "alpha\nbeta");
    assert_eq!(output.structured["line"], serde_json::Value::Null,);
    assert_eq!(
        output.structured["limit"],
        serde_json::Value::Number(serde_json::Number::from(2)),
    );

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn write_text_file_tool_supports_session_id_output_and_write() {
    let root = temp_root("write-text-session");
    let target = root.join("out.txt");
    let tool = WriteTextFileTool::new(&root);

    let output = tool
        .execute(
            serde_json::json!({"path": "out.txt", "content": "hello", "session_id": "sess-write"}),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await
        .unwrap();

    assert_eq!(
        output.structured["path"],
        serde_json::Value::String(target.to_string_lossy().to_string()),
    );
    assert_eq!(output.structured["bytes_written"], serde_json::json!(5));
    assert_eq!(
        output.structured["status"],
        serde_json::Value::String("ok".to_string())
    );
    assert_eq!(
        output.structured["session_id"],
        serde_json::Value::String("sess-write".to_string())
    );
    assert_eq!(fs::read_to_string(target).unwrap(), "hello");

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn write_text_file_tool_supports_camelcase_session_id_alias_in_output() {
    let root = temp_root("write-text-session-alias");
    let target = root.join("out.txt");
    let tool = WriteTextFileTool::new(&root);

    let output = tool
        .execute(
            serde_json::json!({"path": "out.txt", "content": "abc", "sessionId": "sess-camel"}),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await
        .unwrap();

    assert_eq!(
        output.structured["session_id"],
        serde_json::Value::String("sess-camel".to_string())
    );
    assert_eq!(fs::read_to_string(target).unwrap(), "abc");

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn read_text_file_tool_supports_camelcase_session_id_alias() {
    let root = temp_root("read-text-sessionid-alias");
    let target = root.join("notes.txt");
    fs::write(&target, "alpha\nbeta\n").unwrap();

    let tool = ReadTextFileTool::new(&root);
    let output = tool
        .execute(
            serde_json::json!({"path": "notes.txt", "sessionId": "sess-camel"}),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await
        .unwrap();

    assert_eq!(
        output.structured["session_id"],
        serde_json::Value::String("sess-camel".to_string())
    );

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn read_text_file_rejects_invalid_line_number() {
    let root = temp_root("read-text-invalid-line");
    let target = root.join("notes.txt");
    fs::write(&target, "alpha\nbeta\n").unwrap();

    let tool = ReadTextFileTool::new(&root);
    let bad_line = tool
        .execute(
            serde_json::json!({ "path": "notes.txt", "line": 0 }),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await;
    assert!(bad_line.is_err());
    let err = bad_line.unwrap_err().to_string();
    assert!(err.contains("line must be 1-based"));

    let out_of_range = tool
        .execute(
            serde_json::json!({ "path": "notes.txt", "line": 10 }),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await;
    assert!(out_of_range.is_err());
    let err = out_of_range.unwrap_err().to_string();
    assert!(err.contains("outside available file range"));

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn write_file_tool_creates_parent_directory_and_writes_content() {
    let root = temp_root("write");

    let tool = WriteFileTool::new(&root);
    let output = tool
        .execute(
            serde_json::json!({"path": "nested/dir/result.txt", "content": "written"}),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await
        .unwrap();

    assert_eq!(output.structured["bytes_written"], serde_json::json!(7));
    let written = fs::read_to_string(root.join("nested/dir/result.txt")).unwrap();
    assert_eq!(written, "written");

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
/// Verifies relative paths with `.` segments stay inside the configured root.
async fn write_file_tool_accepts_dot_relative_path_inside_root() {
    let root = temp_root("write-dot-relative");

    let tool = WriteFileTool::new(&root);
    let output = tool
        .execute(
            serde_json::json!({"path": "./poem.txt", "content": "verse"}),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await
        .unwrap();

    assert_eq!(output.structured["bytes_written"], serde_json::json!(5));
    assert_eq!(fs::read_to_string(root.join("poem.txt")).unwrap(), "verse");

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
/// Verifies non-existent nested paths normalize lexically before sandbox checks.
async fn write_file_tool_accepts_nonexistent_nested_path_with_dot_segments() {
    let root = temp_root("write-nested-dot-segments");

    let tool = WriteFileTool::new(&root);
    let output = tool
        .execute(
            serde_json::json!({"path": "nested/./dir/result.txt", "content": "written"}),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await
        .unwrap();

    assert_eq!(output.structured["bytes_written"], serde_json::json!(7));
    assert_eq!(
        fs::read_to_string(root.join("nested/dir/result.txt")).unwrap(),
        "written"
    );

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
/// Verifies the default CLI-style root `.` accepts `./poem.txt` inside the workspace.
async fn write_file_tool_accepts_dot_relative_path_with_dot_root() {
    let _cwd_guard = enter_temp_cwd("write-dot-root").await;
    let target = std::env::current_dir().unwrap().join("poem.txt");

    let tool = WriteFileTool::new(".");
    let output = tool
        .execute(
            serde_json::json!({"path": "./poem.txt", "content": "workspace verse"}),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await
        .unwrap();

    assert_eq!(output.structured["bytes_written"], serde_json::json!(15));
    assert_eq!(fs::read_to_string(&target).unwrap(), "workspace verse");
}

#[tokio::test]
async fn file_read_and_write_tools_reject_unsafe_paths() {
    let root = temp_root("reject");
    let read_tool = ReadFileTool::new(&root);
    let write_tool = WriteFileTool::new(&root);

    let absolute_path = temp_root("reject-absolute-path").join("guard.txt");

    let read_result = read_tool
        .execute(
            serde_json::json!({"path": "../outside.txt"}),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await;
    assert!(read_result.is_err());
    let read_error = read_result.unwrap_err().to_string();
    assert!(read_error.contains("path traversal") || read_error.contains("absolute paths"));

    let absolute_read_result = read_tool
        .execute(
            serde_json::json!({"path": absolute_path.to_string_lossy()}),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await;
    assert!(absolute_read_result.is_err());
    assert!(
        absolute_read_result
            .unwrap_err()
            .to_string()
            .contains("absolute paths are not allowed")
    );

    let write_result = write_tool
        .execute(
            serde_json::json!({"path": absolute_path.to_string_lossy(), "content": "x"}),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await;
    assert!(write_result.is_err());
    assert!(
        write_result
            .unwrap_err()
            .to_string()
            .contains("absolute paths are not allowed")
    );

    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(absolute_path.parent().unwrap());
}

#[tokio::test]
async fn file_tools_respect_max_byte_limits() {
    let root = temp_root("limits");
    let read_tool = ReadFileTool::with_max_bytes(&root, 4);
    let write_tool = WriteFileTool::with_max_bytes(&root, 4);

    fs::write(root.join("small.txt"), "12345").unwrap();
    let oversized_read = read_tool
        .execute(
            serde_json::json!({ "path": "small.txt" }),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await;
    assert!(oversized_read.is_err());

    let oversized_write = write_tool
        .execute(
            serde_json::json!({"path": "too-big.txt", "content": "12345"}),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await;
    assert!(oversized_write.is_err());

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn file_write_tool_execution_requires_approval_when_forced() {
    let root = temp_root("approval-enforced");
    let registry = ToolRegistry::default();

    for tool in default_file_tools_with_root(&root) {
        registry.register_arc(tool).await;
    }

    let result = ToolExecutor::execute_all(
        &registry,
        vec![ToolCallRequest::new(
            "call_1",
            "file_write",
            serde_json::json!({"path": "must_not_run.txt", "content": "block-me"}),
        )],
        ToolContext::new(SessionId::new(), ThreadId::new()).with_tool_approval_enforcement(true),
    )
    .await;

    assert!(result.is_err());
    let message = result.unwrap_err().to_string();
    assert!(
        message.contains("requires approval") || message.contains("requires explicit approval")
    );

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn tool_approval_callback_can_deny_execution() {
    let root = temp_root("approval-callback-deny");
    let registry = ToolRegistry::default();
    for tool in default_file_tools_with_root(&root) {
        registry.register_arc(tool).await;
    }

    let denied = Arc::new(AtomicBool::new(false));
    let denied_for = denied.clone();
    let context = ToolContext::new(SessionId::new(), ThreadId::new())
        .with_tool_approval_enforcement(true)
        .with_tool_approval_handler(move |request| {
            denied_for.store(true, Ordering::Relaxed);
            let _ = request.call_id.as_ref();
            false
        });

    let result = ToolExecutor::execute_all(
        &registry,
        vec![ToolCallRequest::new(
            "call_1",
            "write_text_file",
            serde_json::json!({"path": "not-allowed.txt", "content": "blocked", "session_id": "sess-1"}),
        )],
        context,
    )
    .await;

    assert!(result.is_err());
    assert!(denied.load(Ordering::Relaxed));
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("requires approval")
    );

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn tool_approval_callback_can_allow_execution() {
    let root = temp_root("approval-callback-allow");
    let registry = ToolRegistry::default();
    for tool in default_file_tools_with_root(&root) {
        registry.register_arc(tool).await;
    }

    let allowed = Arc::new(AtomicBool::new(false));
    let observed_session = Arc::new(AtomicBool::new(false));
    let observed_session_for = observed_session.clone();
    let allowed_for = allowed.clone();
    let context = ToolContext::new(SessionId::new(), ThreadId::new())
        .with_tool_approval_enforcement(true)
        .with_tool_approval_handler(move |request| {
            if request.tool == "write_text_file" {
                if let Some(session_id) = request
                    .arguments
                    .get("session_id")
                    .and_then(|value| value.as_str())
                {
                    observed_session_for.store(session_id == "sess-2", Ordering::Relaxed);
                }
                allowed_for.store(true, Ordering::Relaxed);
                true
            } else {
                false
            }
        });

    let result = ToolExecutor::execute_all(
        &registry,
        vec![ToolCallRequest::new(
            "call_2",
            "write_text_file",
            serde_json::json!({"path": "allowed.txt", "content": "ok", "session_id": "sess-2"}),
        )],
        context,
    )
    .await
    .unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0].output.structured["status"],
        serde_json::Value::String("ok".to_string())
    );
    assert_eq!(fs::read_to_string(root.join("allowed.txt")).unwrap(), "ok");
    assert!(allowed.load(Ordering::Relaxed));
    assert!(observed_session.load(Ordering::Relaxed));

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn tool_approval_callback_can_read_camelcase_session_id_field() {
    let root = temp_root("approval-callback-camel-session-id");
    let registry = ToolRegistry::default();
    for tool in default_file_tools_with_root(&root) {
        registry.register_arc(tool).await;
    }

    let observed_session = Arc::new(AtomicBool::new(false));
    let observed_session_for = observed_session.clone();

    let context = ToolContext::new(SessionId::new(), ThreadId::new())
        .with_tool_approval_enforcement(true)
        .with_tool_approval_handler(move |request| {
            if let Some(session_id) = request
                .arguments
                .get("session_id")
                .or_else(|| request.arguments.get("sessionId"))
                .and_then(|value| value.as_str())
            {
                observed_session_for.store(session_id == "sess-3", Ordering::Relaxed);
            }
            true
        });

    let result = ToolExecutor::execute_all(
        &registry,
        vec![ToolCallRequest::new(
            "call_3",
            "write_text_file",
            serde_json::json!({"path": "session-alias.txt", "content": "ok", "sessionId": "sess-3"}),
        )],
        context,
    )
    .await
    .unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0].output.structured["status"],
        serde_json::Value::String("ok".to_string())
    );
    assert_eq!(
        fs::read_to_string(root.join("session-alias.txt")).unwrap(),
        "ok"
    );
    assert!(observed_session.load(Ordering::Relaxed));

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn tool_approval_callback_accepts_fs_namespaced_write_text_file() {
    let root = temp_root("approval-callback-ns-write");
    let target = root.join("allowed.txt");
    let (session_id, session_id_str) = namespaced_session_pair();
    let registry = ToolRegistry::default();
    for tool in default_file_tools_with_root(&root) {
        registry.register_arc(tool).await;
    }

    let observed_tool = Arc::new(AtomicBool::new(false));
    let observed_tool_for = observed_tool.clone();
    let context = ToolContext::new(session_id, ThreadId::new())
        .with_tool_approval_enforcement(true)
        .with_tool_approval_handler(move |request| {
            if request.tool == "fs/write_text_file" {
                observed_tool_for.store(true, Ordering::Relaxed);
                true
            } else {
                false
            }
        });

    let result = ToolExecutor::execute_all(
        &registry,
        vec![ToolCallRequest::new(
            "call_4",
            "fs/write_text_file",
            serde_json::json!({"path": target.to_string_lossy(), "content": "ok", "session_id": session_id_str}),
        )],
        context,
    )
    .await
    .unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0].output.structured["status"],
        serde_json::Value::String("ok".to_string())
    );
    assert!(observed_tool.load(Ordering::Relaxed));
    assert_eq!(fs::read_to_string(target).unwrap(), "ok");

    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[tokio::test]
/// Reads through a symlinked path outside the root must be rejected.
async fn read_file_tool_rejects_symlinked_path_escape() {
    let root = temp_root("symlink-read-root");
    let outside_root = temp_root("symlink-read-outside");

    let outside_path = outside_root.join("secret.txt");
    fs::write(&outside_path, "secret").unwrap();

    let link_path = root.join("outside_link.txt");
    symlink(&outside_path, &link_path).unwrap();

    let read_tool = ReadFileTool::new(&root);
    let result = read_tool
        .execute(
            serde_json::json!({"path": "outside_link.txt"}),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await;

    assert!(result.is_err());
    let message = result.unwrap_err().to_string();
    assert!(
        message.contains("canonicalized target must stay inside the tool root")
            || message.contains("target must stay inside the tool root")
    );

    let _ = fs::remove_dir_all(&outside_root);
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[tokio::test]
/// Writes through a symlinked parent directory outside the root must be rejected.
async fn write_file_tool_rejects_symlinked_parent_escape() {
    let root = temp_root("symlink-write-parent-root");
    let outside_root = temp_root("symlink-write-parent-outside");
    let outside_parent = outside_root.join("allowed");
    fs::create_dir_all(&outside_parent).unwrap();

    let link_parent = root.join("parent_link");
    symlink(&outside_parent, &link_parent).unwrap();

    let write_tool = WriteFileTool::new(&root);
    let result = write_tool
        .execute(
            serde_json::json!({"path": "parent_link/owned.txt", "content": "attempt"}),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await;

    assert!(result.is_err());
    let message = result.unwrap_err().to_string();
    assert!(
        message.contains("path traversal through symlink is not allowed")
            || message.contains("target parent must stay inside the tool root")
            || message.contains("target must stay inside the tool root")
            || message.contains("canonicalized target must stay inside the tool root")
    );

    let _ = fs::remove_dir_all(&outside_root);
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[tokio::test]
/// Writing to an existing symlink file path must be rejected.
async fn write_file_tool_rejects_existing_target_symlink() {
    let root = temp_root("symlink-write-target-root");
    let outside_root = temp_root("symlink-write-target-outside");

    let outside_file = outside_root.join("outside.txt");
    fs::write(&outside_file, "outside").unwrap();

    let link_target = root.join("outside.txt");
    symlink(&outside_file, &link_target).unwrap();

    let write_tool = WriteFileTool::new(&root);
    let result = write_tool
        .execute(
            serde_json::json!({"path": "outside.txt", "content": "blocked"}),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await;

    assert!(result.is_err());
    let message = result.unwrap_err().to_string();
    assert!(
        message.contains("target path must not be a symlink")
            || message.contains("path traversal through symlink is not allowed")
            || message.contains("target must stay inside the tool root")
    );

    let _ = fs::remove_dir_all(&outside_root);
    let _ = fs::remove_dir_all(&root);
}
