//! ACP-backed terminal backend for the shell tool.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::{
    ClientCapabilities, CreateTerminalRequest, KillTerminalRequest, ReleaseTerminalRequest,
    SessionId as AcpSessionId, TerminalId, TerminalOutputRequest, WaitForTerminalExitRequest,
};
use agent_client_protocol::{Client, ConnectionTo};
use async_trait::async_trait;
use protocol::SessionId;
use tools::{
    RunningTerminal, TerminalBackend, TerminalBackendError, TerminalCreateParams,
    TerminalExitResult, TerminalOutputSnapshot,
};

/// ACP client route used by terminal backend requests.
#[derive(Clone)]
struct AcpClientTerminalRoute {
    /// Client connection for sending agent-to-client terminal requests.
    client: ConnectionTo<Client>,
    /// Capabilities reported by the client during initialize.
    capabilities: ClientCapabilities,
}

/// Router from internal sessions to ACP client connections for terminal operations.
#[derive(Default)]
pub struct AcpClientTerminalRouter {
    routes: Mutex<HashMap<SessionId, AcpClientTerminalRoute>>,
}

impl AcpClientTerminalRouter {
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
                AcpClientTerminalRoute {
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
    ) -> Result<AcpClientTerminalRoute, TerminalBackendError> {
        self.routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .cloned()
            .ok_or_else(|| {
                TerminalBackendError::InvalidRequest(format!(
                    "no ACP client terminal route for session {session_id}"
                ))
            })
    }
}

/// Terminal backend that delegates command execution to the ACP client.
pub struct AcpTerminalBackend {
    router: Arc<AcpClientTerminalRouter>,
}

impl AcpTerminalBackend {
    /// Create an ACP terminal backend using the given router.
    #[must_use]
    pub fn new(router: Arc<AcpClientTerminalRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl TerminalBackend for AcpTerminalBackend {
    async fn create(
        &self,
        params: TerminalCreateParams,
    ) -> Result<Box<dyn RunningTerminal>, TerminalBackendError> {
        let route = self.router.route_for(&params.session_id)?;
        if !route.capabilities.terminal {
            return Err(TerminalBackendError::InvalidRequest(
                "ACP client does not support terminal capability".to_string(),
            ));
        }

        let acp_session_id = AcpSessionId::new(params.session_id.0.clone());
        let acp_env: Vec<_> = params
            .env
            .into_iter()
            .map(|e| agent_client_protocol::schema::EnvVariable::new(e.name, e.value))
            .collect();
        let response = route
            .client
            .send_request(
                CreateTerminalRequest::new(acp_session_id, params.command)
                    .args(params.args)
                    .env(acp_env)
                    .cwd(params.cwd)
                    .output_byte_limit(params.output_byte_limit)
                    .meta(params.meta),
            )
            .block_task()
            .await
            .map_err(|error| {
                TerminalBackendError::Io(format!("ACP terminal/create failed: {error}"))
            })?;

        Ok(Box::new(AcpRunningTerminal {
            session_id: params.session_id,
            terminal_id: response.terminal_id,
            client: route.client,
        }))
    }
}

/// Handle to a terminal running on the ACP client.
struct AcpRunningTerminal {
    session_id: SessionId,
    terminal_id: TerminalId,
    client: ConnectionTo<Client>,
}

impl Drop for AcpRunningTerminal {
    fn drop(&mut self) {
        let client = self.client.clone();
        let session_id = AcpSessionId::new(self.session_id.0.clone());
        let terminal_id = self.terminal_id.clone();
        tokio::spawn(async move {
            let _ = client
                .send_request(ReleaseTerminalRequest::new(session_id, terminal_id))
                .block_task()
                .await;
        });
    }
}

#[async_trait]
impl RunningTerminal for AcpRunningTerminal {
    async fn output(&self) -> Result<TerminalOutputSnapshot, TerminalBackendError> {
        let response = self
            .client
            .send_request(TerminalOutputRequest::new(
                AcpSessionId::new(self.session_id.0.clone()),
                self.terminal_id.clone(),
            ))
            .block_task()
            .await
            .map_err(|error| {
                TerminalBackendError::Io(format!("ACP terminal/output failed: {error}"))
            })?;

        Ok(TerminalOutputSnapshot {
            stdout: response.output,
            stderr: String::new(),
            exit_status: response.exit_status.map(|es| TerminalExitResult {
                exit_code: es.exit_code.map_or(-1, |c| c as i32),
            }),
        })
    }

    async fn wait_for_exit(&self) -> Result<TerminalExitResult, TerminalBackendError> {
        let response = self
            .client
            .send_request(WaitForTerminalExitRequest::new(
                AcpSessionId::new(self.session_id.0.clone()),
                self.terminal_id.clone(),
            ))
            .block_task()
            .await
            .map_err(|error| {
                TerminalBackendError::Io(format!("ACP terminal/wait_for_exit failed: {error}"))
            })?;

        Ok(TerminalExitResult {
            exit_code: response.exit_status.exit_code.map_or(-1, |c| c as i32),
        })
    }

    async fn kill(&self) -> Result<(), TerminalBackendError> {
        self.client
            .send_request(KillTerminalRequest::new(
                AcpSessionId::new(self.session_id.0.clone()),
                self.terminal_id.clone(),
            ))
            .block_task()
            .await
            .map_err(|error| {
                TerminalBackendError::Io(format!("ACP terminal/kill failed: {error}"))
            })?;
        Ok(())
    }

    async fn write_stdin(&self, _bytes: &[u8]) -> Result<(), TerminalBackendError> {
        Err(TerminalBackendError::InvalidRequest(
            "ACP terminal backend does not support stdin writes".to_string(),
        ))
    }
}
