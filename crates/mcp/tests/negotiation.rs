use std::time::Duration;

use mcp::{InitializeClient, McpClient};
use protocol::McpServerId;
use rmcp::model::{
    ClientJsonRpcMessage, ClientRequest, Implementation, InitializeResult,
    ProtocolVersion, RequestId, ServerJsonRpcMessage, ServerResult,
};
use rmcp::transport::{IntoTransport, Transport};

/// Initialization accepts every Pi-supported server version without another handshake.
#[tokio::test]
async fn accepts_each_supported_server_version() {
    for version in [
        ProtocolVersion::V_2025_11_25,
        ProtocolVersion::V_2025_06_18,
        ProtocolVersion::V_2025_03_26,
        ProtocolVersion::V_2024_11_05,
    ] {
        let (server_stream, client_stream) = tokio::io::duplex(8192);
        let mut server =
            IntoTransport::<rmcp::RoleServer, _, _>::into_transport(
                server_stream,
            );
        let expected = version.to_string();
        let peer = tokio::spawn(async move {
            let ClientJsonRpcMessage::Request(request) =
                server.receive().await.expect("initialize")
            else {
                panic!("request")
            };
            let ClientRequest::InitializeRequest(initialize) = request.request
            else {
                panic!("initialize first")
            };
            assert_eq!(
                initialize.params.protocol_version,
                ProtocolVersion::V_2025_11_25
            );
            server
                .send(ServerJsonRpcMessage::response(
                    ServerResult::InitializeResult(
                        InitializeResult::new(Default::default())
                            .with_protocol_version(version)
                            .with_server_info(Implementation::new(
                                "negotiation",
                                "1",
                            )),
                    ),
                    request.id,
                ))
                .await
                .expect("response");
            let next = server.receive().await;
            assert!(
                matches!(next, Some(ClientJsonRpcMessage::Notification(_))),
                "exactly one initialized notification"
            );
        });
        let client = InitializeClient::connect(
            McpServerId::try_from("negotiation").expect("id"),
            client_stream,
        )
        .await;
        peer.await.expect("peer");
        let client = client.expect("supported older version");
        assert_eq!(client.protocol().to_string(), expected);
        client.shutdown().await.expect("shutdown");
    }
}

/// Invalid selection and mismatched IDs close before initialized or capability requests.
#[tokio::test]
async fn rejects_invalid_handshake_before_initialized() {
    for case in 0..4 {
        let wrong_id = case == 1;
        let (server_stream, client_stream) = tokio::io::duplex(8192);
        let mut server =
            IntoTransport::<rmcp::RoleServer, _, _>::into_transport(
                server_stream,
            );
        let peer = tokio::spawn(async move {
            let ClientJsonRpcMessage::Request(request) =
                server.receive().await.expect("initialize")
            else {
                panic!("request")
            };
            server
                .send(ServerJsonRpcMessage::response(
                    if case == 2 {
                        ServerResult::ListToolsResult(Default::default())
                    } else {
                        ServerResult::InitializeResult(
                            InitializeResult::new(Default::default())
                                .with_protocol_version(match case {
                                    0 => ProtocolVersion::V_2026_07_28,
                                    3 => serde_json::from_str("\"2099-01-01\"")
                                        .expect("wire version"),
                                    _ => ProtocolVersion::V_2025_11_25,
                                }),
                        )
                    },
                    if wrong_id {
                        RequestId::Number(999)
                    } else {
                        request.id
                    },
                ))
                .await
                .expect("response");
            tokio::time::timeout(Duration::from_secs(2), server.receive())
                .await
                .expect("connection closes")
        });
        InitializeClient::connect(
            McpServerId::try_from("negotiation").expect("id"),
            client_stream,
        )
        .await
        .expect_err("invalid negotiation");
        assert!(
            peer.await.expect("peer").is_none(),
            "rejected handshake must not send initialized"
        );
    }
}
