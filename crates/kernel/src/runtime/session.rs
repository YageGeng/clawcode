use super::*;

impl Kernel {
    /// Generates, persists, and registers one new session for a working directory.
    pub async fn create_generated_session(
        &self,
        cwd: PathBuf,
    ) -> Result<SessionId, KernelError> {
        let session_id =
            SessionId::try_from(self.id_generator.next(IdKind::Session))
                .map_err(|error| KernelError::Protocol(error.to_string()))?;
        self.create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await?;
        Ok(session_id)
    }

    /// Creates a persisted session with a root `main` lane and registers it.
    pub async fn create_session(
        &self,
        options: SessionCreateOptions,
    ) -> Result<PathBuf, KernelError> {
        let session_id = options.session_id.clone();
        let cwd = options.cwd.clone();
        {
            let sessions = self
                .sessions
                .read()
                .map_err(|_poison_error| KernelError::Poisoned)?;
            if sessions.contains_key(&session_id) {
                return Err(KernelError::DuplicateSession(session_id));
            }
        }
        let store = self.store_factory.create(options)?;
        let path = store.path().to_path_buf();
        let runtime = self.build_session_runtime(store, cwd).await?;
        self.register_session(session_id.clone(), runtime)?;
        self.dispatch(ExtensionEvent::ProjectTrust, &session_id, None)
            .await?;
        self.dispatch(ExtensionEvent::ResourcesDiscover, &session_id, None)
            .await?;
        self.dispatch(ExtensionEvent::SessionStart, &session_id, None)
            .await?;
        self.dispatch(ExtensionEvent::ModelSelect, &session_id, None)
            .await?;
        self.dispatch(ExtensionEvent::ThinkingLevelSelect, &session_id, None)
            .await?;
        Ok(path)
    }

    /// Lists persisted sessions by scanning v4 headers instead of a global index.
    pub fn list_sessions(
        &self,
        cwd: Option<&std::path::Path>,
    ) -> Result<Vec<SessionSummary>, KernelError> {
        Ok(self
            .store_factory
            .list(cwd)?
            .into_iter()
            .map(|metadata| {
                SessionSummary::builder()
                    .session_id(metadata.id)
                    .cwd(metadata.cwd)
                    .created_at_ms(metadata.created_at_ms)
                    .modified_at_ms(metadata.modified_at_ms)
                    .parent_session_id(metadata.parent_session_id)
                    .name(metadata.name)
                    .build()
            })
            .collect())
    }

    /// Opens one persisted session or reuses the matching live runtime idempotently.
    pub async fn resume_session(
        &self,
        session_id: SessionId,
        cwd: PathBuf,
    ) -> Result<PathBuf, KernelError> {
        self.dispatch(ExtensionEvent::SessionBeforeSwitch, &session_id, None)
            .await?;
        let existing = self
            .sessions
            .read()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .get(&session_id)
            .map(Arc::clone);
        if let Some(existing) = existing {
            if existing.cwd != cwd {
                return Err(KernelError::SessionCwdMismatch {
                    session_id,
                    expected: existing.cwd.clone(),
                    received: cwd,
                });
            }
            let path = existing
                .store
                .lock()
                .map_err(|_poison_error| KernelError::Poisoned)?
                .path()
                .to_path_buf();
            self.dispatch(ExtensionEvent::SessionSwitch, &session_id, None)
                .await?;
            return Ok(path);
        }
        {
            let sessions = self
                .sessions
                .read()
                .map_err(|_poison_error| KernelError::Poisoned)?;
            if sessions.contains_key(&session_id) {
                return Err(KernelError::DuplicateSession(session_id));
            }
        }
        let metadata = self
            .store_factory
            .list(Some(&cwd))?
            .into_iter()
            .find(|metadata| metadata.id == session_id)
            .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))?;
        let store = self.store_factory.open(&metadata.path)?;
        let path = store.path().to_path_buf();
        let runtime = self.build_session_runtime(store, cwd).await?;
        self.register_session(session_id.clone(), runtime)?;
        self.dispatch(ExtensionEvent::ProjectTrust, &session_id, None)
            .await?;
        self.dispatch(ExtensionEvent::ResourcesDiscover, &session_id, None)
            .await?;
        self.dispatch(ExtensionEvent::SessionStart, &session_id, None)
            .await?;
        self.dispatch(ExtensionEvent::ModelSelect, &session_id, None)
            .await?;
        self.dispatch(ExtensionEvent::ThinkingLevelSelect, &session_id, None)
            .await?;
        self.dispatch(ExtensionEvent::SessionSwitch, &session_id, None)
            .await?;
        Ok(path)
    }

    /// Cancels active work, waits for it to settle, and releases in-memory session resources.
    pub async fn close_session(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        let session = self.session(session_id)?;
        session
            .cancellation
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .cancel();
        let _run_guard = session.run_gate.lock().await;
        self.dispatch(ExtensionEvent::SessionShutdown, session_id, None)
            .await?;
        self.sessions
            .write()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .remove(session_id);
        Ok(())
    }

    /// Releases an active runtime before removing its persisted session history idempotently.
    pub async fn delete_session(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        match self.close_session(session_id).await {
            Ok(()) | Err(KernelError::SessionNotFound(_)) => {}
            Err(error) => return Err(error),
        }
        self.store_factory.delete(session_id)?;
        Ok(())
    }

    /// Cancels the active run for a session; the next run receives a fresh token.
    pub fn cancel_session(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        let session = self.session(session_id)?;
        session
            .cancellation
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .cancel();
        Ok(())
    }

    /// Returns the compaction-aware active model context for runtime inspection.
    pub fn session_messages(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<AgentMessage>, KernelError> {
        self.session(session_id)?
            .history
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)
            .map(|history| history.clone())
    }

    /// Returns every persisted message on the active branch for ACP replay and UI history.
    pub fn session_transcript(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<AgentMessage>, KernelError> {
        let session = self.session(session_id)?;
        let store = session
            .store
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        let Some(leaf) = store.lane(&session.lane) else {
            return Ok(Vec::new());
        };
        store
            .branch(leaf)?
            .into_iter()
            .filter(|entry| entry.kind == EntryKind::Message)
            .map(|entry| {
                serde_json::from_value(serde_json::Value::Object(entry.payload))
                    .map_err(|error| {
                        store::StoreError::InvalidSession(format!(
                            "invalid message entry {}: {error}",
                            entry.id
                        ))
                        .into()
                    })
            })
            .collect()
    }

    /// Returns all persisted entries and the active main-lane cursor.
    pub fn session_tree(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionTreeSnapshot, KernelError> {
        let session = self.session(session_id)?;
        let store = session
            .store
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        let entries = store
            .entries()
            .into_iter()
            .map(|entry| {
                SessionTreeEntry::builder()
                    .entry_id(entry.id)
                    .parent_id(entry.parent_id)
                    .kind(entry.kind.as_str().to_string())
                    .timestamp_ms(entry.timestamp_ms)
                    .payload(serde_json::Value::Object(entry.payload))
                    .build()
            })
            .collect();
        Ok(SessionTreeSnapshot::builder()
            .session_id(session_id.clone())
            .lane(session.lane.clone())
            .leaf_id(store.lane(&session.lane).cloned())
            .entries(entries)
            .name(store.name().map(ToOwned::to_owned))
            .build())
    }

    /// Persists a normalized manual session title without modifying transcript entries.
    pub async fn rename_session(
        &self,
        session_id: &SessionId,
        title: SessionTitle,
    ) -> Result<(), KernelError> {
        self.session(session_id)?
            .store
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .set_name(Some(title.as_str().to_string()))?;
        self.dispatch(ExtensionEvent::SessionInfoChanged, session_id, None)
            .await?;
        Ok(())
    }

    /// Moves the main lane and rebuilds model history from the selected branch.
    pub async fn navigate_session(
        &self,
        session_id: &SessionId,
        target: Option<EntryId>,
    ) -> Result<SessionTreeSnapshot, KernelError> {
        self.dispatch(ExtensionEvent::SessionBeforeTree, session_id, None)
            .await?;
        let session = self.session(session_id)?;
        let _run_guard = session.run_gate.lock().await;
        let history = {
            let mut store = session
                .store
                .lock()
                .map_err(|_poison_error| KernelError::Poisoned)?;
            store.move_lane(&session.lane, target)?;
            Self::history_from_store(store.as_ref(), &session.lane)?
        };
        *session
            .history
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)? = history;
        self.dispatch(ExtensionEvent::SessionTree, session_id, None)
            .await?;
        self.session_tree(session_id)
    }

    /// Forks one selected ancestor branch into a new persisted session and registers it.
    pub async fn fork_session(
        &self,
        source_session_id: &SessionId,
        source_leaf: EntryId,
        new_session_id: SessionId,
        cwd: PathBuf,
    ) -> Result<PathBuf, KernelError> {
        self.dispatch(
            ExtensionEvent::SessionBeforeFork,
            source_session_id,
            None,
        )
        .await?;
        let source = self.session(source_session_id)?;
        let source_path = source
            .store
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .path()
            .to_path_buf();
        let store = self.store_factory.fork(
            &source_path,
            SessionForkOptions {
                create: SessionCreateOptions {
                    session_id: new_session_id.clone(),
                    cwd: cwd.clone(),
                    parent_session_id: Some(source_session_id.clone()),
                },
                source_leaf,
                lane: source.lane.clone(),
            },
        )?;
        let path = store.path().to_path_buf();
        let runtime = self.build_session_runtime(store, cwd).await?;
        self.register_session(new_session_id.clone(), runtime)?;
        self.dispatch(ExtensionEvent::SessionFork, &new_session_id, None)
            .await?;
        Ok(path)
    }

    /// Generates a new session id and forks one selected ancestor branch.
    pub async fn fork_generated_session(
        &self,
        source_session_id: &SessionId,
        source_leaf: EntryId,
        cwd: PathBuf,
    ) -> Result<SessionId, KernelError> {
        let session_id =
            SessionId::try_from(self.id_generator.next(IdKind::Session))
                .map_err(|error| KernelError::Protocol(error.to_string()))?;
        self.fork_session(
            source_session_id,
            source_leaf,
            session_id.clone(),
            cwd,
        )
        .await?;
        Ok(session_id)
    }

    /// Builds session-scoped tools and reconstructs model history from the active main branch.
    async fn build_session_runtime(
        &self,
        store: Box<dyn SessionStore>,
        cwd: PathBuf,
    ) -> Result<Arc<SessionRuntime>, KernelError> {
        let mut tools = (*self.tools).clone();
        let mcp_servers = if let Some(factory) = &self.mcp_factory {
            let mcp_session = factory.create().await?;
            tools.merge(mcp_session.tools)?;
            mcp_session.servers
        } else {
            Vec::new()
        };
        let lane = LaneId::try_from("main")
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        let history = Self::history_from_store(store.as_ref(), &lane)?;
        let queue = PendingQueue::from_records(&store.records())?;
        let project_context =
            self.system_prompt_factory.project_context(&cwd)?;
        Ok(Arc::new(
            SessionRuntime::builder()
                .store(Mutex::new(store))
                .lane(lane)
                .cwd(cwd)
                .tools(Arc::new(tools))
                .mcp_servers(Arc::from(mcp_servers))
                .project_context(project_context)
                .history(Mutex::new(history))
                .queue(Mutex::new(queue))
                .run_gate(AsyncMutex::new(()))
                .active_run_id(Mutex::new(None))
                .cancellation(Mutex::new(CancellationToken::new()))
                .event_sequence(AtomicU64::new(0))
                .build(),
        ))
    }

    /// Reconstructs compaction-aware model context from one persisted lane branch.
    fn history_from_store(
        store: &dyn SessionStore,
        lane: &LaneId,
    ) -> Result<Vec<AgentMessage>, KernelError> {
        let Some(leaf) = store.lane(lane) else {
            return Ok(Vec::new());
        };
        let mut history = Vec::new();
        for entry in store.branch(leaf)? {
            match entry.kind {
                EntryKind::Message => {
                    // Persisted transcript corruption is a session-schema
                    // failure, including missing mandatory Assistant metadata.
                    let message = serde_json::from_value(
                        serde_json::Value::Object(entry.payload),
                    )
                    .map_err(|error| {
                        store::StoreError::InvalidSession(format!(
                            "invalid message entry {}: {error}",
                            entry.id
                        ))
                    })?;
                    history.push(message);
                }
                EntryKind::Compaction => {
                    let compaction_entry_id = entry.id.clone();
                    let data: CompactionData = serde_json::from_value(
                        serde_json::Value::Object(entry.payload),
                    )?;
                    let details = data.details.ok_or_else(|| {
                        KernelError::Protocol(
                            "compaction details are required".to_string(),
                        )
                    })?;
                    let summary_message = AgentMessage {
                        identity: MessageIdentity {
                            message_id: MessageId::try_from(format!(
                                "compaction-summary-{}",
                                compaction_entry_id
                            ))
                            .map_err(|error| {
                                KernelError::Protocol(error.to_string())
                            })?,
                            turn_id: details.turn_id,
                        },
                        timing: MessageTiming::try_from((
                            details.started_at_ms,
                            details.started_at_ms,
                            details.ended_at_ms,
                        ))
                        .map_err(|error| {
                            KernelError::Protocol(error.to_string())
                        })?,
                        content: MessageContent::System {
                            blocks: vec![ContentBlock::Text {
                                text: data.summary,
                            }],
                        },
                    };
                    history = Vec::with_capacity(
                        data.retained_tail.len().saturating_add(1),
                    );
                    history.push(summary_message);
                    history.extend(data.retained_tail);
                }
                EntryKind::ModelChange
                | EntryKind::ThinkingLevelChange
                | EntryKind::ActiveToolsChange
                | EntryKind::BranchSummary
                | EntryKind::Custom => {}
            }
        }
        Ok(history)
    }

    /// Registers a fully constructed runtime without retaining a map guard across async hooks.
    fn register_session(
        &self,
        session_id: SessionId,
        runtime: Arc<SessionRuntime>,
    ) -> Result<(), KernelError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        if sessions.contains_key(&session_id) {
            return Err(KernelError::DuplicateSession(session_id));
        }
        sessions.insert(session_id, runtime);
        Ok(())
    }

    /// Resolves one registered session without holding the map lock afterward.
    pub(super) fn session(
        &self,
        session_id: &SessionId,
    ) -> Result<Arc<SessionRuntime>, KernelError> {
        self.sessions
            .read()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .get(session_id)
            .map(Arc::clone)
            .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))
    }

    /// Persists one message and then exposes it to subsequent model contexts.
    pub(super) fn persist_message(
        &self,
        session: &SessionRuntime,
        message: &AgentMessage,
    ) -> Result<(), KernelError> {
        let entry_id = EntryId::try_from(self.id_generator.next(IdKind::Entry))
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        self.persist_message_at(session, message, entry_id)
    }

    /// Persists one message at an identifier reserved by queue acceptance.
    pub(super) fn persist_message_at(
        &self,
        session: &SessionRuntime,
        message: &AgentMessage,
        entry_id: EntryId,
    ) -> Result<(), KernelError> {
        session
            .store
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .append_entry(
                &session.lane,
                NewEntry {
                    id: entry_id,
                    kind: EntryKind::Message,
                    payload: serde_json::to_value(message)?,
                },
            )?;
        session
            .history
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .push(message.clone());
        Ok(())
    }

    /// Sets the first non-empty user line as the default title exactly once.
    pub(super) fn ensure_default_title(
        &self,
        session: &SessionRuntime,
        message: &AgentMessage,
    ) -> Result<Option<String>, KernelError> {
        let MessageContent::User { blocks } = &message.content else {
            return Ok(None);
        };
        let Some(title) = blocks
            .iter()
            .filter_map(ContentBlock::text)
            .flat_map(str::lines)
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(|line| line.chars().take(80).collect::<String>())
        else {
            return Ok(None);
        };
        let mut store = session
            .store
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        if store.name().is_some() {
            return Ok(None);
        }
        store.set_name(Some(title.clone()))?;
        Ok(Some(title))
    }

    /// Appends one durable pi v4 lane record for run recovery and inspection.
    pub(super) fn record_operation(
        &self,
        session: &SessionRuntime,
        run_id: &RunId,
        kind: RecordKind,
        payload: serde_json::Value,
    ) -> Result<(), KernelError> {
        let record_id =
            RecordId::try_from(self.id_generator.next(IdKind::Record))
                .map_err(|error| KernelError::Protocol(error.to_string()))?;
        session
            .store
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .append_record(
                NewRecord::builder()
                    .id(record_id)
                    .lane(session.lane.clone())
                    .run_id(Some(run_id.clone()))
                    .kind(kind)
                    .payload(payload)
                    .build(),
            )?;
        Ok(())
    }
}
