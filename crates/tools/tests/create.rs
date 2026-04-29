#[cfg(unix)]
use std::os::unix::fs as unix_fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::PathBuf,
    process::id,
    time::{SystemTime, UNIX_EPOCH},
};
use tools::{ToolCallRequest, ToolContext, ToolRouter, build_default_tool_registry_plan};

/// Verifies the extracted tools crate exposes the default patch and shell tools.
#[tokio::test]
async fn default_router_exposes_file_tools() {
    let router = ToolRouter::from_path(".").await;
    let names = router
        .definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect::<Vec<_>>();

    assert!(names.contains(&"apply_patch".to_string()));
    assert!(names.contains(&"exec_command".to_string()));
    assert!(names.contains(&"write_stdin".to_string()));
}

/// Verifies the default plan captures visible specs and dispatch handlers separately.
#[test]
fn default_tool_registry_plan_contains_specs_and_handlers() {
    let plan = build_default_tool_registry_plan(temp_root("plan"));
    let spec_names = plan
        .specs
        .iter()
        .map(|configured| configured.name().to_string())
        .collect::<Vec<_>>();
    let handler_names = plan
        .handlers
        .iter()
        .map(|planned| planned.handler.name().to_string())
        .collect::<Vec<_>>();

    assert!(spec_names.contains(&"apply_patch".to_string()));
    assert!(handler_names.contains(&"apply_patch".to_string()));
    assert!(
        plan.handlers
            .iter()
            .any(|planned| planned.handler.name() == "apply_patch")
    );
    assert!(
        plan.handlers
            .iter()
            .any(|planned| planned.handler.name() == "exec_command")
    );
    assert!(
        plan.handlers
            .iter()
            .any(|planned| planned.handler.name() == "write_stdin")
    );
}

/// Verifies the default tool plan preserves prompt metadata for builtin visible specs.
#[test]
fn default_tool_registry_plan_preserves_prompt_metadata() {
    let plan = build_default_tool_registry_plan(temp_root("plan-prompt-metadata"));
    let read_spec = plan
        .specs
        .iter()
        .find(|configured| configured.name() == "fs/read_text_file")
        .expect("read_text_file spec should exist");

    assert_eq!(
        read_spec.spec.prompt_metadata.prompt_snippet,
        Some("Read UTF-8 text files from the workspace.")
    );
}

/// Verifies the built-in shell tool can run a one-shot command.
#[tokio::test]
async fn exec_command_runs_a_one_shot_shell_command() {
    let root = temp_root("exec-command-one-shot");
    let router = ToolRouter::from_path(&root).await;

    let output = router
        .dispatch(
            ToolCallRequest::new(
                "call-exec",
                "exec_command",
                serde_json::json!({
                    "cmd": "printf hello-shell",
                    "shell": "/bin/sh"
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap();

    let structured = output.structured.to_serde_value();
    assert_eq!(structured["stdout"], "hello-shell");
    assert_eq!(structured["running"], false);
}

/// Verifies absolute workspace-root paths are accepted for `exec_command` workdir.
#[tokio::test]
async fn exec_command_accepts_workspace_absolute_workdir() {
    let root = temp_root("exec-command-absolute-workdir");
    let router = ToolRouter::from_path(&root).await;

    let output = router
        .dispatch(
            ToolCallRequest::new(
                "call-absolute-workdir",
                "exec_command",
                serde_json::json!({
                    "cmd": "printf hello-absolute",
                    "workdir": root.to_string_lossy(),
                    "shell": "/bin/sh"
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap();

    let structured = output.structured.to_serde_value();
    assert_eq!(structured["stdout"], "hello-absolute");
    assert_eq!(structured["running"], false);
}

/// Verifies absolute paths outside the workspace are rejected by `exec_command`.
#[tokio::test]
async fn exec_command_rejects_workspace_absolute_workdir_outside_root() {
    let root = temp_root("exec-command-absolute-workdir-inside");
    let outside = temp_root("exec-command-absolute-workdir-outside");
    let router = ToolRouter::from_path(&root).await;

    let error = router
        .dispatch(
            ToolCallRequest::new(
                "call-absolute-workdir-outside",
                "exec_command",
                serde_json::json!({
                    "cmd": "pwd",
                    "workdir": outside.to_string_lossy(),
                    "shell": "/bin/sh"
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("workdir must be relative to the workspace root")
    );
}

/// Verifies exec-command intercepts simple shell-wrapped apply_patch invocations.
#[tokio::test]
async fn exec_command_intercepts_apply_patch_shell_command() {
    let root = temp_root("exec-command-apply-patch");
    let router = ToolRouter::from_path(&root).await;

    let output = router
        .dispatch(
            ToolCallRequest::new(
                "call-intercept",
                "exec_command",
                serde_json::json!({
                    "cmd": "apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: via-shell.txt\n+hello from shell interception\n*** End Patch\nEOF",
                    "shell": "/bin/sh"
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap();

    assert!(output.text.contains("A via-shell.txt"));
    assert_eq!(
        fs::read_to_string(root.join("via-shell.txt")).unwrap(),
        "hello from shell interception\n"
    );
}

/// Verifies apply-patch interception does not swallow trailing shell commands.
#[tokio::test]
async fn exec_command_does_not_intercept_apply_patch_with_trailing_commands() {
    let root = temp_root("exec-command-apply-patch-trailing");
    let shell = write_shell_with_local_path(&root);
    write_executable_script(
        &root.join("apply_patch"),
        "#!/bin/sh\ncat >/dev/null\nprintf 'hello from shell interception\\n' > via-shell.txt\n",
    );
    let router = ToolRouter::from_path(&root).await;

    let output = router
        .dispatch(
            ToolCallRequest::new(
                "call-intercept-trailing",
                "exec_command",
                serde_json::json!({
                    "cmd": "apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: via-shell.txt\n+hello from shell interception\n*** End Patch\nEOF\nprintf trailing-command",
                    "shell": shell
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap();

    let structured = output.structured.to_serde_value();
    let stdout = structured["stdout"].as_str().unwrap();
    assert!(stdout.contains("trailing-command"));
    assert_eq!(
        fs::read_to_string(root.join("via-shell.txt")).unwrap(),
        "hello from shell interception\n"
    );
}

/// Verifies intercepted `cd <dir> && apply_patch` refuses symlinks that escape the workspace.
#[cfg(unix)]
#[tokio::test]
async fn exec_command_rejects_apply_patch_cd_symlink_escaping_workspace() {
    let root = temp_root("exec-command-apply-patch-symlink-escape");
    let outside = temp_root("exec-command-apply-patch-symlink-outside");
    unix_fs::symlink(&outside, root.join("escape")).unwrap();
    let router = ToolRouter::from_path(&root).await;

    let error = router
        .dispatch(
            ToolCallRequest::new(
                "call-intercept-symlink-escape",
                "exec_command",
                serde_json::json!({
                    "cmd": "cd escape && apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: escaped.txt\n+outside\n*** End Patch\nEOF",
                    "shell": "/bin/sh"
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("must stay inside the workspace root")
    );
    assert!(!outside.join("escaped.txt").exists());
}

/// Verifies the shell tools can keep a process alive and continue it with stdin writes.
#[tokio::test]
async fn write_stdin_continues_a_shell_session() {
    let root = temp_root("exec-command-session");
    let router = ToolRouter::from_path(&root).await;

    let exec_output = router
        .dispatch(
            ToolCallRequest::new(
                "call-session",
                "exec_command",
                serde_json::json!({
                    "cmd": "read line; printf '%s' \"$line\"",
                    "shell": "/bin/sh",
                    "tty": true,
                    "yield_time_ms": 10
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap();

    let exec_structured = exec_output.structured.to_serde_value();
    let session_id = exec_structured["session_id"].as_str().unwrap().to_string();
    let write_output = router
        .dispatch(
            ToolCallRequest::new(
                "call-write",
                "write_stdin",
                serde_json::json!({
                    "session_id": session_id,
                    "chars": "hello-session\n",
                    "yield_time_ms": 10
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap();

    let write_structured = write_output.structured.to_serde_value();
    assert_eq!(write_structured["stdout"], "hello-session");
    assert_eq!(write_structured["running"], false);
}

/// Verifies completed sessions are removed from the process manager after exit.
#[tokio::test]
async fn write_stdin_rejects_completed_session_ids() {
    let root = temp_root("exec-command-session-cleanup");
    let router = ToolRouter::from_path(&root).await;

    let exec_output = router
        .dispatch(
            ToolCallRequest::new(
                "call-session-cleanup",
                "exec_command",
                serde_json::json!({
                    "cmd": "read line; printf '%s' \"$line\"",
                    "shell": "/bin/sh",
                    "tty": true,
                    "yield_time_ms": 10
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap();

    let exec_structured = exec_output.structured.to_serde_value();
    let session_id = exec_structured["session_id"].as_str().unwrap().to_string();
    router
        .dispatch(
            ToolCallRequest::new(
                "call-write-cleanup",
                "write_stdin",
                serde_json::json!({
                    "session_id": session_id.clone(),
                    "chars": "done\n",
                    "yield_time_ms": 10
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap();

    let error = router
        .dispatch(
            ToolCallRequest::new(
                "call-poll-cleanup",
                "write_stdin",
                serde_json::json!({
                    "session_id": session_id,
                    "chars": "",
                    "yield_time_ms": 10
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("unknown session_id"));
}

/// Verifies the built-in apply-patch tool can create a new file.
#[tokio::test]
async fn apply_patch_adds_a_file() {
    let root = temp_root("apply-patch-add");
    let router = ToolRouter::from_path(&root).await;

    let output = router
        .dispatch(
            ToolCallRequest::new(
                "call-add",
                "apply_patch",
                serde_json::json!({
                    "patchText": "*** Begin Patch\n*** Add File: note.txt\n+hello\n+tools\n*** End Patch"
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap();

    assert!(output.text.contains("A note.txt"));
    assert_eq!(
        fs::read_to_string(root.join("note.txt")).unwrap(),
        "hello\ntools\n"
    );
}

/// Verifies the built-in apply-patch tool can update an existing file.
#[tokio::test]
async fn apply_patch_updates_a_file() {
    let root = temp_root("apply-patch-update");
    fs::write(root.join("note.txt"), "hello\ntools\n").unwrap();
    let router = ToolRouter::from_path(&root).await;

    let output = router
        .dispatch(
            ToolCallRequest::new(
                "call-update",
                "apply_patch",
                serde_json::json!({
                    "patchText": "*** Begin Patch\n*** Update File: note.txt\n@@\n hello\n-tools\n+world\n*** End Patch"
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap();

    assert!(output.text.contains("M note.txt"));
    assert_eq!(
        fs::read_to_string(root.join("note.txt")).unwrap(),
        "hello\nworld\n"
    );
}

/// Verifies the built-in apply-patch tool can delete an existing file.
#[tokio::test]
async fn apply_patch_deletes_a_file() {
    let root = temp_root("apply-patch-delete");
    fs::write(root.join("note.txt"), "hello\ntools\n").unwrap();
    let router = ToolRouter::from_path(&root).await;

    let output = router
        .dispatch(
            ToolCallRequest::new(
                "call-delete",
                "apply_patch",
                serde_json::json!({
                    "patchText": "*** Begin Patch\n*** Delete File: note.txt\n*** End Patch"
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap();

    assert!(output.text.contains("D note.txt"));
    assert!(!root.join("note.txt").exists());
}

/// Verifies the built-in apply-patch tool can rename an existing file.
#[tokio::test]
async fn apply_patch_moves_a_file() {
    let root = temp_root("apply-patch-move");
    fs::write(root.join("from.txt"), "hello\ntools\n").unwrap();
    let router = ToolRouter::from_path(&root).await;

    let output = router
        .dispatch(
            ToolCallRequest::new(
                "call-move",
                "apply_patch",
                serde_json::json!({
                    "patchText": "*** Begin Patch\n*** Update File: from.txt\n*** Move to: to.txt\n@@\n hello\n tools\n*** End Patch"
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap();

    assert!(output.text.contains("M from.txt -> to.txt"));
    assert!(!root.join("from.txt").exists());
    assert_eq!(
        fs::read_to_string(root.join("to.txt")).unwrap(),
        "hello\ntools\n"
    );
}

/// Verifies a self-rename `Move to` header behaves like a normal in-place update.
#[tokio::test]
async fn apply_patch_ignores_move_to_when_source_equals_target() {
    let root = temp_root("apply-patch-move-same-path");
    fs::write(root.join("same.txt"), "hello\ntools\n").unwrap();
    let router = ToolRouter::from_path(&root).await;

    let output = router
        .dispatch(
            ToolCallRequest::new(
                "call-move-same-path",
                "apply_patch",
                serde_json::json!({
                    "patchText": "*** Begin Patch\n*** Update File: same.txt\n*** Move to: same.txt\n@@\n hello\n-tools\n+world\n*** End Patch"
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap();

    assert!(output.text.contains("M same.txt"));
    assert!(root.join("same.txt").exists());
    assert_eq!(
        fs::read_to_string(root.join("same.txt")).unwrap(),
        "hello\nworld\n"
    );
}

/// Verifies the tool accepts `patchText` and returns spec-shaped structured metadata.
#[tokio::test]
async fn apply_patch_accepts_patch_text_and_returns_metadata() {
    let root = temp_root("apply-patch-patch-text-metadata");
    let router = ToolRouter::from_path(&root).await;

    let output = router
        .dispatch(
            ToolCallRequest::new(
                "call-patch-text-metadata",
                "apply_patch",
                serde_json::json!({
                    "patchText": "*** Begin Patch\n*** Add File: hello.txt\n+hello metadata\n*** End Patch"
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap();

    assert!(
        output
            .text
            .contains("Success. Updated the following files:")
    );
    assert_eq!(
        output.structured.to_serde_value()["title"],
        "Success. Updated the following files:"
    );
    assert_eq!(
        output.structured.to_serde_value()["metadata"]["files"][0]["relativePath"],
        "hello.txt"
    );
    assert_eq!(
        output.structured.to_serde_value()["metadata"]["files"][0]["type"],
        "add"
    );
    assert!(
        output.structured.to_serde_value()["metadata"]["diff"]
            .as_str()
            .unwrap()
            .contains("hello metadata")
    );
}

/// Verifies dry-run validation keeps file changes atomic when any hunk fails.
#[tokio::test]
async fn apply_patch_is_atomic_when_verification_fails() {
    let root = temp_root("apply-patch-atomic");
    let router = ToolRouter::from_path(&root).await;

    let error = router
        .dispatch(
            ToolCallRequest::new(
                "call-atomic-failure",
                "apply_patch",
                serde_json::json!({
                    "patchText": "*** Begin Patch\n*** Add File: created.txt\n+created before failure\n*** Update File: missing.txt\n@@\n-missing\n+replacement\n*** End Patch"
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("Failed to read file to update"));
    assert!(!root.join("created.txt").exists());
}

/// Verifies wrapper text plus fuzzy matching can still update the expected file content.
#[tokio::test]
async fn apply_patch_extracts_embedded_patch_and_fuzzy_matches_lines() {
    let root = temp_root("apply-patch-embedded-fuzzy");
    fs::write(root.join("quote.txt"), "greet(“hi”);   \n").unwrap();
    let router = ToolRouter::from_path(&root).await;

    let output = router
        .dispatch(
            ToolCallRequest::new(
                "call-embedded-fuzzy",
                "apply_patch",
                serde_json::json!({
                    "patchText": "apply_patch <<'EOF'\n*** Begin Patch\n*** Update File: quote.txt\n@@ greet(\"hi\");\n-greet(\"hi\");\n+greet(\"bye\");\n*** End Patch\nEOF"
                }),
            ),
            ToolContext::new("session-1", "thread-1"),
        )
        .await
        .unwrap();

    assert!(output.text.contains("M quote.txt"));
    assert_eq!(
        fs::read_to_string(root.join("quote.txt")).unwrap(),
        "greet(\"bye\");\n"
    );
}

/// Creates a unique temporary directory rooted under the OS temp directory.
fn temp_root(prefix: &str) -> PathBuf {
    let mut root = std::env::temp_dir();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_nanos();
    root.push(format!(
        "clawcode-tools-test-{prefix}-{pid}-{timestamp}",
        pid = id()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

/// Writes an executable shell script at `path` for tests that need deterministic shell commands.
fn write_executable_script(path: &std::path::Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

/// Creates a shell wrapper that prepends the test root to PATH before delegating to `/bin/sh`.
fn write_shell_with_local_path(root: &std::path::Path) -> String {
    let shell = root.join("test-shell");
    write_executable_script(
        &shell,
        "#!/bin/sh\nPATH=\"$(pwd):$PATH\"\nexport PATH\nexec /bin/sh \"$@\"\n",
    );
    shell.to_string_lossy().into_owned()
}
