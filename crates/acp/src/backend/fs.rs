//! ACP-backed filesystem backend for built-in file tools.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::{
    ClientCapabilities, ReadTextFileRequest, SessionId as AcpSessionId,
    WriteTextFileRequest,
};
use agent_client_protocol::{Client, ConnectionTo};
use async_trait::async_trait;
use protocol::SessionId;
use tools::{
    FsBackend, FsBackendError, FsReadRequest, FsReadResponse, FsWriteRequest,
    FsWriteResponse,
};

/// ACP client route used by filesystem backend requests.
#[derive(Clone)]
struct AcpClientFsRoute {
    /// Client connection for sending agent-to-client fs requests.
    client: ConnectionTo<Client>,
    /// Capabilities reported by the client during initialize.
    capabilities: ClientCapabilities,
}

/// Router from internal sessions to ACP client connections.
#[derive(Default)]
pub struct AcpClientFsRouter {
    /// Active ACP client route per internal session id.
    routes: Mutex<HashMap<SessionId, AcpClientFsRoute>>,
}

impl AcpClientFsRouter {
    /// Register an ACP client route for a session.
    pub fn register_session(
        &self,
        session_id: SessionId,
        client: ConnectionTo<Client>,
        capabilities: ClientCapabilities,
    ) {
        self.routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                session_id,
                AcpClientFsRoute {
                    client,
                    capabilities,
                },
            );
    }

    /// Remove the ACP client route for a closed session.
    pub fn unregister_session(&self, session_id: &SessionId) {
        self.routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session_id);
    }

    /// Return the registered route for a session.
    fn route_for(
        &self,
        session_id: &SessionId,
    ) -> Result<AcpClientFsRoute, FsBackendError> {
        self.routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .cloned()
            .ok_or_else(|| {
                FsBackendError::InvalidRequest(format!(
                    "no ACP client route for session {session_id}"
                ))
            })
    }
}

/// Filesystem backend that delegates text file operations to the ACP client.
pub struct AcpFsBackend {
    /// Router used to find the ACP client for the executing session.
    router: Arc<AcpClientFsRouter>,
}

impl AcpFsBackend {
    /// Create an ACP filesystem backend using the given router.
    #[must_use]
    pub fn new(router: Arc<AcpClientFsRouter>) -> Self {
        Self { router }
    }

    /// Resolve a user path to an absolute path before sending it to ACP.
    fn resolve_absolute(cwd: PathBuf, path: PathBuf) -> PathBuf {
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    }
}

#[async_trait]
impl FsBackend for AcpFsBackend {
    /// Read a text file by sending `fs/read_text_file` to the ACP client.
    async fn read_text_file(
        &self,
        request: FsReadRequest,
    ) -> Result<FsReadResponse, FsBackendError> {
        let route = self.router.route_for(&request.session_id)?;
        if !route.capabilities.fs.read_text_file {
            return Err(FsBackendError::InvalidRequest(
                "ACP client does not support fs/read_text_file".to_string(),
            ));
        }

        let line = request
            .offset
            .checked_add(1)
            .and_then(|line| u32::try_from(line).ok())
            .ok_or_else(|| {
                FsBackendError::InvalidRequest(
                    "read offset is too large".to_string(),
                )
            })?;
        let path = Self::resolve_absolute(request.cwd, request.path);
        let mut acp_request = ReadTextFileRequest::new(
            AcpSessionId::from(&request.session_id),
            path,
        )
        .line(line);

        if let Some(limit) = request.limit {
            let limit = u32::try_from(limit).map_err(|error| {
                FsBackendError::InvalidRequest(format!(
                    "read limit is too large: {error}"
                ))
            })?;

            acp_request = acp_request.limit(limit);
        }

        let response = route
            .client
            .send_request(acp_request)
            .block_task()
            .await
            .map_err(|error| {
                FsBackendError::Io(format!(
                    "ACP read_text_file failed: {error}"
                ))
            })?;

        Ok(FsReadResponse {
            content: response.content,
        })
    }

    /// Write a text file by sending `fs/write_text_file` to the ACP client.
    async fn write_text_file(
        &self,
        request: FsWriteRequest,
    ) -> Result<FsWriteResponse, FsBackendError> {
        let route = self.router.route_for(&request.session_id)?;
        if !route.capabilities.fs.write_text_file {
            return Err(FsBackendError::InvalidRequest(
                "ACP client does not support fs/write_text_file".to_string(),
            ));
        }

        let path = Self::resolve_absolute(request.cwd, request.path);
        let bytes_written = request.content.len();
        route
            .client
            .send_request(WriteTextFileRequest::new(
                AcpSessionId::from(&request.session_id),
                path.clone(),
                request.content,
            ))
            .block_task()
            .await
            .map_err(|error| {
                FsBackendError::Io(format!(
                    "ACP write_text_file failed: {error}"
                ))
            })?;

        Ok(FsWriteResponse {
            bytes_written,
            display_path: path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{
        ReadTextFileResponse, WriteTextFileResponse,
    };
    use agent_client_protocol::{Agent, Channel, Client, Responder};
    use tokio::sync::{mpsc, oneshot};

    /// Create a test ACP connection that records filesystem requests.
    async fn test_client_connection(
        read_tx: mpsc::UnboundedSender<ReadTextFileRequest>,
        write_tx: mpsc::UnboundedSender<WriteTextFileRequest>,
    ) -> ConnectionTo<Client> {
        let (agent_channel, client_channel) = Channel::duplex();
        let (connection_tx, connection_rx) = oneshot::channel();

        tokio::spawn(async move {
            let _ = Agent
                .builder()
                .connect_with(
                    agent_channel,
                    async move |cx: ConnectionTo<Client>| {
                        let _ = connection_tx.send(cx);
                        std::future::pending::<
                            Result<(), agent_client_protocol::Error>,
                        >()
                        .await
                    },
                )
                .await;
        });

        tokio::spawn(async move {
            let _ = Client
                .builder()
                .on_receive_request(
                    move |request: ReadTextFileRequest,
                          responder: Responder<ReadTextFileResponse>,
                          _cx| {
                        let tx = read_tx.clone();
                        async move {
                            let _ = tx.send(request);
                            responder.respond(ReadTextFileResponse::new(
                                "from client",
                            ))
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    move |request: WriteTextFileRequest,
                          responder: Responder<WriteTextFileResponse>,
                          _cx| {
                        let tx = write_tx.clone();
                        async move {
                            let _ = tx.send(request);
                            responder.respond(WriteTextFileResponse::new())
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(client_channel, async move |_cx| {
                    std::future::pending::<
                        Result<(), agent_client_protocol::Error>,
                    >()
                    .await
                })
                .await;
        });

        connection_rx
            .await
            .expect("agent connection should be created")
    }

    /// Register a test session route with read/write fs capabilities.
    async fn backend_with_route(
        session_id: &SessionId,
        read_tx: mpsc::UnboundedSender<ReadTextFileRequest>,
        write_tx: mpsc::UnboundedSender<WriteTextFileRequest>,
    ) -> AcpFsBackend {
        let router = Arc::new(AcpClientFsRouter::default());
        let client = test_client_connection(read_tx, write_tx).await;
        router.register_session(
            session_id.clone(),
            client,
            ClientCapabilities::new().fs(
                agent_client_protocol::schema::FileSystemCapabilities::new()
                    .read_text_file(true)
                    .write_text_file(true),
            ),
        );
        AcpFsBackend::new(router)
    }

    #[tokio::test]
    async fn acp_backend_sends_read_text_file_request() {
        let (read_tx, mut read_rx) = mpsc::unbounded_channel();
        let (write_tx, _write_rx) = mpsc::unbounded_channel();
        let session_id = SessionId::from("session-1");
        let backend = backend_with_route(&session_id, read_tx, write_tx).await;

        let response = backend
            .read_text_file(
                FsReadRequest::builder()
                    .session_id(session_id)
                    .cwd(PathBuf::from("/workspace"))
                    .path(PathBuf::from("sample.txt"))
                    .offset(1)
                    .limit(2)
                    .build(),
            )
            .await
            .expect("ACP read should succeed");

        assert_eq!(response.content, "from client");
        let request =
            read_rx.recv().await.expect("read request should be sent");
        assert_eq!(request.path, PathBuf::from("/workspace/sample.txt"));
        assert_eq!(request.line, Some(2));
        assert_eq!(request.limit, Some(2));
    }

    #[tokio::test]
    async fn acp_backend_sends_write_text_file_request() {
        let (_read_tx, _read_rx) = mpsc::unbounded_channel();
        let (write_tx, mut write_rx) = mpsc::unbounded_channel();
        let session_id = SessionId::from("session-1");
        let backend = backend_with_route(&session_id, _read_tx, write_tx).await;

        let response = backend
            .write_text_file(
                FsWriteRequest::builder()
                    .session_id(session_id)
                    .cwd(PathBuf::from("/workspace"))
                    .path(PathBuf::from("out.txt"))
                    .content("hello".to_string())
                    .build(),
            )
            .await
            .expect("ACP write should succeed");

        assert_eq!(response.bytes_written, 5);
        assert_eq!(response.display_path, PathBuf::from("/workspace/out.txt"));
        let request =
            write_rx.recv().await.expect("write request should be sent");
        assert_eq!(request.path, PathBuf::from("/workspace/out.txt"));
        assert_eq!(request.content, "hello");
    }
}
