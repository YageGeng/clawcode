use super::super::*;
use super::update::{ToolExecutionUpdate, ToolUpdateChannel};

/// Immutable inputs required to execute and correlate one complete tool batch.
#[derive(typed_builder::TypedBuilder)]
pub(in crate::runtime) struct ToolBatch<'a> {
    pub(in crate::runtime) session_id: &'a SessionId,
    pub(in crate::runtime) run_id: &'a RunId,
    pub(in crate::runtime) turn_id: &'a TurnId,
    pub(in crate::runtime) trace_id: &'a TraceId,
    pub(in crate::runtime) session: &'a Arc<Session>,
    pub(in crate::runtime) emitter: &'a EventEmitter,
    pub(in crate::runtime) cancellation: &'a CancellationToken,
    pub(in crate::runtime) tools: Arc<ToolRegistry>,
    pub(in crate::runtime) calls: Vec<ToolCall>,
}

/// Completed source-ordered tool messages plus cancellation state for the Turn.
#[derive(Default, typed_builder::TypedBuilder)]
pub(in crate::runtime) struct ToolBatchResult {
    pub(in crate::runtime) messages: Vec<AgentMessage>,
    pub(in crate::runtime) results: Vec<ToolResult>,
    pub(in crate::runtime) cancelled: bool,
    pub(in crate::runtime) terminate: bool,
}

impl Kernel {
    /// Emits one replaceable tool snapshot through protocol and extension observers.
    async fn emit_tool_update(
        &self,
        batch: &ToolBatch<'_>,
        update: ToolExecutionUpdate,
    ) -> Result<(), KernelError> {
        batch
            .emitter
            .emit(
                batch.turn_id.clone(),
                AgentEventPayload::ToolExecutionUpdate {
                    run_id: batch.run_id.clone(),
                    result: update.result.clone(),
                },
            )
            .await?;
        let context = self.extension_context(
            batch.session,
            Some(batch.run_id),
            Some(batch.turn_id),
        )?;
        batch
            .session
            .extensions
            .runtime_ref()
            .emit_tool_execution_update(
                &protocol::ToolExecutionUpdateEvent {
                    call: update.call,
                    partial_result: update.result,
                },
                &context,
            )
            .await;
        Ok(())
    }

    /// Executes transformed sibling tool calls concurrently and returns source-ordered results.
    pub(in crate::runtime) async fn execute_tool_batch(
        &self,
        mut batch: ToolBatch<'_>,
    ) -> Result<ToolBatchResult, KernelError> {
        let mut pending = futures::stream::FuturesUnordered::new();
        let (update_channel, mut update_stream) = ToolUpdateChannel::new();
        let mut terminate = false;

        let calls = std::mem::take(&mut batch.calls);
        for (index, mut call) in calls.into_iter().enumerate() {
            let started_at = self.clock.now();
            let extension_context = self.extension_context(
                batch.session,
                Some(batch.run_id),
                Some(batch.turn_id),
            )?;
            batch
                .session
                .extensions
                .runtime_ref()
                .emit_tool_execution_start(
                    &protocol::ToolExecutionStartEvent { call: call.clone() },
                    &extension_context,
                )
                .await;
            batch
                .emitter
                .emit_at(
                    batch.turn_id.clone(),
                    started_at,
                    AgentEventPayload::ToolExecutionStart {
                        run_id: batch.run_id.clone(),
                        call: call.clone(),
                    },
                )
                .await?;

            let mut immediate_result = None;
            match batch
                .session
                .extensions
                .runtime_ref()
                .emit_tool_call(
                    protocol::ToolCallEvent { call: call.clone() },
                    &extension_context,
                )
                .await
            {
                protocol::ToolCallResult::Continue => {}
                protocol::ToolCallResult::Replace { arguments } => {
                    call.arguments = arguments;
                }
                protocol::ToolCallResult::Block(block) => {
                    terminate |= block.terminate;
                    // Preserve policy disposition so ACP clients can distinguish
                    // a blocked call from a tool implementation failure.
                    immediate_result = Some(ToolResult::from((
                        call.tool_call_id.clone(),
                        block,
                    )));
                }
            }
            if immediate_result.is_none()
                && let Err(error) = batch.tools.validate(&call)
            {
                immediate_result = Some(
                    ToolResult::builder()
                        .tool_call_id(call.tool_call_id.clone())
                        .blocks(vec![ContentBlock::Text {
                            text: error.to_string(),
                        }])
                        .is_error(true)
                        .build(),
                );
            }

            let tools = Arc::clone(&batch.tools);
            let context = ToolExecutionContext::builder()
                .session_id(batch.session_id.clone())
                .turn_id(batch.turn_id.clone())
                .trace_id(batch.trace_id.clone())
                .cwd(batch.session.cwd.clone())
                .cancellation(batch.cancellation.clone())
                .updates(update_channel.publisher(call.clone()))
                .build();
            pending.push(async move {
                let result = match immediate_result {
                    Some(result) => result,
                    None => match tools.execute(call.clone(), &context).await {
                        Ok(result) => result,
                        Err(error) => ToolResult::builder()
                            .tool_call_id(call.tool_call_id.clone())
                            .blocks(vec![ContentBlock::Text {
                                text: error.to_string(),
                            }])
                            .is_error(true)
                            .build(),
                    },
                };
                (index, started_at, call, result)
            });
        }
        drop(update_channel);

        let mut completed = Vec::new();
        let mut cancelled = false;
        let mut updates_open = true;
        while !pending.is_empty() {
            let next = tokio::select! {
                biased;
                update = update_stream.changed(), if updates_open => {
                    match update {
                        Ok(updates) => {
                            for update in updates {
                                self.emit_tool_update(&batch, update).await?;
                            }
                        }
                        Err(_closed) => updates_open = false,
                    }
                    continue;
                }
                () = batch.cancellation.cancelled(), if !cancelled => {
                    cancelled = true;
                    continue;
                }
                completed = pending.next() => completed,
            };
            let Some((index, started_at, call, mut result)) = next else {
                break;
            };
            if let Some(update) = update_stream.take_for(&call.tool_call_id) {
                self.emit_tool_update(&batch, update).await?;
            }
            let context = self.extension_context(
                batch.session,
                Some(batch.run_id),
                Some(batch.turn_id),
            )?;
            let patch = batch
                .session
                .extensions
                .runtime_ref()
                .emit_tool_result(
                    protocol::ToolResultEvent {
                        call: call.clone(),
                        result: result.clone(),
                    },
                    &context,
                )
                .await;
            if let Some(blocks) = patch.blocks {
                result.blocks = blocks;
            }
            if let Some(details) = patch.details {
                result.details = Some(details);
            }
            if let Some(is_error) = patch.is_error {
                result.is_error = is_error;
            }
            let timestamp = self.clock.now();
            batch
                .session
                .extensions
                .runtime_ref()
                .emit_tool_execution_end(
                    &protocol::ToolExecutionEndEvent {
                        call,
                        result: result.clone(),
                    },
                    &context,
                )
                .await;
            batch
                .emitter
                .emit_at(
                    batch.turn_id.clone(),
                    timestamp,
                    AgentEventPayload::ToolExecutionEnd {
                        run_id: batch.run_id.clone(),
                        result: result.clone(),
                    },
                )
                .await?;
            let message = AgentMessage {
                identity: MessageIdentity {
                    message_id: self.message_id()?,
                    turn_id: batch.turn_id.clone(),
                },
                timing: MessageTiming::try_from((
                    started_at, started_at, timestamp,
                ))
                .map_err(|error| KernelError::Protocol(error.to_string()))?,
                content: MessageContent::ToolResult {
                    tool_call_id: result.tool_call_id.clone(),
                    blocks: result.blocks.clone(),
                    is_error: result.is_error,
                    details: result.details.clone(),
                },
            };
            completed.push((index, message, result));
        }
        for update in update_stream.take_unseen() {
            self.emit_tool_update(&batch, update).await?;
        }
        completed.sort_by_key(|(index, _message, _result)| *index);
        let (messages, results): (Vec<_>, Vec<_>) = completed
            .into_iter()
            .map(|(_index, message, result)| (message, result))
            .unzip();
        Ok(ToolBatchResult::builder()
            .messages(messages)
            .results(results)
            .cancelled(cancelled)
            .terminate(terminate)
            .build())
    }
}
