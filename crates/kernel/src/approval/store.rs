//! Session-scoped approval cache.

use std::collections::HashMap;
use std::sync::Arc;

use protocol::ReviewDecision;
use serde::Serialize;
use tokio::sync::Mutex;

/// Session-scoped approval cache keyed by serialized approval keys.
#[derive(Clone, Default, Debug)]
pub struct ApprovalStore {
    /// Serialized key to cached decision.
    map: HashMap<String, ReviewDecision>,
}

impl ApprovalStore {
    /// Load a cached decision for a serializable approval key.
    pub fn get<K>(&self, key: &K) -> Option<ReviewDecision>
    where
        K: Serialize,
    {
        let serialized = serde_json::to_string(key).ok()?;
        self.map.get(&serialized).cloned()
    }

    /// Store a cached decision for a serializable approval key.
    pub fn put<K>(&mut self, key: K, value: ReviewDecision)
    where
        K: Serialize,
    {
        // Approval keys must be JSON-stable; invalid keys are ignored instead
        // of poisoning the session cache.
        if let Ok(serialized) = serde_json::to_string(&key) {
            self.map.insert(serialized, value);
        }
    }
}

/// Return a cached session approval or fetch and cache a new decision.
pub async fn with_cached_approval<K, F, Fut>(
    store: &Arc<Mutex<ApprovalStore>>,
    keys: Vec<K>,
    fetch: F,
) -> ReviewDecision
where
    K: Serialize,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ReviewDecision>,
{
    if keys.is_empty() {
        return fetch().await;
    }

    let already_approved = {
        let store = store.lock().await;
        keys.iter().all(|key| {
            matches!(store.get(key), Some(ReviewDecision::ApprovedForSession))
        })
    };

    if already_approved {
        return ReviewDecision::ApprovedForSession;
    }

    let decision = fetch().await;

    if matches!(decision, ReviewDecision::ApprovedForSession) {
        let mut store = store.lock().await;
        for key in keys {
            store.put(key, ReviewDecision::ApprovedForSession);
        }
    }

    decision
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct TestKey {
        command: Vec<String>,
    }

    /// Verifies cached approvals require every key to be approved for session.
    #[tokio::test]
    async fn cached_approval_requires_all_keys() {
        let store = Arc::new(Mutex::new(ApprovalStore::default()));
        store.lock().await.put(
            TestKey {
                command: vec!["cargo".to_string()],
            },
            ReviewDecision::ApprovedForSession,
        );

        let decision = with_cached_approval(
            &store,
            vec![
                TestKey {
                    command: vec!["cargo".to_string()],
                },
                TestKey {
                    command: vec!["test".to_string()],
                },
            ],
            || async { ReviewDecision::Approved },
        )
        .await;

        assert_eq!(decision, ReviewDecision::Approved);
    }

    /// Verifies ApprovedForSession writes every key into the cache.
    #[tokio::test]
    async fn approved_for_session_caches_all_keys() {
        let store = Arc::new(Mutex::new(ApprovalStore::default()));
        let keys = vec![
            TestKey {
                command: vec!["cargo".to_string()],
            },
            TestKey {
                command: vec!["test".to_string()],
            },
        ];

        let decision = with_cached_approval(&store, keys, || async {
            ReviewDecision::ApprovedForSession
        })
        .await;

        assert_eq!(decision, ReviewDecision::ApprovedForSession);
        assert!(matches!(
            store.lock().await.get(&TestKey {
                command: vec!["cargo".to_string()]
            }),
            Some(ReviewDecision::ApprovedForSession)
        ));
    }
}
