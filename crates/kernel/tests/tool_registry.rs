use kernel::{
    session::{SessionId, ThreadId},
    tools::{
        Tool, ToolCallRequest, ToolContext,
        builtin::default_read_only_tools,
        builtin::{JsonTool, TimeTool},
        executor::ToolExecutor,
        registry::ToolRegistry,
    },
};

#[tokio::test]
async fn registry_exposes_sorted_definitions_and_executes_echo() {
    let registry = ToolRegistry::default();
    for tool in default_read_only_tools() {
        registry.register_arc(tool).await;
    }

    let definitions = registry.definitions().await;
    let names = definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["echo", "json", "time"]);

    let results = ToolExecutor::execute_all(
        &registry,
        vec![ToolCallRequest::new(
            "call_1",
            "echo",
            serde_json::json!({"text": "hello"}),
        )],
        ToolContext::new(SessionId::new(), ThreadId::new()),
    )
    .await
    .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].output.text, "hello");
}

#[tokio::test]
async fn json_tool_schema_is_typed_and_formats_json_string_input() {
    let tool = JsonTool;
    let parameters = tool.parameters();

    assert_eq!(parameters["type"], "object");
    assert_eq!(parameters["properties"]["value"]["type"], "string");

    let output = tool
        .execute(
            serde_json::json!({"value": "{\"hello\":\"world\",\"count\":2}"}),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await
        .unwrap();

    assert!(output.text.contains("\"hello\": \"world\""));
    assert_eq!(output.structured["count"], 2);
}

#[tokio::test]
async fn time_tool_returns_local_time_text_and_keeps_unix_seconds() {
    let tool = TimeTool;

    let output = tool
        .execute(
            serde_json::json!({}),
            ToolContext::new(SessionId::new(), ThreadId::new()),
        )
        .await
        .unwrap();

    assert!(output.text.contains('-'));
    assert!(output.text.contains(':'));
    assert_eq!(
        output.structured["local_time"],
        serde_json::Value::String(output.text.clone())
    );
    assert!(output.structured["unix_seconds"].is_u64());
}
