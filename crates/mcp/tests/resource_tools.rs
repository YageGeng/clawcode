use async_trait::async_trait;
use mcp::{
    McpClient, McpConnector, McpError, McpFactory, McpHost, RuntimeMcpServer,
    SessionMcpFactory,
};
use protocol::*;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tools::ToolExecutionContext;

#[allow(dead_code)]
mod session_support;

/// Deterministic remote catalog whose read log proves opaque URI routing.
struct Resources {
    catalog: McpCatalog,
}

#[async_trait]
impl McpConnector for Resources {
    /// Shares the catalog while each client retains its own configured route.
    async fn connect(
        &self,
        server: &RuntimeMcpServer,
        _session: &SessionId,
        _host: Arc<dyn McpHost>,
    ) -> Result<Arc<dyn McpClient>, McpError> {
        Ok(Arc::new(ResourceClient {
            server: server.server_id.clone(),
            catalog: self.catalog.clone(),
            capabilities: McpServerCapabilities::builder()
                .resources(true)
                .build(),
        }))
    }
}

/// Normalized reference client used to test real Session routing and model adapters.
struct ResourceClient {
    server: McpServerId,
    catalog: McpCatalog,
    capabilities: McpServerCapabilities,
}

#[async_trait]
impl McpClient for ResourceClient {
    /// Returns the structured owner.
    fn server_id(&self) -> &McpServerId {
        &self.server
    }
    /// Returns the negotiated reference revision.
    fn protocol(&self) -> McpProtocolVersion {
        McpProtocolVersion::V2025_11_25
    }
    /// Omits optional implementation metadata.
    fn server_info(&self) -> Option<&McpServerImplementation> {
        None
    }
    /// Declares resource capability even for an empty catalog.
    fn capabilities(&self) -> &McpServerCapabilities {
        &self.capabilities
    }
    /// Returns the complete startup snapshot.
    async fn discover(&self) -> Result<McpCatalog, McpError> {
        Ok(self.catalog.clone())
    }
    /// Resource discovery never invokes remote tools.
    async fn call_tool(
        &self,
        _request: McpToolRequest,
        _control: mcp::McpRequestControl,
    ) -> Result<McpToolResult, McpError> {
        unreachable!("tool invocation")
    }
    /// Prompt support is unrelated to this adapter.
    async fn get_prompt(
        &self,
        _request: McpPromptRequest,
    ) -> Result<McpPromptResult, McpError> {
        unreachable!("prompt")
    }
    /// Echoes the exact remote URI to prove no host filesystem or HTTP lookup occurred.
    async fn read_resource(
        &self,
        request: McpResourceRequest,
        _cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<McpResourceResult, McpError> {
        Ok(McpResourceResult {
            contents: vec![ContentBlock::Text {
                text: request.reference.remote_uri,
            }],
        })
    }
    /// Completion support is unrelated to this adapter.
    async fn complete(
        &self,
        _request: McpCompletionRequest,
    ) -> Result<McpCompletionResult, McpError> {
        unreachable!("complete")
    }
    /// The normalized client owns no transport resources.
    async fn shutdown(&self) -> Result<(), McpError> {
        Ok(())
    }
}

/// Creates a deterministic catalog and the production Session adapters.
async fn session(count: usize, exposure: ToolExposure) -> Arc<mcp::McpSession> {
    let id = McpServerId::try_from("books").expect("id");
    let catalog = McpCatalog::builder()
        .resources(
            (0..count)
                .map(|index| {
                    McpResourceInfo::builder()
                        .reference(McpResourceRef {
                            server_id: id.clone(),
                            remote_uri: format!("book://{index:04}"),
                        })
                        .name(format!("Book {index}"))
                        .build()
                })
                .collect(),
        )
        .build();
    let mut server = session_support::runtime_server("books", true);
    server.exposure = exposure;
    let factory =
        SessionMcpFactory::new(vec![server], Arc::new(Resources { catalog }));
    static NEXT: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);
    let mut request = session_support::session_request();
    request.session_id = SessionId::try_from(format!(
        "resource-session-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
    .expect("id");
    Arc::new(factory.create(request).await.expect("session"))
}

/// Creates the same invocation context used by Kernel tools.
fn context(session: &mcp::McpSession) -> ToolExecutionContext {
    ToolExecutionContext::builder()
        .session_id(session.session_id().clone())
        .turn_id(TurnId::try_from("turn").expect("id"))
        .trace_id(TraceId::try_from("trace").expect("id"))
        .cwd("/workspace".into())
        .cancellation(CancellationToken::new())
        .updates(Arc::new(tools::DiscardToolUpdates))
        .build()
}

/// Invokes an ordinary registered tool and decodes its complete JSON page.
async fn page(
    session: &Arc<mcp::McpSession>,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, tools::ToolError> {
    let result = session
        .tool_registry()
        .expect("registry")
        .execute(
            ToolCall {
                tool_call_id: ToolCallId::try_from("list").expect("id"),
                name: "list_mcp_resources".into(),
                arguments,
            },
            &context(session),
        )
        .await?;
    let text = result
        .blocks
        .first()
        .and_then(ContentBlock::text)
        .expect("text");
    assert!(text.len() <= 20 * 1024);
    Ok(serde_json::from_str(text).expect("complete JSON"))
}

/// Default/count pagination is forward-only and context-bound; unlisted URIs still route remotely.
#[tokio::test]
async fn resource_pages_and_unlisted_reads() {
    let session = session(230, ToolExposure::Direct).await;
    let first = page(&session, serde_json::json!({})).await.expect("first");
    assert_eq!(first["resources"].as_array().expect("array").len(), 50);
    let cursor = first["nextCursor"].as_str().expect("cursor");
    let second = page(&session, serde_json::json!({"cursor":cursor,"limit":1}))
        .await
        .expect("second");
    assert_eq!(second["resources"].as_array().expect("array").len(), 1);
    assert_eq!(
        second["resources"][0]["reference"]["remoteUri"],
        "book://0050"
    );
    for limit in [
        serde_json::json!(0),
        serde_json::json!(201),
        serde_json::json!(1.5),
        serde_json::Value::Null,
    ] {
        page(&session, serde_json::json!({"limit":limit}))
            .await
            .expect_err("invalid limit");
    }
    page(
        &session,
        serde_json::json!({"cursor":cursor,"server":"books"}),
    )
    .await
    .expect_err("cursor filter mismatch");
    let foreign = self::session(230, ToolExposure::Direct).await;
    page(&foreign, serde_json::json!({"cursor":cursor}))
        .await
        .expect_err("foreign Session cursor");
    foreign.shutdown().await.expect("shutdown");
    let registry = session.tool_registry().expect("registry");
    registry
        .execute(
            ToolCall {
                tool_call_id: ToolCallId::try_from("templates").expect("id"),
                name: "list_mcp_resource_templates".into(),
                arguments: serde_json::json!({"cursor":cursor}),
            },
            &context(&session),
        )
        .await
        .expect_err("resource cursor cannot list templates");
    let large = page(&session, serde_json::json!({"limit":200}))
        .await
        .expect("large page");
    assert_eq!(large["resources"].as_array().expect("array").len(), 200);
    let result = registry.execute(ToolCall { tool_call_id: ToolCallId::try_from("read").expect("id"), name: "read_mcp_resource".into(), arguments: serde_json::json!({"server":"books","uri":"file:///opaque/template/未列出"}) }, &context(&session)).await.expect("read");
    assert_eq!(
        result.blocks.first().and_then(ContentBlock::text),
        Some("file:///opaque/template/未列出")
    );
    let revision = session.snapshot().revision;
    session
        .reconnect(&McpServerId::try_from("books").expect("id"))
        .await
        .expect("reconnect");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while session.snapshot().revision == revision {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("new catalog publication");
    page(&session, serde_json::json!({"cursor":cursor}))
        .await
        .expect_err("stale cursor");
    session.shutdown().await.expect("shutdown");
}

/// An oversized URI is rejected as one record; it is never truncated or returned as an empty loop.
#[tokio::test]
async fn oversized_single_resource_is_an_error() {
    let id = McpServerId::try_from("books").expect("id");
    let catalog = McpCatalog::builder()
        .resources(vec![
            McpResourceInfo::builder()
                .reference(McpResourceRef {
                    server_id: id,
                    remote_uri: format!("book://{}", "a".repeat(30_000)),
                })
                .name("oversized".into())
                .build(),
        ])
        .build();
    let factory = SessionMcpFactory::new(
        vec![session_support::runtime_server("books", true)],
        Arc::new(Resources { catalog }),
    );
    let session = Arc::new(
        factory
            .create(session_support::session_request())
            .await
            .expect("session"),
    );
    let error = page(&session, serde_json::json!({}))
        .await
        .expect_err("oversized record");
    assert!(error.to_string().contains("cannot fit"));
    session.shutdown().await.expect("shutdown");
}

/// Long valid server filters retain usable continuations under the same page byte budget.
#[tokio::test]
async fn long_server_filter_keeps_generated_cursors_usable() {
    let name = "s".repeat(3100);
    let server_id =
        McpServerId::try_from(name.as_str()).expect("long server id");
    let catalog = McpCatalog::builder()
        .resources(
            (0..2)
                .map(|index| {
                    McpResourceInfo::builder()
                        .reference(McpResourceRef {
                            server_id: server_id.clone(),
                            remote_uri: format!("opaque://{index}"),
                        })
                        .name(index.to_string())
                        .build()
                })
                .collect(),
        )
        .build();
    let factory = SessionMcpFactory::new(
        vec![session_support::runtime_server(&name, true)],
        Arc::new(Resources { catalog }),
    );
    let session = Arc::new(
        factory
            .create(session_support::session_request())
            .await
            .expect("session"),
    );
    let first = page(&session, serde_json::json!({"server":name,"limit":1}))
        .await
        .expect("first");
    let second = page(&session, serde_json::json!({"server":name,"limit":1,"cursor":first["nextCursor"]})).await.expect("generated cursor");
    assert_eq!(second["resources"].as_array().expect("array").len(), 1);
    session.shutdown().await.expect("shutdown");
}

/// Hidden server resources cannot become accessible through direct URI reads.
#[tokio::test]
async fn hidden_server_cannot_be_read() {
    let session = session(1, ToolExposure::Hidden).await;
    let result = session
        .read_resource(
            McpResourceRequest {
                reference: McpResourceRef {
                    server_id: McpServerId::try_from("books").expect("id"),
                    remote_uri: "book://0000".into(),
                },
            },
            &CancellationToken::new(),
        )
        .await;
    result.expect_err("hidden server");
    session.shutdown().await.expect("shutdown");
}

/// A final page drops its continuation, so prefix page sizes are not monotonic.
#[tokio::test]
async fn complete_last_page_can_fit_when_its_prefix_does_not() {
    let id = McpServerId::try_from("books").expect("id");
    let resources = vec![
        McpResourceInfo::builder()
            .reference(McpResourceRef {
                server_id: id.clone(),
                remote_uri: "a".into(),
            })
            .name("a".into())
            .description(Some("x".repeat(20_280)))
            .build(),
        McpResourceInfo::builder()
            .reference(McpResourceRef {
                server_id: id,
                remote_uri: "b".into(),
            })
            .name("b".into())
            .build(),
    ];
    let full_size =
        serde_json::to_vec(&serde_json::json!({"resources":resources}))
            .expect("JSON")
            .len();
    assert!(full_size <= 20 * 1024);
    let factory = SessionMcpFactory::new(
        vec![session_support::runtime_server("books", true)],
        Arc::new(Resources {
            catalog: McpCatalog::builder().resources(resources).build(),
        }),
    );
    let session = Arc::new(
        factory
            .create(session_support::session_request())
            .await
            .expect("session"),
    );
    let result = page(&session, serde_json::json!({})).await;
    session.shutdown().await.expect("shutdown");
    let page = result.expect("the complete final page fits");
    assert_eq!(page["resources"].as_array().expect("array").len(), 2);
    assert!(page.get("nextCursor").is_none());
}
