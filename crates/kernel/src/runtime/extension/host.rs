use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use extension::{ExtensionHost, ExtensionHostError, RegisteredCommand};
use protocol::{
    EntryId, ExtensionEntryData, ExtensionEventData, ExtensionExecRequest,
    ExtensionExecResult, ExtensionFlagDefinition, ExtensionInvocation,
    ExtensionMessageDraft, ExtensionSnapshot, ExtensionUserMessage, IdKind,
    ModelProfile, SessionId, SessionSummary, SessionTreeSnapshot,
    ThinkingLevel, TurnId,
};
use store::{EntryKind, NewEntry};

use super::super::SessionRuntime;
use super::context::ExtensionMessageMaterializer;

/// Event sink used when an out-of-band extension command has no ACP stream.
struct DiscardEventSink;

#[async_trait]
impl crate::EventSink for DiscardEventSink {
    /// Accepts lifecycle events while persistence remains the source of truth.
    async fn emit(
        &self,
        _event: protocol::AgentEvent,
    ) -> Result<(), crate::SinkError> {
        Ok(())
    }
}

/// Session-local implementation of extension actions that do not replace the runtime.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct SessionExtensionHost {
    session: Arc<SessionRuntime>,
    kernel: super::super::Kernel,
    clock: Arc<dyn store::Clock>,
    id_generator: Arc<dyn protocol::IdGenerator>,
}

impl SessionExtensionHost {
    /// Converts one generated identifier into a sanitized host-operation error.
    fn entry_id(&self) -> Result<EntryId, ExtensionHostError> {
        EntryId::try_from(self.id_generator.next(IdKind::Entry))
            .map_err(|error| ExtensionHostError::Operation(error.to_string()))
    }

    /// Creates the shared message materializer for this session host.
    fn message_materializer(&self) -> ExtensionMessageMaterializer {
        ExtensionMessageMaterializer::new(
            Arc::clone(&self.clock),
            Arc::clone(&self.id_generator),
        )
    }

    /// Appends one durable session-setting entry before publishing in-memory state.
    fn persist_setting(
        &self,
        kind: EntryKind,
        payload: serde_json::Value,
    ) -> Result<(), ExtensionHostError> {
        self.session
            .store
            .lock()
            .map_err(|_poison_error| {
                ExtensionHostError::Operation(
                    "session store lock poisoned".to_string(),
                )
            })?
            .append_entry(
                &self.session.lane,
                NewEntry {
                    id: self.entry_id()?,
                    kind,
                    payload,
                },
            )
            .map_err(|error| {
                ExtensionHostError::Operation(error.to_string())
            })?;
        Ok(())
    }

    /// Emits the complete command snapshot when this Session has a live client sink.
    async fn publish_available_commands(
        &self,
        session_id: &SessionId,
    ) -> Result<(), ExtensionHostError> {
        let sink = self
            .session
            .event_sink
            .lock()
            .map_err(|_poison_error| {
                ExtensionHostError::Operation(
                    "event sink lock poisoned".to_string(),
                )
            })?
            .as_ref()
            .map(Arc::clone);
        if let Some(sink) = sink {
            let event =
                self.kernel.available_commands_event(session_id).map_err(
                    |error| ExtensionHostError::Operation(error.to_string()),
                )?;
            sink.emit(event).await.map_err(|error| {
                ExtensionHostError::Operation(error.to_string())
            })?;
        }
        Ok(())
    }
}

#[async_trait]
impl ExtensionHost for SessionExtensionHost {
    /// Captures current state from the owning session runtime.
    fn snapshot(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<ExtensionSnapshot, ExtensionHostError> {
        self.session
            .extension_snapshot()
            .map_err(|error| ExtensionHostError::Operation(error.to_string()))
    }

    /// Reports whether no run currently owns this session.
    fn is_idle(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<bool, ExtensionHostError> {
        self.session
            .active_run_id
            .lock()
            .map(|run_id| run_id.is_none())
            .map_err(|_poison_error| {
                ExtensionHostError::Operation(
                    "active run lock poisoned".to_string(),
                )
            })
    }

    /// Reports whether steering or follow-up messages are queued.
    fn has_pending_messages(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<bool, ExtensionHostError> {
        self.session
            .queue
            .lock()
            .map(|queue| !queue.ids().is_empty())
            .map_err(|_poison_error| {
                ExtensionHostError::Operation("queue lock poisoned".to_string())
            })
    }

    /// Cancels active work without waiting while a handler is executing.
    async fn abort(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<(), ExtensionHostError> {
        self.session
            .cancellation
            .lock()
            .map_err(|_poison_error| {
                ExtensionHostError::Operation(
                    "cancellation lock poisoned".to_string(),
                )
            })?
            .cancel();
        Ok(())
    }

    /// Requests graceful application shutdown through the shared Kernel token.
    async fn shutdown(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<(), ExtensionHostError> {
        self.kernel.shutdown.cancel();
        Ok(())
    }

    /// Compacts an idle session without inventing an ACP client callback.
    async fn compact(
        &self,
        invocation: &ExtensionInvocation,
    ) -> Result<protocol::CompactionResult, ExtensionHostError> {
        self.session.ensure_extension_operation()?;
        if !self.is_idle(invocation)? {
            return Err(ExtensionHostError::Operation(
                "cannot compact while the session is running".to_string(),
            ));
        }
        self.kernel
            .compact_session(&invocation.session_id, Arc::new(DiscardEventSink))
            .await
            .map_err(|error| ExtensionHostError::Operation(error.to_string()))
    }

    /// Builds the current complete system prompt without mutating history.
    fn system_prompt(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<String, ExtensionHostError> {
        let tools = self
            .session
            .tool_state
            .snapshot()
            .and_then(|tools| tools.active_registry())
            .map_err(|error| {
                ExtensionHostError::Operation(error.to_string())
            })?;
        self.session
            .build_system_prompt(&tools, self.kernel.include_skill_instructions)
            .map(|prompt| prompt.text)
            .map_err(|error| ExtensionHostError::Operation(error.to_string()))
    }

    /// Persists and emits one complete extension message.
    async fn send_extension_message(
        &self,
        invocation: &ExtensionInvocation,
        draft: ExtensionMessageDraft,
    ) -> Result<protocol::AgentMessage, ExtensionHostError> {
        let turn_id = invocation
            .turn_id
            .clone()
            .unwrap_or_else(|| TurnId::system(&invocation.session_id));
        let message = self.message_materializer().materialize(
            turn_id,
            Some(&invocation.extension_id),
            draft,
        )?;
        let entry_id = self.entry_id()?;
        self.session
            .store
            .lock()
            .map_err(|_poison_error| {
                ExtensionHostError::Operation("store lock poisoned".to_string())
            })?
            .append_entry(
                &self.session.lane,
                NewEntry {
                    id: entry_id,
                    kind: EntryKind::Message,
                    payload: serde_json::to_value(&message).map_err(
                        |error| {
                            ExtensionHostError::Operation(error.to_string())
                        },
                    )?,
                },
            )
            .map_err(|error| {
                ExtensionHostError::Operation(error.to_string())
            })?;
        self.session
            .history
            .lock()
            .map_err(|_poison_error| {
                ExtensionHostError::Operation(
                    "history lock poisoned".to_string(),
                )
            })?
            .push(message.clone());
        let sink = self
            .session
            .event_sink
            .lock()
            .map_err(|_poison_error| {
                ExtensionHostError::Operation(
                    "event sink lock poisoned".to_string(),
                )
            })?
            .as_ref()
            .map(Arc::clone);
        if let Some(sink) = sink {
            let sequence = self
                .session
                .event_sequence
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                .checked_add(1)
                .ok_or_else(|| {
                    ExtensionHostError::Operation(
                        "event sequence overflow".to_string(),
                    )
                })?;
            sink.emit(protocol::AgentEvent {
                metadata: protocol::EventMetadata {
                    turn_id: message.identity.turn_id.clone(),
                    timestamp_ms: self.clock.now(),
                    sequence: protocol::Sequence::try_from(sequence).map_err(
                        |error| {
                            ExtensionHostError::Operation(error.to_string())
                        },
                    )?,
                },
                payload: protocol::AgentEventPayload::MessageEnd {
                    message: message.clone(),
                },
            })
            .await
            .map_err(|error| {
                ExtensionHostError::Operation(error.to_string())
            })?;
        }
        Ok(message)
    }

    /// Routes extension-authored input through the normal idle or queued path.
    async fn send_user_message(
        &self,
        invocation: &ExtensionInvocation,
        request: ExtensionUserMessage,
    ) -> Result<(), ExtensionHostError> {
        self.session.ensure_extension_operation()?;
        if self.is_idle(invocation)? {
            self.kernel
                .run_with_source(
                    protocol::RunRequest {
                        session_id: invocation.session_id.clone(),
                        input: request.text,
                    },
                    Arc::new(DiscardEventSink),
                    protocol::InputSource::Extension,
                    protocol::TraceId::try_from(
                        self.kernel.id_generator.next(protocol::IdKind::Trace),
                    )
                    .map_err(|error| {
                        ExtensionHostError::Operation(error.to_string())
                    })?,
                )
                .await
                .map_err(|error| {
                    ExtensionHostError::Operation(error.to_string())
                })?;
            return Ok(());
        }

        let context = self
            .kernel
            .extension_context(
                &self.session,
                invocation.run_id.as_ref(),
                invocation.turn_id.as_ref(),
            )
            .map_err(|error| {
                ExtensionHostError::Operation(error.to_string())
            })?;
        let delivery = request.delivery;
        let original_text = request.text;
        let input = self
            .session
            .extensions
            .emit_input(
                protocol::InputEvent {
                    text: original_text.clone(),
                    source: protocol::InputSource::Extension,
                    streaming_behavior: Some(match delivery {
                        protocol::QueueKind::Steering => {
                            protocol::InputStreamingBehavior::Steer
                        }
                        protocol::QueueKind::FollowUp => {
                            protocol::InputStreamingBehavior::FollowUp
                        }
                    }),
                },
                &context,
            )
            .await;
        let text = match input {
            protocol::InputResult::Continue => original_text,
            protocol::InputResult::Transform { text } => text,
            protocol::InputResult::Handled => return Ok(()),
        };
        self.kernel
            .queue_message(&invocation.session_id, delivery, text)
            .await
            .map_err(|error| {
                ExtensionHostError::Operation(error.to_string())
            })?;
        Ok(())
    }

    /// Appends one typed extension-owned custom entry.
    async fn append_entry(
        &self,
        invocation: &ExtensionInvocation,
        entry: ExtensionEntryData,
    ) -> Result<EntryId, ExtensionHostError> {
        if entry.extension_id != invocation.extension_id {
            return Err(ExtensionHostError::Operation(
                "extension entry identity does not match handler".to_string(),
            ));
        }
        let entry_id = self.entry_id()?;
        self.session
            .store
            .lock()
            .map_err(|_poison_error| {
                ExtensionHostError::Operation("store lock poisoned".to_string())
            })?
            .append_extension_entry(&self.session.lane, entry_id.clone(), entry)
            .map_err(|error| {
                ExtensionHostError::Operation(error.to_string())
            })?;
        Ok(entry_id)
    }

    /// Sets or clears the persisted session name.
    async fn set_session_name(
        &self,
        invocation: &ExtensionInvocation,
        name: Option<String>,
    ) -> Result<(), ExtensionHostError> {
        self.session
            .store
            .lock()
            .map_err(|_poison_error| {
                ExtensionHostError::Operation("store lock poisoned".to_string())
            })?
            .set_name(name.clone())
            .map_err(|error| {
                ExtensionHostError::Operation(error.to_string())
            })?;
        let context = self
            .kernel
            .extension_context(
                &self.session,
                invocation.run_id.as_ref(),
                invocation.turn_id.as_ref(),
            )
            .map_err(|error| {
                ExtensionHostError::Operation(error.to_string())
            })?;
        self.session
            .extensions
            .emit_session_info_changed(
                &protocol::SessionInfoChangedEvent { name },
                &context,
            )
            .await;
        Ok(())
    }

    /// Sets or clears one persisted entry label.
    async fn set_label(
        &self,
        _invocation: &ExtensionInvocation,
        entry_id: EntryId,
        label: Option<String>,
    ) -> Result<(), ExtensionHostError> {
        self.session
            .store
            .lock()
            .map_err(|_poison_error| {
                ExtensionHostError::Operation("store lock poisoned".to_string())
            })?
            .set_label(entry_id, label)
            .map_err(|error| ExtensionHostError::Operation(error.to_string()))
    }

    /// Executes one server-local process with timeout and active-run cancellation.
    async fn exec(
        &self,
        _invocation: &ExtensionInvocation,
        request: ExtensionExecRequest,
    ) -> Result<ExtensionExecResult, ExtensionHostError> {
        let mut command = tokio::process::Command::new(&request.command);
        command
            .args(&request.arguments)
            .envs(&request.environment)
            .current_dir(
                request.cwd.unwrap_or_else(|| self.session.cwd.clone()),
            )
            .kill_on_drop(true);
        let cancellation = self
            .session
            .cancellation
            .lock()
            .map_err(|_poison_error| {
                ExtensionHostError::Operation(
                    "cancellation lock poisoned".to_string(),
                )
            })?
            .clone();
        let output = match request.timeout_ms {
            Some(timeout_ms) => tokio::select! {
                () = cancellation.cancelled() => {
                    return Err(ExtensionHostError::Operation(
                        "process execution cancelled".to_string(),
                    ));
                }
                output = tokio::time::timeout(
                    std::time::Duration::from_millis(timeout_ms),
                    command.output(),
                ) => output
                    .map_err(|_elapsed| ExtensionHostError::Operation(
                        "process execution timed out".to_string(),
                    ))?
                    .map_err(|error| ExtensionHostError::Operation(error.to_string()))?,
            },
            None => tokio::select! {
                () = cancellation.cancelled() => {
                    return Err(ExtensionHostError::Operation(
                        "process execution cancelled".to_string(),
                    ));
                }
                output = command.output() => output
                    .map_err(|error| ExtensionHostError::Operation(error.to_string()))?,
            },
        };
        Ok(ExtensionExecResult {
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// Returns names active for future turns.
    fn active_tools(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<Vec<String>, ExtensionHostError> {
        self.session
            .tool_state
            .snapshot()
            .map(|tools| tools.active_names())
            .map_err(|error| ExtensionHostError::Operation(error.to_string()))
    }

    /// Returns every tool name available to this session.
    fn all_tools(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<Vec<String>, ExtensionHostError> {
        self.session
            .tool_state
            .snapshot()
            .map(|tools| tools.available_names())
            .map_err(|error| ExtensionHostError::Operation(error.to_string()))
    }

    /// Replaces active names after validating one available-tool snapshot.
    async fn set_active_tools(
        &self,
        _invocation: &ExtensionInvocation,
        names: Vec<String>,
    ) -> Result<(), ExtensionHostError> {
        let available =
            self.session.tool_state.snapshot().map_err(|error| {
                ExtensionHostError::Operation(error.to_string())
            })?;
        // Persist only after validation so recovery never observes an invalid set.
        if let Err(error) = available.validate_active(&names) {
            return match error {
                tools::ToolError::NotFound(name) => {
                    Err(ExtensionHostError::UnknownTool(name))
                }
                error => Err(ExtensionHostError::Operation(error.to_string())),
            };
        }
        self.persist_setting(
            EntryKind::ActiveToolsChange,
            serde_json::json!({ "activeTools": names }),
        )?;
        self.session
            .tool_state
            .set_active(names)
            .map_err(|error| ExtensionHostError::Operation(error.to_string()))
    }

    /// Registers or replaces one tool for future Turn snapshots.
    async fn register_tool(
        &self,
        _invocation: &ExtensionInvocation,
        tool: Arc<dyn tools::AgentTool>,
    ) -> Result<(), ExtensionHostError> {
        self.session
            .tool_state
            .register(tool)
            .map_err(|error| ExtensionHostError::Operation(error.to_string()))
    }

    /// Returns one immutable command snapshot.
    fn commands(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<Vec<RegisteredCommand>, ExtensionHostError> {
        self.session
            .commands
            .snapshot()
            .map(|commands| commands.commands())
            .map_err(|error| ExtensionHostError::Operation(error.to_string()))
    }

    /// Returns immutable static extension flags.
    fn flags(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<Vec<ExtensionFlagDefinition>, ExtensionHostError> {
        Ok(self.session.flags.to_vec())
    }

    /// Registers or replaces one session-local command.
    async fn register_command(
        &self,
        invocation: &ExtensionInvocation,
        command: RegisteredCommand,
    ) -> Result<(), ExtensionHostError> {
        // The host repeats ownership binding so direct trait callers cannot
        // replace commands belonging to another extension identity.
        let command = command.bind_owner(&invocation.extension_id);
        self.session.commands.upsert(command).map_err(|error| {
            ExtensionHostError::Operation(error.to_string())
        })?;
        self.publish_available_commands(&invocation.session_id)
            .await
    }

    /// Removes one caller-owned command and publishes the replacement snapshot.
    async fn unregister_command(
        &self,
        invocation: &ExtensionInvocation,
        name: &str,
    ) -> Result<(), ExtensionHostError> {
        let removed = self
            .session
            .commands
            .remove(&invocation.extension_id, name)
            .map_err(|error| {
                ExtensionHostError::Operation(error.to_string())
            })?;
        if removed {
            self.publish_available_commands(&invocation.session_id)
                .await?;
        }
        Ok(())
    }

    /// Persists and selects one configured model for future Turn snapshots.
    async fn set_model(
        &self,
        invocation: &ExtensionInvocation,
        provider_id: &str,
        model_id: &str,
    ) -> Result<ModelProfile, ExtensionHostError> {
        let model = self
            .session
            .models
            .resolve(provider_id, model_id)
            .ok_or_else(|| {
                ExtensionHostError::Operation(format!(
                    "model is not configured: {provider_id}/{model_id}"
                ))
            })?;
        let previous_model = self
            .session
            .model
            .read()
            .map_err(|_poison_error| {
                ExtensionHostError::Operation("model lock poisoned".to_string())
            })?
            .profile()
            .clone();
        if previous_model == *model.profile() {
            return Ok(previous_model);
        }
        self.persist_setting(
            EntryKind::ModelChange,
            serde_json::json!({
                "provider": provider_id,
                "modelId": model_id,
            }),
        )?;
        *self.session.model.write().map_err(|_poison_error| {
            ExtensionHostError::Operation("model lock poisoned".to_string())
        })? = Arc::clone(&model);
        let context = self
            .kernel
            .extension_context(
                &self.session,
                invocation.run_id.as_ref(),
                invocation.turn_id.as_ref(),
            )
            .map_err(|error| {
                ExtensionHostError::Operation(error.to_string())
            })?;
        self.session
            .extensions
            .emit_model_select(
                &protocol::ModelSelectEvent {
                    model: model.profile().clone(),
                    previous_model: Some(previous_model),
                    source: protocol::ModelSelectSource::Set,
                },
                &context,
            )
            .await;
        Ok(model.profile().clone())
    }

    /// Returns the current session thinking level.
    fn thinking_level(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<ThinkingLevel, ExtensionHostError> {
        self.session
            .thinking_level
            .read()
            .map(|level| *level)
            .map_err(|_poison_error| {
                ExtensionHostError::Operation(
                    "thinking lock poisoned".to_string(),
                )
            })
    }

    /// Persists the selected thinking level and notifies model observers.
    async fn set_thinking_level(
        &self,
        invocation: &ExtensionInvocation,
        level: ThinkingLevel,
    ) -> Result<ThinkingLevel, ExtensionHostError> {
        let previous_level =
            *self
                .session
                .thinking_level
                .read()
                .map_err(|_poison_error| {
                    ExtensionHostError::Operation(
                        "thinking lock poisoned".to_string(),
                    )
                })?;
        if previous_level == level {
            return Ok(level);
        }
        self.persist_setting(
            EntryKind::ThinkingLevelChange,
            serde_json::json!({ "thinkingLevel": level }),
        )?;
        *self
            .session
            .thinking_level
            .write()
            .map_err(|_poison_error| {
                ExtensionHostError::Operation(
                    "thinking lock poisoned".to_string(),
                )
            })? = level;
        let context = self
            .kernel
            .extension_context(
                &self.session,
                invocation.run_id.as_ref(),
                invocation.turn_id.as_ref(),
            )
            .map_err(|error| {
                ExtensionHostError::Operation(error.to_string())
            })?;
        self.session
            .extensions
            .emit_thinking_level_select(
                &protocol::ThinkingLevelSelectEvent {
                    level,
                    previous_level,
                },
                &context,
            )
            .await;
        Ok(level)
    }

    /// Publishes one value to the session-local extension event log.
    async fn publish_event(
        &self,
        _invocation: &ExtensionInvocation,
        event: ExtensionEventData,
    ) -> Result<(), ExtensionHostError> {
        let _receivers = self.session.event_bus.send(event);
        Ok(())
    }

    /// Subscribes to future session-local extension event values.
    fn subscribe_events(
        &self,
        _invocation: &ExtensionInvocation,
    ) -> Result<
        tokio::sync::broadcast::Receiver<ExtensionEventData>,
        ExtensionHostError,
    > {
        Ok(self.session.event_bus.subscribe())
    }

    /// Waits until the current active run has released the session.
    async fn wait_for_idle(
        &self,
        invocation: &ExtensionInvocation,
    ) -> Result<(), ExtensionHostError> {
        loop {
            let notified = self.session.idle_notify.notified();
            if self.is_idle(invocation)? {
                return Ok(());
            }
            notified.await;
        }
    }

    /// Creates and registers one new server-side session.
    async fn create_session(
        &self,
        _invocation: &ExtensionInvocation,
        cwd: PathBuf,
    ) -> Result<SessionSummary, ExtensionHostError> {
        let session_id =
            self.kernel.create_generated_session(cwd).await.map_err(
                |error| ExtensionHostError::Operation(error.to_string()),
            )?;
        self.kernel
            .list_sessions(None)
            .map_err(|error| ExtensionHostError::Operation(error.to_string()))?
            .into_iter()
            .find(|summary| summary.session_id == session_id)
            .ok_or_else(|| {
                ExtensionHostError::Operation(
                    "created session metadata is unavailable".to_string(),
                )
            })
    }

    /// Restores one persisted session and returns its metadata snapshot.
    async fn switch_session(
        &self,
        _invocation: &ExtensionInvocation,
        session_id: SessionId,
    ) -> Result<SessionSummary, ExtensionHostError> {
        let summary = self
            .kernel
            .list_sessions(None)
            .map_err(|error| ExtensionHostError::Operation(error.to_string()))?
            .into_iter()
            .find(|summary| summary.session_id == session_id)
            .ok_or_else(|| {
                ExtensionHostError::Operation(format!(
                    "session not found: {session_id}"
                ))
            })?;
        self.kernel
            .resume_session(session_id, summary.cwd.clone())
            .await
            .map_err(|error| {
                ExtensionHostError::Operation(error.to_string())
            })?;
        Ok(summary)
    }

    /// Forks the current session at one persisted entry.
    async fn fork_session(
        &self,
        invocation: &ExtensionInvocation,
        entry_id: EntryId,
    ) -> Result<SessionSummary, ExtensionHostError> {
        self.session.ensure_extension_operation()?;
        let session_id = self
            .kernel
            .fork_generated_session(
                &invocation.session_id,
                entry_id,
                self.session.cwd.clone(),
            )
            .await
            .map_err(|error| {
                ExtensionHostError::Operation(error.to_string())
            })?;
        self.kernel
            .list_sessions(None)
            .map_err(|error| ExtensionHostError::Operation(error.to_string()))?
            .into_iter()
            .find(|summary| summary.session_id == session_id)
            .ok_or_else(|| {
                ExtensionHostError::Operation(
                    "forked session metadata is unavailable".to_string(),
                )
            })
    }

    /// Navigates the current tree when no generated summary is requested.
    async fn navigate_tree(
        &self,
        invocation: &ExtensionInvocation,
        entry_id: EntryId,
        summarize: bool,
    ) -> Result<SessionTreeSnapshot, ExtensionHostError> {
        self.session.ensure_extension_operation()?;
        self.kernel
            .navigate_session_with_summary(
                &invocation.session_id,
                entry_id,
                summarize,
            )
            .await
            .map_err(|error| ExtensionHostError::Operation(error.to_string()))
    }
}
