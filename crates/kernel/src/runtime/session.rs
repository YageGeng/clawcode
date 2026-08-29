use super::*;

mod replay;

/// Releases one exclusively reserved Session identifier when its operation exits.
struct SessionIdReservation {
    session_id: SessionId,
    pending_sessions: Arc<Mutex<HashSet<SessionId>>>,
}

impl Drop for SessionIdReservation {
    /// Removes only the Session identifier owned by this exclusive operation.
    fn drop(&mut self) {
        if let Ok(mut pending_sessions) = self.pending_sessions.lock() {
            pending_sessions.remove(&self.session_id);
        }
    }
}

/// Session-local selections reconstructed from Pi v4 branch entries.
struct RestoredSessionSettings {
    model: Arc<dyn Model>,
    thinking_level: ThinkingLevel,
    active_tools: Vec<String>,
}

impl RestoredSessionSettings {
    /// Replays settings while retaining preferences for dynamic capabilities that may reconnect.
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
        tracing::info!("started Session creation {}", session_id);
        let result: Result<PathBuf, KernelError> = async {
            let _reservation = self.reserve_session_build(&session_id)?;
            let store = self.store_factory.create(options)?;
            let path = store.path().to_path_buf();
            let runtime = self.build_session(store, cwd).await?;
            let _registration =
                self.register_session(session_id.clone(), &runtime).await?;
            if let Err(error) = self
                .start_extensions(&runtime, protocol::SessionStartReason::New)
                .await
            {
                if let Err(rollback_error) =
                    self.rollback_session_registration(&session_id, true).await
                {
                    tracing::warn!(
                        "failed to clean up Session {} after startup error: {}",
                        session_id,
                        rollback_error
                    );
                }
                return Err(error);
            }
            self.checkpoint_started_session(&session_id, &runtime, true)
                .await?;
            Ok(path)
        }
        .await;
        match result {
            Ok(path) => {
                tracing::info!("completed Session creation {}", session_id);
                Ok(path)
            }
            Err(error) => {
                tracing::error!(
                    "failed Session creation {}: {}",
                    session_id,
                    error
                );
                Err(error)
            }
        }
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

    /// Reads active Run state without waiting for the Session operation gate.
    pub fn session_runtime(
        &self,
        session_id: &SessionId,
    ) -> Result<protocol::SessionRuntimeSnapshot, KernelError> {
        let running = match self.session(session_id) {
            Ok(session) => session.execution.active_run()?.is_some(),
            Err(KernelError::SessionNotFound(_)) => {
                // Persisted Sessions are idle before their first Resume in this
                // process, so runtime discovery must not require loading them.
                if self
                    .store_factory
                    .list(None)?
                    .iter()
                    .any(|metadata| &metadata.id == session_id)
                {
                    false
                } else {
                    return Err(KernelError::SessionNotFound(
                        session_id.clone(),
                    ));
                }
            }
            Err(error) => return Err(error),
        };
        Ok(protocol::SessionRuntimeSnapshot {
            session_id: session_id.clone(),
            running,
        })
    }

    /// Validates a live Session attachment without waiting for its operation gate.
    pub fn validate_session_attachment(
        &self,
        session_id: &SessionId,
        cwd: &std::path::Path,
    ) -> Result<protocol::SessionRuntimeSnapshot, KernelError> {
        let session = self.session(session_id).map_err(|error| {
            tracing::warn!(
                "failed to validate attachment for Session {}: {}",
                session_id,
                error
            );
            error
        })?;
        if session.cwd.as_path() != cwd {
            let error = KernelError::SessionCwdMismatch {
                session_id: session_id.clone(),
                expected: session.cwd.clone(),
                received: cwd.to_path_buf(),
            };
            tracing::warn!(
                "failed to validate attachment for Session {}: {}",
                session_id,
                error
            );
            return Err(error);
        }
        let running = session.execution.active_run().map_err(|error| {
            tracing::warn!(
                "failed to read active Run while validating attachment for Session {}: {}",
                session_id,
                error
            );
            error
        })?;
        Ok(protocol::SessionRuntimeSnapshot {
            session_id: session_id.clone(),
            running: running.is_some(),
        })
    }

    /// Opens one persisted session or reuses the matching live runtime idempotently.
    pub async fn resume_session(
        &self,
        session_id: SessionId,
        cwd: PathBuf,
    ) -> Result<PathBuf, KernelError> {
        tracing::info!("started Session Resume {}", session_id);
        let result: Result<PathBuf, KernelError> = async {
        let existing = self
            .sessions
            .read()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .get(&session_id)
            .map(Arc::clone);
        if let Some(existing) = existing {
            // A live map entry is published during startup for hook context, but
            // external Resume must not observe it as an initialized Session.
            existing.ensure_active()?;
            if existing.cwd != cwd {
                return Err(KernelError::SessionCwdMismatch {
                    session_id: session_id.clone(),
                    expected: existing.cwd.clone(),
                    received: cwd,
                });
            }
            let path = existing.transcript.path()?;
            let context = self.extension_context(&existing, None, None)?;
            let before = existing
                .extensions
                .runtime_ref()
                .emit_session_before_switch(
                    &protocol::SessionBeforeSwitchEvent {
                        reason: protocol::SessionSwitchReason::Resume,
                        target_session_id: Some(session_id.clone()),
                    },
                    &context,
                )
                .await;
            // The context contains an owned Session snapshot and is no longer
            // needed after the hook; release it before adding another await so
            // the Resume future does not retain that snapshot in its state.
            drop(context);
            if before.cancel {
                return Err(KernelError::ExtensionBlocked(
                    "session resume cancelled".to_string(),
                ));
            }
            // The hook stays outside the operation gate so Extension Host calls
            // can reenter safely. Acquiring afterward rejects a Resume when
            // Close won the lifecycle race while the hook was suspended.
            let _operation_guard = existing.acquire_operation().await?;
            existing.sync_store()?;
            tracing::info!("completed Session Resume {} using live runtime", session_id);
            return Ok(path);
        }
        let _reservation = self.reserve_session_build(&session_id)?;
        let metadata = self
            .store_factory
            .list(Some(&cwd))?
            .into_iter()
            .find(|metadata| metadata.id == session_id)
            .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))?;
        let store = self.store_factory.open(&metadata.path)?;
        let path = store.path().to_path_buf();
        let runtime = self.build_session(store, cwd).await?;
        let context = self.extension_context(&runtime, None, None)?;
        let before = runtime
            .extensions
            .runtime_ref()
            .emit_session_before_switch(
                &protocol::SessionBeforeSwitchEvent {
                    reason: protocol::SessionSwitchReason::Resume,
                    target_session_id: Some(session_id.clone()),
                },
                &context,
            )
            .await;
        if before.cancel {
            if let Err(error) =
                Self::release_unregistered_session(&runtime).await
            {
                // Preserve the extension cancellation as the operation result
                // while keeping cleanup failures visible to operators.
                tracing::warn!(
                    "failed to clean up MCP after Session Resume cancellation: {}",
                    error
                );
            }
            return Err(KernelError::ExtensionBlocked(
                "session resume cancelled".to_string(),
            ));
        }
        let _registration =
            self.register_session(session_id.clone(), &runtime).await?;
        if let Err(error) = self
            .start_extensions(&runtime, protocol::SessionStartReason::Resume)
            .await
        {
            if let Err(rollback_error) =
                self.rollback_session_registration(&session_id, false).await
            {
                tracing::warn!(
                    "failed to clean up Session {} after Resume startup error: {}",
                    session_id,
                    rollback_error
                );
            }
            return Err(error);
        }
        self.checkpoint_started_session(&session_id, &runtime, false)
            .await?;
        tracing::info!("completed Session Resume {} from persisted Store", session_id);
        Ok(path)
        }
        .await;
        if let Err(error) = &result {
            tracing::error!("failed Session Resume {}: {}", session_id, error);
        }
        result
    }

    /// Cancels active work, waits for it to settle, and releases in-memory session resources.
    pub async fn close_session(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        self.close_session_with_reason(
            session_id,
            protocol::TerminalRemovalReason::SessionClosed,
        )
        .await
    }

    /// Closes one Session using the terminal removal reason of its caller.
    pub(super) async fn close_session_with_reason(
        &self,
        session_id: &SessionId,
        terminal_reason: protocol::TerminalRemovalReason,
    ) -> Result<(), KernelError> {
        tracing::info!("started Session close {}", session_id);
        let result: Result<(), KernelError> = async {
            let session = self.session(session_id)?;
            if session.is_closed()? {
                // A prior close finalized extension and MCP state but retained
                // this Session because terminal cleanup failed. Retry only the
                // owned terminal resources before removing the closed runtime.
                session.terminals.shutdown(terminal_reason).await?;
                let mut sessions = self
                    .sessions
                    .write()
                    .map_err(|_poison_error| KernelError::Poisoned)?;
                if sessions
                    .get(session_id)
                    .is_some_and(|current| Arc::ptr_eq(current, &session))
                {
                    sessions.remove(session_id);
                }
                return Ok(());
            }
            session.execution.cancel()?;
            let _run_guard = session.acquire_operation().await?;
            // Transition before invoking hooks so reentrant host operations fail
            // immediately instead of waiting on the gate held by this shutdown.
            session.begin_closing()?;
            let context = match self.extension_context(&session, None, None) {
                Ok(context) => context,
                Err(error) => {
                    if let Err(abort_error) = session.abort_closing() {
                        tracing::warn!(
                            "failed to restore Session {} after shutdown setup error: {}",
                            session_id,
                            abort_error
                        );
                    }
                    return Err(error);
                }
            };
            session
                .extensions
                .runtime_ref()
                .emit_session_shutdown(
                    &protocol::SessionShutdownEvent {
                        reason: protocol::SessionShutdownReason::Quit,
                        target_session_id: None,
                    },
                    &context,
                )
                .await;
            // Once MCP teardown starts the runtime cannot safely become active
            // again, so every finalization step runs even when an earlier one fails.
            let mcp_result = session.mcp.shutdown().await;
            let terminal_result = session
                .terminals
                .shutdown(terminal_reason)
                .await
                .map(|_count| ())
                .map_err(KernelError::from);
            let terminals_cleaned = terminal_result.is_ok();
            let sync_result = session.sync_store();
            let lifecycle_result = session.finish_closing();
            session.extensions.runtime_ref().invalidate();
            let removal_result = if terminals_cleaned {
                self.sessions
                    .write()
                    .map_err(|_poison_error| KernelError::Poisoned)
                    .map(|mut sessions| {
                        if sessions
                            .get(session_id)
                            .is_some_and(|current| Arc::ptr_eq(current, &session))
                        {
                            sessions.remove(session_id);
                        }
                    })
            } else {
                // Keep the closed Session as the strong retry owner whenever
                // the terminal manager could not confirm process-tree cleanup.
                Ok(())
            };
            Self::preserve_primary_error(
                Self::preserve_primary_error(
                    Self::preserve_primary_error(
                        Self::preserve_primary_error(
                            mcp_result,
                            terminal_result,
                            "clean terminals during Session close",
                        ),
                        sync_result,
                        "sync Store during Session close",
                    ),
                    lifecycle_result,
                    "finish Session lifecycle during close",
                ),
                removal_result,
                "remove Session runtime after close",
            )
        }
        .await;
        match result {
            Ok(()) => {
                tracing::info!("completed Session close {}", session_id);
                Ok(())
            }
            Err(error) => {
                tracing::error!(
                    "failed Session close {}: {}",
                    session_id,
                    error
                );
                Err(error)
            }
        }
    }

    /// Releases an active runtime before removing its persisted session history idempotently.
    pub async fn delete_session(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        tracing::info!("started Session deletion {}", session_id);
        let result: Result<(), KernelError> = async {
            // Reserve before closing so construction cannot claim the identifier
            // between runtime removal and persisted Store deletion.
            let _reservation = self.reserve_session_deletion(session_id)?;
            match self.close_session(session_id).await {
                Ok(()) | Err(KernelError::SessionNotFound(_)) => {}
                Err(error) => return Err(error),
            }
            self.store_factory.delete(session_id)?;
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                tracing::info!("completed Session deletion {}", session_id);
                Ok(())
            }
            Err(error) => {
                tracing::error!(
                    "failed Session deletion {}: {}",
                    session_id,
                    error
                );
                Err(error)
            }
        }
    }

    /// Cancels the active run for a session; the next run receives a fresh token.
    pub fn cancel_session(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        let session = self.session(session_id)?;
        session.execution.cancel()
    }

    /// Returns the compaction-aware active model context for runtime inspection.
    pub fn session_messages(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<AgentMessage>, KernelError> {
        self.session(session_id)?.transcript.history()
    }

    /// Returns all persisted entries and the active main-lane cursor.
    pub fn session_tree(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionTreeSnapshot, KernelError> {
        self.session(session_id)?.transcript.tree_snapshot()
    }

    /// Persists a normalized manual session title without modifying transcript entries.
    pub async fn rename_session(
        &self,
        session_id: &SessionId,
        title: SessionTitle,
    ) -> Result<(), KernelError> {
        let session = self.session(session_id)?;
        session
            .transcript
            .set_name(Some(title.as_str().to_string()))?;
        session.sync_store()?;
        let context = self.extension_context(&session, None, None)?;
        session
            .extensions
            .runtime_ref()
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
            .runtime_ref()
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
        let _reservation = self.reserve_session_build(&new_session_id)?;
        let source_path = source.transcript.path()?;
        let store = self.store_factory.fork(
            &source_path,
            SessionForkOptions {
                create: SessionCreateOptions {
                    session_id: new_session_id.clone(),
                    cwd: cwd.clone(),
                    parent_session_id: Some(source_session_id.clone()),
                },
                source_leaf,
                lane: source.transcript.lane(),
            },
        )?;
        let path = store.path().to_path_buf();
        let runtime = self.build_session(store, cwd).await?;
        let _registration = self
            .register_session(new_session_id.clone(), &runtime)
            .await?;
        if let Err(error) = self
            .start_extensions(&runtime, protocol::SessionStartReason::Fork)
            .await
        {
            if let Err(rollback_error) = self
                .rollback_session_registration(&new_session_id, true)
                .await
            {
                tracing::warn!(
                    "failed to clean up forked Session {} after startup error: {}",
                    new_session_id,
                    rollback_error
                );
            }
            return Err(error);
        }
        self.checkpoint_started_session(&new_session_id, &runtime, true)
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
        session: &Arc<Session>,
        reason: protocol::SessionStartReason,
    ) -> Result<(), KernelError> {
        let session_id = session.transcript.session_id()?;
        tracing::debug!(
            "started Session extension startup {} with reason {:?}",
            session_id,
            reason
        );
        let result: Result<(), KernelError> = async {
            let context = self.extension_context(session, None, None)?;
            let trust = session
                .extensions
                .runtime_ref()
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
                .runtime_ref()
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
                .runtime_ref()
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
            let prompt = self.prompt_factory.create(
                protocol::PromptResourceRequest {
                    cwd: session.cwd.clone(),
                    project_resources_allowed,
                    extension_prompt_paths: resources.prompt_paths,
                },
            )?;
            // Publish Prompt and Skill resources together so readers cannot observe
            // a partially initialized startup snapshot.
            session
                .resources
                .set(SessionResources::new(Arc::new(prompt), skills))
                .map_err(|_resources| {
                    KernelError::Protocol(
                        "session resources already initialized".to_string(),
                    )
                })?;
            let (restored_model, restored_thinking_level) =
                session.model.snapshot()?;
            let restored_model = restored_model.profile().clone();
            session
                .extensions
                .runtime_ref()
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
                .runtime_ref()
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
        .await;
        match result {
            Ok(()) => {
                tracing::debug!(
                    "completed Session extension startup {} with reason {:?}",
                    session_id,
                    reason
                );
                Ok(())
            }
            Err(error) => {
                tracing::error!(
                    "failed Session extension startup {} with reason {:?}: {}",
                    session_id,
                    reason,
                    error
                );
                Err(error)
            }
        }
    }

    /// Builds session-scoped tools and reconstructs model history from the active main branch.
    async fn build_session(
        &self,
        store: Box<dyn SessionStore>,
        cwd: PathBuf,
    ) -> Result<Arc<Session>, KernelError> {
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
            tracing::info!(
                "started MCP Session creation for session {}",
                session_id
            );
            let mcp_session = match factory
                .create(
                    ::mcp::McpSessionRequest::builder()
                        .session_id(session_id.clone())
                        .cwd(cwd.clone())
                        .host(Arc::clone(&host) as Arc<dyn ::mcp::McpHost>)
                        .shutdown(self.shutdown.child_token())
                        .build(),
                )
                .await
            {
                Ok(mcp_session) => {
                    tracing::info!(
                        "completed MCP Session creation for session {}",
                        session_id
                    );
                    Arc::new(mcp_session)
                }
                Err(error) => {
                    tracing::error!(
                        "failed MCP Session creation for session {}: {}",
                        session_id,
                        error
                    );
                    return Err(error.into());
                }
            };
            let mcp_tools = match mcp_session.tool_registry() {
                Ok(tools) => tools,
                Err(error) => {
                    // MCP was already created, so a catalog failure must not
                    // leave its background transports alive.
                    if let Err(shutdown_error) = mcp_session.shutdown().await {
                        tracing::warn!(
                            "failed to shut down MCP after catalog construction failed: {}",
                            shutdown_error
                        );
                    }
                    return Err(error.into());
                }
            };
            (Some(mcp_session), mcp_tools, Some(host))
        } else {
            (None, ToolRegistry::default(), None)
        };
        let cleanup_mcp = mcp.as_ref().map(Arc::clone);
        let result: Result<Arc<Session>, KernelError> = async {
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
            // Resume starts above both shared Store mutations and the extra
            // Usage events synthesized from persisted Assistant messages.
            let initial_event_sequence =
                Self::replay_sequence_floor(store.as_ref(), &lane)?;
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
            let tools = Arc::new(SessionToolState::new(
                builtins,
                extension_tools,
                mcp_tools,
                settings.active_tools,
            )?);
            let execution = Arc::new(
                SessionExecution::builder()
                    .lifecycle(RwLock::new(SessionLifecycle::Starting))
                    .gate(AsyncMutex::new(()))
                    .active_run(Mutex::new(None))
                    .idle_notify(Notify::new())
                    .cancellation(Mutex::new(CancellationToken::new()))
                    .queue(Mutex::new(queue))
                    .sequence(AtomicU64::new(initial_event_sequence))
                    .build(),
            );
            // Diagnostics share the transcript component without retaining the
            // Session and creating a reference cycle through ExtensionRuntime.
            let transcript =
                Arc::new(SessionTranscript::new(store, lane.clone(), history));
            let extension_runtime = Arc::new(ExtensionRuntime::new(
                Arc::clone(&extension_registry),
                Arc::new(
                    KernelExtensionDiagnostics::builder()
                        .clock(Arc::clone(&self.clock))
                        .execution(Arc::clone(&execution))
                        .transcript(Arc::clone(&transcript))
                        .id_generator(Arc::clone(&self.id_generator))
                        .build(),
                ),
            ));
            let extensions = SessionExtensions::builder()
                .runtime(extension_runtime)
                .commands(commands)
                .flags(Arc::from(flags))
                .events(broadcast::channel(64).0)
                .build();
            let runtime = Arc::new(
                Session::builder()
                    .cwd(cwd)
                    .transcript(transcript)
                    .mcp(SessionMcpRuntime::new(mcp))
                    .terminals(Arc::new(TerminalManager::new(
                        session_id.clone(),
                    )))
                    .resources(OnceLock::new())
                    .extensions(extensions)
                    .tools(tools)
                    .model(SessionModelState::new(
                        Arc::clone(&self.models),
                        settings.model,
                        settings.thinking_level,
                    ))
                    .execution(execution)
                    .build(),
            );
            if let Some(host) = mcp_host
                && let Err(error) = host.attach(&runtime)
            {
                runtime.extensions.runtime_ref().invalidate();
                return Err(error);
            }
            if let Err(error) = runtime.start_mcp_projection().await {
                runtime.extensions.runtime_ref().invalidate();
                let _ = runtime.mcp.shutdown().await;
                return Err(error);
            }
            Ok(runtime)
        }
        .await;
        if result.is_err()
            && let Some(mcp) = cleanup_mcp
            && let Err(error) = mcp.shutdown().await
        {
            // Preserve the original construction error while making cleanup
            // failure observable for operators.
            tracing::warn!(
                "failed to shut down MCP after Session construction failed: {}",
                error
            );
        }
        result
    }

    /// Registers a constructed runtime or releases its unregistered resources on failure.
    async fn register_session(
        &self,
        session_id: SessionId,
        runtime: &Arc<Session>,
    ) -> Result<tokio::sync::OwnedRwLockReadGuard<()>, KernelError> {
        // Keep shutdown behind this runtime until startup hooks and the durable
        // checkpoint finish at the caller's registration-guard boundary.
        let startup_guard =
            Arc::clone(&self.session_startups).read_owned().await;
        let registration = (|| {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_poison_error| KernelError::Poisoned)?;
            // Recheck while holding the map lock that shutdown snapshots. This
            // orders cancellation against publication without a TOCTOU window.
            if self.shutdown.is_cancelled() {
                return Err(KernelError::ShuttingDown);
            }
            if sessions.contains_key(&session_id) {
                return Err(KernelError::DuplicateSession(session_id.clone()));
            }
            sessions.insert(session_id.clone(), Arc::clone(runtime));
            Ok(())
        })();
        if let Err(error) = registration {
            if let Err(cleanup_error) =
                Self::release_unregistered_session(runtime).await
            {
                tracing::warn!(
                    "failed to release unregistered Session {} after registration error: {}",
                    session_id,
                    cleanup_error
                );
            }
            return Err(error);
        }
        Ok(startup_guard)
    }

    /// Reserves one identifier across Store opening, runtime construction, and startup hooks.
    fn reserve_session_build(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionIdReservation, KernelError> {
        if self.shutdown.is_cancelled() {
            return Err(KernelError::ShuttingDown);
        }
        // Hold the map read lock until the pending id is inserted so registration
        // and reservation cannot both observe the identifier as absent.
        let sessions = self
            .sessions
            .read()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        if sessions.contains_key(session_id) {
            return Err(KernelError::DuplicateSession(session_id.clone()));
        }
        let mut pending_sessions = self
            .pending_sessions
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        if !pending_sessions.insert(session_id.clone()) {
            return Err(KernelError::DuplicateSession(session_id.clone()));
        }
        Ok(SessionIdReservation {
            session_id: session_id.clone(),
            pending_sessions: Arc::clone(&self.pending_sessions),
        })
    }

    /// Reserves one identifier across runtime shutdown and persisted Store deletion.
    fn reserve_session_deletion(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionIdReservation, KernelError> {
        // Match construction's lock order and keep the map stable until the id
        // is reserved, closing both absent-runtime races around registration.
        let _sessions = self
            .sessions
            .read()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        let mut pending_sessions = self
            .pending_sessions
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        if !pending_sessions.insert(session_id.clone()) {
            return Err(KernelError::DuplicateSession(session_id.clone()));
        }
        Ok(SessionIdReservation {
            session_id: session_id.clone(),
            pending_sessions: Arc::clone(&self.pending_sessions),
        })
    }

    /// Invalidates and shuts down a runtime that never entered the Session map.
    async fn release_unregistered_session(
        runtime: &Arc<Session>,
    ) -> Result<(), KernelError> {
        runtime.extensions.runtime_ref().invalidate();
        let mcp_result = runtime.mcp.shutdown().await;
        let terminal_result = runtime
            .terminals
            .shutdown(protocol::TerminalRemovalReason::SessionClosed)
            .await
            .map(|_count| ())
            .map_err(KernelError::from);
        Self::preserve_primary_error(
            mcp_result,
            terminal_result,
            "clean terminals for unregistered Session",
        )
    }

    /// Keeps the first lifecycle error while logging a later cleanup failure.
    fn preserve_primary_error(
        primary: Result<(), KernelError>,
        secondary: Result<(), KernelError>,
        secondary_action: &str,
    ) -> Result<(), KernelError> {
        match (primary, secondary) {
            (Ok(()), result) => result,
            (Err(primary_error), Ok(())) => Err(primary_error),
            (Err(primary_error), Err(secondary_error)) => {
                tracing::warn!(
                    "failed to {} after an earlier lifecycle error: {}",
                    secondary_action,
                    secondary_error
                );
                Err(primary_error)
            }
        }
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
            runtime.extensions.runtime_ref().invalidate();
            let mcp_result = runtime.mcp.shutdown().await;
            let terminal_result = runtime
                .terminals
                .shutdown(protocol::TerminalRemovalReason::SessionClosed)
                .await
                .map(|_count| ())
                .map_err(KernelError::from);
            let delete_result = if delete_persisted {
                self.store_factory
                    .delete(session_id)
                    .map_err(KernelError::from)
            } else {
                Ok(())
            };
            return Self::preserve_primary_error(
                Self::preserve_primary_error(
                    mcp_result,
                    terminal_result,
                    "clean terminals during registration rollback",
                ),
                delete_result,
                "delete persisted Session during registration rollback",
            );
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
        runtime: &Arc<Session>,
        delete_persisted: bool,
    ) -> Result<(), KernelError> {
        // Activation follows the durable startup checkpoint so no public
        // operation can observe resources that may still be rolled back.
        let checkpoint = runtime
            .sync_store()
            .and_then(|()| runtime.finish_starting());
        if let Err(error) = checkpoint {
            tracing::error!(
                "failed to checkpoint started session {}; rolling back registration: {}",
                session_id,
                error
            );
            if let Err(rollback_error) = self
                .rollback_session_registration(session_id, delete_persisted)
                .await
            {
                tracing::warn!(
                    "failed to clean up Session {} after checkpoint error: {}",
                    session_id,
                    rollback_error
                );
            }
            return Err(error);
        }
        Ok(())
    }

    /// Resolves one registered session without holding the map lock afterward.
    pub(super) fn session(
        &self,
        session_id: &SessionId,
    ) -> Result<Arc<Session>, KernelError> {
        let session = self
            .sessions
            .read()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .get(session_id)
            .map(Arc::clone)
            .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))?;
        // Registered startup entries exist for Extension context construction,
        // but ordinary kernel callers may use only fully initialized Sessions.
        session.ensure_active()?;
        Ok(session)
    }

    /// Persists one message and then exposes it to subsequent model contexts.
    pub(super) fn persist_message(
        &self,
        session: &Session,
        message: &AgentMessage,
    ) -> Result<(), KernelError> {
        let entry_id = EntryId::try_from(self.id_generator.next(IdKind::Entry))
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        self.persist_message_at(session, message, entry_id)
    }

    /// Persists one message at an identifier reserved by queue acceptance.
    pub(super) fn persist_message_at(
        &self,
        session: &Session,
        message: &AgentMessage,
        entry_id: EntryId,
    ) -> Result<(), KernelError> {
        session.transcript.append_context_message(entry_id, message)
    }

    /// Sets the first non-empty user line as the default title exactly once.
    pub(super) fn ensure_default_title(
        &self,
        session: &Session,
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
        session.transcript.set_default_name(title)
    }

    /// Appends one durable pi v4 lane record for run recovery and inspection.
    pub(super) fn record_operation(
        &self,
        session: &Session,
        run_id: &RunId,
        kind: RecordKind,
        payload: serde_json::Value,
    ) -> Result<(), KernelError> {
        let record_id =
            RecordId::try_from(self.id_generator.next(IdKind::Record))
                .map_err(|error| KernelError::Protocol(error.to_string()))?;
        session.transcript.append_record(
            NewRecord::builder()
                .id(record_id)
                .lane(session.transcript.lane())
                .run_id(Some(run_id.clone()))
                .kind(kind)
                .payload(payload)
                .build(),
        )?;
        Ok(())
    }
}
