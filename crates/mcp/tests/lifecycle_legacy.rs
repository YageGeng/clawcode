mod support;

use std::time::Duration;

use mcp::{LegacyClient, McpClient, McpError};
use protocol::McpProtocolVersion;
use rmcp::model::{
    ClientJsonRpcMessage, ClientRequest, Implementation, InitializeResult,
    ListToolsResult, ProtocolVersion, ServerJsonRpcMessage, ServerResult, Tool,
};
use rmcp::transport::{IntoTransport, Transport};

/// Legacy startup sends exact initialize, initialized, and capability-gated listing in order.
#[tokio::test]
async fn legacy_lifecycle_is_exact_and_ordered() {
    let (server_transport, client_transport) = tokio::io::duplex(8_192);
    let mut server = IntoTransport::<rmcp::RoleServer, _, _>::into_transport(
        server_transport,
    );
    let server_task = tokio::spawn(async move {
        let ClientJsonRpcMessage::Request(initialize) =
            server.receive().await.expect("initialize request")
        else {
            panic!("expected initialize request");
        };
        let ClientRequest::InitializeRequest(request) = initialize.request
        else {
            panic!("legacy client must initialize first");
        };
        assert_eq!(
            request.params.protocol_version,
            ProtocolVersion::V_2025_11_25
        );
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::InitializeResult(
                    InitializeResult::new(support::tools_capabilities())
                        .with_protocol_version(ProtocolVersion::V_2025_11_25)
                        .with_server_info(Implementation::new(
                            "legacy-reference",
                            "1.0.0",
                        )),
                ),
                initialize.id,
            ))
            .await
            .expect("initialize response");

        let ClientJsonRpcMessage::Notification(initialized) =
            server.receive().await.expect("initialized notification")
        else {
            panic!("expected initialized notification");
        };
        assert!(matches!(
            initialized.notification,
            rmcp::model::ClientNotification::InitializedNotification(_)
        ));

        let ClientJsonRpcMessage::Request(first_page) =
            server.receive().await.expect("tools/list request")
        else {
            panic!("expected tools/list request");
        };
        assert!(matches!(
            first_page.request,
            ClientRequest::ListToolsRequest(_)
        ));
        let mut first_result =
            ListToolsResult::with_all_items(vec![Tool::new(
                "first",
                "first Tool",
                serde_json::Map::new(),
            )]);
        first_result.next_cursor = Some("next-page".to_string());
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::ListToolsResult(first_result),
                first_page.id,
            ))
            .await
            .expect("first tools/list response");

        let ClientJsonRpcMessage::Request(second_page) =
            server.receive().await.expect("second tools/list request")
        else {
            panic!("expected second tools/list request");
        };
        let ClientRequest::ListToolsRequest(request) = &second_page.request
        else {
            panic!("expected paginated tools/list request");
        };
        assert_eq!(
            request
                .params
                .as_ref()
                .and_then(|params| params.cursor.as_deref()),
            Some("next-page")
        );
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::ListToolsResult(ListToolsResult::with_all_items(
                    vec![Tool::new(
                        "second",
                        "second Tool",
                        serde_json::Map::new(),
                    )],
                )),
                second_page.id,
            ))
            .await
            .expect("second tools/list response");
    });

    let client = LegacyClient::connect(support::server_id(), client_transport)
        .await
        .expect("legacy connection");
    assert_eq!(client.protocol(), McpProtocolVersion::V2025_11_25);
    let catalog = client.discover().await.expect("legacy catalog");
    assert_eq!(catalog.tools.len(), 2);
    assert_eq!(catalog.tools[0].reference.remote_name, "first");
    assert_eq!(catalog.tools[1].reference.remote_name, "second");
    client.shutdown().await.expect("legacy shutdown");
    server_task.await.expect("reference server");
}

/// Legacy startup rejects a different selected protocol and never probes discovery.
#[tokio::test]
async fn legacy_protocol_mismatch_does_not_fallback() {
    let (server_transport, client_transport) = tokio::io::duplex(8_192);
    let mut server = IntoTransport::<rmcp::RoleServer, _, _>::into_transport(
        server_transport,
    );
    let server_task = tokio::spawn(async move {
        let ClientJsonRpcMessage::Request(initialize) =
            server.receive().await.expect("initialize request")
        else {
            panic!("expected initialize request");
        };
        assert!(matches!(
            initialize.request,
            ClientRequest::InitializeRequest(_)
        ));
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::InitializeResult(
                    InitializeResult::new(Default::default())
                        .with_protocol_version(ProtocolVersion::V_2026_07_28),
                ),
                initialize.id,
            ))
            .await
            .expect("mismatched initialize response");
        let next =
            tokio::time::timeout(Duration::from_millis(100), server.receive())
                .await;
        if let Ok(Some(ClientJsonRpcMessage::Request(request))) = next {
            assert!(!matches!(
                request.request,
                ClientRequest::DiscoverRequest(_)
            ));
        }
    });

    let error = LegacyClient::connect(support::server_id(), client_transport)
        .await
        .expect_err("protocol mismatch");
    assert!(matches!(
        error,
        McpError::ProtocolMismatch {
            expected: McpProtocolVersion::V2025_11_25,
            ..
        }
    ));
    server_task.await.expect("reference server");
}
