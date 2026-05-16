//! ACP client connection and request helpers for the local TUI.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::*;
use agent_client_protocol::{Agent, Client, ConnectionTo, Responder};
use anyhow::{Context, anyhow};
use tokio::sync::mpsc;

use crate::acp_server;
use crate::ui::approval::{ApprovalDecision, PendingApproval};

/// App-level events emitted by ACP callbacks or background request tasks.
pub enum AppEvent {
    /// ACP session output notification from the agent.
    SessionNotification(Box<SessionNotification>),
    /// ACP permission request that needs an overlay decision.
    PermissionRequested(PendingApproval),
    /// Prompt request finished with an ACP stop reason.
    PromptFinished(StopReason),
    /// Prompt request failed before returning a response.
    PromptFailed(String),
    /// ACP connection or callback error.
    AcpError(String),
}

/// TUI-side ACP client plus pending permission responders.
#[derive(Clone)]
pub struct AcpClient {
    /// Active connection from the TUI client to the ACP agent.
    conn: ConnectionTo<Agent>,
    /// Pending permission responder map keyed by local request id.
    permissions: PendingPermissions,
}

impl AcpClient {
    /// Sends ACP initialize and returns the agent metadata.
    pub async fn initialize(&self) -> anyhow::Result<InitializeResponse> {
        self.conn
            .send_request(InitializeRequest::new(ProtocolVersion::V1))
            .block_task()
            .await
            .context("initialize ACP agent")
    }

    /// Requests persisted sessions for the provided working directory.
    pub async fn list_sessions(&self, cwd: PathBuf) -> anyhow::Result<ListSessionsResponse> {
        self.conn
            .send_request(ListSessionsRequest::new().cwd(cwd))
            .block_task()
            .await
            .context("list ACP sessions")
    }

    /// Creates a new ACP session rooted at the provided working directory.
    pub async fn new_session(&self, cwd: PathBuf) -> anyhow::Result<NewSessionResponse> {
        self.conn
            .send_request(NewSessionRequest::new(cwd))
            .block_task()
            .await
            .context("create ACP session")
    }

    /// Loads an existing ACP session rooted at the provided working directory.
    pub async fn load_session(
        &self,
        session_id: SessionId,
        cwd: PathBuf,
    ) -> anyhow::Result<LoadSessionResponse> {
        self.conn
            .send_request(LoadSessionRequest::new(session_id, cwd))
            .block_task()
            .await
            .context("load ACP session")
    }

    /// Sends a prompt and waits for the ACP prompt response.
    pub async fn prompt(&self, session_id: SessionId, text: String) -> anyhow::Result<StopReason> {
        let request =
            PromptRequest::new(session_id, vec![ContentBlock::Text(TextContent::new(text))]);
        let response: PromptResponse = self
            .conn
            .send_request(request)
            .block_task()
            .await
            .context("send ACP prompt")?;
        Ok(response.stop_reason)
    }

    /// Sends an ACP cancellation notification for the current prompt turn.
    pub fn cancel(&self, session_id: SessionId) -> anyhow::Result<()> {
        self.conn
            .send_notification(CancelNotification::new(session_id))
            .context("send ACP cancel")
    }

    /// Resolves a pending ACP permission request from a local UI decision.
    pub fn resolve_permission(
        &self,
        request_id: u64,
        decision: ApprovalDecision,
    ) -> anyhow::Result<()> {
        self.permissions
            .respond_selected(request_id, decision.option_id())
    }

    /// Rejects every pending ACP permission request before shutting down.
    pub fn reject_pending_permissions(&self) {
        self.permissions.reject_all();
    }
}

/// Thread-safe responder registry for one-shot ACP permission requests.
#[derive(Default)]
struct PendingPermissions {
    /// Responders keyed by local request id.
    inner: Arc<Mutex<HashMap<u64, Responder<RequestPermissionResponse>>>>,
}

impl Clone for PendingPermissions {
    /// Clones the shared responder registry handle.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl PendingPermissions {
    /// Stores a permission responder until the UI picks an approval option.
    fn insert(&self, request_id: u64, responder: Responder<RequestPermissionResponse>) {
        let mut inner = self.inner.lock().expect("permission mutex poisoned");
        inner.insert(request_id, responder);
    }

    /// Sends a selected ACP permission option for a pending request.
    fn respond_selected(
        &self,
        request_id: u64,
        option_id: PermissionOptionId,
    ) -> anyhow::Result<()> {
        let responder = {
            let mut inner = self.inner.lock().expect("permission mutex poisoned");
            inner.remove(&request_id)
        }
        .ok_or_else(|| anyhow!("permission request {request_id} is no longer pending"))?;

        responder
            .respond(RequestPermissionResponse::new(
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id)),
            ))
            .context("respond to ACP permission request")
    }

    /// Cancels all pending permission requests during shutdown.
    fn reject_all(&self) {
        let responders = {
            let mut inner = self.inner.lock().expect("permission mutex poisoned");
            inner
                .drain()
                .map(|(_, responder)| responder)
                .collect::<Vec<_>>()
        };

        for responder in responders {
            let _ = responder.respond(RequestPermissionResponse::new(
                RequestPermissionOutcome::Cancelled,
            ));
        }
    }
}

/// Runs a caller-provided async block against an in-process ACP client.
pub async fn with_in_process_client<F, Fut, T>(
    app_tx: mpsc::UnboundedSender<AppEvent>,
    run: F,
) -> anyhow::Result<T>
where
    F: FnOnce(AcpClient) -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let (client_io, server) = acp_server::start()?;
    let permissions = PendingPermissions::default();
    let next_request_id = Arc::new(AtomicU64::new(1));

    let notification_tx = app_tx.clone();
    let permission_tx = app_tx;
    let permission_map = permissions.clone();
    let permission_counter = Arc::clone(&next_request_id);

    let result = Client
        .builder()
        .on_receive_notification(
            move |notification: SessionNotification, _cx: ConnectionTo<Agent>| {
                let tx = notification_tx.clone();
                async move {
                    if tx
                        .send(AppEvent::SessionNotification(Box::new(notification)))
                        .is_err()
                    {
                        return Err(agent_client_protocol::Error::internal_error()
                            .data("TUI event receiver closed"));
                    }
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            move |request: RequestPermissionRequest,
                  responder: Responder<RequestPermissionResponse>,
                  _cx| {
                let tx = permission_tx.clone();
                let pending = permission_map.clone();
                let request_id = permission_counter.fetch_add(1, Ordering::Relaxed);
                async move {
                    pending.insert(request_id, responder);
                    let approval = PendingApproval::from_request(request_id, &request);
                    if tx.send(AppEvent::PermissionRequested(approval)).is_err() {
                        let _ = pending
                            .respond_selected(request_id, ApprovalDecision::RejectOnce.option_id());
                    }
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(client_io, async move |conn: ConnectionTo<Agent>| {
            let client = AcpClient { conn, permissions };
            run(client)
                .await
                .map_err(agent_client_protocol::util::internal_error)
        })
        .await
        .map_err(|error| anyhow!("ACP client failed: {error}"));

    server.shutdown().await;
    result
}
