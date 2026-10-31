use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use protocol::{
    ContentBlock, SessionId, ToolCall, ToolCallId, ToolDefinition, ToolResult,
    TurnId,
};
use tokio_util::sync::CancellationToken;
use tools::{
    AgentTool, BuiltinToolFactory, DiscardToolUpdates, ToolError,
    ToolExecutionContext, ToolFactory, ToolRegistry,
};

/// Creates one correlated tool call with explicit JSON arguments.
fn tool_call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall::builder()
        .tool_call_id(
            ToolCallId::try_from("tool-call-1").expect("tool call id"),
        )
        .name(name.to_string())
        .arguments(arguments)
        .build()
}

/// Returns a direct execution context rooted at the process directory.
fn execution_context() -> ToolExecutionContext {
    ToolExecutionContext::builder()
        .session_id(SessionId::try_from("session-1").expect("session id"))
        .turn_id(TurnId::try_from("turn-1").expect("turn id"))
        .trace_id(protocol::TraceId::try_from("trace-1").expect("trace id"))
        .cwd(std::env::current_dir().expect("current directory"))
        .cancellation(CancellationToken::new())
        .updates(Arc::new(DiscardToolUpdates))
        .build()
}

/// Tool that exposes which immutable registry snapshot executed it.
struct TaggedTool(&'static str);

#[async_trait]
impl AgentTool for TaggedTool {
    /// Returns one stable test-tool definition.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "tagged".to_string(),
            description: "Return the registry snapshot tag".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    /// Returns this tool instance's immutable tag.
    async fn execute(
        &self,
        call: ToolCall,
        _context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![ContentBlock::Text {
                text: self.0.to_string(),
            }])
            .is_error(false)
            .build())
    }
}

/// Built-in tools reject malformed arguments before side-effectful execution.
#[test]
fn builtins_validate_arguments_during_preflight() {
    let registry = BuiltinToolFactory::new().create().expect("built-in tools");

    assert!(
        registry
            .validate(&tool_call("bash", serde_json::json!({})))
            .is_err()
    );
    assert!(
        registry
            .validate(&tool_call(
                "read",
                serde_json::json!({ "path": "README.md", "offset": "bad" }),
            ))
            .is_err()
    );
}

/// Cloned registries preserve old tool implementations after later upserts.
#[tokio::test]
async fn registry_clones_preserve_immutable_turn_snapshots() {
    let mut initial = ToolRegistry::default();
    initial
        .register(Arc::new(TaggedTool("old")))
        .expect("register old tool");
    let old_snapshot = initial.clone();

    initial.upsert(Arc::new(TaggedTool("new")));
    let new_snapshot = initial;
    let call = tool_call("tagged", serde_json::json!({}));
    let context = execution_context();

    let old = old_snapshot
        .execute(call.clone(), &context)
        .await
        .expect("old execution");
    let new = new_snapshot
        .execute(call.clone(), &context)
        .await
        .expect("new execution");
    assert_eq!(old.blocks[0].text(), Some("old"));
    assert_eq!(new.blocks[0].text(), Some("new"));
}

/// Records expensive definition construction at the public Tool boundary.
struct CountedTool {
    name: &'static str,
    definitions: Arc<AtomicUsize>,
}

#[async_trait]
impl AgentTool for CountedTool {
    /// Materializes a schema to expose accidental definition work during composition.
    fn definition(&self) -> ToolDefinition {
        self.definitions.fetch_add(1, Ordering::Relaxed);
        ToolDefinition {
            name: self.name.to_string(),
            description: "Count definition materialization".to_string(),
            parameters: serde_json::json!({ "type": "object", "description": "x".repeat(4096) }),
        }
    }

    /// Registry composition must never execute the Tool.
    async fn execute(
        &self,
        _call: ToolCall,
        _context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        unreachable!("composition does not execute Tools")
    }
}

/// Existing registry keys must survive overlay, subset, and merge without new schemas.
#[test]
fn registry_composition_reuses_registered_names() {
    let definitions = Arc::new(AtomicUsize::new(0));
    let mut source = ToolRegistry::default();
    for name in ["first", "second"] {
        source
            .register(Arc::new(CountedTool {
                name,
                definitions: Arc::clone(&definitions),
            }))
            .expect("register Tool");
    }
    definitions.store(0, Ordering::Relaxed);
    let mut available = ToolRegistry::default();
    available.overlay(&source);
    let selected = available.subset(["first", "second"]).expect("subset");
    let mut merged = ToolRegistry::default();
    merged.merge(selected).expect("merge");
    assert_eq!(merged.names(), vec!["first", "second"]);
    assert_eq!(
        definitions.load(Ordering::Relaxed),
        0,
        "composition must reuse known keys"
    );
}

/// Reusing keys preserves override order and never overwrites a rejected duplicate.
#[tokio::test]
async fn registry_composition_preserves_duplicate_and_override_semantics() {
    let mut target = ToolRegistry::default();
    target
        .register(Arc::new(TaggedTool("old")))
        .expect("old Tool");
    let mut incoming = ToolRegistry::default();
    incoming
        .register(Arc::new(TaggedTool("new")))
        .expect("new Tool");
    assert!(
        matches!(target.merge(incoming.clone()), Err(ToolError::Duplicate(name)) if name == "tagged")
    );
    let context = execution_context();
    let call = tool_call("tagged", serde_json::json!({}));
    assert_eq!(
        target
            .execute(call.clone(), &context)
            .await
            .expect("retained Tool")
            .blocks[0]
            .text(),
        Some("old")
    );
    target.overlay(&incoming);
    let selected = target.subset(["tagged"]).expect("selected Tool");
    assert_eq!(
        selected
            .execute(call, &context)
            .await
            .expect("overridden Tool")
            .blocks[0]
            .text(),
        Some("new")
    );
    assert!(
        matches!(target.subset(["tagged", "tagged"]), Err(ToolError::Duplicate(name)) if name == "tagged")
    );
    assert!(
        matches!(target.subset(["missing"]), Err(ToolError::NotFound(name)) if name == "missing")
    );
}

/// File tools report the path execution resolves, not the raw argument text.
#[tokio::test]
async fn file_tools_report_paths_resolved_like_execution() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("page.txt"), "page").expect("page");
    let registry = BuiltinToolFactory::new().create().expect("registry");
    let home = dirs::home_dir().expect("home directory");
    let file_url = url::Url::from_file_path(root.path().join("new.txt"))
        .expect("file URL")
        .to_string();
    for (call, path, line) in [
        (
            tool_call(
                "read",
                serde_json::json!({ "path": "@page.txt", "offset": 7 }),
            ),
            root.path().join("page.txt"),
            Some(7),
        ),
        (
            tool_call(
                "write",
                serde_json::json!({ "path": file_url, "content": "" }),
            ),
            root.path().join("new.txt"),
            None,
        ),
        (
            tool_call(
                "edit",
                serde_json::json!({ "path": "~/notes.md", "edits": [] }),
            ),
            home.join("notes.md"),
            None,
        ),
    ] {
        assert_eq!(
            registry.locations(&call, root.path()).await,
            vec![protocol::ToolLocation { path, line }],
            "{}",
            call.name
        );
    }
    assert!(
        registry
            .locations(
                &tool_call("bash", serde_json::json!({ "command": "ls" })),
                root.path(),
            )
            .await
            .is_empty()
    );
}
