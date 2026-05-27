//! Agent control plane: spawn, send message, list, close, status tracking.
//!
//! `AgentControl` is the central handle for multi-agent operations.
//! One instance is shared across all agents in a session tree.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use protocol::{AgentPath, AgentStatus, InterAgentMessage, Op, SessionId};
use tokio::sync::{Mutex, watch};

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

/// Internal request payload for spawning a sub-agent thread.
#[derive(Clone, Debug, typed_builder::TypedBuilder)]
pub(crate) struct AgentSpawnRequest {
    /// Parent agent path that owns the new child path.
    pub parent_path: AgentPath,
    /// Direct child task name requested by the model.
    pub task_name: String,
    /// Agent role selected by the `agent_type` field.
    pub role_name: String,
    /// Optional `provider/model` override for the child agent.
    #[builder(default, setter(strip_option))]
    pub model: Option<String>,
    /// Initial task prompt sent to the child agent.
    pub prompt: String,
    /// Working directory inherited by the child agent.
    pub cwd: PathBuf,
}

/// Central control plane for multi-agent operations.
#[derive(typed_builder::TypedBuilder)]
pub struct AgentControl {
    #[builder(default = AgentRegistry::new())]
    pub registry: Arc<AgentRegistry>,
    #[builder(default = AgentRoleSet::with_builtins())]
    pub roles: AgentRoleSet,
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

    /// Build UI-only metadata for the root session tree.
    pub async fn agent_ui_snapshot(
        &self,
        root_session_id: &SessionId,
    ) -> Vec<protocol::AgentUiMetadata> {
        let mut entries = self
            .registry
            .registered_agent_metadata()
            .into_iter()
            .filter_map(|metadata| protocol::AgentUiMetadata::try_from(metadata).ok())
            .collect::<Vec<_>>();

        // The picker must always offer the main agent as the first switch target.
        if !entries
            .iter()
            .any(|entry| entry.session_id == *root_session_id && entry.is_root)
        {
            entries.push(
                protocol::AgentUiMetadata::builder()
                    .session_id(root_session_id.clone())
                    .agent_path(AgentPath::root())
                    .status(AgentStatus::Running)
                    .is_root(true)
                    .build(),
            );
        }

        entries.sort_by(|left, right| match (left.is_root, right.is_root) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => left.agent_path.as_str().cmp(right.agent_path.as_str()),
        });
        entries
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
        request: AgentSpawnRequest,
    ) -> Result<LiveAgent, String> {
        let parent_path = &request.parent_path;
        let task_name = request.task_name.as_str();
        let role_name = request.role_name.as_str();
        let model_override = request.model.as_deref();
        let prompt = request.prompt.as_str();
        let role = self
            .roles
            .get(role_name)
            .ok_or_else(|| format!("unknown agent_type '{role_name}'"))?;
        let agent_prompt = role.prompt_override().map(ToString::to_string);

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

        let session_id = SessionId::from(uuid::Uuid::new_v4().to_string());

        // Resolve parent session id before commit so it can be stored in metadata
        // and used to inherit the parent's runtime model when no override is set.
        let parent_sid = self.registry.agent_id_for_path(parent_path);
        let inherited_model = if let Some(parent_sid) = &parent_sid
            && let Some(parent_thread) = self.thread_manager.get_thread(parent_sid).await
        {
            Some(parent_thread.current_model().await)
        } else {
            None
        };
        // Step 4: resolve LLM (request override, role override, parent model, or active model)
        let llm =
            self.resolve_llm_for_role(role_name, model_override, inherited_model.as_deref())?;

        let context: Box<dyn crate::context::ContextManager> = Box::new(InMemoryContext::new());

        // Create status watch channel for future status-tracking
        let (status_tx, _status_rx) = watch::channel(AgentStatus::PendingInit);
        self.status_watchers
            .lock()
            .await
            .insert(session_id.clone(), status_tx);

        let child_cwd = request.cwd.clone();
        let app_config = self.config_handle.current();
        let approval = Arc::new(ApprovalPolicy::new(app_config.approval));
        let store = self
            .session_store
            .as_ref()
            .ok_or_else(|| "cannot spawn subagent without session store".to_string())?;
        let child_recorder: Arc<dyn SessionRecorder> = store
            .create_session(
                CreateSessionParams::builder()
                    .session_id(session_id.clone())
                    .agent_path(child_path.clone())
                    .cwd(child_cwd.clone())
                    .provider_id(llm.provider_id().to_string())
                    .model_id(llm.model_id().to_string())
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
            .cwd(request.cwd)
            .llm(llm)
            .llm_factory(Arc::clone(&self.llm_factory))
            .tools(Arc::clone(&self.tools))
            .context(context)
            .agent_path(child_path.clone())
            .agent_control(Arc::clone(self))
            .approval(approval)
            .app_config(app_config)
            .recorder(Arc::clone(&child_recorder))
            .build();
        let mut params = params;
        params.agent_prompt = agent_prompt;

        self.thread_manager
            .spawn_thread(params)
            .await
            .map_err(|error| error.to_string())?;

        // Step 7: commit — publishes agent metadata to registry
        let mut metadata = {
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
        self.registry
            .update_last_task_message(&session_id, prompt.to_string());
        metadata.last_task_message = Some(prompt.to_string());

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
            .content(content.clone())
            .trigger_turn(trigger_turn)
            .build();
        self.thread_manager
            .send_op(&target_id, Op::InterAgentMessage { message })
            .await
            .map_err(|error| error.to_string())?;
        self.registry.update_last_task_message(&target_id, content);
        Ok(())
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

        let content = render_subagent_notification(
            &child_path,
            child_name.as_str(),
            child_session_id,
            &status,
            final_message,
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
            .map_err(|error| error.to_string())?;

        Ok(())
    }

    /// List active sub-agents, optionally filtered by path prefix.
    ///
    /// Uses the canonical agent path string for Codex V2 output.
    pub(crate) fn list_agents(&self, prefix: Option<&AgentPath>) -> Vec<ListedAgent> {
        let root_path = AgentPath::root();
        let mut listed_agents = Vec::new();

        if prefix.as_ref().is_none_or(|prefix| {
            &root_path == *prefix || is_descendant_path(&root_path, prefix.as_str())
        }) && self.registry.agent_id_for_path(&root_path).is_some()
        {
            listed_agents.push(ListedAgent {
                agent_name: root_path.to_string(),
                agent_status: AgentStatus::Running,
                last_task_message: Some("Main thread".to_string()),
            });
        }

        listed_agents.extend(
            self.registry
                .live_agents()
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
                    agent_name: m
                        .agent_path
                        .as_ref()
                        .map(|p| p.to_string())
                        .or_else(|| m.agent_id.as_ref().map(ToString::to_string))
                        .unwrap_or_default(),
                    agent_status: m.agent_status,
                    last_task_message: m.last_task_message,
                }),
        );
        listed_agents.sort_by(|left, right| left.agent_name.cmp(&right.agent_name));
        listed_agents
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
    /// status watchers. Descendants are cleaned up
    /// before the target agent itself.
    pub(crate) async fn close_agent(&self, agent_path: &AgentPath) -> Result<AgentStatus, String> {
        if agent_path.is_root() {
            return Err("The root agent can't be closed with close_agent".to_string());
        }

        let thread_id = self
            .registry
            .agent_id_for_path(agent_path)
            .ok_or_else(|| format!("agent not found: {agent_path}"))?;
        let previous_status = self
            .registry
            .agent_metadata_for_thread(thread_id.clone())
            .map(|metadata| metadata.agent_status)
            .unwrap_or(AgentStatus::NotFound);

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

        // Clean up status watchers for the closed session tree.
        {
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

        Ok(previous_status)
    }

    /// Subscribe to status changes.
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

    /// Subscribe to mailbox activity for a registered agent path.
    pub(crate) async fn subscribe_mailbox_activity(
        &self,
        agent_path: &AgentPath,
    ) -> Result<watch::Receiver<()>, String> {
        let thread_id = self
            .registry
            .agent_id_for_path(agent_path)
            .ok_or_else(|| format!("agent not found: {agent_path}"))?;
        self.subscribe_session_mailbox_activity(&thread_id).await
    }

    /// Subscribe to mailbox activity for a concrete live session id.
    pub(crate) async fn subscribe_session_mailbox_activity(
        &self,
        session_id: &SessionId,
    ) -> Result<watch::Receiver<()>, String> {
        let thread = self
            .thread_manager
            .get_thread(session_id)
            .await
            .ok_or_else(|| format!("session not found: {session_id}"))?;
        Ok(thread.input_queue.lock().await.subscribe_mailbox())
    }

    /// Resolve the LLM for a role and optional model override.
    ///
    /// Request-level overrides use `provider/model` syntax and take precedence
    /// over role-specific model overrides, the parent runtime model, and config.
    fn resolve_llm_for_role(
        &self,
        role_name: &str,
        model_override: Option<&str>,
        inherited_model: Option<&str>,
    ) -> Result<ArcLlm, String> {
        if let Some(model_spec) = model_override {
            let Some((provider_id, model_id)) = model_spec.split_once('/') else {
                return Err(format!(
                    "model override must use provider/model: {model_spec}"
                ));
            };
            return self
                .llm_factory
                .get(provider_id, model_id)
                .ok_or_else(|| format!("model override is not available: {model_spec}"));
        }
        // Try role-specific model override (e.g. "deepseek/deepseek-v4-flash")
        if let Some(role) = self.roles.get(role_name)
            && let Some(model_spec) = role.model_override()
            && let Some((provider_id, model_id)) = model_spec.split_once('/')
            && let Some(llm) = self.llm_factory.get(provider_id, model_id)
        {
            return Ok(llm);
        }
        if let Some(model_spec) = inherited_model {
            let Some((provider_id, model_id)) = model_spec.split_once('/') else {
                return Err(format!(
                    "parent model must use provider/model: {model_spec}"
                ));
            };
            return self
                .llm_factory
                .get(provider_id, model_id)
                .ok_or_else(|| format!("parent model is not available: {model_spec}"));
        }
        // Fall back to active_model from config when no live parent model is available.
        let cfg = self.config_handle.current();
        if let Some((provider_id, model_id)) = cfg.active_model.split_once('/')
            && let Some(llm) = self.llm_factory.get(provider_id, model_id)
        {
            return Ok(llm);
        }
        Err("no LLM configured for agent spawn".to_string())
    }
}

impl TryFrom<AgentMetadata> for protocol::AgentUiMetadata {
    type Error = ();

    /// Convert registry metadata into UI metadata when it has a session id and path.
    fn try_from(metadata: AgentMetadata) -> Result<Self, Self::Error> {
        let session_id = metadata.agent_id.ok_or(())?;
        let agent_path = metadata.agent_path.ok_or(())?;
        let is_root = agent_path.is_root();
        let mut ui_metadata = protocol::AgentUiMetadata::builder()
            .session_id(session_id)
            .agent_path(agent_path)
            .status(metadata.agent_status)
            .is_root(is_root)
            .build();
        // typed-builder changes the builder type after each optional setter, so optional registry
        // fields are copied onto the completed value instead of conditionally reassigning builders.
        ui_metadata.parent_session_id = metadata.parent_session_id;
        ui_metadata.nickname = metadata.agent_nickname;
        ui_metadata.role = metadata.agent_role;
        Ok(ui_metadata)
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

/// Render a terminal child update in a structured, Codex-style notification envelope.
fn render_subagent_notification(
    child_path: &AgentPath,
    child_name: &str,
    child_session_id: &SessionId,
    status: &AgentStatus,
    final_message: &str,
) -> String {
    let payload = serde_json::json!({
        "agent_path": child_path,
        "agent_name": child_name,
        "child_session_id": child_session_id,
        "status": status,
        "final_message": final_message,
    });
    format!(
        "<subagent_notification>\n{}\n</subagent_notification>",
        payload
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::session::Thread;
    use async_trait::async_trait;
    use config::{AppConfig, ConfigHandle};
    use protocol::Usage;
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

    /// Build an app config with two resolvable provider/model pairs.
    fn app_config_with_two_models() -> AppConfig {
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
                },
                {
                    "id": "openai",
                    "display_name": "OpenAI",
                    "provider_type": "openai-completions",
                    "base_url": "https://example.invalid",
                    "api_key": "test-key",
                    "models": [{ "id": "gpt-5.4" }]
                }
            ]
        }))
        .expect("valid app config")
    }

    /// Build a minimal thread handle for AgentControl routing tests.
    fn test_thread(session_id: SessionId, tx_op: mpsc::UnboundedSender<Op>) -> Thread {
        let (tx_event, rx_event) = mpsc::unbounded_channel();
        let (cancel_tx, _cancel_rx) = watch::channel(false);
        Thread::builder()
            .session_id(session_id)
            .agent_path(AgentPath::root())
            .current_model(Arc::new(tokio::sync::RwLock::new(
                "test/provider-model".to_string(),
            )))
            .current_usage(Arc::new(tokio::sync::RwLock::new(Usage::default())))
            .cwd(PathBuf::from("/tmp/project"))
            .tx_op(tx_op)
            .tx_event(Arc::new(tokio::sync::Mutex::new(tx_event)))
            .initial_rx_event(Arc::new(tokio::sync::Mutex::new(Some(rx_event))))
            .pending_approvals(Arc::new(tokio::sync::Mutex::new(HashMap::<
                String,
                oneshot::Sender<protocol::ReviewDecision>,
            >::new())))
            .cancel_tx(cancel_tx)
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
        let parent_id = SessionId::from("parent");
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
                AgentSpawnRequest::builder()
                    .parent_path(AgentPath::root())
                    .task_name("child".to_string())
                    .role_name("default".to_string())
                    .prompt("do the work".to_string())
                    .cwd(temp.path().to_path_buf())
                    .build(),
            )
            .await
            .expect("spawn child");

        let open_children = store
            .list_agent_children(&parent_id, Some(AgentEdgeStatus::Open))
            .expect("list open children");
        assert_eq!(open_children.len(), 1);
        assert_eq!(open_children[0].child_session_id, child.thread_id);
        assert_eq!(open_children[0].child_role.as_deref(), Some("default"));
        let listed = control.list_agents(None);
        let listed_child = listed
            .iter()
            .find(|agent| agent.agent_name == "/root/child")
            .expect("spawned child should be listed");
        assert_eq!(
            listed_child.last_task_message.as_deref(),
            Some("do the work")
        );
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
        let parent_id = SessionId::from("parent");
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
            .register_recorder(SessionId::from("parent"), parent_recorder)
            .await;

        let error = control
            .spawn(
                AgentSpawnRequest::builder()
                    .parent_path(AgentPath::root())
                    .task_name("child".to_string())
                    .role_name("default".to_string())
                    .prompt("do work".to_string())
                    .cwd(temp.path().to_path_buf())
                    .build(),
            )
            .await
            .expect_err("graph write should fail");

        assert!(error.contains("forced graph failure"));
        let sessions = store.list_sessions(None).expect("list sessions");
        assert_eq!(sessions.len(), 1);
    }

    #[tokio::test]
    async fn spawn_uses_requested_model_override() {
        let temp = tempfile::tempdir().expect("temp data home");
        let data_home = temp.path().to_string_lossy().to_string();
        let store = Arc::new(FileSessionStore::new(Some(&data_home)));
        let session_store: Arc<dyn SessionStore> = Arc::clone(&store) as Arc<dyn SessionStore>;
        let app_config = app_config_with_two_models();
        let config_handle = ConfigHandle::from_config(app_config.clone());
        let llm_factory = Arc::new(LlmFactory::new(config_handle.clone()));
        let tools = Arc::new(ToolRegistry::new());
        let thread_manager = Arc::new(ThreadManager::new());
        let control = AgentControl::new(
            llm_factory,
            config_handle,
            Arc::clone(&tools),
            app_config.multi_agent.clone(),
            Arc::clone(&thread_manager),
            Some(session_store),
            Some(Arc::clone(&store) as Arc<dyn AgentGraphStore>),
        );
        let parent_id = SessionId::from("parent");
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
        control.register_recorder(parent_id, parent_recorder).await;

        let child = control
            .spawn(
                AgentSpawnRequest::builder()
                    .parent_path(AgentPath::root())
                    .task_name("child".to_string())
                    .role_name("default".to_string())
                    .model("openai/gpt-5.4".to_string())
                    .prompt("do the work".to_string())
                    .cwd(temp.path().to_path_buf())
                    .build(),
            )
            .await
            .expect("spawn child with model override");

        let thread = thread_manager
            .get_thread(&child.thread_id)
            .await
            .expect("child thread should be live");
        assert_eq!(thread.current_model().await, "openai/gpt-5.4");
    }

    /// Verifies subagents inherit the parent's live model when no model override is provided.
    #[tokio::test]
    async fn spawn_inherits_parent_runtime_model_without_model_override() {
        let temp = tempfile::tempdir().expect("temp data home");
        let data_home = temp.path().to_string_lossy().to_string();
        let store = Arc::new(FileSessionStore::new(Some(&data_home)));
        let session_store: Arc<dyn SessionStore> = Arc::clone(&store) as Arc<dyn SessionStore>;
        let app_config = app_config_with_two_models();
        let config_handle = ConfigHandle::from_config(app_config.clone());
        let llm_factory = Arc::new(LlmFactory::new(config_handle.clone()));
        let tools = Arc::new(ToolRegistry::new());
        let thread_manager = Arc::new(ThreadManager::new());
        let control = AgentControl::new(
            llm_factory,
            config_handle,
            Arc::clone(&tools),
            app_config.multi_agent.clone(),
            Arc::clone(&thread_manager),
            Some(session_store),
            Some(Arc::clone(&store) as Arc<dyn AgentGraphStore>),
        );
        let parent_id = SessionId::from("parent");
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

        let (tx_op, _rx_op) = mpsc::unbounded_channel();
        let parent_thread = test_thread(parent_id, tx_op);
        *parent_thread.current_model.write().await = "openai/gpt-5.4".to_string();
        thread_manager.insert_thread(parent_thread).await;

        let child = control
            .spawn(
                AgentSpawnRequest::builder()
                    .parent_path(AgentPath::root())
                    .task_name("child".to_string())
                    .role_name("default".to_string())
                    .prompt("do the work".to_string())
                    .cwd(temp.path().to_path_buf())
                    .build(),
            )
            .await
            .expect("spawn child without model override");

        let thread = thread_manager
            .get_thread(&child.thread_id)
            .await
            .expect("child thread should be live");
        assert_eq!(thread.current_model().await, "openai/gpt-5.4");
    }

    #[tokio::test]
    async fn spawn_rejects_unknown_model_override() {
        let control = agent_control_no_persistence();
        control
            .registry
            .register_root_thread(SessionId::from("root-session"));

        let error = control
            .spawn(
                AgentSpawnRequest::builder()
                    .parent_path(AgentPath::root())
                    .task_name("child".to_string())
                    .role_name("default".to_string())
                    .model("missing/model".to_string())
                    .prompt("do work".to_string())
                    .cwd(PathBuf::from("/tmp/project"))
                    .build(),
            )
            .await
            .expect_err("unknown model override should fail");

        assert_eq!(error, "model override is not available: missing/model");
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
        let parent_id = SessionId::from("parent");
        let child_id = SessionId::from("child");
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
            .send_message(
                AgentPath::root(),
                child_path.clone(),
                "follow up".to_string(),
                true,
            )
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

        let listed = control.list_agents(Some(&child_path));
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].agent_name, "/root/child");
        assert_eq!(listed[0].last_task_message.as_deref(), Some("follow up"));
    }

    #[tokio::test]
    async fn spawn_rejects_unknown_agent_type_before_starting_child() {
        let control = agent_control_no_persistence();
        control
            .registry
            .register_root_thread(SessionId::from("root-session"));

        let error = control
            .spawn(
                AgentSpawnRequest::builder()
                    .parent_path(AgentPath::root())
                    .task_name("child".to_string())
                    .role_name("missing-role".to_string())
                    .prompt("inspect".to_string())
                    .cwd(PathBuf::from("/tmp/project"))
                    .build(),
            )
            .await
            .expect_err("unknown role should fail before spawn");

        assert_eq!(error, "unknown agent_type 'missing-role'");
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
        let parent_id = SessionId::from("parent");
        let child_id = SessionId::from("child");
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
    async fn child_terminal_turn_uses_structured_subagent_notification() {
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
        let parent_id = SessionId::from("parent");
        let child_id = SessionId::from("child");
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
                Some(parent_id),
            )
            .expect("restore child");

        control
            .notify_child_terminal_turn(
                &child_id,
                AgentStatus::Completed {
                    message: Some("done".to_string()),
                },
            )
            .await
            .expect("notify parent");

        let Op::InterAgentMessage { message } = rx_op.recv().await.expect("parent notification")
        else {
            panic!("expected inter-agent message");
        };
        assert!(!message.trigger_turn);
        assert!(message.content.contains("<subagent_notification>"));
        assert!(message.content.contains("\"agent_path\":\"/root/child\""));
        assert!(message.content.contains("\"child_session_id\":\"child\""));
        assert!(message.content.contains("\"final_message\":\"done\""));
        assert!(message.content.contains("</subagent_notification>"));
    }

    #[tokio::test]
    async fn subscribe_status_recreates_missing_watcher_from_registry_status() {
        let control = agent_control_no_persistence();
        let child_id = SessionId::from("child");
        control
            .registry
            .register_root_thread(SessionId::from("root"));
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

    /// Verifies UI snapshots always include the root session as the first entry.
    #[tokio::test]
    async fn agent_ui_snapshot_includes_root_first() {
        let control = agent_control_no_persistence();
        let root_id = SessionId::from("root-session");
        control.registry.register_root_thread(root_id.clone());

        let snapshot = control.agent_ui_snapshot(&root_id).await;

        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].session_id, root_id);
        assert!(snapshot[0].is_root);
        assert_eq!(snapshot[0].agent_path, AgentPath::root());
        assert_eq!(snapshot[0].status, AgentStatus::Running);
    }

    /// Verifies UI snapshots include registered children with session ids and parent ids.
    #[tokio::test]
    async fn agent_ui_snapshot_includes_registered_child_metadata() {
        let control = agent_control_no_persistence();
        let root_id = SessionId::from("root-session");
        let child_id = SessionId::from("child-session");
        control.registry.register_root_thread(root_id.clone());
        control
            .registry
            .restore_agent(
                child_id.clone(),
                AgentPath::root().join("inspect"),
                Some("finder".to_string()),
                Some("worker".to_string()),
                Some(root_id.clone()),
            )
            .expect("restore child");

        let snapshot = control.agent_ui_snapshot(&root_id).await;

        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].session_id, root_id);
        assert_eq!(snapshot[1].session_id, child_id);
        assert_eq!(snapshot[1].parent_session_id.as_ref(), Some(&root_id));
        assert_eq!(snapshot[1].nickname.as_deref(), Some("finder"));
        assert_eq!(snapshot[1].role.as_deref(), Some("worker"));
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
                SessionId::from(id.to_string()),
                path,
                nick.map(|n| n.to_string()),
                Some("default".to_string()),
                parent.map(|p| SessionId::from(p.to_string())),
            )
            .expect("seed agent");
    }

    /// Build a test inter-agent message for mailbox queue assertions.
    fn test_inter_agent_message(content: &str) -> InterAgentMessage {
        InterAgentMessage::builder()
            .from(AgentPath::root().join("child"))
            .to(AgentPath::root())
            .content(content.to_string())
            .trigger_turn(false)
            .build()
    }

    #[tokio::test]
    async fn session_mailbox_subscription_reflects_pending_queue_only() {
        let control = agent_control_no_persistence();
        let root_id = SessionId::from("root");
        control.registry.register_root_thread(root_id.clone());
        let (tx_parent, _rx_parent) = mpsc::unbounded_channel::<Op>();
        let root_thread = test_thread(root_id.clone(), tx_parent);
        let input_queue = Arc::clone(&root_thread.input_queue);
        control.thread_manager.insert_thread(root_thread).await;

        input_queue
            .lock()
            .await
            .enqueue_mailbox_communication(test_inter_agent_message("pending"));
        let pending_rx = control
            .subscribe_session_mailbox_activity(&root_id)
            .await
            .expect("mailbox subscription");
        assert!(pending_rx.has_changed().expect("mailbox watcher open"));

        let drained = input_queue.lock().await.drain_mailbox_input_items();
        let fresh_rx = control
            .subscribe_session_mailbox_activity(&root_id)
            .await
            .expect("fresh mailbox subscription");

        assert_eq!(drained.len(), 1);
        assert!(!fresh_rx.has_changed().expect("mailbox watcher open"));
    }

    #[tokio::test]
    async fn list_agents_returns_non_root_agents() {
        let control = agent_control_no_persistence();
        control
            .registry
            .register_root_thread(SessionId::from("root"));
        seed_agent(&control, "a", "alice", Some("Alice"), Some("root"));
        seed_agent(&control, "b", "bob", Some("Bob"), Some("root"));

        let list = control.list_agents(None);
        assert_eq!(list.len(), 3);
        let names: Vec<&str> = list.iter().map(|a| a.agent_name.as_str()).collect();
        assert_eq!(names[0], "/root");
        assert!(names.contains(&"/root/alice"));
        assert!(names.contains(&"/root/bob"));
    }

    #[tokio::test]
    async fn list_agents_omits_terminal_agents() {
        let control = agent_control_no_persistence();
        control
            .registry
            .register_root_thread(SessionId::from("root"));
        let (tx_parent, _rx_parent) = mpsc::unbounded_channel::<Op>();
        control
            .thread_manager
            .insert_thread(test_thread(SessionId::from("root"), tx_parent))
            .await;
        seed_agent(&control, "done", "done", Some("Done"), Some("root"));
        let (status_tx, _status_rx) = watch::channel(AgentStatus::PendingInit);
        control
            .status_watchers
            .lock()
            .await
            .insert(SessionId::from("done"), status_tx);

        control
            .notify_child_terminal_turn(
                &SessionId::from("done"),
                AgentStatus::Completed {
                    message: Some("finished".to_string()),
                },
            )
            .await
            .expect("notify terminal status");

        let list = control.list_agents(None);

        assert_eq!(list.len(), 1);
        assert_eq!(list[0].agent_name, "/root");
    }

    #[tokio::test]
    async fn list_agents_filters_by_path_prefix() {
        let control = agent_control_no_persistence();
        control
            .registry
            .register_root_thread(SessionId::from("root"));
        seed_agent(&control, "alpha", "team/alpha", Some("Alpha"), Some("root"));
        seed_agent(&control, "beta", "team/beta", Some("Beta"), Some("root"));
        seed_agent(&control, "sibling", "team_ab", Some("TeamAB"), Some("root"));
        seed_agent(&control, "other", "other", Some("Other"), Some("root"));

        let list = control.list_agents(Some(&AgentPath::root().join("team")));
        assert_eq!(list.len(), 2);
        let names: Vec<&str> = list.iter().map(|a| a.agent_name.as_str()).collect();
        assert!(names.contains(&"/root/team/alpha"));
        assert!(names.contains(&"/root/team/beta"));
        assert!(!names.contains(&"/root/team_ab"));
        assert!(!names.contains(&"/root/other"));
    }

    #[tokio::test]
    async fn list_agents_empty_when_no_sub_agents() {
        let control = agent_control_no_persistence();
        control
            .registry
            .register_root_thread(SessionId::from("lonely_root"));
        let list = control.list_agents(None);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].agent_name, "/root");
        assert_eq!(list[0].last_task_message.as_deref(), Some("Main thread"));
    }

    // ── close_agent ──

    /// Verifies that closing an agent with descendants cascades the close
    /// to all children, grandchildren, etc.
    #[tokio::test]
    async fn close_agent_cascades_to_descendants() {
        let control = agent_control_no_persistence();
        control
            .registry
            .register_root_thread(SessionId::from("root"));

        // Build a three-level tree:
        //   /root/team_a          (has tx_op)
        //   /root/team_a/sub_1    (has tx_op)
        //   /root/team_a/sub_1/deep (has tx_op)
        //   /root/team_b          (unrelated, should NOT be closed)
        let parent_id = SessionId::from("team_a");
        let sub1_id = SessionId::from("sub_1");
        let deep_id = SessionId::from("deep");
        let team_b_id = SessionId::from("team_b");

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
            .register_root_thread(SessionId::from("root"));
        let (tx_parent, _rx_parent) = mpsc::unbounded_channel::<Op>();
        let (tx_child, _rx_child) = mpsc::unbounded_channel::<Op>();
        let (tx_deep, _rx_deep) = mpsc::unbounded_channel::<Op>();
        control
            .thread_manager
            .insert_thread(test_thread(SessionId::from("team_a"), tx_parent))
            .await;
        control
            .thread_manager
            .insert_thread(test_thread(SessionId::from("deep"), tx_deep))
            .await;
        control
            .thread_manager
            .insert_thread(test_thread(SessionId::from("child"), tx_child))
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
            &SessionId::from("deep"),
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
            .register_root_thread(SessionId::from("root"));

        let result = control.close_agent(&AgentPath::root().join("ghost")).await;
        result.expect_err("closing a missing agent should fail");
    }

    #[tokio::test]
    async fn close_agent_rejects_root_agent() {
        let control = agent_control_no_persistence();
        control
            .registry
            .register_root_thread(SessionId::from("root"));

        let result = control.close_agent(&AgentPath::root()).await;

        assert_eq!(
            result.expect_err("closing root should fail"),
            "The root agent can't be closed with close_agent"
        );
        assert_eq!(
            control.registry.agent_id_for_path(&AgentPath::root()),
            Some(SessionId::from("root"))
        );
    }

    #[tokio::test]
    async fn followup_to_closed_agent_fails_after_registry_release() {
        let control = agent_control_no_persistence();
        control
            .registry
            .register_root_thread(SessionId::from("root"));
        let target_id = SessionId::from("target");
        let (tx_target, _rx_target) = mpsc::unbounded_channel::<Op>();
        control
            .thread_manager
            .insert_thread(test_thread(target_id.clone(), tx_target))
            .await;
        seed_agent(
            &control,
            target_id.0.as_ref(),
            "target",
            Some("Target"),
            Some("root"),
        );

        control
            .close_agent(&AgentPath::root().join("target"))
            .await
            .expect("close target");
        let result = control
            .send_message(
                AgentPath::root().join("child"),
                AgentPath::root().join("target"),
                "followup".to_string(),
                true,
            )
            .await;

        assert_eq!(
            result.expect_err("closed target should reject followup"),
            "agent not found: /root/target"
        );
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
                AgentSpawnRequest::builder()
                    .parent_path(deep_parent)
                    .task_name("too_deep".to_string())
                    .role_name("default".to_string())
                    .prompt("do work".to_string())
                    .cwd(PathBuf::from("/tmp"))
                    .build(),
            )
            .await
            .expect_err("should reject deep spawn");

        assert!(error.contains("spawn depth"));
    }
}
