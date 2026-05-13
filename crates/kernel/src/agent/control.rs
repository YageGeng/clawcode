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
use crate::context::InMemoryContext;
use config::MultiAgentConfig;
use provider::factory::ArcLlm;
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
}

impl AgentControl {
    /// Create a new AgentControl. Root registration is deferred — the
    /// caller must call `registry.register_root_thread(session_id)` when
    /// the first session is created.
    pub(crate) fn new(
        llm_factory: Arc<provider::factory::LlmFactory>,
        config_handle: config::ConfigHandle,
        tools: Arc<ToolRegistry>,
        config: MultiAgentConfig,
    ) -> Arc<Self> {
        Arc::new(
            AgentControl::builder()
                .config(config)
                .llm_factory(llm_factory)
                .tools(tools)
                .config_handle(config_handle)
                .build(),
        )
    }

    /// Spawn a sub-agent under `parent_path` and kick off its first turn.
    ///
    /// # Flow
    /// 1. Check depth limit
    /// 2. Reserve a spawn slot (enforces thread cap)
    /// 3. Reserve path + nickname in registry
    /// 4. Resolve LLM for the role
    /// 5. Spawn a new session thread via [`crate::session::spawn_thread`]
    /// 6. Register the child's mailbox for message routing
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

        // Step 4: resolve LLM (role override or fallback to active model)
        let llm = self.resolve_llm_for_role(role_name);

        let context: Box<dyn crate::context::ContextManager> = Box::new(InMemoryContext::new());

        // Create status watch channel for future status-tracking
        let (status_tx, _status_rx) = watch::channel(AgentStatus::PendingInit);
        self.status_watchers
            .lock()
            .await
            .insert(session_id.clone(), status_tx);

        // Step 5: create the child session thread
        let handle = crate::session::spawn_thread(
            session_id.clone(),
            cwd,
            llm,
            Arc::clone(&self.tools),
            context,
            child_path.clone(),
            Some(Arc::clone(self)),
            Arc::new(crate::approval::ApprovalPolicy::default()),
        );

        // Step 6: register mailbox so other agents can send messages here
        self.mailboxes
            .lock()
            .await
            .insert(session_id.clone(), handle.mailbox.clone());

        // Step 7: commit — publishes agent metadata to registry
        let metadata = AgentMetadata::builder()
            .agent_id(session_id.clone())
            .agent_path(child_path.clone())
            .agent_nickname(nickname.clone())
            .agent_role(role_name.to_string())
            .build();

        reservation.commit(metadata.clone());

        // Step 8: send initial prompt to kick off first turn
        let _ = handle.tx_op.send(Op::InterAgentMessage {
            from: parent_path.clone(),
            to: child_path,
            content: prompt.to_string(),
        });

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

    /// Send a message to a target agent via its mailbox.
    ///
    /// Resolves the target agent's `SessionId` from the registry, then
    /// looks up its [`Mailbox`] and delivers the message. If `trigger_turn`
    /// is true, the target's `run_loop` will wake up and execute a turn.
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

        let msg = InterAgentMessage::builder()
            .from(from)
            .to(to.clone())
            .content(content)
            .trigger_turn(trigger_turn)
            .build();

        let mailboxes = self.mailboxes.lock().await;
        let mb = mailboxes
            .get(&target_id)
            .ok_or_else(|| format!("mailbox not found for agent: {to}"))?;
        mb.send(msg);

        Ok(())
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
                        .is_some_and(|p| p.0.starts_with(&prefix.0))
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
                agent_status: AgentStatus::Running,
                last_task_message: m.last_task_message,
            })
            .collect()
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
            .live_agents()
            .into_iter()
            .filter(|m| {
                m.agent_path
                    .as_ref()
                    .is_some_and(|p| p.0.starts_with(&prefix) && p.0 != prefix)
            })
            .filter_map(|m| m.agent_id)
            .collect();

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
        self.status_watchers
            .lock()
            .await
            .get(thread_id)
            .map(|tx| tx.subscribe())
    }

    /// Resolve the LLM for a role: try the role's model override first,
    /// then fall back to the globally configured active model.
    ///
    /// Panics if no LLM can be resolved — callers should ensure at
    /// least one provider is configured before calling spawn.
    fn resolve_llm_for_role(&self, role_name: &str) -> ArcLlm {
        // Try role-specific model override (e.g. "deepseek/deepseek-v4-flash")
        if let Some(role) = self.roles.get(role_name)
            && let Some(model_spec) = role.model_override()
            && let Some((provider_id, model_id)) = model_spec.split_once('/')
            && let Some(llm) = self.llm_factory.get(provider_id, model_id)
        {
            return llm;
        }
        // Fall back to active_model from config
        let cfg = self.config_handle.current();
        if let Some((provider_id, model_id)) = cfg.active_model.split_once('/')
            && let Some(llm) = self.llm_factory.get(provider_id, model_id)
        {
            return llm;
        }
        panic!("no LLM configured for agent spawn")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_special_chars() {
        assert_eq!(sanitize_name("code-reviewer"), "code_reviewer");
        assert_eq!(sanitize_name("Hello World!"), "hello_world_");
    }
}
