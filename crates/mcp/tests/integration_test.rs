//! Integration tests for the MCP client module.
//!
//! Uses fixture MCP servers (EchoServer, CalcServer, EmptyServer) over
//! in-memory duplex channels to test the full client lifecycle.

mod fixtures;

use fixtures::{CalcServer, EchoServer, EmptyServer, spawn_server};

/// Filter text content from a CallToolResult.
fn extract_text(result: rmcp::model::CallToolResult) -> String {
    result
        .content
        .into_iter()
        .filter_map(|c| match c.raw {
            rmcp::model::RawContent::Text(t) => Some(t.text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn connect_to_echo_server_lists_one_tool() {
    let (transport, _guard) = spawn_server(EchoServer);

    let client = rmcp::serve_client(mcp::Handler, transport)
        .await
        .expect("handshake should succeed");

    let result = client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    assert_eq!(result.tools.len(), 1);
    assert_eq!(result.tools[0].name, "echo");
}

#[tokio::test]
async fn call_echo_tool_returns_message() {
    let (transport, _guard) = spawn_server(EchoServer);

    let client = rmcp::serve_client(mcp::Handler, transport)
        .await
        .expect("handshake should succeed");

    let params = rmcp::model::CallToolRequestParams::new("echo").with_arguments(
        serde_json::Map::from_iter([(
            "message".to_string(),
            serde_json::Value::String("hello world".into()),
        )]),
    );

    let result = client
        .call_tool(params)
        .await
        .expect("call_tool should succeed");
    assert_eq!(extract_text(result), "hello world");
}

#[tokio::test]
async fn calc_server_lists_two_tools() {
    let (transport, _guard) = spawn_server(CalcServer);

    let client = rmcp::serve_client(mcp::Handler, transport)
        .await
        .expect("handshake should succeed");

    let result = client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    assert_eq!(result.tools.len(), 2);

    let names: Vec<&str> = result.tools.iter().map(|t| t.name.as_ref()).collect();
    assert!(names.contains(&"add"));
    assert!(names.contains(&"multiply"));
}

#[tokio::test]
async fn call_add_tool_returns_sum() {
    let (transport, _guard) = spawn_server(CalcServer);

    let client = rmcp::serve_client(mcp::Handler, transport)
        .await
        .expect("handshake should succeed");

    let params = rmcp::model::CallToolRequestParams::new("add").with_arguments(
        serde_json::Map::from_iter([
            ("a".to_string(), serde_json::json!(3)),
            ("b".to_string(), serde_json::json!(7)),
        ]),
    );

    let result = client
        .call_tool(params)
        .await
        .expect("call_tool should succeed");
    assert_eq!(extract_text(result), "10");
}

#[tokio::test]
async fn empty_server_has_no_tools() {
    let (transport, _guard) = spawn_server(EmptyServer);

    let client = rmcp::serve_client(mcp::Handler, transport)
        .await
        .expect("handshake should succeed");

    let result = client
        .list_tools(None)
        .await
        .expect("list_tools should succeed");
    assert!(result.tools.is_empty());
}
