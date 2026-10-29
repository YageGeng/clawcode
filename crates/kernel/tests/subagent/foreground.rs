use kernel::{AnswerOutcome, WhenBusy};
use protocol::{
    ContentBlock, MessageContent, ModelStreamEvent, SessionOwnership,
    StopReason, ToolCall, ToolCallId, ToolResultDetails,
};

use super::fixture::{Route, TestKernel, finished, request, text};

/// Overrides the built-in delegation name through the real extension registry.
struct OverrideSubagent;
impl extension::ExtensionModule for OverrideSubagent {
    /// Identifies the custom delegation extension.
    fn descriptor(&self) -> protocol::ExtensionDescriptor {
        protocol::ExtensionDescriptor {
            id: protocol::ExtensionId::try_from("custom-subagent").expect("id"),
            name: "Custom subagent".into(),
            version: "1".into(),
        }
    }
    /// Registers the replacement before Kernel built-ins are installed.
    fn register(
        &self,
        registrar: &mut extension::ExtensionRegistrar,
    ) -> Result<(), extension::ExtensionError> {
        registrar.register_tool(std::sync::Arc::new(Self))
    }
}
#[async_trait::async_trait]
impl tools::AgentTool for OverrideSubagent {
    /// Advertises the built-in name to exercise extension precedence.
    fn definition(&self) -> protocol::ToolDefinition {
        protocol::ToolDefinition {
            name: "subagent".into(),
            description: "Custom delegate".into(),
            parameters: serde_json::json!({"type":"object"}),
        }
    }
    /// Returns a distinguishable extension result without creating a child.
    async fn execute(
        &self,
        call: ToolCall,
        _context: &tools::ToolExecutionContext,
    ) -> Result<protocol::ToolResult, tools::ToolError> {
        Ok(protocol::ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![ContentBlock::Text {
                text: "extension".into(),
            }])
            .is_error(false)
            .build())
    }
}

/// Installed extensions retain ownership of a delegation tool's name.
#[tokio::test]
async fn extension_subagent_tool_wins() {
    let extensions =
        std::sync::Arc::new(extension::StaticExtensionFactory::new(
            Default::default(),
            vec![std::sync::Arc::new(|| {
                Ok(std::sync::Arc::new(OverrideSubagent))
            })],
        ));
    let test = TestKernel::with_extensions(
        |r| {
            if r.messages
                .last()
                .is_some_and(|m| text(&m.content) == "delegate")
            {
                Route::Events(vec![
                    delegate("task", "call-1"),
                    finished(StopReason::ToolUse),
                ])
            } else {
                Route::answer("done")
            }
        },
        extensions,
    );
    let owner = test.create("owner").await;
    test.kernel
        .submit(request(&owner, "delegate", WhenBusy::FollowUp))
        .await
        .expect("run");
    assert!(
        test.kernel
            .session_transcript(&owner)
            .expect("transcript")
            .iter()
            .any(|m| matches!(m.content, MessageContent::ToolResult { .. })
                && m.content.text_content().as_deref() == Some("extension"))
    );
    assert!(test.owned_children(&owner).is_empty());
    test.kernel.close_session(&owner).await.expect("close");
}

/// Emits a tool call using the actual production registry and execution pipeline.
pub fn delegate(task: &str, call: &str) -> ModelStreamEvent {
    ModelStreamEvent::ToolCall(ToolCall {
        tool_call_id: ToolCallId::try_from(call).expect("call"),
        name: "subagent".into(),
        arguments: serde_json::json!({"task":task}),
    })
}

/// Foreground delegation persists independent history, answers, ownership, and tool exclusions.
#[tokio::test]
async fn subagent_returns_child_answer() {
    let test = TestKernel::new(|r| {
        match r.messages.last().map(|m| text(&m.content)).as_deref() {
            Some("delegate") => Route::Events(vec![
                delegate("Name three primes.", "call-1"),
                finished(StopReason::ToolUse),
            ]),
            Some("Name three primes.") => Route::answer("2, 3, 5"),
            _ => Route::answer("done"),
        }
    });
    let owner = test.create("owner").await;
    test.kernel
        .submit(request(&owner, "delegate", WhenBusy::FollowUp))
        .await
        .expect("submit");
    let children = test.owned_children(&owner);
    let child = children.first().expect("child");
    assert_eq!(children.len(), 1);
    assert_eq!(child.name.as_deref(), Some("subagent: Name three primes."));
    assert_eq!(
        child.ownership,
        Some(SessionOwnership::Tool {
            session_id: owner.clone(),
            tool_call_id: ToolCallId::try_from("call-1").expect("id")
        })
    );
    let transcript =
        test.kernel.session_transcript(&owner).expect("transcript");
    assert!(transcript.iter().any(|m| matches!(&m.content, MessageContent::ToolResult { blocks, is_error: false, details: Some(ToolResultDetails::Subagent { session_id }), .. } if session_id == &child.session_id && blocks == &vec![ContentBlock::Text { text: "2, 3, 5".into() }])));
    {
        let requests = test.model.requests.lock().expect("requests");
        let child_request = requests
            .iter()
            .find(|r| {
                r.messages
                    .last()
                    .is_some_and(|m| text(&m.content) == "Name three primes.")
            })
            .expect("child request");
        assert_eq!(
            child_request
                .messages
                .iter()
                .filter(|m| matches!(m.content, MessageContent::User { .. }))
                .count(),
            1
        );
        assert!(
            !child_request
                .messages
                .iter()
                .any(|m| text(&m.content) == "delegate")
        );
        assert!(!child_request.tools.iter().any(|t| {
            ["subagent", "background_subagent"].contains(&t.name.as_str())
        }));
    }

    test.kernel
        .session_transcript(&child.session_id)
        .expect("child live");
    test.kernel.close_session(&owner).await.expect("close");
}

/// Empty tasks cannot allocate child sessions.
#[tokio::test]
async fn blank_task_is_rejected_without_child() {
    let test = TestKernel::new(|r| {
        if r.messages
            .last()
            .is_some_and(|m| text(&m.content) == "delegate")
        {
            Route::Events(vec![
                delegate("  ", "call-1"),
                finished(StopReason::ToolUse),
            ])
        } else {
            Route::answer("done")
        }
    });
    let owner = test.create("owner").await;
    test.kernel
        .submit(request(&owner, "delegate", WhenBusy::FollowUp))
        .await
        .expect("submit");
    assert!(test.owned_children(&owner).is_empty());
    assert!(
        test.kernel
            .session_transcript(&owner)
            .expect("transcript")
            .iter()
            .any(|m| matches!(
                m.content,
                MessageContent::ToolResult {
                    is_error: false,
                    ..
                }
            ) && m.content.text_content().as_deref()
                == Some("subagent needs a task."))
    );
    test.kernel.close_session(&owner).await.expect("close");
}

/// Parallel calls from one Turn each receive isolated child history and answers.
#[tokio::test]
async fn parallel_calls_get_separate_children() {
    let test = TestKernel::new(|r| {
        match r.messages.last().map(|m| text(&m.content)).as_deref() {
            Some("delegate") => Route::Events(vec![
                delegate("one", "call-1"),
                delegate("two", "call-2"),
                finished(StopReason::ToolUse),
            ]),
            Some("one") => Route::answer("first child"),
            Some("two") => Route::answer("second child"),
            _ => Route::answer("done"),
        }
    });
    let owner = test.create("owner").await;
    test.kernel
        .submit(request(&owner, "delegate", WhenBusy::FollowUp))
        .await
        .expect("submit");
    assert_eq!(test.owned_children(&owner).len(), 2);
    let transcript =
        test.kernel.session_transcript(&owner).expect("transcript");
    for answer in ["first child", "second child"] {
        assert!(transcript.iter().any(|m| matches!(
            m.content,
            MessageContent::ToolResult {
                is_error: false,
                ..
            }
        )
            && m.content.text_content().as_deref() == Some(answer)));
    }
    test.kernel.close_session(&owner).await.expect("close");
}

/// Parent cancellation holds the run until the child's cancellation has settled.
#[tokio::test]
async fn aborting_parent_cancels_child() {
    cancellation_case("parent").await;
}

/// Stopping a child is a tool failure that permits its owner to continue.
#[tokio::test]
async fn stopping_child_directly_reports_stop() {
    cancellation_case("child").await;
}

/// Deleting a running child still releases its waiting parent with a stopped result.
#[tokio::test]
async fn deleting_child_during_call_reports_stop() {
    cancellation_case("delete").await;
}

/// Exercises the same live parent-child call with three externally observable cancellation paths.
async fn cancellation_case(action: &str) {
    let test = TestKernel::new(|r| {
        match r.messages.last().map(|m| text(&m.content)).as_deref() {
            Some("delegate") => Route::Events(vec![
                delegate("hang", "call-1"),
                finished(StopReason::ToolUse),
            ]),
            Some("hang") => Route::Hang,
            _ => Route::answer("ok"),
        }
    });
    let owner = test.create("owner").await;
    let kernel = test.kernel.clone();
    let input = request(&owner, "delegate", WhenBusy::FollowUp);
    let run = tokio::spawn(async move { kernel.submit(input).await });
    test.wait_until(|| test.model.last_texts().contains(&"hang".to_string()))
        .await;
    let child = test
        .owned_children(&owner)
        .first()
        .expect("child")
        .session_id
        .clone();
    match action {
        "parent" => test.kernel.cancel_session(&owner).expect("cancel parent"),
        "delete" => test
            .kernel
            .delete_session(&child)
            .await
            .expect("delete child"),
        _ => test.kernel.cancel_session(&child).expect("cancel child"),
    }
    let result = run.await.expect("join").expect("run");
    assert_eq!(
        result.outcome,
        if action == "parent" {
            AnswerOutcome::Cancelled
        } else {
            AnswerOutcome::Completed
        }
    );
    let expected = if action == "parent" {
        "Operation aborted"
    } else {
        "Subagent was stopped before answering."
    };
    assert!(
        test.kernel
            .session_transcript(&owner)
            .expect("transcript")
            .iter()
            .any(|m| matches!(
                m.content,
                MessageContent::ToolResult { is_error: true, .. }
            ) && m.content.text_content().as_deref()
                == Some(expected))
    );
    test.kernel.close_session(&owner).await.expect("close");
}
