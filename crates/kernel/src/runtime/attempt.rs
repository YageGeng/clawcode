use protocol::{
    AgentMessage, AssistantMetadata, ContentBlock, MessageId, MessageIdentity,
    MessageTiming, ModelFailure, ModelFinal, ModelProfile,
    ModelRetryDisposition, ModelStreamEvent, ModelUsage, StopReason,
    TimestampMs, ToolCall, TurnId,
};

use crate::ModelError;

/// Mutable state for exactly one provider call and its persisted Assistant message.
pub(super) struct AssistantAttempt {
    identity: MessageIdentity,
    profile: ModelProfile,
    timestamp_ms: TimestampMs,
    started_at_ms: Option<TimestampMs>,
    ended_at_ms: Option<TimestampMs>,
    blocks: Vec<ContentBlock>,
    tool_calls: Vec<ToolCall>,
    final_: Option<ModelFinal>,
}

/// Terminal cause supplied by the runtime after the provider stream settles.
pub(super) enum AssistantAttemptSettlement {
    /// The response stream reached EOF and must contain provider Final metadata.
    Completed {
        /// Time at which EOF was observed, used only for an output-free attempt.
        settled_at_ms: TimestampMs,
    },
    /// The request or stream failed with a structured model error.
    Failed {
        /// Error returned by the model boundary.
        error: ModelError,
        /// Time at which the failure was observed.
        settled_at_ms: TimestampMs,
    },
    /// Session cancellation interrupted the request or response stream.
    Cancelled {
        /// Time at which cancellation was observed.
        settled_at_ms: TimestampMs,
    },
}

/// Complete persisted result of one Assistant attempt.
pub(super) enum AssistantAttemptResult {
    /// Provider generation completed and its tool calls may execute.
    Completed {
        /// Complete Assistant message.
        message: AgentMessage,
        /// Tool calls retained in provider source order.
        tool_calls: Vec<ToolCall>,
        /// Normalized provider stop reason.
        stop_reason: StopReason,
    },
    /// Provider generation failed after the attempt had started.
    Failed {
        /// Complete error Assistant message.
        message: AgentMessage,
        /// Structured failure used by the later agent retry policy.
        failure: ModelFailure,
    },
    /// Provider generation was cancelled by the active session.
    Cancelled {
        /// Complete cancelled Assistant message.
        message: AgentMessage,
    },
}

impl AssistantAttempt {
    /// Creates an empty attempt with immutable message identity and model profile.
    pub(super) fn new(
        message_id: MessageId,
        turn_id: TurnId,
        profile: ModelProfile,
        timestamp_ms: TimestampMs,
    ) -> Self {
        Self {
            identity: MessageIdentity {
                message_id,
                turn_id,
            },
            profile,
            timestamp_ms,
            started_at_ms: None,
            ended_at_ms: None,
            blocks: Vec::new(),
            tool_calls: Vec::new(),
            final_: None,
        }
    }

    /// Applies one ordered model event while enforcing a single terminal Final.
    pub(super) fn apply(
        &mut self,
        event: &ModelStreamEvent,
        timestamp_ms: TimestampMs,
    ) -> Result<(), ModelError> {
        if self.final_.is_some() {
            return Err(ModelError::Protocol(
                "model emitted data after terminal metadata".to_string(),
            ));
        }

        match event {
            ModelStreamEvent::TextDelta(delta) => {
                self.started_at_ms.get_or_insert(timestamp_ms);
                self.ended_at_ms = Some(timestamp_ms);
                match self.blocks.last_mut() {
                    Some(ContentBlock::Text { text }) => text.push_str(delta),
                    _ => self.blocks.push(ContentBlock::Text {
                        text: delta.clone(),
                    }),
                }
            }
            ModelStreamEvent::ReasoningDelta(delta) => {
                self.started_at_ms.get_or_insert(timestamp_ms);
                self.ended_at_ms = Some(timestamp_ms);
                match self.blocks.last_mut() {
                    Some(ContentBlock::Reasoning { text }) => {
                        text.push_str(delta)
                    }
                    _ => self.blocks.push(ContentBlock::Reasoning {
                        text: delta.clone(),
                    }),
                }
            }
            ModelStreamEvent::ToolCall(call) => {
                self.started_at_ms.get_or_insert(timestamp_ms);
                self.ended_at_ms = Some(timestamp_ms);
                self.blocks.push(ContentBlock::ToolCall {
                    tool_call_id: call.tool_call_id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                });
                self.tool_calls.push(call.clone());
            }
            ModelStreamEvent::Finished(final_) => {
                self.final_ = Some(final_.clone());
            }
        }
        Ok(())
    }

    /// Builds the current immutable Assistant snapshot for streaming observers.
    pub(super) fn snapshot(
        &self,
        observed_at_ms: TimestampMs,
    ) -> Result<AgentMessage, ModelError> {
        let started_at_ms = self.started_at_ms.unwrap_or(self.timestamp_ms);
        let ended_at_ms = self.ended_at_ms.unwrap_or(observed_at_ms);
        let final_ = self
            .final_
            .clone()
            .unwrap_or_else(|| Self::terminal_final(StopReason::EndTurn));
        Ok(AgentMessage {
            identity: self.identity.clone(),
            timing: MessageTiming::try_from((
                self.timestamp_ms,
                started_at_ms,
                ended_at_ms,
            ))
            .map_err(|error| ModelError::Protocol(error.to_string()))?,
            content: protocol::MessageContent::Assistant {
                blocks: self.blocks.clone(),
                metadata: AssistantMetadata::builder()
                    .provider_id(self.profile.provider_id.clone())
                    .model_id(self.profile.model_id.clone())
                    .stop_reason(final_.stop_reason)
                    .raw_stop_reason(final_.raw_stop_reason)
                    .usage(final_.usage)
                    .build(),
            },
        })
    }

    /// Converts the attempt into one complete success, failure, or cancellation message.
    pub(super) fn settle(
        mut self,
        settlement: AssistantAttemptSettlement,
    ) -> Result<AssistantAttemptResult, ModelError> {
        let (final_, error, failure, cancelled, settled_at_ms) =
            match settlement {
                AssistantAttemptSettlement::Completed { settled_at_ms } => {
                    match self.final_.take() {
                        Some(final_) => {
                            (final_, None, None, false, settled_at_ms)
                        }
                        None => {
                            let failure = ModelFailure {
                                summary: "model stream ended without terminal metadata"
                                    .to_string(),
                                status: None,
                                retry_disposition:
                                    ModelRetryDisposition::Retryable,
                            };
                            (
                                Self::terminal_final(StopReason::Error),
                                Some(failure.summary.clone()),
                                Some(failure),
                                false,
                                settled_at_ms,
                            )
                        }
                    }
                }
                AssistantAttemptSettlement::Failed {
                    error,
                    settled_at_ms,
                } => {
                    if matches!(error, ModelError::Cancelled) {
                        (
                            Self::terminal_final(StopReason::Cancelled),
                            None,
                            None,
                            true,
                            settled_at_ms,
                        )
                    } else {
                        let failure = match error {
                            ModelError::Preflight(failure)
                            | ModelError::Request(failure)
                            | ModelError::Stream(failure) => failure,
                            ModelError::Unavailable(summary)
                            | ModelError::Protocol(summary) => ModelFailure {
                                summary,
                                status: None,
                                retry_disposition:
                                    ModelRetryDisposition::NonRetryable,
                            },
                            // Cancelled is handled above; keep this fallback
                            // non-panicking so a logic drift degrades gracefully.
                            ModelError::Cancelled => ModelFailure {
                                summary: "model request cancelled".to_string(),
                                status: None,
                                retry_disposition:
                                    ModelRetryDisposition::NonRetryable,
                            },
                        };
                        (
                            Self::terminal_final(StopReason::Error),
                            Some(failure.summary.clone()),
                            Some(failure),
                            false,
                            settled_at_ms,
                        )
                    }
                }
                AssistantAttemptSettlement::Cancelled { settled_at_ms } => (
                    Self::terminal_final(StopReason::Cancelled),
                    None,
                    None,
                    true,
                    settled_at_ms,
                ),
            };

        let started_at_ms = self.started_at_ms.unwrap_or(self.timestamp_ms);
        let ended_at_ms = self.ended_at_ms.unwrap_or(settled_at_ms);
        if self.blocks.is_empty() {
            self.blocks.push(ContentBlock::Text {
                text: String::new(),
            });
        }
        let stop_reason = final_.stop_reason;
        let message = AgentMessage {
            identity: self.identity,
            timing: MessageTiming::try_from((
                self.timestamp_ms,
                started_at_ms,
                ended_at_ms,
            ))
            .map_err(|error| ModelError::Protocol(error.to_string()))?,
            content: protocol::MessageContent::Assistant {
                blocks: self.blocks,
                metadata: AssistantMetadata::builder()
                    .provider_id(self.profile.provider_id)
                    .model_id(self.profile.model_id)
                    .stop_reason(stop_reason)
                    .raw_stop_reason(final_.raw_stop_reason)
                    .usage(final_.usage)
                    .error(error)
                    .build(),
            },
        };

        if cancelled {
            Ok(AssistantAttemptResult::Cancelled { message })
        } else if let Some(failure) = failure {
            Ok(AssistantAttemptResult::Failed { message, failure })
        } else {
            Ok(AssistantAttemptResult::Completed {
                message,
                tool_calls: self.tool_calls,
                stop_reason,
            })
        }
    }

    /// Builds a kernel terminal result when the provider supplied no Final metadata.
    fn terminal_final(stop_reason: StopReason) -> ModelFinal {
        ModelFinal {
            stop_reason,
            raw_stop_reason: None,
            usage: ModelUsage::builder()
                .input_tokens(0)
                .output_tokens(0)
                .cache_read_tokens(0)
                .cache_write_tokens(0)
                .total_tokens(0)
                .build(),
        }
    }
}
