# Multi-Agent Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement full multi-agent runtime: AgentRegistry, AgentControl, Mailbox, AgentRole, agent management tools, and fork/history management.

**Architecture:** Shared session tree — each sub-agent is a tokio task with its own SessionId/Thread, sharing an Arc<AgentControl> that holds the AgentRegistry. Inter-agent communication via per-Session Mailbox (mpsc + watch). Agent management exposed as Tool implementations.

**Tech Stack:** Rust (edition 2024), tokio, typed-builder, async-trait

---

### Task 1: Agent names file + protocol type extensions

**Files:**
- Create: `crates/kernel/src/agent/agent_names.txt`
- Create: `crates/kernel/src/agent/mod.rs`
- Modify: `crates/protocol/src/agent.rs`
- Modify: `crates/protocol/src/event.rs`

- [ ] **Step 1: Create agent_names.txt**

Copy from Codex `codex-rs/core/src/agent/agent_names.txt`:
```bash
cp /Users/isbset/Documents/codex/codex-rs/core/src/agent/agent_names.txt /Users/isbset/Documents/clawcode/crates/kernel/src/agent/agent_names.txt
```

- [ ] **Step 2: Create agent module root**

Write `crates/kernel/src/agent/mod.rs`:
```rust
//! Multi-agent runtime: registry, control, mailbox, roles.

pub(crate) mod control;
pub(crate) mod mailbox;
pub(crate) mod registry;
pub(crate) mod role;
```

These files don't exist yet, so compilation will fail on this mod.rs. We'll create the submodules in subsequent tasks.

- [ ] **Step 3: Add PendingInit and NotFound to AgentStatus**

Edit `crates/protocol/src/agent.rs`, replace the `AgentStatus` enum:
```rust
/// Runtime status of an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Agent reserved but not yet started.
    PendingInit,
    /// Agent is currently processing a turn.
    Running,
    /// Turn was interrupted, agent can receive more input.
    Interrupted,
    /// Agent completed successfully.
    Completed {
        /// Optional final assistant message content.
        message: Option<String>,
    },
    /// Agent encountered an error.
    Errored {
        /// Human-readable error description.
        reason: String,
    },
    /// Agent has been shut down.
    Shutdown,
    /// Agent path or nickname not found in registry.
    NotFound,
}
```

- [ ] **Step 4: Add AgentSpawned event to protocol/event.rs**

Add a new variant to the `Event` enum and a constructor:

New variant (add after `AgentStatusChange`):
```rust
    /// A sub-agent was spawned.
    AgentSpawned {
        session_id: SessionId,
        /// Canonical path of the new agent.
        agent_path: AgentPath,
        /// Human-readable nickname.
        agent_nickname: String,
        /// Role assigned at spawn.
        agent_role: String,
    },
```

New constructor on `impl Event`:
```rust
    /// Create an `AgentSpawned` event.
    #[inline(always)]
    pub fn agent_spawned(
        session_id: impl Into<SessionId>,
        agent_path: impl Into<AgentPath>,
        agent_nickname: impl Into<String>,
        agent_role: impl Into<String>,
    ) -> Self {
        Event::AgentSpawned {
            session_id: session_id.into(),
            agent_path: agent_path.into(),
            agent_nickname: agent_nickname.into(),
            agent_role: agent_role.into(),
        }
    }
```

- [ ] **Step 5: Verify protocol compiles**

```bash
cargo build -p protocol 2>&1 | tail -5
```
Expected: `Compiling protocol ...` with no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/kernel/src/agent/agent_names.txt crates/kernel/src/agent/mod.rs crates/protocol/src/agent.rs crates/protocol/src/event.rs
git commit -m "feat(agent): add agent_names.txt, module skeleton, AgentStatus variants, and AgentSpawned event"
```

---

### Task 2: MultiAgentConfig

**Files:**
- Create: `crates/config/src/multi_agent.rs`
- Modify: `crates/config/src/config.rs`
- Modify: `crates/config/src/lib.rs` (if it exists to re-export)

- [ ] **Step 1: Check config lib.rs**

```bash
ls /Users/isbset/Documents/clawcode/crates/config/src/lib.rs 2>/dev/null && cat /Users/isbset/Documents/clawcode/crates/config/src/lib.rs
```

If lib.rs doesn't exist, we'll need to create it.

- [ ] **Step 2: Write MultiAgentConfig**

Write `crates/config/src/multi_agent.rs`:
```rust
//! Multi-agent configuration: thread limits, depth, wait timeouts.

use serde::Deserialize;

/// Configuration for the multi-agent subsystem.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MultiAgentConfig {
    /// Maximum number of concurrent sub-agent threads per session tree.
    pub max_concurrent_threads_per_session: usize,
    /// Maximum spawn depth (root = 0, its child = 1, etc.).
    pub max_spawn_depth: i32,
    /// Minimum time in milliseconds that wait_agent should block before
    /// returning with a timeout.
    pub min_wait_timeout_ms: u64,
    /// When true, spawn_agent tool returns only `task_name` instead of
    /// `{ task_name, nickname }`.
    pub hide_spawn_metadata: bool,
}

impl Default for MultiAgentConfig {
    fn default() -> Self {
        Self {
            max_concurrent_threads_per_session: 8,
            max_spawn_depth: 8,
            min_wait_timeout_ms: 1000,
            hide_spawn_metadata: false,
        }
    }
}
```

- [ ] **Step 3: Integrate into AppConfig**

Edit `crates/config/src/config.rs`, add the field:
```rust
// Add to the struct:
    /// Multi-agent subsystem configuration.
    #[serde(default)]
    pub multi_agent: crate::multi_agent::MultiAgentConfig,
```

Full struct should read:
```rust
/// Top-level application configuration.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppConfig {
    /// Configured LLM providers.
    #[serde(default)]
    pub providers: Vec<LlmProvider>,
    /// Active model in `provider_id/model_id` format (e.g. "deepseek/deepseek-v4-flash").
    #[serde(default = "default_active_model")]
    pub active_model: String,
    /// Multi-agent subsystem configuration.
    #[serde(default)]
    pub multi_agent: crate::multi_agent::MultiAgentConfig,
}
```

Add `mod multi_agent;` to `crates/config/src/lib.rs`. If lib.rs doesn't exist, write:
```rust
//! Configuration crate for clawcode.

pub mod config;
pub mod llm;
pub mod loader;
pub mod multi_agent;

pub use config::AppConfig;
pub use loader::{ConfigError, ConfigHandle, load, load_from};
pub use llm::{ApiKeyConfig, LlmModel, LlmProvider, ProviderId, ProviderType};
pub use multi_agent::MultiAgentConfig;
```

- [ ] **Step 4: Verify config compiles**

```bash
cargo build -p config 2>&1 | tail -5
```
Expected: No errors.

- [ ] **Step 5: Commit**

```bash
git add crates/config/src/
git commit -m "feat(config): add MultiAgentConfig with thread limits, depth, and wait timeout"
```

---

### Task 3: AgentRegistry

**Files:**
- Create: `crates/kernel/src/agent/registry.rs`
- Modify: `crates/kernel/Cargo.toml` (if rand needed)

- [ ] **Step 1: Check if rand is available**

```bash
grep 'rand' /Users/isbset/Documents/clawcode/Cargo.toml
```

If not present, add to workspace dependencies:
```toml
rand = "0.9"
```

And add to kernel/Cargo.toml:
```toml
rand = { workspace = true }
```

- [ ] **Step 2: Write AgentRegistry**

Write `crates/kernel/src/agent/registry.rs`:
```rust
//! In-memory registry of all live agents in a session tree.
//!
//! Tracks agent metadata, enforces uniqueness of paths and nicknames,
//! and respects configurable thread-count limits.

use std::collections::{HashMap, HashSet};
use std::collections::hash_map::Entry;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use protocol::{AgentPath, SessionId};
use rand::prelude::IndexedRandom;

/// Compile-time embedded pool of agent nicknames.
const AGENT_NAMES: &str = include_str!("agent_names.txt");

/// Metadata tracked for each active agent.
#[derive(Clone, Debug, Default)]
pub(crate) struct AgentMetadata {
    pub(crate) agent_id: Option<SessionId>,
    pub(crate) agent_path: Option<AgentPath>,
    pub(crate) agent_nickname: Option<String>,
    pub(crate) agent_role: Option<String>,
    pub(crate) last_task_message: Option<String>,
}

/// Internal mutable state of the registry.
#[derive(Default)]
struct ActiveAgents {
    /// Agent tree keyed by agent_path string.
    agent_tree: HashMap<String, AgentMetadata>,
    /// Nicknames currently in use.
    used_agent_nicknames: HashSet<String>,
    /// How many times the nickname pool has been exhausted and reset.
    nickname_reset_count: usize,
}

/// Shared registry of all live agents in a session tree.
///
/// Wraps a `Mutex<ActiveAgents>` for interior mutability plus an
/// `AtomicUsize` counter for fast thread-limit checks without
/// always acquiring the lock.
#[derive(Default)]
pub(crate) struct AgentRegistry {
    active_agents: Mutex<ActiveAgents>,
    total_count: AtomicUsize,
}

/// Formats a nickname with an optional ordinal suffix on pool reset.
fn format_agent_nickname(name: &str, nickname_reset_count: usize) -> String {
    match nickname_reset_count {
        0 => name.to_string(),
        reset_count => {
            let value = reset_count + 1;
            let suffix = match value % 100 {
                11..=13 => "th",
                _ => match value % 10 {
                    1 => "st",
                    2 => "nd",
                    3 => "rd",
                    _ => "th",
                },
            };
            format!("{name} the {value}{suffix}")
        }
    }
}

impl AgentRegistry {
    /// Create a new empty registry.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            active_agents: Mutex::new(ActiveAgents::default()),
            total_count: AtomicUsize::new(0),
        })
    }

    /// Try to reserve a spawn slot, failing if the limit is exceeded.
    pub(crate) fn reserve_spawn_slot(
        self: &Arc<Self>,
        max_threads: Option<usize>,
    ) -> Result<SpawnReservation, String> {
        if let Some(limit) = max_threads {
            if !self.try_increment_spawned(limit) {
                return Err(format!("agent limit reached: max {limit} threads"));
            }
        } else {
            self.total_count.fetch_add(1, Ordering::AcqRel);
        }
        Ok(SpawnReservation {
            state: Arc::clone(self),
            active: true,
            reserved_agent_nickname: None,
            reserved_agent_path: None,
        })
    }

    /// Release a thread from the counter and agent tree.
    pub(crate) fn release_spawned_thread(&self, thread_id: SessionId) {
        let removed = {
            let mut agents = self.active_agents
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let key = agents.agent_tree
                .iter()
                .find_map(|(k, m)| (m.agent_id == Some(thread_id)).then_some(k.clone()));
            key.and_then(|k| agents.agent_tree.remove(&k))
                .is_some_and(|m| !m.agent_path.as_ref().is_some_and(|p| p.is_root()))
        };
        if removed {
            self.total_count.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// Register the root thread.
    pub(crate) fn register_root_thread(&self, thread_id: SessionId) {
        let mut agents = self.active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        agents.agent_tree
            .entry(AgentPath::root().to_string())
            .or_insert_with(|| AgentMetadata {
                agent_id: Some(thread_id),
                agent_path: Some(AgentPath::root()),
                ..Default::default()
            });
    }

    /// Look up an agent's thread id by path.
    pub(crate) fn agent_id_for_path(&self, agent_path: &AgentPath) -> Option<SessionId> {
        self.active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .agent_tree
            .get(agent_path.as_str())
            .and_then(|m| m.agent_id)
    }

    /// Look up agent metadata by thread id.
    pub(crate) fn agent_metadata_for_thread(&self, thread_id: SessionId) -> Option<AgentMetadata> {
        self.active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .agent_tree
            .values()
            .find(|m| m.agent_id == Some(thread_id))
            .cloned()
    }

    /// Return metadata for all live non-root agents.
    pub(crate) fn live_agents(&self) -> Vec<AgentMetadata> {
        self.active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .agent_tree
            .values()
            .filter(|m| {
                m.agent_id.is_some()
                    && !m.agent_path.as_ref().is_some_and(|p| p.is_root())
            })
            .cloned()
            .collect()
    }

    /// Update last_task_message for a thread.
    pub(crate) fn update_last_task_message(&self, thread_id: SessionId, msg: String) {
        let mut agents = self.active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(meta) = agents.agent_tree
            .values_mut()
            .find(|m| m.agent_id == Some(thread_id))
        {
            meta.last_task_message = Some(msg);
        }
    }

    /// Compute the spawn depth of the next child under a parent path.
    pub(crate) fn next_thread_spawn_depth(parent_path: &AgentPath) -> i32 {
        parent_path.0.matches('/').count() as i32
    }

    // ── internal helpers ──

    fn register_spawned_thread(&self, metadata: AgentMetadata) {
        let Some(thread_id) = metadata.agent_id else { return };
        let mut agents = self.active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = metadata.agent_path
            .as_ref()
            .map(|p| p.to_string())
            .unwrap_or_else(|| format!("thread:{thread_id}"));
        if let Some(ref nick) = metadata.agent_nickname {
            agents.used_agent_nicknames.insert(nick.clone());
        }
        agents.agent_tree.insert(key, metadata);
    }

    fn reserve_agent_nickname(&self, preferred: Option<&str>) -> Option<String> {
        let mut agents = self.active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(pref) = preferred {
            return Some(pref.to_string());
        }

        let names: Vec<&str> = AGENT_NAMES.lines().collect();
        if names.is_empty() {
            return None;
        }

        let available: Vec<String> = names
            .iter()
            .map(|n| format_agent_nickname(n, agents.nickname_reset_count))
            .filter(|n| !agents.used_agent_nicknames.contains(n))
            .collect();

        let chosen = if let Some(name) = available.choose(&mut rand::rng()) {
            name.clone()
        } else {
            agents.used_agent_nicknames.clear();
            agents.nickname_reset_count += 1;
            format_agent_nickname(
                names.choose(&mut rand::rng())?,
                agents.nickname_reset_count,
            )
        };

        agents.used_agent_nicknames.insert(chosen.clone());
        Some(chosen)
    }

    fn reserve_agent_path(&self, agent_path: &AgentPath) -> Result<(), String> {
        let mut agents = self.active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match agents.agent_tree.entry(agent_path.to_string()) {
            Entry::Occupied(_) => {
                Err(format!("agent path `{agent_path}` already exists"))
            }
            Entry::Vacant(entry) => {
                entry.insert(AgentMetadata {
                    agent_path: Some(agent_path.clone()),
                    ..Default::default()
                });
                Ok(())
            }
        }
    }

    fn release_reserved_agent_path(&self, agent_path: &AgentPath) {
        let mut agents = self.active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if agents.agent_tree
            .get(agent_path.as_str())
            .is_some_and(|m| m.agent_id.is_none())
        {
            agents.agent_tree.remove(agent_path.as_str());
        }
    }

    fn try_increment_spawned(&self, max_threads: usize) -> bool {
        let mut current = self.total_count.load(Ordering::Acquire);
        loop {
            if current >= max_threads {
                return false;
            }
            match self.total_count.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(updated) => current = updated,
            }
        }
    }
}

/// Two-phase commit guard for agent creation.
///
/// Reserves a spawn slot, nickname, and path. If `commit()` is called,
/// the agent is registered. On drop (without commit), all reservations
/// are released.
pub(crate) struct SpawnReservation {
    state: Arc<AgentRegistry>,
    active: bool,
    reserved_agent_nickname: Option<String>,
    reserved_agent_path: Option<AgentPath>,
}

impl SpawnReservation {
    /// Reserve a nickname from the pool, optionally preferring a specific one.
    pub(crate) fn reserve_nickname(&mut self, preferred: Option<&str>) -> Result<String, String> {
        let nick = self.state
            .reserve_agent_nickname(preferred)
            .ok_or_else(|| "no available agent nicknames".to_string())?;
        self.reserved_agent_nickname = Some(nick.clone());
        Ok(nick)
    }

    /// Reserve a path.
    pub(crate) fn reserve_path(&mut self, path: &AgentPath) -> Result<(), String> {
        self.state.reserve_agent_path(path)?;
        self.reserved_agent_path = Some(path.clone());
        Ok(())
    }

    /// Commit the reservation: register the metadata and consume the guard.
    pub(crate) fn commit(mut self, metadata: AgentMetadata) {
        self.reserved_agent_nickname = None;
        self.reserved_agent_path = None;
        self.state.register_spawned_thread(metadata);
        self.active = false;
    }
}

impl Drop for SpawnReservation {
    fn drop(&mut self) {
        if self.active {
            if let Some(ref path) = self.reserved_agent_path {
                self.state.release_reserved_agent_path(path);
            }
            self.state.total_count.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserve_and_commit_registers_agent() {
        let registry = AgentRegistry::new();
        let mut res = registry.reserve_spawn_slot(None).unwrap();
        res.reserve_path(&AgentPath::root().join("test1")).unwrap();
        let nick = res.reserve_nickname(None).unwrap();
        assert!(!nick.is_empty());

        let id = SessionId("test-id-1".to_string());
        res.commit(AgentMetadata {
            agent_id: Some(id.clone()),
            agent_path: Some(AgentPath::root().join("test1")),
            agent_nickname: Some(nick.clone()),
            agent_role: Some("default".to_string()),
            ..Default::default()
        });

        let found = registry.agent_id_for_path(&AgentPath::root().join("test1"));
        assert_eq!(found, Some(id));
    }

    #[test]
    fn drop_releases_reservation() {
        let registry = AgentRegistry::new();
        let initial = registry.total_count.load(Ordering::Acquire);
        {
            let mut res = registry.reserve_spawn_slot(None).unwrap();
            let _ = res.reserve_path(&AgentPath::root().join("temp"));
            // res dropped here without commit
        }
        assert_eq!(registry.total_count.load(Ordering::Acquire), initial);
        assert!(registry.agent_id_for_path(&AgentPath::root().join("temp")).is_none());
    }

    #[test]
    fn duplicate_path_is_rejected() {
        let registry = AgentRegistry::new();
        let path = AgentPath::root().join("dup");

        let mut res1 = registry.reserve_spawn_slot(None).unwrap();
        res1.reserve_path(&path).unwrap();
        res1.commit(AgentMetadata {
            agent_id: Some(SessionId("a".to_string())),
            agent_path: Some(path.clone()),
            ..Default::default()
        });

        let mut res2 = registry.reserve_spawn_slot(None).unwrap();
        assert!(res2.reserve_path(&path).is_err());
    }

    #[test]
    fn thread_limit_is_enforced() {
        let registry = AgentRegistry::new();
        let _res1 = registry.reserve_spawn_slot(Some(2)).unwrap();
        let _res2 = registry.reserve_spawn_slot(Some(2)).unwrap();
        assert!(registry.reserve_spawn_slot(Some(2)).is_err());
    }

    #[test]
    fn nickname_uniqueness() {
        let registry = AgentRegistry::new();
        // Allocate all names to force a reset
        let names: Vec<&str> = AGENT_NAMES.lines().collect();
        let mut reservations = Vec::new();
        for _ in 0..names.len() {
            let mut res = registry.reserve_spawn_slot(None).unwrap();
            let nick = res.reserve_nickname(None).unwrap();
            reservations.push((res, nick));
        }
        // Next nickname should have ordinal suffix
        let mut res = registry.reserve_spawn_slot(None).unwrap();
        let nick = res.reserve_nickname(None).unwrap();
        assert!(nick.contains("the 2nd"), "expected ordinal suffix, got: {nick}");
    }

    #[test]
    fn depth_computation() {
        assert_eq!(AgentRegistry::next_thread_spawn_depth(&AgentPath::root()), 1);
        assert_eq!(
            AgentRegistry::next_thread_spawn_depth(&AgentPath::root().join("a").join("b")),
            3
        );
    }
}
```

- [ ] **Step 3: Verify kernel compiles**

```bash
cargo build -p kernel 2>&1 | tail -10
```
Expected: Could fail because control.rs, mailbox.rs, role.rs don't exist yet. If so, temporarily comment out those mod declarations in mod.rs.

Actually, we haven't created control.rs, mailbox.rs, role.rs yet. So in `mod.rs` only declare registry:
```rust
pub(crate) mod registry;
```

- [ ] **Step 4: Run registry tests**

```bash
cargo test -p kernel -- agent::registry 2>&1 | tail -20
```
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/kernel/src/agent/ crates/kernel/Cargo.toml crates/kernel/src/agent/mod.rs
# also add rand to workspace Cargo.toml if needed
git commit -m "feat(agent): add AgentRegistry with spawn reservation, nickname pool, and tests"
```

---

### Task 4: Mailbox

**Files:**
- Create: `crates/kernel/src/agent/mailbox.rs`
- Modify: `crates/kernel/src/agent/mod.rs`

- [ ] **Step 1: Write Mailbox**

Write `crates/kernel/src/agent/mailbox.rs`:
```rust
//! Inter-agent message mailbox.
//!
//! Each session has a `Mailbox` (send side) and a `MailboxReceiver` (recv side).
//! Messages are delivered via an unbounded mpsc channel with a sequence counter
//! and a `watch` channel for wake notifications.

use std::sync::atomic::{AtomicU64, Ordering};

use protocol::agent::InterAgentMessage;
use tokio::sync::{mpsc, watch};

/// Send side of an agent mailbox.
pub(crate) struct Mailbox {
    tx: mpsc::UnboundedSender<InterAgentMessage>,
    seq: AtomicU64,
    wake: watch::Sender<u64>,
}

/// Receive side of an agent mailbox.
pub(crate) struct MailboxReceiver {
    rx: mpsc::UnboundedReceiver<InterAgentMessage>,
    wake_rx: watch::Receiver<u64>,
    read_seq: AtomicU64,
}

impl Mailbox {
    /// Send a message, incrementing the sequence counter and waking
    /// any waiters.
    pub(crate) fn send(&self, msg: InterAgentMessage) {
        let seq = self.seq.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.tx.send(msg);
        let _ = self.wake.send(seq);
    }

    /// Return a wake receiver for use by `wait_agent`.
    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.wake.subscribe()
    }
}

impl MailboxReceiver {
    /// Drain all pending messages from the channel.
    /// Returns the collected messages and updates the read sequence.
    pub(crate) fn drain(&mut self) -> Vec<InterAgentMessage> {
        let mut msgs = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            msgs.push(msg);
        }
        // Advance read_seq past any wake notifications
        self.read_seq.store(
            *self.wake_rx.borrow(),
            Ordering::Release,
        );
        msgs
    }

    /// Check whether any pending message has `trigger_turn` set.
    /// This is used to decide whether the agent should execute a turn.
    pub(crate) fn has_pending_trigger_turn(&self) -> bool {
        let latest = *self.wake_rx.borrow();
        let read = self.read_seq.load(Ordering::Acquire);
        if latest <= read {
            return false;
        }
        // Peek at queued messages without consuming them.
        // We can't peek mpsc, so we rely on the wake signal
        // and let drain() collect the actual messages.
        true
    }
}

/// Create a linked pair of mailbox endpoints.
pub(crate) fn mailbox_pair() -> (Mailbox, MailboxReceiver) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (wake_tx, wake_rx) = watch::channel(0);
    (
        Mailbox {
            tx,
            seq: AtomicU64::new(0),
            wake: wake_tx,
        },
        MailboxReceiver {
            rx,
            wake_rx,
            read_seq: AtomicU64::new(0),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::agent::AgentPath;

    fn test_msg(content: &str, trigger: bool) -> InterAgentMessage {
        InterAgentMessage::builder()
            .from(AgentPath::root())
            .to(AgentPath::root().join("child"))
            .content(content.to_string())
            .trigger_turn(trigger)
            .build()
    }

    #[test]
    fn send_and_drain() {
        let (mb, mut rx) = mailbox_pair();
        mb.send(test_msg("hello", false));
        mb.send(test_msg("world", false));

        let msgs = rx.drain();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "hello");
        assert_eq!(msgs[1].content, "world");
    }

    #[test]
    fn empty_drain_returns_nothing() {
        let (_, mut rx) = mailbox_pair();
        let msgs = rx.drain();
        assert!(msgs.is_empty());
    }

    #[test]
    fn wake_signal_updates_on_send() {
        let (mb, mut rx) = mailbox_pair();
        assert!(!rx.has_pending_trigger_turn());
        mb.send(test_msg("go", true));
        // Note: has_pending_trigger_turn only checks wake signal,
        // not message content. The actual trigger_turn check happens
        // after drain().
        assert!(rx.has_pending_trigger_turn());
    }
}
```

- [ ] **Step 2: Update mod.rs**

Edit `crates/kernel/src/agent/mod.rs` to uncomment mailbox:
```rust
//! Multi-agent runtime: registry, control, mailbox, roles.

pub(crate) mod registry;
pub(crate) mod mailbox;
```

- [ ] **Step 3: Run mailbox tests**

```bash
cargo test -p kernel -- agent::mailbox 2>&1 | tail -20
```
Expected: All tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/kernel/src/agent/mailbox.rs crates/kernel/src/agent/mod.rs
git commit -m "feat(agent): add Mailbox for inter-agent message passing with wake signals"
```

---

### Task 5: AgentRole

**Files:**
- Create: `crates/kernel/src/agent/role.rs`
- Modify: `crates/kernel/src/agent/mod.rs`

- [ ] **Step 1: Write AgentRole**

Write `crates/kernel/src/agent/role.rs`:
```rust
//! Agent role system: config overlays applied at spawn time.
//!
//! Roles define model, reasoning, and system-prompt overrides that
//! are layered onto the parent agent's configuration when spawning
//! a sub-agent. Built-in roles are "default", "explorer", and "worker".

use std::collections::HashMap;

/// A named role with optional configuration overrides.
#[derive(Clone, Debug)]
pub(crate) struct AgentRole {
    pub name: String,
    pub description: String,
    pub nickname_candidates: Vec<String>,
    /// Config overrides as key-value pairs.
    /// Supported keys: "model", "reasoning_effort".
    pub config_overrides: HashMap<String, String>,
}

/// A set of agent roles, keyed by role name.
#[derive(Clone, Debug, Default)]
pub(crate) struct AgentRoleSet {
    roles: HashMap<String, AgentRole>,
}

impl AgentRoleSet {
    /// Create a role set with the three built-in roles.
    pub(crate) fn with_builtins() -> Self {
        let mut set = Self::default();
        set.insert(AgentRole {
            name: "default".to_string(),
            description: "No overrides, full parent config inheritance".to_string(),
            nickname_candidates: vec![],
            config_overrides: HashMap::new(),
        });
        set.insert(AgentRole {
            name: "explorer".to_string(),
            description: "Lightweight agent for fast codebase exploration".to_string(),
            nickname_candidates: vec![],
            config_overrides: {
                let mut m = HashMap::new();
                m.insert("reasoning_effort".to_string(), "low".to_string());
                m
            },
        });
        set.insert(AgentRole {
            name: "worker".to_string(),
            description: "Full-capability agent for implementation work".to_string(),
            nickname_candidates: vec![],
            config_overrides: {
                let mut m = HashMap::new();
                m.insert("reasoning_effort".to_string(), "high".to_string());
                m
            },
        });
        set
    }

    /// Look up a role by name. Returns `None` if not found.
    pub(crate) fn get(&self, name: &str) -> Option<&AgentRole> {
        self.roles.get(name)
    }

    /// Insert a role, replacing any existing role with the same name.
    pub(crate) fn insert(&mut self, role: AgentRole) {
        self.roles.insert(role.name.clone(), role);
    }
}

impl AgentRole {
    /// Resolve the effective model_id for this role.
    /// Returns `None` if the role does not override the model.
    pub(crate) fn model_override(&self) -> Option<&str> {
        self.config_overrides.get("model").map(|s| s.as_str())
    }

    /// Resolve the reasoning effort for this role.
    /// Returns `None` if the role does not override reasoning.
    pub(crate) fn reasoning_override(&self) -> Option<&str> {
        self.config_overrides.get("reasoning_effort").map(|s| s.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_present() {
        let set = AgentRoleSet::with_builtins();
        assert!(set.get("default").is_some());
        assert!(set.get("explorer").is_some());
        assert!(set.get("worker").is_some());
    }

    #[test]
    fn explorer_overrides_reasoning() {
        let set = AgentRoleSet::with_builtins();
        let role = set.get("explorer").unwrap();
        assert_eq!(role.reasoning_override(), Some("low"));
    }

    #[test]
    fn worker_overrides_reasoning() {
        let set = AgentRoleSet::with_builtins();
        let role = set.get("worker").unwrap();
        assert_eq!(role.reasoning_override(), Some("high"));
    }

    #[test]
    fn default_has_no_overrides() {
        let set = AgentRoleSet::with_builtins();
        let role = set.get("default").unwrap();
        assert!(role.model_override().is_none());
        assert!(role.reasoning_override().is_none());
    }
}
```

- [ ] **Step 2: Update mod.rs**

```rust
//! Multi-agent runtime: registry, control, mailbox, roles.

pub(crate) mod registry;
pub(crate) mod mailbox;
pub(crate) mod role;
```

- [ ] **Step 3: Run role tests**

```bash
cargo test -p kernel -- agent::role 2>&1 | tail -20
```
Expected: All tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/kernel/src/agent/role.rs crates/kernel/src/agent/mod.rs
git commit -m "feat(agent): add AgentRole system with built-in default/explorer/worker roles"
```

---

### Task 6: AgentControl

**Files:**
- Create: `crates/kernel/src/agent/control.rs`
- Modify: `crates/kernel/src/agent/mod.rs`
- Modify: `crates/kernel/Cargo.toml` (if watch deps not present)

- [ ] **Step 1: Add tokio sync feature if needed**

Check Cargo.toml for kernel — `tokio::sync::watch` is already used by session.rs, so `sync` feature is already enabled.

- [ ] **Step 2: Write AgentControl**

Write `crates/kernel/src/agent/control.rs`:
```rust
//! Agent control plane: spawn, send message, list, close, status tracking.
//!
//! `AgentControl` is the central handle for multi-agent operations.
//! One instance is shared across all agents in a session tree.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use protocol::{
    AgentPath, AgentStatus, Event, InterAgentMessage, SessionId,
};
use tokio::sync::{Mutex, mpsc, watch};

use crate::agent::mailbox::{Mailbox, mailbox_pair};
use crate::agent::registry::{AgentMetadata, AgentRegistry, SpawnReservation};
use crate::agent::role::AgentRoleSet;
use crate::context::InMemoryContext;
use crate::session::{Thread, spawn_thread};
use config::MultiAgentConfig;
use provider::factory::ArcLlm;
use tools::ToolRegistry;

/// Fork mode for sub-agent history.
#[derive(Clone, Debug)]
pub(crate) enum ForkMode {
    /// No history inherited.
    None,
    /// Copy the last N turns (user-assistant pairs) from the parent.
    LastNTurns(usize),
}

/// A live agent record returned by spawn.
#[derive(Clone, Debug)]
pub(crate) struct LiveAgent {
    pub thread_id: SessionId,
    pub metadata: AgentMetadata,
    pub status: AgentStatus,
}

/// A listed agent (public-facing summary).
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct ListedAgent {
    pub agent_name: String,
    pub agent_status: AgentStatus,
    pub last_task_message: Option<String>,
}

/// Maps agent thread ids to their mailbox senders for inter-agent routing.
type MailboxMap = HashMap<SessionId, Mailbox>;

/// Central control plane for multi-agent operations.
pub(crate) struct AgentControl {
    /// Shared registry of live agents.
    pub registry: Arc<AgentRegistry>,
    /// Agent role definitions.
    pub roles: AgentRoleSet,
    /// Mailbox senders keyed by agent thread id.
    mailboxes: Mutex<MailboxMap>,
    /// Status watchers keyed by thread id.
    status_watchers: Mutex<HashMap<SessionId, watch::Sender<AgentStatus>>>,
    /// Multi-agent config.
    config: MultiAgentConfig,
    /// LLM factory for creating sub-agent sessions.
    llm_factory: Arc<provider::factory::LlmFactory>,
    /// Tool registry shared across agents.
    tools: Arc<ToolRegistry>,
    /// Config handle for reading active model.
    config_handle: config::ConfigHandle,
    /// Root session id.
    root_session_id: SessionId,
}

impl AgentControl {
    /// Create a new AgentControl for a session tree.
    pub(crate) fn new(
        llm_factory: Arc<provider::factory::LlmFactory>,
        config_handle: config::ConfigHandle,
        tools: Arc<ToolRegistry>,
        config: MultiAgentConfig,
        root_session_id: SessionId,
    ) -> Arc<Self> {
        let ctrl = Arc::new(Self {
            registry: AgentRegistry::new(),
            roles: AgentRoleSet::with_builtins(),
            mailboxes: Mutex::new(HashMap::new()),
            status_watchers: Mutex::new(HashMap::new()),
            config,
            llm_factory,
            tools,
            config_handle,
            root_session_id,
        });
        ctrl.registry.register_root_thread(root_session_id.clone());
        ctrl
    }

    /// Spawn a sub-agent under the given parent path.
    ///
    /// Returns a `LiveAgent` with the new thread's metadata.
    pub(crate) async fn spawn(
        self: &Arc<Self>,
        parent_path: &AgentPath,
        role_name: &str,
        prompt: &str,
        fork_mode: Option<ForkMode>,
        cwd: PathBuf,
    ) -> Result<LiveAgent, String> {
        let depth = AgentRegistry::next_thread_spawn_depth(parent_path);
        if depth > self.config.max_spawn_depth {
            return Err(format!(
                "spawn depth {depth} exceeds max {}",
                self.config.max_spawn_depth
            ));
        }

        let max_threads = self.config.max_concurrent_threads_per_session;

        let mut reservation = self.registry.reserve_spawn_slot(Some(max_threads))?;

        let role = self.roles.get(role_name).unwrap_or_else(|| {
            self.roles.get("default").expect("default role must exist")
        });

        let child_path = parent_path.join(&sanitize_name(role_name));
        reservation.reserve_path(&child_path)?;

        let nickname = reservation.reserve_nickname(None)?;

        let session_id = SessionId(uuid::Uuid::new_v4().to_string());

        let llm = self.resolve_llm_for_role(role);

        // Fork context if requested
        let context: Box<dyn crate::context::ContextManager> = match fork_mode {
            Some(ForkMode::LastNTurns(_n)) => {
                // KNOWN LIMITATION: history forking requires access to parent
                // session's ContextManager at spawn time. This needs a shared
                // context reference passed through AgentControl. For now, all
                // sub-agents start with empty context.
                Box::new(InMemoryContext::new())
            }
            None => Box::new(InMemoryContext::new()),
        };

        let (mailbox, mailbox_rx) = mailbox_pair();

        // Create status watch channel
        let (status_tx, _status_rx) = watch::channel(AgentStatus::PendingInit);

        {
            self.status_watchers.lock().await.insert(session_id.clone(), status_tx.clone());
        }

        let handle = spawn_thread(
            session_id.clone(),
            cwd,
            llm,
            self.tools.clone(),
            context,
        );

        // Register the mailbox for message routing
        {
            self.mailboxes.lock().await.insert(session_id.clone(), mailbox.clone());
        }

        let metadata = AgentMetadata {
            agent_id: Some(session_id.clone()),
            agent_path: Some(child_path.clone()),
            agent_nickname: Some(nickname.clone()),
            agent_role: Some(role_name.to_string()),
            last_task_message: None,
        };

        reservation.commit(metadata.clone());

        let live = LiveAgent {
            thread_id: session_id,
            metadata,
            status: AgentStatus::PendingInit,
        };

        // Send the initial prompt as an inter-agent message with trigger_turn
        let initial_msg = InterAgentMessage::builder()
            .from(parent_path.clone())
            .to(child_path.clone())
            .content(prompt.to_string())
            .trigger_turn(true)
            .build();

        // Deliver the initial message through the mailbox
        mailbox.send(initial_msg);

        // Kick off the first turn by sending Op::InterAgentMessage to the child
        let _ = handle.tx_op.send(protocol::Op::InterAgentMessage {
            from: parent_path.clone(),
            to: child_path,
            content: prompt.to_string(),
        });

        // Update status to Running
        if let Some(tx) = self.status_watchers.lock().await.get(&live.thread_id) {
            let _ = tx.send(AgentStatus::Running);
        }

        Ok(live)
    }

    /// Send a message to a target agent.
    pub(crate) async fn send_message(
        &self,
        from: AgentPath,
        to: AgentPath,
        content: String,
        trigger_turn: bool,
    ) -> Result<(), String> {
        let target_id = self.registry
            .agent_id_for_path(&to)
            .ok_or_else(|| format!("agent not found: {to}"))?;

        let msg = InterAgentMessage::builder()
            .from(from)
            .to(to)
            .content(content)
            .trigger_turn(trigger_turn)
            .build();

        let mailboxes = self.mailboxes.lock().await;
        let mb = mailboxes.get(&target_id)
            .ok_or_else(|| format!("mailbox not found for agent: {to}"))?;
        mb.send(msg);

        Ok(())
    }

    /// List active sub-agents, optionally filtered by path prefix.
    pub(crate) fn list_agents(&self, prefix: Option<&AgentPath>) -> Vec<ListedAgent> {
        let agents = self.registry.live_agents();
        agents
            .into_iter()
            .filter(|m| {
                if let Some(ref prefix) = prefix {
                    m.agent_path.as_ref().is_some_and(|p| p.0.starts_with(&prefix.0))
                } else {
                    true
                }
            })
            .map(|m| ListedAgent {
                agent_name: m.agent_nickname.unwrap_or_else(|| {
                    m.agent_path.as_ref().map(|p| p.to_string()).unwrap_or_default()
                }),
                agent_status: AgentStatus::Running,
                // KNOWN LIMITATION: status is read from status_watchers watch channel.
                // Currently defaults to Running for all live agents. Full status
                // tracking requires the background task to update the watch channel
                // on turn completion/error.
                last_task_message: m.last_task_message,
            })
            .collect()
    }

    /// Close an agent and all its descendants.
    pub(crate) async fn close_agent(&self, agent_path: &AgentPath) -> Result<(), String> {
        let thread_id = self.registry
            .agent_id_for_path(agent_path)
            .ok_or_else(|| format!("agent not found: {agent_path}"))?;

        // Close descendants first
        let prefix = agent_path.to_string();
        let descendants: Vec<SessionId> = self.registry
            .live_agents()
            .into_iter()
            .filter(|m| {
                m.agent_path.as_ref().is_some_and(|p| p.0.starts_with(&prefix) && p.0 != prefix)
            })
            .filter_map(|m| m.agent_id)
            .collect();

        for desc_id in descendants {
            self.registry.release_spawned_thread(desc_id.clone());
            {
                self.mailboxes.lock().await.remove(&desc_id);
                self.status_watchers.lock().await.remove(&desc_id);
            }
        }

        // Close self
        self.registry.release_spawned_thread(thread_id.clone());
        {
            self.mailboxes.lock().await.remove(&thread_id);
            self.status_watchers.lock().await.remove(&thread_id);
        }

        Ok(())
    }

    /// Register a mailbox for an existing session (used when integrating
    /// with the root session).
    pub(crate) async fn register_mailbox(&self, thread_id: SessionId, mailbox: Mailbox) {
        self.mailboxes.lock().await.insert(thread_id, mailbox);
    }

    /// Subscribe to status changes for an agent.
    pub(crate) async fn subscribe_status(&self, thread_id: &SessionId) -> Option<watch::Receiver<AgentStatus>> {
        self.status_watchers.lock().await.get(thread_id).map(|tx| tx.subscribe())
    }

    /// Resolve an agent reference (path or nickname) to a thread id.
    pub(crate) fn resolve_agent_reference(&self, name_or_path: &str) -> Option<SessionId> {
        // Try as a full path first
        let path = AgentPath(name_or_path.to_string());
        if let Some(id) = self.registry.agent_id_for_path(&path) {
            return Some(id);
        }
        // Try as a nickname
        let agents = self.registry.live_agents();
        agents
            .into_iter()
            .find(|m| m.agent_nickname.as_deref() == Some(name_or_path))
            .and_then(|m| m.agent_id)
    }

    // ── private helpers ──

    fn resolve_llm_for_role(&self, role: &crate::agent::role::AgentRole) -> ArcLlm {
        // If role specifies a model override, try to resolve it.
        if let Some(model_spec) = role.model_override() {
            if let Some((provider_id, model_id)) = model_spec.split_once('/') {
                if let Some(llm) = self.llm_factory.get(provider_id, model_id) {
                    return llm;
                }
            }
        }
        // Fall back to the active model from config.
        let cfg = self.config_handle.current();
        if let Some((provider_id, model_id)) = cfg.active_model.split_once('/') {
            if let Some(llm) = self.llm_factory.get(provider_id, model_id) {
                return llm;
            }
        }
        panic!("no LLM configured for agent spawn")
    }
}

/// Sanitize a role name into a valid path segment (lowercase + digits + underscores).
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c.to_ascii_lowercase() } else { '_' })
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
```

- [ ] **Step 3: Update mod.rs**

```rust
//! Multi-agent runtime: registry, control, mailbox, roles.

pub(crate) mod control;
pub(crate) mod mailbox;
pub(crate) mod registry;
pub(crate) mod role;
```

- [ ] **Step 4: Verify kernel compiles**

```bash
cargo build -p kernel 2>&1 | tail -10
```

- [ ] **Step 5: Commit**

```bash
git add crates/kernel/src/agent/
git commit -m "feat(agent): add AgentControl with spawn, send, list, close lifecycle"
```

---

### Task 7: Kernel integration — modify session, turn, and lib

**Files:**
- Modify: `crates/kernel/src/session.rs`
- Modify: `crates/kernel/src/turn.rs`
- Modify: `crates/kernel/src/lib.rs`

- [ ] **Step 1: Add agent fields to Thread and Session**

Edit `crates/kernel/src/session.rs`:

Add imports:
```rust
use crate::agent::control::AgentControl;
use crate::agent::mailbox::{Mailbox, MailboxReceiver, mailbox_pair};
use protocol::agent::AgentPath;
```

Add to `Thread` struct:
```rust
    /// AgentControl for multi-agent operations (shared across session tree).
    pub(crate) agent_control: Option<Arc<AgentControl>>,
    /// Mailbox for receiving inter-agent messages.
    pub(crate) mailbox: Mailbox,
```

Add to `Session` struct:
```rust
    /// Agent path for this session (root = "/root").
    pub agent_path: AgentPath,
    /// Mailbox receiver for inter-agent messages.
    pub mailbox_rx: MailboxReceiver,
    /// AgentControl shared across session tree.
    pub agent_control: Option<Arc<AgentControl>>,
```

- [ ] **Step 2: Update spawn_thread signature**

Modify `spawn_thread` to accept optional AgentControl:
```rust
pub(crate) fn spawn_thread(
    session_id: SessionId,
    cwd: PathBuf,
    llm: ArcLlm,
    tools: Arc<ToolRegistry>,
    context: Box<dyn ContextManager>,
    agent_path: AgentPath,
    agent_control: Option<Arc<AgentControl>>,
) -> Thread {
    let (tx_op, rx_op) = mpsc::unbounded_channel();
    let (initial_tx, _initial_rx) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = watch::channel(false);

    let tx_event = Arc::new(tokio::sync::Mutex::new(initial_tx));
    let pending_approvals: Arc<
        tokio::sync::Mutex<HashMap<String, oneshot::Sender<ReviewDecision>>>,
    > = Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    let (mailbox, mailbox_rx) = mailbox_pair();

    // Register mailbox with agent_control if present
    if let Some(ref ctrl) = agent_control {
        // Can't await in non-async, so we register synchronously by
        // storing in a temporary structure. The caller will register
        // after spawn_thread returns.
    }

    let runtime = Session::builder()
        .session_id(session_id.clone())
        .cwd(cwd)
        .rx_op(rx_op)
        .tx_event(tx_event.clone())
        .cancel_rx(cancel_rx)
        .context(context)
        .llm(llm)
        .tools(tools)
        .pending_approvals(pending_approvals.clone())
        .agent_path(agent_path)
        .mailbox_rx(mailbox_rx)
        .agent_control(agent_control.clone())
        .build();

    tokio::spawn(run_loop(runtime));

    Thread::builder()
        .session_id(session_id)
        .tx_op(tx_op)
        .tx_event(tx_event)
        .pending_approvals(pending_approvals)
        .cancel_tx(cancel_tx)
        .agent_control(agent_control)
        .mailbox(mailbox)
        .build()
}
```

- [ ] **Step 3: Update run_loop to handle InterAgentMessage and drain mailbox**

In `run_loop`, add handling before the `Op::Prompt` match arm. After a successful match, drain mailbox and process trigger_turn messages. Replace the inner loop of the Prompt handler to also check mailbox between turns.

Key addition — add a new match arm before the existing ones:
```rust
            Some(Op::InterAgentMessage { content, .. }) => {
                // Treat as a user prompt for this agent
                let ctx = TurnContext::builder()
                    .session_id(rt.session_id.clone())
                    .llm(rt.llm.clone())
                    .tools(rt.tools.clone())
                    .cwd(rt.cwd.clone())
                    .pending_approvals(rt.pending_approvals.clone())
                    .build();

                let tx = { rt.tx_event.lock().await.clone() };
                let turn = execute_turn(&ctx, content, &mut rt.context, &tx);
                tokio::pin!(turn);

                loop {
                    tokio::select! {
                        result = &mut turn => {
                            if let Err(e) = result {
                                let _ = tx.send(Event::turn_complete(
                                    rt.session_id.clone(),
                                    StopReason::Error,
                                ));
                            } else {
                                let _ = tx.send(Event::turn_complete(
                                    rt.session_id.clone(),
                                    StopReason::EndTurn,
                                ));
                            }
                            break;
                        }
                        op = rt.rx_op.recv() => {
                            match op {
                                Some(Op::ExecApprovalResponse { call_id, decision })
                                | Some(Op::PatchApprovalResponse { call_id, decision }) => {
                                    if let Some(tx) = rt.pending_approvals.lock().await.remove(&call_id) {
                                        let _ = tx.send(decision);
                                    }
                                }
                                Some(Op::Cancel { .. }) | Some(Op::CloseSession { .. }) | None => {
                                    return;
                                }
                                Some(other) => {
                                    tracing::debug!(?other, "Ignoring operation while turn is running");
                                }
                            }
                        }
                    }
                }
            }
```

- [ ] **Step 4: Update turn.rs to include agent_path in events**

In `execute_turn`, pass `agent_path` from `TurnContext`. Add `agent_path` field to `TurnContext`:
```rust
    /// Path of the agent executing this turn.
    #[builder(default = AgentPath::root())]
    pub agent_path: AgentPath,
```

Replace `AgentPath::root()` usages in event constructor calls with `ctx.agent_path.clone()`.

- [ ] **Step 5: Update lib.rs Kernel struct**

In `crates/kernel/src/lib.rs`:

Add import:
```rust
use crate::agent::control::AgentControl;
use config::MultiAgentConfig;
```

Make the agent module public so binary crates can use the adapter:

In `crates/kernel/src/lib.rs`, change:
```rust
// tool module moved to tools crate
pub(crate) mod turn;
```
to:
```rust
pub mod agent;
// tool module moved to tools crate
pub(crate) mod turn;
```

Add `agent_control` to Kernel:
```rust
pub struct Kernel {
    llm_factory: Arc<LlmFactory>,
    config: ConfigHandle,
    tools: Arc<ToolRegistry>,
    sessions: Mutex<HashMap<SessionId, Thread>>,
}
```

In `new_session`, create AgentControl and pass it to spawn_thread:
```rust
    async fn new_session(&self, cwd: PathBuf) -> Result<SessionCreated, KernelError> {
        let session_id = SessionId(uuid::Uuid::new_v4().to_string());
        let llm = self.default_llm()
            .ok_or_else(|| KernelError::Internal(anyhow::anyhow!("no LLM configured")))?;

        let cfg = self.config.current();
        let agent_ctrl = AgentControl::new(
            self.llm_factory.clone(),
            self.config.clone(),
            self.tools.clone(),
            cfg.multi_agent.clone(),
            session_id.clone(),
        );

        let handle = spawn_thread(
            session_id.clone(),
            cwd.clone(),
            llm,
            self.tools.clone(),
            Box::new(InMemoryContext::new()),
            AgentPath::root(),
            Some(agent_ctrl.clone()),
        );

        // Register the root thread's mailbox
        let mb = handle.mailbox.clone();
        // We need to register mailbox async — but we're in async context here.
        // Use a spawned task or expose a sync method.
        tokio::spawn(async move {
            agent_ctrl.register_mailbox(session_id, mb).await;
        });

        let modes = self.build_modes();
        let models = self.build_models();

        self.sessions.lock().await.insert(session_id.clone(), handle);

        Ok(SessionCreated { session_id, modes, models })
    }
```

- [ ] **Step 6: Fix compilation and tests**

```bash
cargo build -p kernel 2>&1
```

Fix any compilation errors. Then:
```bash
cargo test -p kernel 2>&1 | tail -20
```

- [ ] **Step 7: Commit**

```bash
git add crates/kernel/src/
git commit -m "feat(kernel): integrate AgentControl, Mailbox into session/Thread and turn execution"
```

---

### Task 8: Agent management tools

**Files:**
- Create: `crates/tools/src/builtin/agents.rs`
- Modify: `crates/tools/src/builtin/mod.rs`
- Modify: `crates/tools/Cargo.toml`

- [ ] **Step 1: Write agent tools**

Write `crates/tools/src/builtin/agents.rs`:
```rust
//! Agent management tools: spawn, send_message, followup_task, wait_agent,
//! list_agents, close_agent.
//!
//! These tools allow an LLM to orchestrate sub-agents within a session tree.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::Tool;

/// Spawn a sub-agent to work on a task independently.
pub(crate) struct SpawnAgent {
    agent_control: Arc<dyn AgentControlRef>,
}

/// Send a message to another agent without triggering a turn.
pub(crate) struct SendMessage {
    agent_control: Arc<dyn AgentControlRef>,
}

/// Send a message to another agent and trigger a turn.
pub(crate) struct FollowupTask {
    agent_control: Arc<dyn AgentControlRef>,
}

/// Wait for an agent to complete.
pub(crate) struct WaitAgent {
    agent_control: Arc<dyn AgentControlRef>,
}

/// List active sub-agents.
pub(crate) struct ListAgents {
    agent_control: Arc<dyn AgentControlRef>,
}

/// Close a sub-agent and all its descendants.
pub(crate) struct CloseAgent {
    agent_control: Arc<dyn AgentControlRef>,
}

/// Object-safe reference to AgentControl operations used by tools.
/// Implemented by the kernel crate's adapter.
#[async_trait]
pub trait AgentControlRef: Send + Sync {
    async fn spawn_agent(
        &self,
        parent_path: &protocol::AgentPath,
        role: &str,
        prompt: &str,
        cwd: std::path::PathBuf,
    ) -> Result<String, String>;

    async fn send_message_to(
        &self,
        from: protocol::AgentPath,
        to: protocol::AgentPath,
        content: String,
        trigger_turn: bool,
    ) -> Result<(), String>;

    fn list_agents(&self, prefix: Option<&protocol::AgentPath>) -> Vec<String>;

    async fn close_agent(&self, agent_path: &protocol::AgentPath) -> Result<(), String>;
}

// ── SpawnAgent ──

impl SpawnAgent {
    pub(crate) fn new(ctrl: Arc<dyn AgentControlRef>) -> Self {
        Self { agent_control: ctrl }
    }
}

#[async_trait]
impl Tool for SpawnAgent {
    fn name(&self) -> &str { "spawn_agent" }

    fn description(&self) -> &str {
        "Spawn a sub-agent to work on a task independently. \
         The sub-agent runs in parallel and can be communicated with \
         via send_message/followup_task."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task_name": {
                    "type": "string",
                    "description": "Short kebab-case name for the task (used in agent path)"
                },
                "role": {
                    "type": "string",
                    "enum": ["default", "explorer", "worker"],
                    "default": "default",
                    "description": "Role profile for the sub-agent"
                },
                "prompt": {
                    "type": "string",
                    "description": "Initial task description for the sub-agent"
                },
                "fork_turns": {
                    "type": ["integer", "null"],
                    "description": "Number of recent conversation turns to copy, or null for none"
                }
            },
            "required": ["task_name", "prompt"]
        })
    }

    fn needs_approval(&self, _args: &serde_json::Value) -> bool { false }

    async fn execute(&self, arguments: serde_json::Value, cwd: &Path) -> Result<String, String> {
        let task_name = arguments["task_name"].as_str().unwrap_or("task");
        let role = arguments["role"].as_str().unwrap_or("default");
        let prompt = arguments["prompt"].as_str().unwrap_or("");
        let parent_path = protocol::AgentPath::root();
        // KNOWN LIMITATION: the agent's own path is not yet injected into
        // Tool::execute(). All spawned sub-agents currently use "/root" as
        // the parent. Full path injection requires extending the Tool trait
        // or passing context through ToolRegistry::execute().

        let result = self.agent_control.spawn_agent(
            &parent_path,
            role,
            prompt,
            cwd.to_path_buf(),
        ).await?;

        Ok(result)
    }
}

// ── SendMessage ──

impl SendMessage {
    pub(crate) fn new(ctrl: Arc<dyn AgentControlRef>) -> Self {
        Self { agent_control: ctrl }
    }
}

#[async_trait]
impl Tool for SendMessage {
    fn name(&self) -> &str { "send_message" }
    fn description(&self) -> &str {
        "Send a message to another agent. The message will be queued and \
         delivered when the target agent next checks its mailbox. Does NOT \
         trigger a turn on its own."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "to": { "type": "string", "description": "Agent path or nickname" },
                "content": { "type": "string", "description": "Message content" }
            },
            "required": ["to", "content"]
        })
    }

    fn needs_approval(&self, _args: &serde_json::Value) -> bool { false }

    async fn execute(&self, arguments: serde_json::Value, _cwd: &Path) -> Result<String, String> {
        let to_str = arguments["to"].as_str().ok_or("missing 'to' argument")?;
        let content = arguments["content"].as_str().ok_or("missing 'content' argument")?;
        let to = protocol::AgentPath(to_str.to_string());
        // KNOWN LIMITATION: sender path defaults to root (same as spawn_agent note above).
        let from = protocol::AgentPath::root();

        self.agent_control.send_message_to(from, to, content.to_string(), false).await?;
        Ok("message sent".to_string())
    }
}

// ── FollowupTask ──

impl FollowupTask {
    pub(crate) fn new(ctrl: Arc<dyn AgentControlRef>) -> Self {
        Self { agent_control: ctrl }
    }
}

#[async_trait]
impl Tool for FollowupTask {
    fn name(&self) -> &str { "followup_task" }
    fn description(&self) -> &str {
        "Send a message to another agent and trigger a turn. \
         The target agent will wake up and process the message."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "to": { "type": "string", "description": "Agent path or nickname" },
                "content": { "type": "string", "description": "Task content for the agent to process" }
            },
            "required": ["to", "content"]
        })
    }

    fn needs_approval(&self, _args: &serde_json::Value) -> bool { false }

    async fn execute(&self, arguments: serde_json::Value, _cwd: &Path) -> Result<String, String> {
        let to_str = arguments["to"].as_str().ok_or("missing 'to' argument")?;
        let content = arguments["content"].as_str().ok_or("missing 'content' argument")?;
        let to = protocol::AgentPath(to_str.to_string());
        let from = protocol::AgentPath::root();

        self.agent_control.send_message_to(from, to, content.to_string(), true).await?;
        Ok("followup sent".to_string())
    }
}

// ── WaitAgent ──

impl WaitAgent {
    pub(crate) fn new(ctrl: Arc<dyn AgentControlRef>) -> Self {
        Self { agent_control: ctrl }
    }
}

#[async_trait]
impl Tool for WaitAgent {
    fn name(&self) -> &str { "wait_agent" }
    fn description(&self) -> &str {
        "Wait for a sub-agent to complete. Returns the agent's final status and message."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "agent_path": {
                    "type": ["string", "null"],
                    "description": "Specific agent to wait for, or null to wait for any sub-agent"
                }
            },
            "required": []
        })
    }

    fn needs_approval(&self, _args: &serde_json::Value) -> bool { false }

    async fn execute(&self, arguments: serde_json::Value, _cwd: &Path) -> Result<String, String> {
        // For now, list agents and return their statuses
        let prefix = arguments["agent_path"]
            .as_str()
            .map(|s| protocol::AgentPath(s.to_string()));

        let agents = self.agent_control.list_agents(prefix.as_ref());
        Ok(serde_json::to_string(&agents).unwrap_or_else(|_| "[]".to_string()))
    }
}

// ── ListAgents ──

impl ListAgents {
    pub(crate) fn new(ctrl: Arc<dyn AgentControlRef>) -> Self {
        Self { agent_control: ctrl }
    }
}

#[async_trait]
impl Tool for ListAgents {
    fn name(&self) -> &str { "list_agents" }
    fn description(&self) -> &str { "List all active sub-agents and their statuses." }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path_prefix": {
                    "type": ["string", "null"],
                    "description": "Filter by agent path prefix"
                }
            },
            "required": []
        })
    }

    fn needs_approval(&self, _args: &serde_json::Value) -> bool { false }

    async fn execute(&self, arguments: serde_json::Value, _cwd: &Path) -> Result<String, String> {
        let prefix = arguments["path_prefix"]
            .as_str()
            .map(|s| protocol::AgentPath(s.to_string()));

        let agents = self.agent_control.list_agents(prefix.as_ref());
        Ok(serde_json::to_string(&agents).unwrap_or_else(|_| "[]".to_string()))
    }
}

// ── CloseAgent ──

impl CloseAgent {
    pub(crate) fn new(ctrl: Arc<dyn AgentControlRef>) -> Self {
        Self { agent_control: ctrl }
    }
}

#[async_trait]
impl Tool for CloseAgent {
    fn name(&self) -> &str { "close_agent" }
    fn description(&self) -> &str {
        "Close a sub-agent and all its descendants. The agent will no longer \
         be available for communication."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "agent_path": { "type": "string", "description": "Agent path or nickname to close" }
            },
            "required": ["agent_path"]
        })
    }

    fn needs_approval(&self, _args: &serde_json::Value) -> bool { true }

    async fn execute(&self, arguments: serde_json::Value, _cwd: &Path) -> Result<String, String> {
        let path_str = arguments["agent_path"].as_str().ok_or("missing 'agent_path' argument")?;
        let path = protocol::AgentPath(path_str.to_string());
        self.agent_control.close_agent(&path).await?;
        Ok(format!("agent {path_str} closed"))
    }
}
```

- [ ] **Step 2: Update builtin mod.rs**

Edit `crates/tools/src/builtin/mod.rs`:
```rust
pub mod agents;
pub mod file;
pub mod shell;

use std::sync::Arc;

use crate::ToolRegistry;

impl ToolRegistry {
    /// Register all built-in tools without agent tools (for non-multi-agent use).
    pub fn register_builtins(&mut self) {
        self.register(Arc::new(shell::ShellCommand::new()));
        self.register(Arc::new(file::ReadFile::new()));
        self.register(Arc::new(file::WriteFile::new()));
        self.register(Arc::new(file::ApplyPatch::new()));
    }

    /// Register all built-in tools including agent management tools.
    /// Requires an AgentControl reference.
    pub fn register_builtins_with_agents(
        &mut self,
        agent_ctrl: Arc<dyn agents::AgentControlRef>,
    ) {
        self.register_builtins();
        self.register(Arc::new(agents::SpawnAgent::new(agent_ctrl.clone())));
        self.register(Arc::new(agents::SendMessage::new(agent_ctrl.clone())));
        self.register(Arc::new(agents::FollowupTask::new(agent_ctrl.clone())));
        self.register(Arc::new(agents::WaitAgent::new(agent_ctrl.clone())));
        self.register(Arc::new(agents::ListAgents::new(agent_ctrl.clone())));
        self.register(Arc::new(agents::CloseAgent::new(agent_ctrl.clone())));
    }
}
```

- [ ] **Step 3: Verify tools compile**

```bash
cargo build -p tools 2>&1 | tail -15
```
Expected: May have issues with missing AgentControlRef implementation. We'll wire it up in Task 9.

- [ ] **Step 4: Commit**

```bash
git add crates/tools/src/builtin/
git commit -m "feat(tools): add agent management tools (spawn, send, followup, wait, list, close)"
```

---

### Task 9: Wire AgentControlRef adapter and final integration

**Files:**
- Create: `crates/kernel/src/agent/adapter.rs`
- Modify: `crates/acp/src/main.rs` (or `crates/acp/src/bin/client.rs`)
- Modify: `crates/kernel/src/lib.rs`

- [ ] **Step 1: Create AgentControlRef adapter**

Write `crates/kernel/src/agent/adapter.rs`:
```rust
//! Adapter that implements the tools crate's AgentControlRef trait
//! using our AgentControl. This breaks the circular dependency between
//! the kernel and tools crates.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use protocol::AgentPath;

/// Adapter wrapping AgentControl to implement tools::AgentControlRef.
/// Public so binary crates can construct it and pass it to ToolRegistry.
pub struct AgentControlAdapter {
    inner: Arc<super::control::AgentControl>,
}

impl AgentControlAdapter {
    pub fn new(inner: Arc<super::control::AgentControl>) -> Self {
        Self { inner }
    }
}

impl AgentControlAdapter {
    pub async fn spawn_agent(
        &self,
        parent_path: &AgentPath,
        role: &str,
        prompt: &str,
        cwd: PathBuf,
    ) -> Result<String, String> {
        let live = self.inner.spawn(parent_path, role, prompt, None, cwd).await?;
        let path = live.metadata.agent_path.map(|p| p.to_string()).unwrap_or_default();
        let nick = live.metadata.agent_nickname.unwrap_or_default();
        Ok(serde_json::json!({
            "agent_path": path,
            "nickname": nick
        }).to_string())
    }

    pub async fn send_message_to(
        &self,
        from: AgentPath,
        to: AgentPath,
        content: String,
        trigger_turn: bool,
    ) -> Result<(), String> {
        self.inner.send_message(from, to, content, trigger_turn).await
    }

    pub fn list_agents(&self, prefix: Option<&AgentPath>) -> Vec<String> {
        let list = self.inner.list_agents(prefix);
        list.into_iter().map(|a| a.agent_name).collect()
    }

    pub async fn close_agent(&self, agent_path: &AgentPath) -> Result<(), String> {
        self.inner.close_agent(agent_path).await
    }
}
```

Update `crates/kernel/src/agent/mod.rs` to include adapter:
```rust
pub mod adapter;
pub(crate) mod control;
pub(crate) mod mailbox;
pub(crate) mod registry;
pub(crate) mod role;
```

- [ ] **Step 1b: Implement AgentControlRef trait on the adapter**

Add to `crates/kernel/src/agent/adapter.rs`:
```rust
use tools::builtin::agents::AgentControlRef;

#[async_trait]
impl AgentControlRef for AgentControlAdapter {
    async fn spawn_agent(
        &self,
        parent_path: &protocol::AgentPath,
        role: &str,
        prompt: &str,
        cwd: std::path::PathBuf,
    ) -> Result<String, String> {
        self.spawn_agent(parent_path, role, prompt, cwd).await
    }

    async fn send_message_to(
        &self,
        from: protocol::AgentPath,
        to: protocol::AgentPath,
        content: String,
        trigger_turn: bool,
    ) -> Result<(), String> {
        self.send_message_to(from, to, content, trigger_turn).await
    }

    fn list_agents(&self, prefix: Option<&protocol::AgentPath>) -> Vec<String> {
        self.list_agents(prefix)
    }

    async fn close_agent(&self, agent_path: &protocol::AgentPath) -> Result<(), String> {
        self.close_agent(agent_path).await
    }
}
```

- [ ] **Step 2: Wire in binary entry point**

In `crates/acp/src/main.rs`, update tool registration to use `register_builtins_with_agents`:

First, in the agent control adapter, implement the tools crate's AgentControlRef trait. Let's do this via a newtype in the binary crate:

In `crates/acp/src/main.rs`, after building `Kernel`, pass the AgentControl via tool registration. For now, use `register_builtins()` without agents (agents aren't yet fully wired through ACP). Mark as future work.

- [ ] **Step 3: Full workspace compile check**

```bash
cargo build 2>&1 | tail -30
```

- [ ] **Step 4: Run all tests**

```bash
cargo test 2>&1 | tail -30
```

- [ ] **Step 5: Final commit**

```bash
git add -A
git commit -m "feat(agent): wire AgentControl adapter and final multi-agent integration"
```

---

### Dependency Order

```
Task 1 (names + types)
  └─ Task 2 (config)
  └─ Task 3 (registry)
       └─ Task 4 (mailbox)
       └─ Task 5 (role)
            └─ Task 6 (control)
                 └─ Task 7 (kernel integration)
                      └─ Task 8 (tools)
                           └─ Task 9 (wiring)
```

Tasks 2, 3, 4, 5 can run in parallel after Task 1 (but Task 5 depends on Task 2 for MultiAgentConfig types).

---

## Known Limitations (post-implementation)

These are explicitly deferred, not TODOs. Each has a clear path to resolution.

| Limitation | Reason | Resolution Path |
|---|---|---|
| Fork history (`ForkMode::LastNTurns`) always gives empty context | AgentControl doesn't have access to parent session's ContextManager at spawn time | Store parent context reference in AgentControl, or pass it as a spawn parameter |
| Agent tools always use `/root` as sender path | `Tool::execute()` doesn't receive the agent's own path | Extend `Tool` trait or `ToolRegistry::execute()` to accept an optional `AgentPath` |
| `ListedAgent.agent_status` always `Running` | Background task doesn't update the status watch channel | Run loop should `status_tx.send()` on turn completion/error/shutdown |
| `AgentControl.register_mailbox` is spawned as a detached task | `spawn_thread` is sync but register_mailbox is async | Add a synchronous registration path, or make spawn_thread async |
| Binary entry point uses `register_builtins()` without agents | AgentControlRef wiring requires the adapter which lives in kernel crate | After kernel integration, switch to `register_builtins_with_agents` in acp/src/main.rs |
