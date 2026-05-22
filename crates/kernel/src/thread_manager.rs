use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use config::AppConfig;
use protocol::message::Message;
use protocol::{AgentPath, Event, KernelError, Op, SessionId};
use provider::factory::ArcLlm;
use store::SessionRecorder;
use tokio::sync::{Mutex, mpsc, watch};
use tools::ToolRegistry;

use crate::agent::control::AgentControl;
use crate::approval::ApprovalPolicy;
use crate::context::{ContextManager, InMemoryContext};
use crate::session::{Thread, spawn_thread};

/// Parameters required to spawn a live thread runtime.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct SpawnThreadParams {
    /// Session identifier for the new thread.
    pub(crate) session_id: SessionId,
    /// Working directory for tool execution.
    pub(crate) cwd: PathBuf,
    /// LLM handle used by the thread.
    pub(crate) llm: ArcLlm,
    /// Tool registry available to the thread.
    pub(crate) tools: Arc<ToolRegistry>,
    /// Initial conversation context for the thread.
    pub(crate) context: Box<dyn ContextManager>,
    /// Agent path associated with the thread.
    pub(crate) agent_path: AgentPath,
    /// Agent control handle for multi-agent routing.
    pub(crate) agent_control: Arc<AgentControl>,
    /// Approval policy used by the thread.
    pub(crate) approval: Arc<ApprovalPolicy>,
    /// Application config snapshot used by the thread.
    pub(crate) app_config: Arc<AppConfig>,
    /// Recorder attached before the first turn starts.
    pub(crate) recorder: Arc<dyn SessionRecorder>,
}

/// Parameters required to load a persisted thread runtime.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct LoadThreadParams {
    /// Session identifier for the restored thread.
    pub(crate) session_id: SessionId,
    /// Working directory for tool execution.
    pub(crate) cwd: PathBuf,
    /// Replayed canonical conversation history.
    pub(crate) history: Vec<Message>,
    /// LLM handle used by the thread.
    pub(crate) llm: ArcLlm,
    /// Tool registry available to the thread.
    pub(crate) tools: Arc<ToolRegistry>,
    /// Agent path associated with the thread.
    pub(crate) agent_path: AgentPath,
    /// Agent control handle for multi-agent routing.
    pub(crate) agent_control: Arc<AgentControl>,
    /// Approval policy used by the thread.
    pub(crate) approval: Arc<ApprovalPolicy>,
    /// Application config snapshot used by the thread.
    pub(crate) app_config: Arc<AppConfig>,
    /// Recorder attached before the restored thread runs again.
    pub(crate) recorder: Arc<dyn SessionRecorder>,
}

/// Owns live thread handles and routes operations to them.
pub(crate) struct ThreadManager {
    threads: Mutex<HashMap<SessionId, Thread>>,
}

impl ThreadManager {
    /// Create an empty live thread manager.
    pub(crate) fn new() -> Self {
        Self {
            threads: Mutex::new(HashMap::new()),
        }
    }

    /// Insert an already spawned thread into the live thread map.
    pub(crate) async fn insert_thread(&self, thread: Thread) {
        self.threads
            .lock()
            .await
            .insert(thread.session_id.clone(), thread);
    }

    /// Return a cloned live thread handle for a session id.
    pub(crate) async fn get_thread(&self, session_id: &SessionId) -> Option<Thread> {
        self.threads.lock().await.get(session_id).cloned()
    }

    /// Return cloned handles for all live sessions.
    pub(crate) async fn live_sessions(&self) -> Vec<Thread> {
        self.threads.lock().await.values().cloned().collect()
    }

    /// Create a fresh event receiver for a live thread.
    pub(crate) async fn take_rx(
        &self,
        session_id: &SessionId,
    ) -> Result<mpsc::UnboundedReceiver<Event>, KernelError> {
        let thread = self
            .get_thread(session_id)
            .await
            .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))?;
        Ok(thread.take_rx().await)
    }

    /// Return a cancellation watch receiver for a live thread.
    pub(crate) async fn cancel_rx(
        &self,
        session_id: &SessionId,
    ) -> Result<watch::Receiver<bool>, KernelError> {
        let thread = self
            .get_thread(session_id)
            .await
            .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))?;
        Ok(thread.cancel_tx.subscribe())
    }

    /// Send an operation to a live thread.
    pub(crate) async fn send_op(&self, session_id: &SessionId, op: Op) -> Result<(), KernelError> {
        let Some(thread) = self.get_thread(session_id).await else {
            return Err(KernelError::SessionNotFound(session_id.clone()));
        };
        thread.tx_op.send(op).map_err(|error| {
            KernelError::Internal(anyhow::anyhow!(
                "failed to send operation to session {session_id}: {error}"
            ))
        })
    }

    /// Signal cancellation for a live thread while keeping it registered.
    pub(crate) async fn cancel_thread(&self, session_id: &SessionId) -> Result<(), KernelError> {
        let thread = self
            .get_thread(session_id)
            .await
            .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))?;
        let _ = thread.cancel_tx.send(true);
        Ok(())
    }

    /// Close and remove a live thread.
    pub(crate) async fn close_thread(&self, session_id: &SessionId) -> Result<Thread, KernelError> {
        let removed = self
            .threads
            .lock()
            .await
            .remove(session_id)
            .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))?;
        let _ = removed.tx_op.send(Op::CloseSession {
            session_id: session_id.clone(),
        });
        Ok(removed)
    }

    /// Spawn a new live thread and register it before returning.
    pub(crate) async fn spawn_thread(
        &self,
        params: SpawnThreadParams,
    ) -> Result<Thread, KernelError> {
        let thread = spawn_thread(
            params.session_id,
            params.cwd,
            params.llm,
            params.tools,
            params.context,
            params.agent_path,
            params.agent_control,
            params.approval,
            params.app_config,
            params.recorder,
        );
        self.insert_thread(thread.clone()).await;
        Ok(thread)
    }

    /// Load a persisted thread from replayed history and register it before returning.
    pub(crate) async fn load_thread(
        &self,
        params: LoadThreadParams,
    ) -> Result<Thread, KernelError> {
        let context: Box<dyn ContextManager> =
            Box::new(InMemoryContext::from_messages(params.history));
        let params = SpawnThreadParams::builder()
            .session_id(params.session_id)
            .cwd(params.cwd)
            .llm(params.llm)
            .tools(params.tools)
            .context(context)
            .agent_path(params.agent_path)
            .agent_control(params.agent_control)
            .approval(params.approval)
            .app_config(params.app_config)
            .recorder(params.recorder)
            .build();
        self.spawn_thread(params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::agent::mailbox::mailbox_pair;
    use protocol::{KernelError, Op, SessionId};
    use tokio::sync::{mpsc, oneshot, watch};

    /// Build a real recorder for thread-manager tests.
    fn test_recorder() -> Arc<dyn SessionRecorder> {
        Arc::new(store::FileSessionRecorder::new(
            std::env::temp_dir().join(format!("clawcode-thread-{}.jsonl", uuid::Uuid::new_v4())),
        ))
    }

    #[tokio::test]
    async fn thread_manager_returns_session_not_found_for_missing_send() {
        let manager = ThreadManager::new();
        let missing = SessionId("missing".to_string());

        let error = manager
            .send_op(
                &missing,
                Op::Cancel {
                    session_id: missing.clone(),
                },
            )
            .await
            .expect_err("missing session should fail");

        assert!(matches!(error, KernelError::SessionNotFound(id) if id == missing));
    }

    #[tokio::test]
    async fn cancel_thread_keeps_operation_channel_open_for_future_prompts() {
        let manager = ThreadManager::new();
        let session_id = SessionId("session".to_string());
        let (tx_op, mut rx_op) = mpsc::unbounded_channel();
        let thread = test_thread(session_id.clone(), tx_op);
        manager.insert_thread(thread).await;

        manager
            .cancel_thread(&session_id)
            .await
            .expect("cancel thread");
        manager
            .send_op(
                &session_id,
                Op::Prompt {
                    session_id: session_id.clone(),
                    text: "still alive".to_string(),
                    system: None,
                },
            )
            .await
            .expect("send prompt after cancel");

        let op = rx_op.recv().await.expect("prompt op");
        assert!(matches!(op, Op::Prompt { .. }));
    }

    /// Build a minimal thread handle for ThreadManager routing tests.
    fn test_thread(session_id: SessionId, tx_op: mpsc::UnboundedSender<Op>) -> Thread {
        let (tx_event, _rx_event) = mpsc::unbounded_channel();
        let (cancel_tx, _cancel_rx) = watch::channel(false);
        let (mailbox, _mailbox_rx) = mailbox_pair();
        Thread::builder()
            .session_id(session_id)
            .cwd(PathBuf::from("/tmp/project"))
            .tx_op(tx_op)
            .tx_event(Arc::new(tokio::sync::Mutex::new(tx_event)))
            .pending_approvals(Arc::new(tokio::sync::Mutex::new(HashMap::<
                String,
                oneshot::Sender<protocol::ReviewDecision>,
            >::new())))
            .cancel_tx(cancel_tx)
            .mailbox(mailbox)
            .tools(Arc::new(ToolRegistry::new()))
            .mcp_manager(Arc::new(mcp::McpConnectionManager::new(
                Vec::new(),
                PathBuf::from("/tmp/clawcode-test-auth"),
            )))
            .recorder(test_recorder())
            .input_queue(Arc::new(tokio::sync::Mutex::new(
                crate::input_queue::InputQueue::default(),
            )))
            .build()
    }
}
