use protocol::{EntryId, LaneId, SessionId};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use store::{
    JsonlStoreFactory, NewEntry, NewRecord, RecordKind, SessionCreateOptions,
    SessionEntry, SessionForkOptions, SessionMetadata, SessionRecord,
    SessionStore, StoreError, StoreFactory,
};

/// Injects one queue write failure while retaining real JSONL persistence and reopen semantics.
pub struct FailingQueueStoreFactory {
    pub inner: JsonlStoreFactory,
    pub fail_consumed: Arc<AtomicBool>,
    pub fail_sync: Arc<AtomicBool>,
}

impl StoreFactory for FailingQueueStoreFactory {
    /// Creates one instrumented real store.
    fn create(
        &self,
        options: SessionCreateOptions,
    ) -> Result<Box<dyn SessionStore>, StoreError> {
        Ok(Box::new(FailingQueueStore {
            inner: self.inner.create(options)?,
            fail_consumed: Arc::clone(&self.fail_consumed),
            fail_sync: Arc::clone(&self.fail_sync),
        }))
    }

    /// Opens one instrumented real store.
    fn open(&self, path: &Path) -> Result<Box<dyn SessionStore>, StoreError> {
        Ok(Box::new(FailingQueueStore {
            inner: self.inner.open(path)?,
            fail_consumed: Arc::clone(&self.fail_consumed),
            fail_sync: Arc::clone(&self.fail_sync),
        }))
    }

    /// Forks one instrumented real store.
    fn fork(
        &self,
        source_path: &Path,
        options: SessionForkOptions,
    ) -> Result<Box<dyn SessionStore>, StoreError> {
        let inner = self.inner.fork(source_path, options)?;
        Ok(Box::new(FailingQueueStore {
            inner,
            fail_consumed: Arc::clone(&self.fail_consumed),
            fail_sync: Arc::clone(&self.fail_sync),
        }))
    }

    /// Delegates session discovery.
    fn list(
        &self,
        cwd: Option<&Path>,
    ) -> Result<Vec<SessionMetadata>, StoreError> {
        self.inner.list(cwd)
    }

    /// Delegates session deletion.
    fn delete(&self, session_id: &SessionId) -> Result<(), StoreError> {
        self.inner.delete(session_id)
    }
}

/// Delegates real persistence except for a single armed failure.
struct FailingQueueStore {
    inner: Box<dyn SessionStore>,
    pub fail_consumed: Arc<AtomicBool>,
    pub fail_sync: Arc<AtomicBool>,
}

impl SessionStore for FailingQueueStore {
    /// Returns the delegated session identifier.
    fn session_id(&self) -> &SessionId {
        self.inner.session_id()
    }

    /// Returns the delegated session path.
    fn path(&self) -> &Path {
        self.inner.path()
    }

    /// Delegates lane creation.
    fn create_lane(
        &mut self,
        lane: LaneId,
        at: Option<EntryId>,
    ) -> Result<(), StoreError> {
        self.inner.create_lane(lane, at)
    }

    /// Delegates lane movement.
    fn move_lane(
        &mut self,
        lane: &LaneId,
        to: Option<EntryId>,
    ) -> Result<(), StoreError> {
        self.inner.move_lane(lane, to)
    }

    /// Delegates entry persistence.
    fn append_entry(
        &mut self,
        lane: &LaneId,
        entry: NewEntry,
    ) -> Result<SessionEntry, StoreError> {
        self.inner.append_entry(lane, entry)
    }

    /// Returns the delegated lane leaf.
    fn lane(&self, lane: &LaneId) -> Option<&EntryId> {
        self.inner.lane(lane)
    }

    /// Returns one delegated entry.
    fn get_entry(&self, id: &EntryId) -> Option<&SessionEntry> {
        self.inner.get_entry(id)
    }

    /// Returns all delegated entries.
    fn entries(&self) -> Vec<SessionEntry> {
        self.inner.entries()
    }

    /// Returns one delegated branch.
    fn branch(&self, leaf: &EntryId) -> Result<Vec<SessionEntry>, StoreError> {
        self.inner.branch(leaf)
    }

    /// Records selected operation mutations before delegating persistence.
    fn append_record(
        &mut self,
        record: NewRecord,
    ) -> Result<SessionRecord, StoreError> {
        if record.kind == RecordKind::QueueCancelled
            && self.fail_consumed.swap(false, Ordering::SeqCst)
        {
            return Err(std::io::Error::other(
                "injected consumed record failure",
            )
            .into());
        }
        self.inner.append_record(record)
    }

    /// Records and delegates one durability checkpoint.
    fn sync(&mut self) -> Result<(), StoreError> {
        if self.fail_sync.swap(false, Ordering::SeqCst) {
            return Err(std::io::Error::other("injected sync failure").into());
        }
        self.inner.sync()
    }

    /// Returns all delegated records.
    fn records(&self) -> Vec<SessionRecord> {
        self.inner.records()
    }

    /// Returns the delegated shared mutation sequence.
    fn last_sequence(&self) -> u64 {
        self.inner.last_sequence()
    }

    /// Records and delegates session-name persistence.
    fn set_name(&mut self, name: Option<String>) -> Result<(), StoreError> {
        self.inner.set_name(name)
    }

    /// Returns the delegated session name.
    fn name(&self) -> Option<&str> {
        self.inner.name()
    }

    /// Delegates entry-label persistence.
    fn set_label(
        &mut self,
        target_id: EntryId,
        label: Option<String>,
    ) -> Result<(), StoreError> {
        self.inner.set_label(target_id, label)
    }

    /// Returns the delegated entry label.
    fn label(&self, target_id: &EntryId) -> Option<&str> {
        self.inner.label(target_id)
    }
}
