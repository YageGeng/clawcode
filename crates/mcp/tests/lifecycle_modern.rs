mod support;

use std::time::Duration;

use mcp::{McpClient, McpClientEvent, McpError, ModernClient};
use protocol::McpProtocolVersion;
use rmcp::model::{
    ClientJsonRpcMessage, ClientRequest, DiscoverResult, GetMeta,
    Implementation, NotificationMetaObject, ProtocolVersion,
    ServerCapabilities, ServerJsonRpcMessage, ServerNotification, ServerResult,
    SubscriptionsAcknowledgedNotification,
    SubscriptionsAcknowledgedNotificationParams, SubscriptionsListenResult,
};
use rmcp::transport::{IntoTransport, Transport};

/// Modern startup uses discovery only and attaches exact request metadata to listing.
#[tokio::test]
async fn modern_lifecycle_is_exact_and_self_describing() {
    let (server_transport, client_transport) = tokio::io::duplex(8_192);
    let mut server = IntoTransport::<rmcp::RoleServer, _, _>::into_transport(
        server_transport,
    );
    let server_task = tokio::spawn(async move {
        let ClientJsonRpcMessage::Request(discover) =
            server.receive().await.expect("discover request")
        else {
            panic!("expected discover request");
        };
        assert!(matches!(
            discover.request,
            ClientRequest::DiscoverRequest(_)
        ));
        assert_eq!(
            discover.request.get_meta().protocol_version(),
            Some(ProtocolVersion::V_2026_07_28)
        );
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::DiscoverResult(
                    DiscoverResult::new(
                        vec![ProtocolVersion::V_2026_07_28],
                        support::tools_capabilities(),
                    )
                    .with_server_info(Implementation::new(
                        "modern-reference",
                        "1.0.0",
                    )),
                ),
                discover.id,
            ))
            .await
            .expect("discover response");

        let ClientJsonRpcMessage::Request(list) =
            server.receive().await.expect("tools/list request")
        else {
            panic!("expected tools/list request");
        };
        assert!(matches!(list.request, ClientRequest::ListToolsRequest(_)));
        assert_eq!(
            list.request.get_meta().protocol_version(),
            Some(ProtocolVersion::V_2026_07_28)
        );
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::ListToolsResult(Default::default()),
                list.id,
            ))
            .await
            .expect("tools/list response");
    });

    let client = ModernClient::connect(support::server_id(), client_transport)
        .await
        .expect("modern connection");
    assert_eq!(client.protocol(), McpProtocolVersion::V2026_07_28);
    let catalog = client.discover().await.expect("modern catalog");
    assert!(catalog.tools.is_empty());
    client.shutdown().await.expect("modern shutdown");
    server_task.await.expect("reference server");
}

/// Modern startup rejects an incompatible discovery result without initialize fallback.
#[tokio::test]
async fn modern_protocol_mismatch_does_not_fallback() {
    let (server_transport, client_transport) = tokio::io::duplex(8_192);
    let mut server = IntoTransport::<rmcp::RoleServer, _, _>::into_transport(
        server_transport,
    );
    let server_task = tokio::spawn(async move {
        let ClientJsonRpcMessage::Request(discover) =
            server.receive().await.expect("discover request")
        else {
            panic!("expected discover request");
        };
        assert!(matches!(
            discover.request,
            ClientRequest::DiscoverRequest(_)
        ));
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::DiscoverResult(DiscoverResult::new(
                    vec![ProtocolVersion::V_2025_11_25],
                    Default::default(),
                )),
                discover.id,
            ))
            .await
            .expect("mismatched discover response");
        let next =
            tokio::time::timeout(Duration::from_millis(100), server.receive())
                .await;
        if let Ok(Some(ClientJsonRpcMessage::Request(request))) = next {
            assert!(!matches!(
                request.request,
                ClientRequest::InitializeRequest(_)
            ));
        }
    });

    let error = ModernClient::connect(support::server_id(), client_transport)
        .await
        .expect_err("protocol mismatch");
    assert!(matches!(
        error,
        McpError::ProtocolMismatch {
            expected: McpProtocolVersion::V2026_07_28,
            ..
        }
    ));
    server_task.await.expect("reference server");
}

/// A gracefully ended Modern subscription is replaced during the next discovery.
#[tokio::test]
async fn modern_subscription_restarts_after_graceful_end() {
    let (server_transport, client_transport) = tokio::io::duplex(16_384);
    let mut server = IntoTransport::<rmcp::RoleServer, _, _>::into_transport(
        server_transport,
    );
    let server_task = tokio::spawn(async move {
        let ClientJsonRpcMessage::Request(discover) =
            server.receive().await.expect("discover request")
        else {
            panic!("expected discover request");
        };
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::DiscoverResult(DiscoverResult::new(
                    vec![ProtocolVersion::V_2026_07_28],
                    ServerCapabilities::builder()
                        .enable_tools()
                        .enable_tool_list_changed()
                        .build(),
                )),
                discover.id,
            ))
            .await
            .expect("discover response");

        for _round in 0..2 {
            let ClientJsonRpcMessage::Request(list) =
                server.receive().await.expect("tools/list request")
            else {
                panic!("expected tools/list request");
            };
            assert!(matches!(list.request, ClientRequest::ListToolsRequest(_)));
            server
                .send(ServerJsonRpcMessage::response(
                    ServerResult::ListToolsResult(Default::default()),
                    list.id,
                ))
                .await
                .expect("tools/list response");

            let ClientJsonRpcMessage::Request(listen) = server
                .receive()
                .await
                .expect("subscriptions/listen request")
            else {
                panic!("expected subscriptions/listen request");
            };
            let ClientRequest::SubscriptionsListenRequest(request) =
                listen.request
            else {
                panic!("expected subscriptions/listen request");
            };
            let mut acknowledgment = SubscriptionsAcknowledgedNotification::new(
                SubscriptionsAcknowledgedNotificationParams::new(
                    request.params.notifications,
                ),
            );
            let mut meta = NotificationMetaObject::new();
            meta.set_subscription_id(listen.id.clone());
            acknowledgment.extensions.insert(meta);
            server
                .send(ServerJsonRpcMessage::notification(
                    ServerNotification::SubscriptionsAcknowledgedNotification(
                        acknowledgment,
                    ),
                ))
                .await
                .expect("subscription acknowledgment");
            server
                .send(ServerJsonRpcMessage::response(
                    ServerResult::SubscriptionsListenResult(
                        SubscriptionsListenResult::complete(listen.id.clone()),
                    ),
                    listen.id,
                ))
                .await
                .expect("subscription completion");
        }
    });

    let client = ModernClient::connect(support::server_id(), client_transport)
        .await
        .expect("modern connection");
    let mut changes = client.subscribe_changes().expect("Modern change stream");
    client.discover().await.expect("first catalog");
    let event =
        tokio::time::timeout(Duration::from_millis(250), changes.recv())
            .await
            .expect("subscription end event")
            .expect("change event");
    assert_eq!(event, McpClientEvent::SubscriptionEnded);
    tokio::time::timeout(Duration::from_millis(250), client.discover())
        .await
        .expect("second discovery must establish another subscription")
        .expect("second catalog");
    client.shutdown().await.expect("modern shutdown");
    server_task.await.expect("reference server");
}
