//! In-memory registry of all live agents in a session tree.
//!
//! Tracks agent metadata, enforces uniqueness of paths and nicknames,
//! and respects configurable thread-count limits.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use protocol::{AgentPath, SessionId};
use rand::prelude::IndexedRandom;

/// Compile-time embedded pool of agent nicknames.
const AGENT_NAMES: &str = include_str!("agent_names.txt");

/// Metadata tracked for each active agent.
#[derive(Clone, Debug, Default, typed_builder::TypedBuilder)]
#[allow(dead_code)]
pub struct AgentMetadata {
    #[builder(default, setter(strip_option))]
    pub(crate) agent_id: Option<SessionId>,
    #[builder(default, setter(strip_option))]
    pub(crate) agent_path: Option<AgentPath>,
    #[builder(default, setter(strip_option))]
    pub(crate) agent_nickname: Option<String>,
    #[builder(default, setter(strip_option))]
    pub(crate) agent_role: Option<String>,
    #[builder(default, setter(strip_option))]
    pub(crate) last_task_message: Option<String>,
    /// Parent session id for subagent edge tracking.
    #[builder(default, setter(strip_option))]
    pub(crate) parent_session_id: Option<SessionId>,
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
#[derive(Default)]
pub struct AgentRegistry {
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
        Ok(SpawnReservation::builder().state(Arc::clone(self)).build())
    }

    /// Release a thread from the counter and agent tree.
    ///
    /// Only decrements `total_count` for non-root agents — the root
    /// thread does not count toward the spawn limit, so removing it
    /// should not decrement the counter.
    pub(crate) fn release_spawned_thread(&self, thread_id: SessionId) {
        let removed = {
            let mut agents = self
                .active_agents
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let key = agents
                .agent_tree
                .iter()
                .find_map(|(k, m)| (m.agent_id.as_ref() == Some(&thread_id)).then_some(k.clone()));
            key.and_then(|k| agents.agent_tree.remove(&k))
                .is_some_and(|m| !m.agent_path.as_ref().is_some_and(|p| p.is_root()))
        };
        if removed {
            self.total_count.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// Register or replace the root thread mapping.
    pub(crate) fn register_root_thread(&self, thread_id: SessionId) {
        let mut agents = self
            .active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Root sessions can be closed and later restored, so the root path must not retain stale ids.
        agents.agent_tree.insert(
            AgentPath::root().to_string(),
            AgentMetadata::builder()
                .agent_id(thread_id)
                .agent_path(AgentPath::root())
                .build(),
        );
    }

    /// Restore a persisted subagent into the registry, bypassing slot counting
    /// and depth checks. Used when resuming a session with existing subagents.
    pub(crate) fn restore_agent(
        &self,
        agent_id: SessionId,
        agent_path: AgentPath,
        nickname: Option<String>,
        role: Option<String>,
        parent_session_id: Option<SessionId>,
    ) -> Result<(), String> {
        let mut agents = self
            .active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if agents.agent_tree.contains_key(agent_path.as_str()) {
            return Err(format!("agent path already exists: {agent_path}"));
        }

        let final_nickname = if let Some(nick) = nickname {
            if agents.used_agent_nicknames.contains(&nick) {
                let variant = format!("{nick}_r");
                agents.used_agent_nicknames.insert(variant.clone());
                Some(variant)
            } else {
                agents.used_agent_nicknames.insert(nick.clone());
                Some(nick)
            }
        } else {
            None
        };

        agents.agent_tree.insert(agent_path.to_string(), {
            let mut meta = AgentMetadata::builder()
                .agent_id(agent_id)
                .agent_path(agent_path)
                .build();
            meta.agent_nickname = final_nickname;
            meta.agent_role = role;
            meta.parent_session_id = parent_session_id;
            meta
        });

        Ok(())
    }

    /// Resolve a target string (path or nickname) to an AgentPath.
    ///
    /// If the target starts with `/`, it is treated as an agent path and
    /// verified against the registry. Otherwise it is treated as a nickname
    /// and the corresponding agent path is looked up.
    pub(crate) fn resolve_target(&self, target: &str) -> Result<AgentPath, String> {
        let agents = self
            .active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if target.starts_with('/') {
            if agents.agent_tree.contains_key(target) {
                return Ok(AgentPath(target.to_string()));
            }
        } else {
            for meta in agents.agent_tree.values() {
                if meta.agent_nickname.as_deref() == Some(target) {
                    return meta
                        .agent_path
                        .clone()
                        .ok_or_else(|| format!("agent {target} has no path"));
                }
            }
        }
        Err(format!("agent not found: {target}"))
    }

    /// Look up an agent's thread id by path.
    pub(crate) fn agent_id_for_path(&self, agent_path: &AgentPath) -> Option<SessionId> {
        self.active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .agent_tree
            .get(agent_path.as_str())
            .and_then(|m| m.agent_id.clone())
    }

    /// Look up agent metadata by thread id.
    #[allow(dead_code)]
    pub(crate) fn agent_metadata_for_thread(&self, thread_id: SessionId) -> Option<AgentMetadata> {
        self.active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .agent_tree
            .values()
            .find(|m| m.agent_id.as_ref() == Some(&thread_id))
            .cloned()
    }

    /// Return metadata for all live non-root agents.
    ///
    /// Filters out the root agent and entries that have only a path
    /// reservation (no `agent_id` yet).
    pub(crate) fn live_agents(&self) -> Vec<AgentMetadata> {
        self.active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .agent_tree
            .values()
            .filter(|m| m.agent_id.is_some() && !m.agent_path.as_ref().is_some_and(|p| p.is_root()))
            .cloned()
            .collect()
    }

    /// Update last_task_message for a thread.
    #[allow(dead_code)]
    pub(crate) fn update_last_task_message(&self, thread_id: SessionId, msg: String) {
        let mut agents = self
            .active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(meta) = agents
            .agent_tree
            .values_mut()
            .find(|m| m.agent_id.as_ref() == Some(&thread_id))
        {
            meta.last_task_message = Some(msg);
        }
    }

    /// Compute the spawn depth of the next child under a parent path.
    pub(crate) fn next_thread_spawn_depth(parent_path: &AgentPath) -> i32 {
        parent_path.0.matches('/').count() as i32
    }

    // ── internal helpers ──

    /// Insert a committed agent's metadata into the tree and mark
    /// its nickname as used. Called by [`SpawnReservation::commit`].
    fn register_spawned_thread(&self, metadata: AgentMetadata) {
        let Some(ref thread_id) = metadata.agent_id else {
            return;
        };
        let mut agents = self
            .active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = metadata
            .agent_path
            .as_ref()
            .map(|p| p.to_string())
            .unwrap_or_else(|| format!("thread:{thread_id}"));
        if let Some(ref nick) = metadata.agent_nickname {
            agents.used_agent_nicknames.insert(nick.clone());
        }
        agents.agent_tree.insert(key, metadata);
    }

    /// Pick a random unused nickname from the embedded name pool.
    ///
    /// When the pool is exhausted (all names are in use), the
    /// `used_agent_nicknames` set is cleared and `nickname_reset_count`
    /// is incremented, causing names to gain ordinal suffixes like
    /// "Euclid the 2nd", "Hypatia the 3rd", etc.
    fn reserve_agent_nickname(&self, preferred: Option<&str>) -> Option<String> {
        let mut agents = self
            .active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(pref) = preferred {
            return Some(pref.to_string());
        }

        let names: Vec<&str> = AGENT_NAMES.lines().collect();
        if names.is_empty() {
            return None;
        }

        // Filter out names already in use (with current suffix applied).
        let available: Vec<String> = names
            .iter()
            .map(|n| format_agent_nickname(n, agents.nickname_reset_count))
            .filter(|n| !agents.used_agent_nicknames.contains(n))
            .collect();

        let chosen = if let Some(name) = available.choose(&mut rand::rng()) {
            name.clone()
        } else {
            // Pool exhausted — reset with next ordinal rank.
            agents.used_agent_nicknames.clear();
            agents.nickname_reset_count += 1;
            format_agent_nickname(names.choose(&mut rand::rng())?, agents.nickname_reset_count)
        };

        agents.used_agent_nicknames.insert(chosen.clone());
        Some(chosen)
    }

    /// Reserve an agent path by inserting a placeholder entry with no
    /// `agent_id`. If the path already exists (whether reserved or
    /// committed), returns an error.
    fn reserve_agent_path(&self, agent_path: &AgentPath) -> Result<(), String> {
        let mut agents = self
            .active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match agents.agent_tree.entry(agent_path.to_string()) {
            Entry::Occupied(_) => Err(format!("agent path `{agent_path}` already exists")),
            Entry::Vacant(entry) => {
                entry.insert(
                    AgentMetadata::builder()
                        .agent_path(agent_path.clone())
                        .build(),
                );
                Ok(())
            }
        }
    }

    /// Remove a previously reserved path placeholder.
    ///
    /// Only removes the entry if it has no `agent_id` — i.e. it was
    /// reserved but never committed. A committed agent's entry is
    /// left untouched (removing it would corrupt the registry).
    fn release_reserved_agent_path(&self, agent_path: &AgentPath) {
        let mut agents = self
            .active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if agents
            .agent_tree
            .get(agent_path.as_str())
            .is_some_and(|m| m.agent_id.is_none())
        {
            agents.agent_tree.remove(agent_path.as_str());
        }
    }

    /// Lock-free CAS loop to increment the spawn counter up to `max_threads`.
    ///
    /// Uses `compare_exchange_weak` which may spuriously fail; the loop
    /// retries with the updated value. Returns `false` if the counter
    /// already reached the limit.
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
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct SpawnReservation {
    state: Arc<AgentRegistry>,
    #[builder(default = true)]
    active: bool,
    #[builder(default, setter(strip_option))]
    reserved_agent_nickname: Option<String>,
    #[builder(default, setter(strip_option))]
    reserved_agent_path: Option<AgentPath>,
}

impl SpawnReservation {
    /// Reserve a nickname from the pool, optionally preferring a specific one.
    pub(crate) fn reserve_nickname(&mut self, preferred: Option<&str>) -> Result<String, String> {
        let nick = self
            .state
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
    /// If the reservation was not committed (e.g. spawn failed midway),
    /// release the reserved path and decrement the spawn counter so
    /// the slot is available again.
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
        res.commit(
            AgentMetadata::builder()
                .agent_id(id.clone())
                .agent_path(AgentPath::root().join("test1"))
                .agent_nickname(nick.clone())
                .agent_role("default".to_string())
                .build(),
        );

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
        assert!(
            registry
                .agent_id_for_path(&AgentPath::root().join("temp"))
                .is_none()
        );
    }

    #[test]
    fn duplicate_path_is_rejected() {
        let registry = AgentRegistry::new();
        let path = AgentPath::root().join("dup");

        let mut res1 = registry.reserve_spawn_slot(None).unwrap();
        res1.reserve_path(&path).unwrap();
        res1.commit(
            AgentMetadata::builder()
                .agent_id(SessionId("a".to_string()))
                .agent_path(path.clone())
                .build(),
        );

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
        let names: Vec<&str> = AGENT_NAMES.lines().collect();
        let mut reservations = Vec::new();
        for _ in 0..names.len() {
            let mut res = registry.reserve_spawn_slot(None).unwrap();
            let nick = res.reserve_nickname(None).unwrap();
            reservations.push((res, nick));
        }
        // Next nickname should have ordinal suffix after pool exhaustion
        let mut res = registry.reserve_spawn_slot(None).unwrap();
        let nick = res.reserve_nickname(None).unwrap();
        assert!(
            nick.contains("the 2nd"),
            "expected ordinal suffix, got: {nick}"
        );
    }

    #[test]
    fn depth_computation() {
        assert_eq!(
            AgentRegistry::next_thread_spawn_depth(&AgentPath::root()),
            1
        );
        assert_eq!(
            AgentRegistry::next_thread_spawn_depth(&AgentPath::root().join("a").join("b")),
            3
        );
    }
}
