use super::*;

mod replay;

/// Session-local selections reconstructed from Pi v4 branch entries.
struct RestoredSessionSettings {
    model: Arc<dyn Model>,
    thinking_level: ThinkingLevel,
    active_tools: Vec<String>,
}

impl RestoredSessionSettings {
    /// Replays setting entries on the active branch and validates current capabilities.
    fn load(
        store: &dyn SessionStore,
        lane: &LaneId,
        models: &ModelCatalog,
        tools: &ToolRegistry,
    ) -> Result<Self, KernelError> {
        let mut settings = Self {
            model: models.active(),
            thinking_level: ThinkingLevel::Off,
            active_tools: tools
                .names()
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
        };
        let Some(leaf) = store.lane(lane) else {
            return Ok(settings);
        };
        for entry in store.branch(leaf)? {
            match entry.kind {
                EntryKind::ModelChange => {
                    let provider_id = entry
                        .payload
                        .get("provider")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| {
                            KernelError::Protocol(format!(
                                "model change {} has no provider",
                                entry.id
                            ))
                        })?;
                    let model_id = entry
                        .payload
                        .get("modelId")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| {
                            KernelError::Protocol(format!(
                                "model change {} has no modelId",
                                entry.id
                            ))
                        })?;
                    settings.model = models
                        .resolve(provider_id, model_id)
                        .ok_or_else(|| {
                            ModelError::Unavailable(format!(
                                "persisted model is not configured: {provider_id}/{model_id}"
                            ))
                        })?;
                }
                EntryKind::ThinkingLevelChange => {
                    settings.thinking_level = serde_json::from_value(
                        entry
                            .payload
                            .get("thinkingLevel")
                            .cloned()
                            .ok_or_else(|| {
                                KernelError::Protocol(format!(
                                    "thinking change {} has no thinkingLevel",
                                    entry.id
                                ))
                            })?,
                    )?;
                }
                EntryKind::ActiveToolsChange => {
                    settings.active_tools = serde_json::from_value(
                        entry.payload.get("activeTools").cloned().ok_or_else(
                            || {
                                KernelError::Protocol(format!(
                                    "active-tools change {} has no activeTools",
                                    entry.id
                                ))
                            },
                        )?,
                    )?;
                    for name in &settings.active_tools {
                        if tools.tool(name).is_none() {
                            return Err(KernelError::Tool(
                                tools::ToolError::NotFound(name.clone()),
                            ));
                        }
                    }
                }
                EntryKind::Message
                | EntryKind::Compaction
                | EntryKind::BranchSummary
                | EntryKind::Custom => {}
            }
        }
        Ok(settings)
    }
}

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
        if let Err(error) = self
            .start_extensions(&session_id, protocol::SessionStartReason::New)
            .await
        {
            self.rollback_session_registration(&session_id, true)
                .await?;
            return Err(error);
        }
        self.checkpoint_started_session(&session_id, true).await?;
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
            let context = self.extension_context(&existing, None, None)?;
            let before = existing
                .extensions
                .emit_session_before_switch(
                    &protocol::SessionBeforeSwitchEvent {
                        reason: protocol::SessionSwitchReason::Resume,
                        target_session_id: Some(session_id.clone()),
                    },
                    &context,
                )
                .await;
            if before.cancel {
                return Err(KernelError::ExtensionBlocked(
                    "session resume cancelled".to_string(),
                ));
            }
            existing.sync_store()?;
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
        let context = self.extension_context(&runtime, None, None)?;
        let before = runtime
            .extensions
            .emit_session_before_switch(
                &protocol::SessionBeforeSwitchEvent {
                    reason: protocol::SessionSwitchReason::Resume,
                    target_session_id: Some(session_id.clone()),
                },
                &context,
            )
            .await;
        if before.cancel {
            runtime.extensions.invalidate();
            return Err(KernelError::ExtensionBlocked(
                "session resume cancelled".to_string(),
            ));
        }
        self.register_session(session_id.clone(), runtime)?;
        if let Err(error) = self
            .start_extensions(&session_id, protocol::SessionStartReason::Resume)
            .await
        {
            self.rollback_session_registration(&session_id, false)
                .await?;
            return Err(error);
        }
        self.checkpoint_started_session(&session_id, false).await?;
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
        // Transition before invoking hooks so reentrant host operations fail
        // immediately instead of waiting on the gate held by this shutdown.
        session.begin_closing()?;
        let context = self.extension_context(&session, None, None)?;
        session
            .extensions
            .emit_session_shutdown(
                &protocol::SessionShutdownEvent {
                    reason: protocol::SessionShutdownReason::Quit,
                    target_session_id: None,
                },
                &context,
            )
            .await;
        if let Some(mcp) = &session.mcp {
            mcp.shutdown().await?;
        }
        session.stop_mcp_projection().await?;
        session.sync_store()?;
        session.finish_closing()?;
        session.extensions.invalidate();
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
        let session = self.session(session_id)?;
        session
            .store
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .set_name(Some(title.as_str().to_string()))?;
        session.sync_store()?;
        let context = self.extension_context(&session, None, None)?;
        session
            .extensions
            .emit_session_info_changed(
                &protocol::SessionInfoChangedEvent {
                    name: Some(title.as_str().to_string()),
                },
                &context,
            )
            .await;
        Ok(())
    }

    /// Forks one selected ancestor branch into a new persisted session and registers it.
    pub async fn fork_session(
        &self,
        source_session_id: &SessionId,
        source_leaf: EntryId,
        new_session_id: SessionId,
        cwd: PathBuf,
    ) -> Result<PathBuf, KernelError> {
        let source = self.session(source_session_id)?;
        let context = self.extension_context(&source, None, None)?;
        let decision = source
            .extensions
            .emit_session_before_fork(
                &protocol::SessionBeforeForkEvent {
                    entry_id: source_leaf.clone(),
                    position: protocol::SessionForkPosition::At,
                },
                &context,
            )
            .await;
        if decision.cancel {
            return Err(KernelError::ExtensionBlocked(
                "session fork cancelled".to_string(),
            ));
        }
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
        if let Err(error) = self
            .start_extensions(
                &new_session_id,
                protocol::SessionStartReason::Fork,
            )
            .await
        {
            self.rollback_session_registration(&new_session_id, true)
                .await?;
            return Err(error);
        }
        self.checkpoint_started_session(&new_session_id, true)
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

    /// Runs typed startup, resource, session, model, and thinking points for one runtime.
    async fn start_extensions(
        &self,
        session_id: &SessionId,
        reason: protocol::SessionStartReason,
    ) -> Result<(), KernelError> {
        let session = self.session(session_id)?;
        let context = self.extension_context(&session, None, None)?;
        let trust = session
            .extensions
            .emit_project_trust(
                &protocol::ProjectTrustEvent {
                    cwd: session.cwd.clone(),
                },
                &context,
            )
            .await;
        // Pi announces the live session before extensions discover resources
        // that will be attached to that session runtime.
        session
            .extensions
            .emit_session_start(
                &protocol::SessionStartEvent {
                    reason,
                    previous_session_id: None,
                },
                &context,
            )
            .await;
        let project_resources_allowed =
            trust.trusted != protocol::ProjectTrustDecision::No;
        // Project trust gates built-in project roots; Extension-owned roots
        // remain discoverable because they are not implicitly project content.
        let resources = session
            .extensions
            .emit_resources_discover(
                &protocol::ResourcesDiscoverEvent {
                    cwd: session.cwd.clone(),
                    reason: protocol::ResourcesDiscoverReason::Startup,
                },
                &context,
            )
            .await;
        let skills = self
            .skill_factory
            .as_ref()
            .map(|factory| {
                factory.create(protocol::SkillResourceRequest {
                    cwd: session.cwd.clone(),
                    project_resources_allowed,
                    extension_skill_paths: resources.skill_paths,
                })
            })
            .transpose()?
            .map(Arc::new);
        session.skills.set(skills).map_err(|_skills| {
            KernelError::Protocol(
                "session Skill resources already initialized".to_string(),
            )
        })?;
        let prompt =
            self.prompt_factory
                .create(protocol::PromptResourceRequest {
                    cwd: session.cwd.clone(),
                    project_resources_allowed,
                    extension_prompt_paths: resources.prompt_paths,
                })?;
        session.prompt.set(Arc::new(prompt)).map_err(|_prompt| {
            KernelError::Protocol(
                "session Prompt resources already initialized".to_string(),
            )
        })?;
        let restored_model = session
            .model
            .read()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .profile()
            .clone();
        let restored_thinking_level = *session
            .thinking_level
            .read()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        session
            .extensions
            .emit_model_select(
                &protocol::ModelSelectEvent {
                    model: restored_model,
                    previous_model: None,
                    source: protocol::ModelSelectSource::Restore,
                },
                &context,
            )
            .await;
        session
            .extensions
            .emit_thinking_level_select(
                &protocol::ThinkingLevelSelectEvent {
                    level: restored_thinking_level,
                    previous_level: protocol::ThinkingLevel::Off,
                },
                &context,
            )
            .await;
        Ok(())
    }

    /// Builds session-scoped tools and reconstructs model history from the active main branch.
    async fn build_session_runtime(
        &self,
        store: Box<dyn SessionStore>,
        cwd: PathBuf,
    ) -> Result<Arc<SessionRuntime>, KernelError> {
        let modules = self.extension_factory.create_modules()?;
        let mut registrar = ExtensionRegistrar::new();
        for module in modules {
            registrar.register_module(module.as_ref())?;
        }
        let extension_registry = Arc::new(registrar.freeze());
        let builtins = (*self.tools).clone();
        let mut extension_tools = ToolRegistry::default();
        let mut extension_tool_names = BTreeSet::new();
        for (_extension_id, tool) in extension_registry.tools() {
            let name = tool.definition().name;
            // The first extension registration wins while still overriding built-ins.
            if extension_tool_names.insert(name) {
                extension_tools.upsert(tool);
            }
        }
        let session_id = store.session_id().clone();
        let (mcp, mcp_tools, mcp_host) = if let Some(factory) =
            &self.mcp_factory
        {
            let host = Arc::new(mcp::SessionMcpHost::new(cwd.clone()));
            let mcp_session = Arc::new(
                factory
                    .create(
                        ::mcp::McpSessionRequest::builder()
                            .session_id(session_id)
                            .cwd(cwd.clone())
                            .host(Arc::clone(&host) as Arc<dyn ::mcp::McpHost>)
                            .shutdown(self.shutdown.child_token())
                            .build(),
                    )
                    .await?,
            );
            let mcp_tools = mcp_session.tool_registry()?;
            (Some(mcp_session), mcp_tools, Some(host))
        } else {
            (None, ToolRegistry::default(), None)
        };
        let mut tools = builtins.clone();
        tools.overlay(&extension_tools);
        tools.overlay(&mcp_tools);
        let lane = LaneId::try_from("main")
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        let settings = RestoredSessionSettings::load(
            store.as_ref(),
            &lane,
            &self.models,
            &tools,
        )?;
        let history = Self::history_from_store(store.as_ref(), &lane)?;
        // Resume starts after persisted entry order so replayed transcript events
        // always sort before newly emitted live events in ACP clients.
        let initial_event_sequence =
            u64::try_from(store.entries().len()).unwrap_or(u64::MAX);
        let queue = PendingQueue::from_records(&store.records())?;
        let commands =
            DynamicCommandRegistry::new(extension_registry.commands())
                .map_err(|error| {
                    KernelError::ExtensionBlocked(error.to_string())
                })?;
        let mut flags = self.static_extensions.flags.clone();
        let mut flag_names = flags
            .iter()
            .map(|flag| flag.name.clone())
            .collect::<BTreeSet<_>>();
        for flag in extension_registry.flags() {
            if flag_names.insert(flag.name.clone()) {
                flags.push(flag);
            }
        }
        let tool_state = Arc::new(SessionToolState::new(
            builtins,
            extension_tools,
            mcp_tools,
            settings.active_tools,
        )?);
        let event_sink = Arc::new(Mutex::new(None));
        let diagnostic_turn_id = Arc::new(Mutex::new(None));
        let event_sequence = Arc::new(AtomicU64::new(initial_event_sequence));
        // Diagnostics share transcript persistence without retaining the session
        // runtime and creating a reference cycle through ExtensionRuntime.
        let store = Arc::new(Mutex::new(store));
        let history = Arc::new(Mutex::new(history));
        let extensions = Arc::new(ExtensionRuntime::new(
            Arc::clone(&extension_registry),
            Arc::new(
                KernelExtensionDiagnostics::builder()
                    .clock(Arc::clone(&self.clock))
                    .sink(Arc::clone(&event_sink))
                    .turn_id(Arc::clone(&diagnostic_turn_id))
                    .sequence(Arc::clone(&event_sequence))
                    .store(Arc::clone(&store))
                    .history(Arc::clone(&history))
                    .lane(lane.clone())
                    .id_generator(Arc::clone(&self.id_generator))
                    .build(),
            ),
        ));
        let runtime = Arc::new(
            SessionRuntime::builder()
                .store(store)
                .lane(lane)
                .cwd(cwd)
                .mcp(mcp)
                .prompt(OnceLock::new())
                .skills(OnceLock::new())
                .extensions(extensions)
                .tool_state(tool_state)
                .mcp_projection(AsyncMutex::new(None))
                .pending_mcp_elicitations(Mutex::new(HashMap::new()))
                .commands(commands)
                .flags(Arc::from(flags))
                .models(Arc::clone(&self.models))
                .model(RwLock::new(settings.model))
                .thinking_level(RwLock::new(settings.thinking_level))
                .event_bus(broadcast::channel(64).0)
                .event_sink(event_sink)
                .diagnostic_turn_id(diagnostic_turn_id)
                .history(history)
                .queue(Mutex::new(queue))
                .lifecycle(RwLock::new(SessionLifecycle::Active))
                .run_gate(AsyncMutex::new(()))
                .active_run_id(Mutex::new(None))
                .idle_notify(Notify::new())
                .cancellation(Mutex::new(CancellationToken::new()))
                .event_sequence(event_sequence)
                .build(),
        );
        if let Some(host) = mcp_host {
            host.attach(&runtime)?;
        }
        runtime.start_mcp_projection().await?;
        Ok(runtime)
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

    /// Removes a partially started runtime and optionally its new durable log.
    async fn rollback_session_registration(
        &self,
        session_id: &SessionId,
        delete_persisted: bool,
    ) -> Result<(), KernelError> {
        let runtime = self
            .sessions
            .write()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .remove(session_id);
        if let Some(runtime) = runtime {
            runtime.extensions.invalidate();
            if let Some(mcp) = &runtime.mcp {
                mcp.shutdown().await?;
            }
            runtime.stop_mcp_projection().await?;
        }
        if delete_persisted {
            self.store_factory.delete(session_id)?;
        }
        Ok(())
    }

    /// Syncs startup writes or removes the partially started runtime on failure.
    async fn checkpoint_started_session(
        &self,
        session_id: &SessionId,
        delete_persisted: bool,
    ) -> Result<(), KernelError> {
        if let Err(error) = self.session(session_id)?.sync_store() {
            tracing::error!(
                "failed to checkpoint started session {}; rolling back registration: {}",
                session_id,
                error
            );
            self.rollback_session_registration(session_id, delete_persisted)
                .await?;
            return Err(error);
        }
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
        let title = match &message.content {
            MessageContent::User { blocks } => blocks
                .iter()
                .filter_map(ContentBlock::text)
                .flat_map(str::lines)
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(|line| line.chars().take(80).collect::<String>()),
            MessageContent::ExpandedUser { expansion } => expansion
                .invocation
                .original
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(|line| line.chars().take(80).collect::<String>()),
            MessageContent::System { .. }
            | MessageContent::Assistant { .. }
            | MessageContent::ToolResult { .. }
            | MessageContent::BashExecution { .. }
            | MessageContent::Extension { .. }
            | MessageContent::SlashCommand { .. }
            | MessageContent::CompactionSummary { .. } => None,
        };
        let Some(title) = title else { return Ok(None) };
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
