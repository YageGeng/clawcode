use protocol::{
    AgentMessage, AssistantMetadata, ContentBlock, MessageId, MessageIdentity,
    MessageTiming, ModelFailure, ModelFinal, ModelProfile,
    ModelRetryDisposition, ModelStreamEvent, ModelUsage, StopReason,
    TimestampMs, TurnId,
};

use crate::ModelError;

/// Mutable state for exactly one provider call and its persisted Assistant message.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct AssistantAttempt {
    identity: MessageIdentity,
    profile: ModelProfile,
    timestamp_ms: TimestampMs,
    #[builder(default)]
    started_at_ms: Option<TimestampMs>,
    #[builder(default)]
    ended_at_ms: Option<TimestampMs>,
    #[builder(default)]
    blocks: Vec<ContentBlock>,
    #[builder(default)]
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
    /// Provider generation completed; extensions still control the final executable message.
    Completed {
        /// Complete Assistant message.
        message: AgentMessage,
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
        Self::builder()
            .identity(MessageIdentity {
                message_id,
                turn_id,
            })
            .profile(profile)
            .timestamp_ms(timestamp_ms)
            .build()
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
                    invalid_arguments: call.invalid_arguments.clone(),
                    tool_call_id: call.tool_call_id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                });
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
                model_message: None,
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
        // A cancellation or late transport error must retain already reported accounting.
        let reported_usage = self.final_.as_ref().map(|final_| final_.usage);
        let (mut final_, error, failure, cancelled, settled_at_ms) =
            match settlement {
                AssistantAttemptSettlement::Completed { settled_at_ms } => {
                    match self.final_.take() {
                        Some(final_) => {
                            (final_, None, None, false, settled_at_ms)
                        }
                        None => {
                            let failure = ModelFailure::builder().summary("model stream ended without terminal metadata"
                                    .to_string()).status(None).retry_disposition(ModelRetryDisposition::Retryable).build();
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
                            | ModelError::Protocol(summary) => {
                                ModelFailure::builder()
                                    .summary(summary)
                                    .status(None)
                                    .retry_disposition(
                                        ModelRetryDisposition::NonRetryable,
                                    )
                                    .build()
                            }
                            // Cancelled is handled above; keep this fallback
                            // non-panicking so a logic drift degrades gracefully.
                            ModelError::Cancelled => ModelFailure::builder()
                                .summary("model request cancelled".to_string())
                                .status(None)
                                .retry_disposition(
                                    ModelRetryDisposition::NonRetryable,
                                )
                                .build(),
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

        if (cancelled || failure.is_some())
            && let Some(usage) = failure
                .as_ref()
                .and_then(|failure| failure.usage.as_deref().copied())
                .or(reported_usage)
        {
            // Failed terminal accounting is authoritative even without a Finished event.
            final_.usage = usage;
        }

        // Final provider blocks may correct or remove text that arrived as deltas.
        if let Some(native) = &final_.model_message {
            self.blocks =
                crate::provider::AssistantProjection::from(native.as_ref())
                    .blocks()?;
        }
        let started_at_ms = self.started_at_ms.unwrap_or(self.timestamp_ms);
        let ended_at_ms = self.ended_at_ms.unwrap_or(settled_at_ms);
        if self.blocks.is_empty() && final_.model_message.is_none() {
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
                model_message: final_.model_message,
                blocks: self.blocks,
                metadata: AssistantMetadata::builder()
                    .provider_id(self.profile.provider_id)
                    .model_id(self.profile.model_id)
                    .stop_reason(stop_reason)
                    .raw_stop_reason(final_.raw_stop_reason)
                    .usage(final_.usage)
                    .error(error)
                    .context_overflow(
                        failure
                            .as_ref()
                            .is_some_and(|failure| failure.context_overflow),
                    )
                    .build(),
            },
        };

        if cancelled {
            Ok(AssistantAttemptResult::Cancelled { message })
        } else if let Some(failure) = failure {
            Ok(AssistantAttemptResult::Failed { message, failure })
        } else {
            Ok(AssistantAttemptResult::Completed { message })
        }
    }

    /// Builds a kernel terminal result when the provider supplied no Final metadata.
    fn terminal_final(stop_reason: StopReason) -> ModelFinal {
        ModelFinal::builder()
            .stop_reason(stop_reason)
            .raw_stop_reason(None)
            .usage(ModelUsage::default())
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::MessageContent;
    use rig_core::message::{AssistantContent, AssistantMessage, Reasoning};

    /// Creates a complete attempt for authoritative-response settlement checks.
    fn attempt() -> AssistantAttempt {
        AssistantAttempt::new(
            MessageId::try_from("m").unwrap(),
            TurnId::try_from("t").unwrap(),
            ModelProfile::builder()
                .provider_id("openai".into())
                .model_id("audit".into())
                .display_name("audit".into())
                .context_tokens(4096)
                .max_output_tokens(128)
                .input(Default::default())
                .build(),
            TimestampMs::from(1),
        )
    }

    /// Final provider content replaces the temporary delta projection before persistence.
    #[test]
    fn final_response_corrects_streamed_projection() {
        let mut attempt = attempt();
        attempt
            .apply(
                &ModelStreamEvent::TextDelta("Bye".into()),
                TimestampMs::from(2),
            )
            .unwrap();
        attempt
            .apply(
                &ModelStreamEvent::ReasoningDelta("Plan A".into()),
                TimestampMs::from(3),
            )
            .unwrap();
        let native = AssistantMessage::new(vec![
            AssistantContent::Reasoning(Reasoning::new("Plan B.")),
            AssistantContent::text("Hello"),
        ])
        .with_stop(rig_core::message::StopReason::Stop);
        attempt
            .apply(
                &ModelStreamEvent::Finished(
                    ModelFinal::builder()
                        .stop_reason(StopReason::EndTurn)
                        .usage(ModelUsage::default())
                        .model_message(Some(Box::new(native.clone())))
                        .build(),
                ),
                TimestampMs::from(4),
            )
            .unwrap();
        let AssistantAttemptResult::Completed { message, .. } = attempt
            .settle(AssistantAttemptSettlement::Completed {
                settled_at_ms: TimestampMs::from(5),
            })
            .unwrap()
        else {
            panic!("completed attempt");
        };
        let MessageContent::Assistant {
            blocks,
            model_message,
            ..
        } = message.content
        else {
            panic!("assistant");
        };
        assert_eq!(
            blocks,
            vec![
                ContentBlock::Reasoning {
                    text: "Plan B.".into()
                },
                ContentBlock::Text {
                    text: "Hello".into()
                }
            ]
        );
        assert_eq!(model_message.as_deref(), Some(&native));
    }

    /// A final answer without reasoning clears an earlier speculative reasoning delta.
    #[test]
    fn final_projection_clears_removed_reasoning() {
        let mut attempt = attempt();
        attempt
            .apply(
                &ModelStreamEvent::ReasoningDelta("discard".into()),
                TimestampMs::from(2),
            )
            .unwrap();
        let native =
            AssistantMessage::new(vec![AssistantContent::text("answer")])
                .with_stop(rig_core::message::StopReason::Stop);
        attempt
            .apply(
                &ModelStreamEvent::Finished(
                    ModelFinal::builder()
                        .stop_reason(StopReason::EndTurn)
                        .usage(ModelUsage::default())
                        .model_message(Some(Box::new(native)))
                        .build(),
                ),
                TimestampMs::from(3),
            )
            .unwrap();
        let AssistantAttemptResult::Completed { message, .. } = attempt
            .settle(AssistantAttemptSettlement::Completed {
                settled_at_ms: TimestampMs::from(4),
            })
            .unwrap()
        else {
            panic!("completed");
        };
        let MessageContent::Assistant { blocks, .. } = message.content else {
            panic!("assistant");
        };
        assert_eq!(
            blocks,
            vec![ContentBlock::Text {
                text: "answer".into()
            }]
        );
    }
    /// A late cancellation cannot erase accounting already reported by the provider.
    #[test]
    fn terminal_cancellation_retains_reported_usage() {
        let mut attempt = attempt();
        attempt
            .apply(
                &ModelStreamEvent::Finished(
                    ModelFinal::builder()
                        .stop_reason(StopReason::EndTurn)
                        .usage(
                            protocol::Usage::new()
                                .input_tokens(10)
                                .output_tokens(2)
                                .total_tokens(12)
                                .into(),
                        )
                        .build(),
                ),
                TimestampMs::from(2),
            )
            .expect("final event");
        let AssistantAttemptResult::Cancelled { message } = attempt
            .settle(AssistantAttemptSettlement::Cancelled {
                settled_at_ms: TimestampMs::from(3),
            })
            .expect("cancelled settlement")
        else {
            panic!("cancelled")
        };
        let MessageContent::Assistant { metadata, .. } = message.content else {
            panic!("assistant")
        };
        assert_eq!(metadata.usage.as_ref().total_tokens, Some(12));
        assert_eq!(metadata.stop_reason, StopReason::Cancelled);
    }
}
