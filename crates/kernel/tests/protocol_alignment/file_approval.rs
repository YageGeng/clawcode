//! Outside-workspace file mutations and the Run's user-wait lifecycle.

use std::path::PathBuf;
use std::sync::Arc;

use protocol::{
    AgentEventPayload, FileApprovalDecision, FileApprovalMode, MessageContent,
    ModelRequest, SessionId,
};

use super::fixture::{
    Route, TestKernel, calls, text, tool_failed, tool_result,
};

/// An older v4 reader can skip the policy, which survives navigation only in its owning Session.
#[tokio::test]
async fn file_approval_uses_v4_custom_entries_and_stays_with_its_session() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let session = test.create("file-policy").await;
    let cwd = test.root.path().to_path_buf();
    let path = test
        .kernel
        .resume_session(session.clone(), cwd.clone())
        .await
        .expect("session path");
    test.kernel
        .set_session_file_approval(&session, FileApprovalMode::Allow)
        .await
        .expect("allow");

    // A custom entry is an existing v4 envelope with an opaque application payload.
    let log = std::fs::read_to_string(&path).expect("session log");
    let change: serde_json::Value =
        serde_json::from_str(log.lines().last().expect("policy mutation"))
            .expect("json");
    assert_eq!(change["kind"], "entry");
    assert_eq!(change["type"], "custom");
    let leaf = test
        .kernel
        .session_tree(&session)
        .expect("tree")
        .leaf_id
        .expect("policy leaf");
    let fork = SessionId::try_from("file-policy-fork").expect("id");
    test.kernel
        .fork_session(&session, leaf, fork.clone(), cwd.clone())
        .await
        .expect("fork");
    assert_eq!(
        test.kernel
            .session_settings(&fork)
            .expect("fork settings")
            .file_approval,
        FileApprovalMode::Host
    );

    // Navigating to the root puts the setting outside every entry on the active branch.
    test.kernel
        .navigate_session(&session, None)
        .await
        .expect("navigate to root");
    test.kernel.close_session(&session).await.expect("close");
    test.kernel
        .resume_session(session.clone(), cwd.clone())
        .await
        .expect("resume on empty branch");
    assert_eq!(
        test.kernel
            .session_settings(&session)
            .expect("settings")
            .file_approval,
        FileApprovalMode::Allow
    );
    test.kernel
        .set_session_file_approval(&session, FileApprovalMode::Host)
        .await
        .expect("restore host policy");
    test.kernel.close_session(&session).await.expect("close");
    test.kernel
        .resume_session(session.clone(), cwd)
        .await
        .expect("resume after reset");
    assert_eq!(
        test.kernel
            .session_settings(&session)
            .expect("settings")
            .file_approval,
        FileApprovalMode::Host
    );
}

/// Reusing a deleted ancestor's name cannot reactivate its grant through a fork.
#[tokio::test]
async fn reused_ancestor_id_does_not_restore_file_approval() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let ancestor = test.create("reused-policy").await;
    let child = SessionId::try_from("reused-policy-child").unwrap();
    let cwd = test.root.path().to_path_buf();
    test.kernel
        .set_session_file_approval(&ancestor, FileApprovalMode::Allow)
        .await
        .unwrap();
    let leaf = test
        .kernel
        .session_tree(&ancestor)
        .unwrap()
        .leaf_id
        .unwrap();
    let fork_path = test
        .kernel
        .fork_session(&ancestor, leaf.clone(), child.clone(), cwd.clone())
        .await
        .unwrap();
    // Older v4 forks copied the complete owner field; the next fork must also
    // detach those entries when its source has already been written to disk.
    test.kernel.close_session(&child).await.unwrap();
    let restore_old_owner = |path: &std::path::Path| {
        let old_fork = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| {
                let mut entry: serde_json::Value =
                    serde_json::from_str(line).unwrap();
                if entry["id"].as_str() == Some(leaf.as_str()) {
                    entry["data"]["sessionId"] = serde_json::json!(ancestor);
                }
                entry.to_string() + "\n"
            })
            .collect::<String>();
        std::fs::write(path, old_fork).unwrap();
    };
    restore_old_owner(&fork_path);
    test.kernel
        .resume_session(child.clone(), cwd.clone())
        .await
        .unwrap();
    test.kernel.delete_session(&ancestor).await.unwrap();
    let reused_path = test
        .kernel
        .fork_session(&child, leaf.clone(), ancestor.clone(), cwd.clone())
        .await
        .unwrap();
    assert_eq!(
        test.kernel
            .session_settings(&ancestor)
            .unwrap()
            .file_approval,
        FileApprovalMode::Host
    );
    test.kernel.close_session(&ancestor).await.unwrap();
    // Also upgrade a grandchild that already had a colliding owner on disk.
    restore_old_owner(&reused_path);
    test.kernel
        .resume_session(ancestor.clone(), cwd.clone())
        .await
        .unwrap();
    assert_eq!(
        test.kernel
            .session_settings(&ancestor)
            .unwrap()
            .file_approval,
        FileApprovalMode::Host
    );
    test.kernel
        .set_session_file_approval(&ancestor, FileApprovalMode::Allow)
        .await
        .unwrap();
    test.kernel.close_session(&ancestor).await.unwrap();
    test.kernel
        .resume_session(ancestor.clone(), cwd)
        .await
        .unwrap();
    assert_eq!(
        test.kernel
            .session_settings(&ancestor)
            .unwrap()
            .file_approval,
        FileApprovalMode::Allow
    );
}

/// A policy written by the N2 preview can be reopened and superseded by a host default.
#[tokio::test]
async fn file_approval_recovers_preview_records_and_later_changes() {
    use std::io::Write;

    let test = TestKernel::new(|_| Route::answer("ok"));
    let session = test.create("file-policy-preview").await;
    let cwd = test.root.path().to_path_buf();
    let path = test
        .kernel
        .resume_session(session.clone(), cwd.clone())
        .await
        .expect("session path");
    test.kernel
        .set_session_file_approval(&session, FileApprovalMode::Allow)
        .await
        .expect("set policy before opening in the preview");
    test.kernel.close_session(&session).await.expect("close");
    let log = std::fs::read_to_string(&path).expect("session log");
    let last: serde_json::Value =
        serde_json::from_str(log.lines().last().expect("last mutation"))
            .expect("json");
    let legacy = serde_json::json!({
        "kind": "record", "type": "file_approval_mode", "id": "preview-setting",
        "lane": "main", "seq": last["seq"].as_u64().expect("sequence") + 1,
        "timestamp": "1", "mode": "ask"
    });
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append"),
        "{legacy}"
    )
    .expect("preview record");

    test.kernel
        .resume_session(session.clone(), cwd.clone())
        .await
        .expect("resume preview");
    assert_eq!(
        test.kernel
            .session_settings(&session)
            .expect("settings")
            .file_approval,
        FileApprovalMode::Ask
    );
    test.kernel
        .set_session_file_approval(&session, FileApprovalMode::Host)
        .await
        .expect("use host");
    test.kernel.close_session(&session).await.expect("close");
    test.kernel
        .resume_session(session.clone(), cwd)
        .await
        .expect("resume");
    assert_eq!(
        test.kernel
            .session_settings(&session)
            .expect("settings")
            .file_approval,
        FileApprovalMode::Host
    );
}

/// Scripts an outside write and edit in parallel with an inside write.
fn file_route(outside: PathBuf) -> impl Fn(&ModelRequest) -> Route {
    move |request| {
        // The kernel can append a hidden roster note after the parent's prompt.
        let last = request.messages.iter().rfind(|message| {
            !matches!(&message.content, MessageContent::Extension { extension } if !extension.display)
        });
        if last.map(|m| text(&m.content)).as_deref() == Some("change files") {
            calls(&[
                (
                    "write-outside",
                    "write",
                    serde_json::json!({"path": outside.join("new/file"), "content": "created"}),
                ),
                (
                    "edit-outside",
                    "edit",
                    serde_json::json!({"path": outside.join("existing"), "oldText": "old", "newText": "new"}),
                ),
                (
                    "write-inside",
                    "write",
                    serde_json::json!({"path": "inside", "content": "workspace"}),
                ),
            ])
        } else {
            Route::answer("done")
        }
    }
}

/// Granting external access preserves the host's isolated saves for workspace files.
#[tokio::test]
async fn file_approval_allow_preserves_workspace_write_and_edit_aliases() {
    let test = TestKernel::new(file_route(PathBuf::from(".")));
    let session = test.create("workspace-file-aliases").await;
    let backups = tempfile::tempdir().expect("backup directory");
    std::fs::create_dir(test.root.path().join("new")).expect("new directory");
    for (path, backup) in [("new/file", "write"), ("existing", "edit")] {
        let original = test.root.path().join(path);
        std::fs::write(&original, "old").expect("original");
        std::fs::hard_link(original, backups.path().join(backup))
            .expect("backup");
    }
    test.kernel
        .set_session_file_approval(&session, FileApprovalMode::Allow)
        .await
        .expect("allow external mutations");
    test.run_text(&session, "change files").await.expect("run");
    for (path, backup, outcome) in [
        ("new/file", "write", "created"),
        ("existing", "edit", "new"),
    ] {
        assert_eq!(
            std::fs::read(backups.path().join(backup)).expect("backup"),
            b"old"
        );
        // Platforms without isolated replacement refuse the shared file.
        let expected = if cfg!(target_os = "linux") {
            outcome
        } else {
            "old"
        };
        assert_eq!(
            std::fs::read_to_string(test.root.path().join(path))
                .expect("workspace file"),
            expected
        );
    }
}

/// Suspends the batch after dispatch begins, while a second client changes the policy.
#[derive(Clone, Default)]
struct HoldFileCall {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl extension::ExtensionHandler<extension::ToolCallPoint> for HoldFileCall {
    /// Lets the client act before the built-in tools resolve their final arguments.
    async fn handle(
        &self,
        event: &protocol::ToolCallEvent,
        _context: &extension::ExtensionContext,
    ) -> Result<protocol::ToolCallResult, extension::ExtensionError> {
        if event.call.tool_call_id.as_str() == "write-inside" {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(protocol::ToolCallResult::Continue)
    }
}

/// Each external call observes the latest Session choice when it needs authorization.
#[tokio::test]
async fn file_approval_changes_apply_to_external_calls_in_an_active_batch() {
    for (before, after, ask) in [
        (FileApprovalMode::Allow, FileApprovalMode::Ask, true),
        (FileApprovalMode::Allow, FileApprovalMode::Host, true),
        (FileApprovalMode::Host, FileApprovalMode::Allow, false),
    ] {
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("existing"), "old")
            .expect("original");
        let pause = HoldFileCall::default();
        let hook = pause.clone();
        let test = TestKernel::with_extensions(
            file_route(outside.path().into()),
            super::insertion::hooks(move |registrar| {
                registrar.on::<extension::ToolCallPoint, _>(hook.clone())
            }),
        );
        let session = test.create("change-file-policy").await;
        test.kernel
            .set_session_file_approval(&session, before)
            .await
            .expect("initial policy");
        let run = test.start(&session, "change files");
        pause.entered.notified().await;
        test.kernel
            .set_session_file_approval(&session, after)
            .await
            .expect("new policy");
        pause.release.notify_one();
        test.wait_until(async || {
            run.is_finished()
                || test.kernel.file_approvals(&session).expect("pending").len()
                    == 2
        })
        .await;
        let pending = test.kernel.file_approvals(&session).expect("pending");
        assert_eq!(
            pending.len(),
            if ask { 2 } else { 0 },
            "{before:?} to {after:?}"
        );
        for request in pending {
            test.kernel
                .respond_file_approval(
                    &session,
                    &request.request_id,
                    FileApprovalDecision::Reject,
                )
                .expect("reject");
        }
        run.await.expect("joined").expect("settled");
        assert_eq!(outside.path().join("new/file").exists(), !ask);
        assert_eq!(
            std::fs::read_to_string(outside.path().join("existing"))
                .expect("external edit"),
            if ask { "old" } else { "new" }
        );
    }
}

/// A child can finish its batch and hand external mutations back to its parent for approval.
#[tokio::test]
async fn child_external_mutations_require_a_parent_retry() {
    let outside = tempfile::tempdir().expect("outside directory");
    let directory = outside.path().canonicalize().expect("outside path");
    let existing = directory.join("existing");
    let new = directory.join("new/file");
    std::fs::write(&existing, "old").expect("original");
    let test = TestKernel::new(file_route(directory));
    let parent = test.create("file-approval-parent").await;
    let child = test
        .kernel
        .create_owned_session(
            kernel::OwnedSessionRequest::builder()
                .ownership(protocol::SessionOwnership {
                    session_id: parent.clone(),
                    name: protocol::AgentName::try_from("worker")
                        .expect("name"),
                    nickname: "Euclid".into(),
                })
                .title(
                    protocol::SessionTitle::try_from("Euclid").expect("title"),
                )
                .model(None)
                .build(),
        )
        .await
        .expect("child");

    let run = test.start(&child, "change files");
    test.wait_until(async || {
        run.is_finished()
            || !test
                .kernel
                .file_approvals(&child)
                .expect("pending")
                .is_empty()
    })
    .await;
    assert!(
        test.kernel
            .file_approvals(&child)
            .expect("pending")
            .is_empty(),
        "a child must return a tool error without waiting for a user decision"
    );
    run.await.expect("joined").expect("child settled");
    assert!(!test.sinks.saw(&child, |event| matches!(
        event,
        AgentEventPayload::FileApprovalRequested { .. }
            | AgentEventPayload::ActionRequired { .. }
    )));
    for (id, action, target) in [
        ("write-outside", "write", &new),
        ("edit-outside", "edit", &existing),
    ] {
        assert!(tool_failed(&test, &child, id));
        let (message, _) = tool_result(&test, &child, id);
        assert!(
            message.contains(action)
                && message.contains(target.to_str().expect("path")),
            "{message}"
        );
        assert!(
            message.contains("parent") && message.contains("approval"),
            "{message}"
        );
    }
    assert!(!tool_failed(&test, &child, "write-inside"));
    assert_eq!(
        std::fs::read_to_string(test.root.path().join("inside"))
            .expect("workspace write"),
        "workspace"
    );
    assert!(!new.parent().expect("new directory").exists());
    assert_eq!(
        std::fs::read_to_string(&existing).expect("unchanged"),
        "old"
    );

    // The same mutations on the parent have their own approval requests before any external I/O.
    let retry = test.start(&parent, "change files");
    test.wait_until(async || {
        retry.is_finished()
            || test.kernel.file_approvals(&parent).expect("pending").len() == 2
    })
    .await;
    let approvals = test.kernel.file_approvals(&parent).expect("pending");
    assert_eq!(approvals.len(), 2);
    assert!(!new.parent().expect("new directory").exists());
    assert_eq!(
        std::fs::read_to_string(&existing).expect("pending edit"),
        "old"
    );
    for request in approvals {
        test.kernel
            .respond_file_approval(
                &parent,
                &request.request_id,
                FileApprovalDecision::Allow,
            )
            .expect("allow parent call");
    }
    retry.await.expect("joined").expect("parent settled");
    assert_eq!(
        std::fs::read_to_string(new).expect("approved write"),
        "created"
    );
    assert_eq!(
        std::fs::read_to_string(existing).expect("approved edit"),
        "new"
    );
}

/// Stopping a Run with two pending file approvals preserves both external targets.
#[tokio::test]
async fn cancelling_parallel_file_approvals_preserves_external_files() {
    let outside = tempfile::tempdir().expect("outside directory");
    std::fs::write(outside.path().join("existing"), "old").expect("original");
    let test = TestKernel::new(file_route(outside.path().into()));
    let session = test.create("cancel-file-approvals").await;
    let run = test.start(&session, "change files");
    test.wait_until(async || {
        run.is_finished()
            || (test.root.path().join("inside").exists()
                && test.kernel.file_approvals(&session).expect("pending").len()
                    == 2
                && test.sinks.saw(&session, |event| {
                    matches!(event, AgentEventPayload::ActionRequired { .. })
                }))
    })
    .await;
    assert!(
        test.sinks.saw(&session, |e| matches!(
            e,
            AgentEventPayload::ActionRequired { .. }
        )),
        "external tools must pause the Run for approval"
    );
    let stale = test.kernel.file_approvals(&session).expect("pending");
    assert_eq!(stale.len(), 2);
    test.kernel.cancel_session(&session).expect("cancel");
    let _outcome = run.await.expect("joined");
    assert!(
        test.kernel
            .file_approvals(&session)
            .expect("pending")
            .is_empty()
    );
    // Each pending request is withdrawn, and the last one clears requires_action.
    let payloads = test.sinks.payloads(&session);
    for request in &stale {
        assert!(payloads.iter().any(|event| matches!(event,
            AgentEventPayload::FileApprovalResolved { request_id, decision: FileApprovalDecision::Cancelled } if request_id == &request.request_id)));
    }
    assert_eq!(
        payloads
            .iter()
            .filter(|event| matches!(
                event,
                AgentEventPayload::ActionRequired { .. }
            ))
            .count(),
        1
    );
    assert_eq!(
        payloads
            .iter()
            .filter(|event| matches!(
                event,
                AgentEventPayload::ActionResolved { .. }
            ))
            .count(),
        1
    );
    assert!(!outside.path().join("new").exists());
    assert_eq!(
        std::fs::read_to_string(outside.path().join("existing"))
            .expect("untouched"),
        "old"
    );
}

/// Old answers cannot grant permission to a new tool invocation with the same model id.
#[tokio::test]
async fn repeated_file_calls_require_independent_decisions() {
    let outside = tempfile::tempdir().expect("outside directory");
    std::fs::write(outside.path().join("existing"), "old").expect("original");
    let test = TestKernel::new(file_route(outside.path().into()));
    let session = test.create("repeat-file-approvals").await;
    let mut previous: Vec<String> = Vec::new();
    for _ in 0..2 {
        let run = test.start(&session, "change files");
        test.wait_until(async || {
            test.kernel.file_approvals(&session).expect("pending").len() == 2
        })
        .await;
        let current = test.kernel.file_approvals(&session).expect("pending");
        for stale in &previous {
            assert!(matches!(
                test.kernel.respond_file_approval(
                    &session,
                    stale,
                    FileApprovalDecision::Allow
                ),
                Err(kernel::KernelError::FileApprovalResolved(_))
            ));
        }
        for request in current {
            test.kernel
                .respond_file_approval(
                    &session,
                    &request.request_id,
                    FileApprovalDecision::Reject,
                )
                .expect("reject");
            previous.push(request.request_id);
        }
        run.await.expect("joined").expect("settled");
        assert!(!outside.path().join("new").exists());
        assert_eq!(
            std::fs::read_to_string(outside.path().join("existing"))
                .expect("untouched"),
            "old"
        );
    }
}

/// An unrepresentable external target fails the tool instead of stranding the Run on a prompt.
#[cfg(unix)]
#[tokio::test]
async fn external_path_that_acp_cannot_represent_fails_closed() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;
    use std::os::unix::fs::symlink;
    let outside = tempfile::tempdir().expect("outside directory");
    let target = outside.path().join(OsString::from_vec(vec![0xff]));
    let test = TestKernel::new(|request| {
        if request.messages.last().map(|m| text(&m.content)).as_deref()
            == Some("write link")
        {
            calls(&[(
                "write-link",
                "write",
                serde_json::json!({"path":"alias", "content":"external"}),
            )])
        } else {
            Route::answer("done")
        }
    });
    let session = test.create("unrepresentable-file-approval").await;
    symlink(&target, test.root.path().join("alias")).expect("alias");
    let run = test.start(&session, "write link");
    test.wait_until(async || {
        run.is_finished()
            || !test
                .kernel
                .file_approvals(&session)
                .expect("pending")
                .is_empty()
    })
    .await;
    assert!(
        test.kernel
            .file_approvals(&session)
            .expect("pending")
            .is_empty(),
        "an unreadable permission must not wait for a client"
    );
    run.await.expect("joined").expect("settled");
    assert!(super::fixture::tool_failed(&test, &session, "write-link"));
    assert!(!target.exists());
}
