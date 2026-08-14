use super::*;

impl Kernel {
    /// Executes sibling tool calls concurrently while returning messages in source order.
    pub(super) async fn execute_tool_batch(
        &self,
        batch: ToolBatch<'_>,
    ) -> Result<ToolBatchResult, KernelError> {
        let mut pending = futures::stream::FuturesUnordered::new();
        let (update_sender, mut update_receiver) = mpsc::unbounded_channel();
        for (index, call) in batch.calls.into_iter().enumerate() {
            let started_at = self.clock.now();
            self.dispatch(
                ExtensionEvent::ToolExecutionStart,
                batch.session_id,
                Some(batch.turn_id.clone()),
            )
            .await?;
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
            let tools = Arc::clone(&batch.session.tools);
            let context = ToolExecutionContext::builder()
                .session_id(batch.session_id.clone())
                .turn_id(batch.turn_id.clone())
                .cwd(batch.session.cwd.clone())
                .cancellation(batch.cancellation.clone())
                .updates(Arc::new(KernelToolUpdates {
                    sender: update_sender.clone(),
                }))
                .build();
            pending.push(async move {
                let result = match tools.execute(call.clone(), &context).await {
                    Ok(result) => result,
                    Err(error) => ToolResult::builder()
                        .tool_call_id(call.tool_call_id)
                        .blocks(vec![ContentBlock::Text {
                            text: error.to_string(),
                        }])
                        .is_error(true)
                        .build(),
                };
                (index, started_at, result)
            });
        }
        drop(update_sender);

        let mut completed = Vec::new();
        let mut cancelled = false;
        let mut updates_open = true;
        while !pending.is_empty() {
            let next = tokio::select! {
                biased;
                update = update_receiver.recv(), if updates_open => {
                    match update {
                        Some(result) => {
                            batch.emitter.emit(
                                batch.turn_id.clone(),
                                AgentEventPayload::ToolExecutionUpdate {
                                    run_id: batch.run_id.clone(),
                                    result,
                                },
                            ).await?;
                            self.dispatch(
                                ExtensionEvent::ToolExecutionUpdate,
                                batch.session_id,
                                Some(batch.turn_id.clone()),
                            )
                            .await?;
                        }
                        None => updates_open = false,
                    }
                    continue;
                }
                () = batch.cancellation.cancelled(), if !cancelled => {
                    cancelled = true;
                    continue;
                }
                completed = pending.next() => completed,
            };
            let Some((index, started_at, result)) = next else {
                break;
            };
            let timestamp = self.clock.now();
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
            self.dispatch(
                ExtensionEvent::ToolExecutionEnd,
                batch.session_id,
                Some(batch.turn_id.clone()),
            )
            .await?;
            completed.push((
                index,
                AgentMessage {
                    identity: MessageIdentity {
                        message_id: self.message_id()?,
                        turn_id: batch.turn_id.clone(),
                    },
                    timing: MessageTiming::try_from((
                        started_at, started_at, timestamp,
                    ))
                    .map_err(|error| {
                        KernelError::Protocol(error.to_string())
                    })?,
                    content: MessageContent::ToolResult {
                        tool_call_id: result.tool_call_id,
                        blocks: result.blocks,
                        is_error: result.is_error,
                        details: result.details,
                    },
                },
            ));
        }
        while let Ok(result) = update_receiver.try_recv() {
            batch
                .emitter
                .emit(
                    batch.turn_id.clone(),
                    AgentEventPayload::ToolExecutionUpdate {
                        run_id: batch.run_id.clone(),
                        result,
                    },
                )
                .await?;
            self.dispatch(
                ExtensionEvent::ToolExecutionUpdate,
                batch.session_id,
                Some(batch.turn_id.clone()),
            )
            .await?;
        }
        completed.sort_by_key(|(index, _message)| *index);
        Ok(ToolBatchResult {
            messages: completed
                .into_iter()
                .map(|(_index, message)| message)
                .collect(),
            cancelled,
        })
    }
}
