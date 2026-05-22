//! Agent control plane: spawn, send message, list, close, status tracking.
//!
//! `AgentControl` is the central handle for multi-agent operations.
//! One instance is shared across all agents in a session tree.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use protocol::{AgentPath, AgentStatus, InterAgentMessage, Op, SessionId};
use tokio::sync::{Mutex, watch};

use crate::agent::mailbox::Mailbox;
use crate::agent::registry::{AgentMetadata, AgentRegistry};
use crate::agent::role::AgentRoleSet;
use crate::approval::ApprovalPolicy;
use crate::context::InMemoryContext;
use crate::thread_manager::{SpawnThreadParams, ThreadManager};
use config::MultiAgentConfig;
use provider::factory::ArcLlm;
use store::{AgentEdgeStatus, AgentGraphStore, CreateSessionParams, SessionRecorder, SessionStore};
use tools::ToolRegistry;

/// Fork mode for sub-agent history (reserved for future).
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) enum ForkMode {
    None,
    LastNTurns(usize),
}

/// A live agent record returned by spawn.
#[derive(Clone, Debug)]
pub struct LiveAgent {
    pub thread_id: SessionId,
    pub metadata: AgentMetadata,
    pub status: AgentStatus,
}

/// A listed agent (public-facing summary).
#[derive(Clone, Debug, serde::Serialize)]
pub struct ListedAgent {
    pub agent_name: String,
    pub agent_status: AgentStatus,
    pub last_task_message: Option<String>,
}

/// Central control plane for multi-agent operations.
#[derive(typed_builder::TypedBuilder)]
pub struct AgentControl {
    #[builder(default = AgentRegistry::new())]
    pub registry: Arc<AgentRegistry>,
    #[builder(default = AgentRoleSet::with_builtins())]
    pub roles: AgentRoleSet,
    #[builder(default)]
    mailboxes: Mutex<HashMap<SessionId, Mailbox>>,
    #[builder(default)]
    status_watchers: Mutex<HashMap<SessionId, watch::Sender<AgentStatus>>>,
    pub config: MultiAgentConfig,
    pub llm_factory: Arc<provider::factory::LlmFactory>,
    pub tools: Arc<ToolRegistry>,
    pub config_handle: config::ConfigHandle,
    /// Live thread lifecycle manager used to spawn and route subagent operations.
    pub(crate) thread_manager: Arc<ThreadManager>,
    /// Session persistence store for writing subagent AgentEdge records.
    #[builder(default)]
    pub session_store: Option<Arc<dyn SessionStore>>,
    /// Durable graph store for parent-child agent topology.
    #[builder(default)]
    pub agent_graph_store: Option<Arc<dyn AgentGraphStore>>,
    /// Recorder handles for live sessions, used to write AgentEdge records on spawn/close.
    #[builder(default)]
    recorders: Mutex<HashMap<SessionId, Arc<dyn SessionRecorder>>>,
}

impl AgentControl {
    /// Create a new AgentControl. Root registration is deferred — the
    /// caller must call `registry.register_root_thread(session_id)` when
    /// the first session is created.
    #[expect(
        clippy::too_many_arguments,
        reason = "AgentControl wires shared kernel services"
    )]
    pub(crate) fn new(
        llm_factory: Arc<provider::factory::LlmFactory>,
        config_handle: config::ConfigHandle,
        tools: Arc<ToolRegistry>,
        config: MultiAgentConfig,
        thread_manager: Arc<ThreadManager>,
        session_store: Option<Arc<dyn SessionStore>>,
        agent_graph_store: Option<Arc<dyn AgentGraphStore>>,
    ) -> Arc<Self> {
        Arc::new(
            AgentControl::builder()
                .config(config)
                .llm_factory(llm_factory)
                .tools(tools)
                .config_handle(config_handle)
                .thread_manager(thread_manager)
                .session_store(session_store)
                .agent_graph_store(agent_graph_store)
                .build(),
        )
    }

    /// Register a session's recorder handle for writing AgentEdge records.
    pub(crate) async fn register_recorder(
        &self,
        session_id: SessionId,
        recorder: Arc<dyn SessionRecorder>,
    ) {
        self.recorders.lock().await.insert(session_id, recorder);
    }

    /// Remove a session's recorder handle.
    pub(crate) async fn unregister_recorder(
        &self,
        session_id: &SessionId,
    ) -> Result<Arc<dyn SessionRecorder>, String> {
        self.recorders
            .lock()
            .await
            .remove(session_id)
            .ok_or_else(|| format!("missing recorder for session {session_id}"))
    }

    /// Spawn a sub-agent under `parent_path` and kick off its first turn.
    ///
    /// # Flow
    /// 1. Check depth limit
    /// 2. Reserve a spawn slot (enforces thread cap)
    /// 3. Reserve path + nickname in registry
    /// 4. Resolve LLM for the role
    /// 5. Create the child session recorder before runtime startup
    /// 6. Spawn the child thread via [`ThreadManager`]
    /// 7. Commit the reservation (publishes agent to registry)
    /// 8. Send the initial prompt as an `InterAgentMessage` to trigger the first turn
    ///
    /// If any step fails before commit, the `SpawnReservation` guard drops and
    /// automatically releases the slot, path, and nickname.
    pub(crate) async fn spawn(
        self: &Arc<Self>,
        parent_path: &AgentPath,
        task_name: &str,
        role_name: &str,
        prompt: &str,
        cwd: PathBuf,
    ) -> Result<LiveAgent, String> {
        // Step 1: depth check
        let depth = AgentRegistry::next_thread_spawn_depth(parent_path);
        if depth > self.config.max_spawn_depth {
            return Err(format!(
                "spawn depth {depth} exceeds max {}",
                self.config.max_spawn_depth
            ));
        }

        // Step 2–3: reserve slot + path + nickname
        let max_threads = self.config.max_concurrent_threads_per_session;
        let mut reservation = self.registry.reserve_spawn_slot(Some(max_threads))?;

        let child_path = parent_path.join(&sanitize_name(task_name));
        reservation.reserve_path(&child_path)?;

        let nickname = reservation.reserve_nickname(None)?;

        let session_id = SessionId(uuid::Uuid::new_v4().to_string());

        // Resolve parent session id before commit so it can be stored in metadata.
        let parent_sid = self.registry.agent_id_for_path(parent_path);
        // Step 4: resolve LLM (role override or fallback to active model)
        let llm = self
            .resolve_llm_for_role(role_name)
            .ok_or_else(|| "no LLM configured for agent spawn".to_string())?;

        let context: Box<dyn crate::context::ContextManager> = Box::new(InMemoryContext::new());

        // Create status watch channel for future status-tracking
        let (status_tx, _status_rx) = watch::channel(AgentStatus::PendingInit);
        self.status_watchers
            .lock()
            .await
            .insert(session_id.clone(), status_tx);

        let child_cwd = cwd.clone();
        let app_config = self.config_handle.current();
        let approval = Arc::new(ApprovalPolicy::new(app_config.approval));
        let store = self
            .session_store
            .as_ref()
            .ok_or_else(|| "cannot spawn subagent without session store".to_string())?;
        let (provider_id, model_id) = self
            .config_handle
            .current()
            .active_model
            .split_once('/')
            .map(|(provider_id, model_id)| (provider_id.to_string(), model_id.to_string()))
            .unwrap_or_default();
        let child_recorder: Arc<dyn SessionRecorder> = store
            .create_session(
                CreateSessionParams::builder()
                    .session_id(session_id.clone())
                    .agent_path(child_path.clone())
                    .cwd(child_cwd.clone())
                    .provider_id(provider_id)
                    .model_id(model_id)
                    .base_system_prompt(String::new())
                    .parent_session_id(parent_sid.clone().ok_or_else(|| {
                        "cannot persist subagent without parent session id".to_string()
                    })?)
                    .agent_role(role_name.to_string())
                    .agent_nickname(nickname.clone())
                    .build(),
            )
            .await
            .map_err(|error| error.to_string())?
            .into();

        // Step 5: create the child session thread after its recorder exists.
        let params = SpawnThreadParams::builder()
            .session_id(session_id.clone())
            .cwd(cwd)
            .llm(llm)
            .tools(Arc::clone(&self.tools))
            .context(context)
            .agent_path(child_path.clone())
            .agent_control(Arc::clone(self))
            .approval(approval)
            .app_config(app_config)
            .recorder(Arc::clone(&child_recorder))
            .build();

        let handle = self
            .thread_manager
            .spawn_thread(params)
            .await
            .map_err(|error| error.to_string())?;

        // Step 6: register mailbox so other agents can send messages here
        self.mailboxes
            .lock()
            .await
            .insert(session_id.clone(), handle.mailbox.clone());

        // Step 7: commit — publishes agent metadata to registry
        let metadata = {
            let builder = AgentMetadata::builder()
                .agent_id(session_id.clone())
                .agent_path(child_path.clone())
                .agent_nickname(nickname.clone())
                .agent_role(role_name.to_string());
            if let Some(ref psid) = parent_sid {
                builder.parent_session_id(psid.clone()).build()
            } else {
                builder.build()
            }
        };

        self.recorders
            .lock()
            .await
            .insert(session_id.clone(), Arc::clone(&child_recorder));

        if let (Some(graph_store), Some(parent_sid)) = (&self.agent_graph_store, &parent_sid) {
            let edge_result = graph_store
                .upsert_agent_edge(
                    parent_sid.clone(),
                    session_id.clone(),
                    child_path.clone(),
                    Some(role_name.to_string()),
                    AgentEdgeStatus::Open,
                )
                .await;
            if let Err(error) = edge_result {
                let _ = self.thread_manager.close_thread(&session_id).await;
                if let Some(store) = &self.session_store {
                    let child_recorder = self.unregister_recorder(&session_id).await?;
                    let _ = store
                        .archive_session(&session_id, child_recorder.as_ref())
                        .await;
                }
                return Err(error.to_string());
            }
        }

        reservation.commit(metadata.clone());

        // Step 8: send initial prompt to kick off first turn.
        self.thread_manager
            .send_op(
                &session_id,
                Op::InterAgentMessage {
                    message: InterAgentMessage::builder()
                        .from(parent_path.clone())
                        .to(child_path.clone())
                        .content(prompt.to_string())
                        .trigger_turn(true)
                        .build(),
                },
            )
            .await
            .map_err(|error| error.to_string())?;

        Ok(LiveAgent {
            thread_id: session_id,
            metadata,
            status: AgentStatus::PendingInit,
        })
    }

    /// Resolve a target string (path or nickname) to an AgentPath.
    pub(crate) fn resolve_target(&self, target: &str) -> Result<AgentPath, String> {
        self.registry.resolve_target(target)
    }

    /// Send a message to a target agent via the thread manager.
    ///
    /// Resolves the target agent's `SessionId` from the registry, then routes
    /// a typed inter-agent operation to the target thread.
    pub(crate) async fn send_message(
        &self,
        from: AgentPath,
        to: AgentPath,
        content: String,
        trigger_turn: bool,
    ) -> Result<(), String> {
        let target_id = self
            .registry
            .agent_id_for_path(&to)
            .ok_or_else(|| format!("agent not found: {to}"))?;

        let message = InterAgentMessage::builder()
            .from(from)
            .to(to.clone())
            .content(content)
            .trigger_turn(trigger_turn)
            .build();
        self.thread_manager
            .send_op(&target_id, Op::InterAgentMessage { message })
            .await
            .map_err(|error| error.to_string())
    }

    /// Notify a parent agent that a child reached a terminal turn status.
    pub(crate) async fn notify_child_terminal_turn(
        &self,
        child_session_id: &SessionId,
        status: AgentStatus,
    ) -> Result<(), String> {
        self.registry
            .update_agent_status(child_session_id, status.clone());
        if let Some(status_tx) = self.status_watchers.lock().await.get(child_session_id) {
            let _ = status_tx.send(status.clone());
        }
        let Some(metadata) = self
            .registry
            .agent_metadata_for_thread(child_session_id.clone())
        else {
            return Ok(());
        };
        let Some(parent_session_id) = metadata.parent_session_id.clone() else {
            return Ok(());
        };
        let child_path = metadata.agent_path.unwrap_or_else(AgentPath::root);
        let child_name = metadata
            .agent_nickname
            .unwrap_or_else(|| child_path.to_string());
        let parent_path = self
            .registry
            .agent_metadata_for_thread(parent_session_id.clone())
            .and_then(|metadata| metadata.agent_path)
            .unwrap_or_else(AgentPath::root);
        let final_message = match &status {
            AgentStatus::Completed { message } => message.as_deref().unwrap_or(""),
            AgentStatus::Errored { reason } => reason.as_str(),
            AgentStatus::Interrupted => "interrupted",
            AgentStatus::Shutdown => "shutdown",
            AgentStatus::PendingInit | AgentStatus::Running | AgentStatus::NotFound => "",
        };
        let content = format!(
            "Subagent {child_name} ({child_session_id}) reached terminal status: {status:?}\n{final_message}"
        );
        let message = InterAgentMessage::builder()
            .from(child_path)
            .to(parent_path)
            .content(content)
            .trigger_turn(false)
            .build();
        self.thread_manager
            .send_op(&parent_session_id, Op::InterAgentMessage { message })
            .await
            .map_err(|error| error.to_string())
    }

    /// List active sub-agents, optionally filtered by path prefix.
    ///
    /// Falls back to the agent path string when no nickname is assigned.
    pub(crate) fn list_agents(&self, prefix: Option<&AgentPath>) -> Vec<ListedAgent> {
        let agents = self.registry.live_agents();
        agents
            .into_iter()
            .filter(|m| {
                if let Some(prefix) = prefix {
                    m.agent_path
                        .as_ref()
                        .is_some_and(|p| p == prefix || is_descendant_path(p, prefix.as_str()))
                } else {
                    true
                }
            })
            .map(|m| ListedAgent {
                agent_name: m.agent_nickname.unwrap_or_else(|| {
                    m.agent_path
                        .as_ref()
                        .map(|p| p.to_string())
                        .unwrap_or_default()
                }),
                agent_status: m.agent_status,
                last_task_message: m.last_task_message,
            })
            .collect()
    }

    /// Write AgentEdge(Closed) to the parent session's recorder before closing a subagent.
    async fn write_closed_agent_edge(&self, thread_id: &SessionId) {
        let Some(graph_store) = &self.agent_graph_store else {
            return;
        };
        let parent_sid = {
            let metadata = self.registry.agent_metadata_for_thread(thread_id.clone());
            metadata.and_then(|m| m.parent_session_id)
        };
        let Some(parent_sid) = parent_sid else {
            return;
        };
        let _ = graph_store
            .set_agent_edge_status(&parent_sid, thread_id, AgentEdgeStatus::Closed)
            .await;
    }

    /// Close an agent and all its descendants.
    ///
    /// Identifies descendant agents by path prefix matching
    /// (e.g. closing `/root/explorer` closes `/root/explorer/researcher`
    /// but not `/root/explorer-other`). Removes entries from registry,
    /// mailbox map, and status watchers. Descendants are cleaned up
    /// before the target agent itself.
    pub(crate) async fn close_agent(&self, agent_path: &AgentPath) -> Result<(), String> {
        let thread_id = self
            .registry
            .agent_id_for_path(agent_path)
            .ok_or_else(|| format!("agent not found: {agent_path}"))?;

        // Identify descendants by path prefix: anything under agent_path/...
        let prefix = agent_path.to_string();
        let descendants: Vec<SessionId> = self
            .registry
            .registered_agents()
            .into_iter()
            .filter(|m| {
                m.agent_path
                    .as_ref()
                    .is_some_and(|p| is_descendant_path(p, &prefix))
            })
            .filter_map(|m| m.agent_id)
            .collect();

        // Persist close: close child session file and write closed edges.
        if let Some(ref store) = self.session_store {
            // Write AgentEdge(Closed) to parent before removing the child recorder.
            self.write_closed_agent_edge(&thread_id).await;
            let child_recorder = self.unregister_recorder(&thread_id).await?;
            let _ = store
                .close_session(&thread_id, child_recorder.as_ref())
                .await;
            // Also handle descendants
            for desc_id in &descendants {
                self.write_closed_agent_edge(desc_id).await;
                let recorder = self.unregister_recorder(desc_id).await?;
                let _ = store.close_session(desc_id, recorder.as_ref()).await;
            }
        }

        // Release descendants first, then self
        for desc_id in &descendants {
            self.registry.release_spawned_thread(desc_id.clone());
        }
        self.registry.release_spawned_thread(thread_id.clone());

        // Clean up mailboxes and status watchers
        {
            let mut mb = self.mailboxes.lock().await;
            for desc_id in &descendants {
                mb.remove(desc_id);
            }
            mb.remove(&thread_id);
            let mut sw = self.status_watchers.lock().await;
            for desc_id in &descendants {
                sw.remove(desc_id);
            }
            sw.remove(&thread_id);
        }

        let _ = self.thread_manager.close_thread(&thread_id).await;
        for desc_id in &descendants {
            let _ = self.thread_manager.close_thread(desc_id).await;
        }

        Ok(())
    }

    /// Register a mailbox for an existing session.
    pub(crate) async fn register_mailbox(&self, thread_id: SessionId, mailbox: Mailbox) {
        self.mailboxes.lock().await.insert(thread_id, mailbox);
    }

    /// Subscribe to status changes.
    #[allow(dead_code)]
    pub(crate) async fn subscribe_status(
        &self,
        thread_id: &SessionId,
    ) -> Option<watch::Receiver<AgentStatus>> {
        let initial_status = self
            .registry
            .agent_metadata_for_thread(thread_id.clone())
            .map(|metadata| metadata.agent_status)?;
        let mut status_watchers = self.status_watchers.lock().await;
        // Restored or partially registered agents may not have an in-memory watcher yet.
        // Recreate it from registry state so wait_agent can still observe terminal status.
        let status_tx = status_watchers
            .entry(thread_id.clone())
            .or_insert_with(|| watch::channel(initial_status).0);
        Some(status_tx.subscribe())
    }

    /// Resolve the LLM for a role: try the role's model override first,
    /// then fall back to the globally configured active model.
    ///
    /// Panics if no LLM can be resolved — callers should ensure at
    /// least one provider is configured before calling spawn.
    fn resolve_llm_for_role(&self, role_name: &str) -> Option<ArcLlm> {
        // Try role-specific model override (e.g. "deepseek/deepseek-v4-flash")
        if let Some(role) = self.roles.get(role_name)
            && let Some(model_spec) = role.model_override()
            && let Some((provider_id, model_id)) = model_spec.split_once('/')
            && let Some(llm) = self.llm_factory.get(provider_id, model_id)
        {
            return Some(llm);
        }
        // Fall back to active_model from config
        let cfg = self.config_handle.current();
        if let Some((provider_id, model_id)) = cfg.active_model.split_once('/')
            && let Some(llm) = self.llm_factory.get(provider_id, model_id)
        {
            return Some(llm);
        }
        None
    }
}

/// Sanitize a user-provided task name into a valid [`AgentPath`] segment.
/// Only lowercase ASCII alphanumerics and underscores are allowed;
/// everything else is replaced with `_`.
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Returns true when `path` is a strict hierarchical child of `prefix`.
fn is_descendant_path(path: &AgentPath, prefix: &str) -> bool {
    path.0
        .strip_prefix(prefix)
        .is_some_and(|suffix| suffix.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::agent::mailbox::mailbox_pair;
    use crate::session::Thread;
    use async_trait::async_trait;
    use config::{AppConfig, ConfigHandle};
    use provider::factory::LlmFactory;
    use store::{
        AgentEdgeStatus, AgentGraphStore, CreateSessionParams, FileSessionStore, SessionRecorder,
        SessionStore,
    };
    use tokio::sync::{mpsc, oneshot, watch};

    #[test]
    fn sanitize_replaces_special_chars() {
        assert_eq!(sanitize_name("code-reviewer"), "code_reviewer");
        assert_eq!(sanitize_name("Hello World!"), "hello_world_");
    }

    /// Build an app config with one OpenAI-compatible provider for spawn tests.
    fn app_config_with_provider() -> AppConfig {
        serde_json::from_value(serde_json::json!({
            "active_model": "deepseek/deepseek-chat",
            "providers": [
                {
                    "id": "deepseek",
                    "display_name": "DeepSeek",
                    "provider_type": "openai-completions",
                    "base_url": "https://example.invalid",
                    "api_key": "test-key",
                    "models": [{ "id": "deepseek-chat" }]
                }
            ]
        }))
        .expect("valid app config")
    }

    /// Build a minimal thread handle for AgentControl routing tests.
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
            .build()
    }

    /// Build a real recorder for agent-control routing tests.
    fn test_recorder() -> Arc<dyn SessionRecorder> {
        Arc::new(store::FileSessionRecorder::new(
            std::env::temp_dir().join(format!("clawcode-agent-{}.jsonl", uuid::Uuid::new_v4())),
        ))
    }

    #[tokio::test]
    async fn spawn_creates_child_session_and_open_graph_edge_before_returning() {
        let temp = tempfile::tempdir().expect("temp data home");
        let data_home = temp.path().to_string_lossy().to_string();
        let store = Arc::new(FileSessionStore::new(Some(&data_home)));
        let session_store: Arc<dyn SessionStore> = Arc::clone(&store) as Arc<dyn SessionStore>;
        let app_config = app_config_with_provider();
        let config_handle = ConfigHandle::from_config(app_config.clone());
        let llm_factory = Arc::new(LlmFactory::new(config_handle.clone()));
        let tools = Arc::new(ToolRegistry::new());
        let thread_manager = Arc::new(ThreadManager::new());
        let control = AgentControl::new(
            llm_factory,
            config_handle,
            Arc::clone(&tools),
            app_config.multi_agent.clone(),
            thread_manager,
            Some(session_store),
            Some(Arc::clone(&store) as Arc<dyn AgentGraphStore>),
        );
        let parent_id = SessionId("parent".to_string());
        let parent_recorder = store
            .create_session(
                CreateSessionParams::builder()
                    .session_id(parent_id.clone())
                    .agent_path(AgentPath::root())
                    .cwd(temp.path().to_path_buf())
                    .provider_id("deepseek".to_string())
                    .model_id("deepseek-chat".to_string())
                    .base_system_prompt(String::new())
                    .build(),
            )
            .await
            .expect("create parent session");
        let parent_recorder: Arc<dyn SessionRecorder> = Arc::from(parent_recorder);
        control.registry.register_root_thread(parent_id.clone());
        control
            .register_recorder(parent_id.clone(), parent_recorder)
            .await;

        let child = control
            .spawn(
                &AgentPath::root(),
                "child",
                "default",
                "do the work",
                temp.path().to_path_buf(),
            )
            .await
            .expect("spawn child");

        let open_children = store
            .list_agent_children(&parent_id, Some(AgentEdgeStatus::Open))
            .expect("list open children");
        assert_eq!(open_children.len(), 1);
        assert_eq!(open_children[0].child_session_id, child.thread_id);
        assert_eq!(open_children[0].child_role.as_deref(), Some("default"));
        assert!(
            store
                .load_session(&child.thread_id)
                .expect("load child")
                .is_some()
        );
    }

    #[tokio::test]
    async fn spawn_closes_child_session_when_graph_edge_write_fails() {
        let temp = tempfile::tempdir().expect("temp data home");
        let data_home = temp.path().to_string_lossy().to_string();
        let store = Arc::new(FileSessionStore::new(Some(&data_home)));
        let session_store: Arc<dyn SessionStore> = Arc::clone(&store) as Arc<dyn SessionStore>;
        let app_config = app_config_with_provider();
        let config_handle = ConfigHandle::from_config(app_config.clone());
        let llm_factory = Arc::new(LlmFactory::new(config_handle.clone()));
        let thread_manager = Arc::new(ThreadManager::new());
        let control = AgentControl::new(
            llm_factory,
            config_handle,
            Arc::new(ToolRegistry::new()),
            app_config.multi_agent.clone(),
            thread_manager,
            Some(session_store),
            Some(Arc::new(FailingAgentGraphStore)),
        );
        let parent_id = SessionId("parent".to_string());
        let parent_recorder = store
            .create_session(
                CreateSessionParams::builder()
                    .session_id(parent_id.clone())
                    .agent_path(AgentPath::root())
                    .cwd(temp.path().to_path_buf())
                    .provider_id("deepseek".to_string())
                    .model_id("deepseek-chat".to_string())
                    .base_system_prompt(String::new())
                    .build(),
            )
            .await
            .expect("create parent");
        let parent_recorder: Arc<dyn SessionRecorder> = Arc::from(parent_recorder);
        control.registry.register_root_thread(parent_id);
        control
            .register_recorder(SessionId("parent".to_string()), parent_recorder)
            .await;

        let error = control
            .spawn(
                &AgentPath::root(),
                "child",
                "default",
                "do work",
                temp.path().to_path_buf(),
            )
            .await
            .expect_err("graph write should fail");

        assert!(error.contains("forced graph failure"));
        let sessions = store.list_sessions(None).expect("list sessions");
        assert_eq!(sessions.len(), 1);
    }

    struct FailingAgentGraphStore;

    #[async_trait]
    impl AgentGraphStore for FailingAgentGraphStore {
        async fn upsert_agent_edge(
            &self,
            _parent_session_id: SessionId,
            _child_session_id: SessionId,
            _child_agent_path: AgentPath,
            _child_role: Option<String>,
            _status: AgentEdgeStatus,
        ) -> std::io::Result<()> {
            Err(std::io::Error::other("forced graph failure"))
        }

        async fn set_agent_edge_status(
            &self,
            _parent_session_id: &SessionId,
            _child_session_id: &SessionId,
            _status: AgentEdgeStatus,
        ) -> std::io::Result<()> {
            Ok(())
        }

        fn list_agent_children(
            &self,
            _parent_session_id: &SessionId,
            _status: Option<AgentEdgeStatus>,
        ) -> std::io::Result<Vec<store::AgentEdge>> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn send_message_routes_through_thread_manager() {
        let app_config = app_config_with_provider();
        let config_handle = ConfigHandle::from_config(app_config.clone());
        let llm_factory = Arc::new(LlmFactory::new(config_handle.clone()));
        let tools = Arc::new(ToolRegistry::new());
        let thread_manager = Arc::new(ThreadManager::new());
        let control = AgentControl::new(
            llm_factory,
            config_handle,
            tools,
            app_config.multi_agent.clone(),
            Arc::clone(&thread_manager),
            None,
            None,
        );
        let parent_id = SessionId("parent".to_string());
        let child_id = SessionId("child".to_string());
        let child_path = AgentPath::root().join("child");
        let (tx_op, mut rx_op) = mpsc::unbounded_channel();
        thread_manager
            .insert_thread(test_thread(child_id.clone(), tx_op))
            .await;
        control.registry.register_root_thread(parent_id.clone());
        control
            .registry
            .restore_agent(
                child_id.clone(),
                child_path.clone(),
                None,
                Some("default".to_string()),
                Some(parent_id),
            )
            .expect("restore child");

        control
            .send_message(
                AgentPath::root(),
                child_path.clone(),
                "hello child".to_string(),
                false,
            )
            .await
            .expect("send message");
        control
            .send_message(AgentPath::root(), child_path, "follow up".to_string(), true)
            .await
            .expect("followup message");

        let first = rx_op.recv().await.expect("first routed message");
        let Op::InterAgentMessage { message } = first else {
            panic!("expected inter-agent message");
        };
        assert!(!message.trigger_turn);
        let second = rx_op.recv().await.expect("second routed message");
        let Op::InterAgentMessage { message } = second else {
            panic!("expected inter-agent message");
        };
        assert!(message.trigger_turn);
    }

    #[tokio::test]
    async fn child_terminal_turn_notifies_parent_without_triggering_turn() {
        let app_config = app_config_with_provider();
        let config_handle = ConfigHandle::from_config(app_config.clone());
        let llm_factory = Arc::new(LlmFactory::new(config_handle.clone()));
        let tools = Arc::new(ToolRegistry::new());
        let thread_manager = Arc::new(ThreadManager::new());
        let control = AgentControl::new(
            llm_factory,
            config_handle,
            tools,
            app_config.multi_agent.clone(),
            Arc::clone(&thread_manager),
            None,
            None,
        );
        let parent_id = SessionId("parent".to_string());
        let child_id = SessionId("child".to_string());
        let child_path = AgentPath::root().join("child");
        let (tx_op, mut rx_op) = mpsc::unbounded_channel();
        thread_manager
            .insert_thread(test_thread(parent_id.clone(), tx_op))
            .await;
        control.registry.register_root_thread(parent_id.clone());
        control
            .registry
            .restore_agent(
                child_id.clone(),
                child_path,
                Some("kid".to_string()),
                Some("default".to_string()),
                Some(parent_id.clone()),
            )
            .expect("restore child");
        let (status_tx, mut status_rx) = watch::channel(AgentStatus::PendingInit);
        control
            .status_watchers
            .lock()
            .await
            .insert(child_id.clone(), status_tx);

        control
            .notify_child_terminal_turn(
                &child_id,
                AgentStatus::Completed {
                    message: Some("done".to_string()),
                },
            )
            .await
            .expect("notify parent");

        status_rx.changed().await.expect("status changed");
        assert_eq!(
            status_rx.borrow().clone(),
            AgentStatus::Completed {
                message: Some("done".to_string())
            }
        );
        let sent = rx_op.recv().await.expect("parent notification");
        let Op::InterAgentMessage { message } = sent else {
            panic!("expected inter-agent message");
        };
        assert!(!message.trigger_turn);
        assert!(message.content.contains("kid"));
        assert!(message.content.contains("done"));
    }

    #[tokio::test]
    async fn subscribe_status_recreates_missing_watcher_from_registry_status() {
        let control = agent_control_no_persistence();
        let child_id = SessionId("child".to_string());
        control
            .registry
            .register_root_thread(SessionId("root".to_string()));
        seed_agent(&control, "child", "child", Some("Child"), Some("root"));
        control.registry.update_agent_status(
            &child_id,
            AgentStatus::Completed {
                message: Some("done".to_string()),
            },
        );

        let status_rx = control
            .subscribe_status(&child_id)
            .await
            .expect("status watcher");

        assert_eq!(
            status_rx.borrow().clone(),
            AgentStatus::Completed {
                message: Some("done".to_string())
            }
        );
    }

    // ── list_agents ──

    /// Builds an AgentControl suitable for in-memory-only tests (no persistence).
    fn agent_control_no_persistence() -> Arc<AgentControl> {
        let app_config = app_config_with_provider();
        let config_handle = ConfigHandle::from_config(app_config.clone());
        let llm_factory = Arc::new(LlmFactory::new(config_handle.clone()));
        let thread_manager = Arc::new(ThreadManager::new());
        AgentControl::new(
            llm_factory,
            config_handle,
            Arc::new(ToolRegistry::new()),
            app_config.multi_agent.clone(),
            thread_manager,
            None,
            None,
        )
    }

    /// Seed a non-root agent into the registry via restore for listing tests.
    fn seed_agent(
        control: &Arc<AgentControl>,
        id: &str,
        name: &str,
        nick: Option<&str>,
        parent: Option<&str>,
    ) {
        let path = AgentPath::root().join(name);
        control
            .registry
            .restore_agent(
                SessionId(id.to_string()),
                path,
                nick.map(|n| n.to_string()),
                Some("default".to_string()),
                parent.map(|p| SessionId(p.to_string())),
            )
            .expect("seed agent");
    }

    #[tokio::test]
    async fn list_agents_returns_non_root_agents() {
        let control = agent_control_no_persistence();
        control
            .registry
            .register_root_thread(SessionId("root".to_string()));
        seed_agent(&control, "a", "alice", Some("Alice"), Some("root"));
        seed_agent(&control, "b", "bob", Some("Bob"), Some("root"));

        let list = control.list_agents(None);
        assert_eq!(list.len(), 2);
        let names: Vec<&str> = list.iter().map(|a| a.agent_name.as_str()).collect();
        assert!(names.contains(&"Alice"));
        assert!(names.contains(&"Bob"));
    }

    #[tokio::test]
    async fn list_agents_omits_terminal_agents() {
        let control = agent_control_no_persistence();
        control
            .registry
            .register_root_thread(SessionId("root".to_string()));
        let (tx_parent, _rx_parent) = mpsc::unbounded_channel::<Op>();
        control
            .thread_manager
            .insert_thread(test_thread(SessionId("root".to_string()), tx_parent))
            .await;
        seed_agent(&control, "done", "done", Some("Done"), Some("root"));
        let (status_tx, _status_rx) = watch::channel(AgentStatus::PendingInit);
        control
            .status_watchers
            .lock()
            .await
            .insert(SessionId("done".to_string()), status_tx);

        control
            .notify_child_terminal_turn(
                &SessionId("done".to_string()),
                AgentStatus::Completed {
                    message: Some("finished".to_string()),
                },
            )
            .await
            .expect("notify terminal status");

        let list = control.list_agents(None);

        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn list_agents_filters_by_path_prefix() {
        let control = agent_control_no_persistence();
        control
            .registry
            .register_root_thread(SessionId("root".to_string()));
        seed_agent(&control, "alpha", "team/alpha", Some("Alpha"), Some("root"));
        seed_agent(&control, "beta", "team/beta", Some("Beta"), Some("root"));
        seed_agent(&control, "sibling", "team_ab", Some("TeamAB"), Some("root"));
        seed_agent(&control, "other", "other", Some("Other"), Some("root"));

        let list = control.list_agents(Some(&AgentPath::root().join("team")));
        assert_eq!(list.len(), 2);
        let names: Vec<&str> = list.iter().map(|a| a.agent_name.as_str()).collect();
        assert!(names.contains(&"Alpha"));
        assert!(names.contains(&"Beta"));
        assert!(!names.contains(&"TeamAB"));
        assert!(!names.contains(&"Other"));
    }

    #[tokio::test]
    async fn list_agents_empty_when_no_sub_agents() {
        let control = agent_control_no_persistence();
        control
            .registry
            .register_root_thread(SessionId("lonely_root".to_string()));
        let list = control.list_agents(None);
        assert!(list.is_empty());
    }

    // ── close_agent ──

    /// Verifies that closing an agent with descendants cascades the close
    /// to all children, grandchildren, etc.
    #[tokio::test]
    async fn close_agent_cascades_to_descendants() {
        let control = agent_control_no_persistence();
        control
            .registry
            .register_root_thread(SessionId("root".to_string()));

        // Build a three-level tree:
        //   /root/team_a          (has tx_op)
        //   /root/team_a/sub_1    (has tx_op)
        //   /root/team_a/sub_1/deep (has tx_op)
        //   /root/team_b          (unrelated, should NOT be closed)
        let parent_id = SessionId("team_a".to_string());
        let sub1_id = SessionId("sub_1".to_string());
        let deep_id = SessionId("deep".to_string());
        let team_b_id = SessionId("team_b".to_string());

        let (tx_parent, _rx_parent) = mpsc::unbounded_channel::<Op>();
        let (tx_sub1, _rx_sub1) = mpsc::unbounded_channel::<Op>();
        let (tx_deep, _rx_deep) = mpsc::unbounded_channel::<Op>();
        let (tx_team_b, _rx_team_b) = mpsc::unbounded_channel::<Op>();

        control
            .thread_manager
            .insert_thread(test_thread(parent_id.clone(), tx_parent))
            .await;
        control
            .thread_manager
            .insert_thread(test_thread(sub1_id.clone(), tx_sub1))
            .await;
        control
            .thread_manager
            .insert_thread(test_thread(deep_id.clone(), tx_deep))
            .await;
        control
            .thread_manager
            .insert_thread(test_thread(team_b_id.clone(), tx_team_b))
            .await;

        seed_agent(&control, "team_a", "team_a", Some("TeamA"), Some("root"));
        seed_agent(
            &control,
            "sub_1",
            "team_a/sub_1",
            Some("Sub1"),
            Some("team_a"),
        );
        seed_agent(
            &control,
            "deep",
            "team_a/sub_1/deep",
            Some("Deep"),
            Some("sub_1"),
        );
        seed_agent(&control, "team_ab", "team_ab", Some("TeamAB"), Some("root"));
        seed_agent(&control, "team_b", "team_b", Some("TeamB"), Some("root"));

        let team_a_path = AgentPath::root().join("team_a");
        control.close_agent(&team_a_path).await.unwrap();

        // team_a and all descendants should be gone
        assert!(
            control
                .registry
                .agent_id_for_path(&AgentPath::root().join("team_a"))
                .is_none()
        );
        assert!(
            control
                .registry
                .agent_id_for_path(&AgentPath::root().join("team_a/sub_1"))
                .is_none()
        );
        assert!(
            control
                .registry
                .agent_id_for_path(&AgentPath::root().join("team_a/sub_1/deep"))
                .is_none()
        );
        // team_b must still be alive
        assert!(
            control
                .registry
                .agent_id_for_path(&AgentPath::root().join("team_b"))
                .is_some()
        );
        // Prefix siblings must not be treated as descendants.
        assert!(
            control
                .registry
                .agent_id_for_path(&AgentPath::root().join("team_ab"))
                .is_some()
        );
    }

    #[tokio::test]
    async fn close_agent_cascades_to_terminal_descendants() {
        let control = agent_control_no_persistence();
        control
            .registry
            .register_root_thread(SessionId("root".to_string()));
        let (tx_parent, _rx_parent) = mpsc::unbounded_channel::<Op>();
        let (tx_child, _rx_child) = mpsc::unbounded_channel::<Op>();
        let (tx_deep, _rx_deep) = mpsc::unbounded_channel::<Op>();
        control
            .thread_manager
            .insert_thread(test_thread(SessionId("team_a".to_string()), tx_parent))
            .await;
        control
            .thread_manager
            .insert_thread(test_thread(SessionId("deep".to_string()), tx_deep))
            .await;
        control
            .thread_manager
            .insert_thread(test_thread(SessionId("child".to_string()), tx_child))
            .await;
        seed_agent(&control, "team_a", "team_a", Some("TeamA"), Some("root"));
        seed_agent(
            &control,
            "child",
            "team_a/child",
            Some("Child"),
            Some("team_a"),
        );
        seed_agent(
            &control,
            "deep",
            "team_a/child/deep",
            Some("Deep"),
            Some("child"),
        );

        control.registry.update_agent_status(
            &SessionId("deep".to_string()),
            AgentStatus::Completed {
                message: Some("done".to_string()),
            },
        );

        control
            .close_agent(&AgentPath::root().join("team_a"))
            .await
            .expect("close parent");

        assert!(
            control
                .registry
                .agent_id_for_path(&AgentPath::root().join("team_a/child/deep"))
                .is_none()
        );
    }

    #[tokio::test]
    async fn close_nonexistent_agent_fails() {
        let control = agent_control_no_persistence();
        control
            .registry
            .register_root_thread(SessionId("root".to_string()));

        let result = control.close_agent(&AgentPath::root().join("ghost")).await;
        assert!(result.is_err());
    }

    // ── spawn depth limit ──

    #[tokio::test]
    async fn spawn_exceeding_depth_limit_is_rejected() {
        let control = agent_control_no_persistence();
        // AgentPath::root() is not needed here — we pass a deep path directly.
        // AgentControl builder sets max_spawn_depth from config.
        // Default MultiAgentConfig::default() has max_spawn_depth = 8.
        // 9 slashes = depth 9 > 8 limit.
        let deep_parent = AgentPath("/root/a/b/c/d/e/f/g/h/i".to_string());

        let error = control
            .spawn(
                &deep_parent,
                "too_deep",
                "default",
                "do work",
                PathBuf::from("/tmp"),
            )
            .await
            .expect_err("should reject deep spawn");

        assert!(error.contains("spawn depth"));
    }
}
