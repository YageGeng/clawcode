//! Outside-workspace file mutations and the Run's user-wait lifecycle.

use std::path::PathBuf;

use protocol::{
    AgentEventPayload, FileApprovalDecision, MessageContent, ModelRequest,
};

use super::fixture::{
    Route, TestKernel, calls, text, tool_failed, tool_result,
};

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
