use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ::tools::ToolUpdateSink;
use protocol::{ToolCall, ToolCallId, ToolResult};
use tokio::sync::watch;

/// Correlates one replaceable tool snapshot with its transformed source call.
#[derive(Clone)]
pub(super) struct ToolExecutionUpdate {
    pub(super) call: ToolCall,
    pub(super) result: ToolResult,
}

/// Latest update plus its monotonic publication revision.
#[derive(Clone)]
struct VersionedToolExecutionUpdate {
    revision: u64,
    update: ToolExecutionUpdate,
}

/// Creates per-call publishers that share one batch-level latest-value map.
pub(super) struct ToolUpdateChannel {
    sender: watch::Sender<BTreeMap<String, VersionedToolExecutionUpdate>>,
    revision: Arc<AtomicU64>,
}

impl ToolUpdateChannel {
    /// Creates one bounded-by-tool-count update channel and its consumer.
    pub(super) fn new() -> (Self, ToolUpdateStream) {
        let (sender, receiver) = watch::channel(BTreeMap::new());
        (
            Self {
                sender,
                revision: Arc::new(AtomicU64::new(0)),
            },
            ToolUpdateStream {
                receiver,
                consumed: BTreeMap::new(),
            },
        )
    }

    /// Binds one transformed call to a shared latest-value publisher.
    pub(super) fn publisher(&self, call: ToolCall) -> Arc<dyn ToolUpdateSink> {
        Arc::new(ToolUpdatePublisher {
            sender: self.sender.clone(),
            revision: Arc::clone(&self.revision),
            call,
        })
    }
}

/// Publishes snapshots for one tool call without queueing obsolete values.
struct ToolUpdatePublisher {
    sender: watch::Sender<BTreeMap<String, VersionedToolExecutionUpdate>>,
    revision: Arc<AtomicU64>,
    call: ToolCall,
}

impl ToolUpdateSink for ToolUpdatePublisher {
    /// Replaces this call's previous snapshot in the shared batch map.
    fn publish(&self, result: ToolResult) {
        let revision = self
            .revision
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        let update = ToolExecutionUpdate {
            call: self.call.clone(),
            result,
        };
        self.sender.send_modify(|latest| {
            latest.insert(
                self.call.tool_call_id.as_str().to_string(),
                VersionedToolExecutionUpdate { revision, update },
            );
        });
    }
}

/// Consumes each call's latest unseen snapshot from one watch channel.
pub(super) struct ToolUpdateStream {
    receiver: watch::Receiver<BTreeMap<String, VersionedToolExecutionUpdate>>,
    consumed: BTreeMap<String, u64>,
}

impl ToolUpdateStream {
    /// Waits for a publication and returns only latest snapshots not yet emitted.
    pub(super) async fn changed(
        &mut self,
    ) -> Result<Vec<ToolExecutionUpdate>, watch::error::RecvError> {
        self.receiver.changed().await?;
        Ok(self.take_unseen())
    }

    /// Takes the latest unseen snapshot for a completing call before its end event.
    pub(super) fn take_for(
        &mut self,
        tool_call_id: &ToolCallId,
    ) -> Option<ToolExecutionUpdate> {
        let key = tool_call_id.as_str();
        let latest = self.receiver.borrow().get(key).cloned()?;
        if self.consumed.get(key).copied().unwrap_or_default()
            >= latest.revision
        {
            return None;
        }
        self.consumed.insert(key.to_string(), latest.revision);
        Some(latest.update)
    }

    /// Takes every latest snapshot whose publication revision remains unseen.
    pub(super) fn take_unseen(&mut self) -> Vec<ToolExecutionUpdate> {
        let latest = self
            .receiver
            .borrow_and_update()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        latest
            .into_iter()
            .filter_map(|(key, latest)| {
                if self.consumed.get(&key).copied().unwrap_or_default()
                    >= latest.revision
                {
                    return None;
                }
                self.consumed.insert(key, latest.revision);
                Some(latest.update)
            })
            .collect()
    }
}
