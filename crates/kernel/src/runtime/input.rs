use super::*;

/// Server-expanded prompt data plus an optional recoverable Skill diagnostic.
pub(super) struct PromptInputExpansion {
    /// Exact text projected into the Model when no resource command matched.
    pub(super) text: String,
    /// Frozen Skill or Prompt Template projection retained for replay.
    pub(super) expansion: Option<protocol::SlashCommandExpansion>,
    /// Skill read failure emitted by the active Kernel Run when present.
    pub(super) diagnostic: Option<protocol::SkillDiagnostic>,
}

impl PromptInputExpansion {
    /// Materializes the persisted user message content for this expansion result.
    pub(super) fn message_content(self) -> MessageContent {
        match self.expansion {
            Some(expansion) => MessageContent::ExpandedUser { expansion },
            None => MessageContent::User {
                blocks: vec![ContentBlock::Text { text: self.text }],
            },
        }
    }
}

impl SessionRuntime {
    /// Expands one transformed input through Session Skill and Template snapshots once.
    pub(super) fn expand_prompt_input(
        &self,
        session_id: &SessionId,
        input: &str,
    ) -> Result<PromptInputExpansion, KernelError> {
        let invocation = protocol::SlashCommandInvocation::try_from(input).ok();
        if let Some(skills) = self.skill_catalog()? {
            match skills.expand_command(input) {
                skill::SkillCommandExpansion::Expanded(text) => {
                    return Ok(PromptInputExpansion {
                        expansion: invocation.map(|invocation| {
                            protocol::SlashCommandExpansion::builder()
                                .invocation(invocation)
                                .source(protocol::SlashCommandSource::Skill)
                                .model_blocks(vec![ContentBlock::Text {
                                    text: text.clone(),
                                }])
                                .build()
                        }),
                        text,
                        diagnostic: None,
                    });
                }
                skill::SkillCommandExpansion::Failed {
                    original,
                    diagnostic,
                } => {
                    tracing::warn!(
                        "failed to expand Skill command for session {}: {}",
                        session_id,
                        diagnostic.message
                    );
                    return Ok(PromptInputExpansion {
                        text: original,
                        expansion: None,
                        diagnostic: Some(*diagnostic),
                    });
                }
                skill::SkillCommandExpansion::NotSkillCommand => {}
            }
        }

        let prompt = self.prompt_session()?;
        let is_template = invocation.as_ref().is_some_and(|invocation| {
            prompt
                .templates()
                .iter()
                .any(|template| template.name == invocation.name)
        });
        if is_template {
            let text = prompt.expand_template(input);
            return Ok(PromptInputExpansion {
                expansion: invocation.map(|invocation| {
                    protocol::SlashCommandExpansion::builder()
                        .invocation(invocation)
                        .source(protocol::SlashCommandSource::PromptTemplate)
                        .model_blocks(vec![ContentBlock::Text {
                            text: text.clone(),
                        }])
                        .build()
                }),
                text,
                diagnostic: None,
            });
        }

        Ok(PromptInputExpansion {
            text: input.to_string(),
            expansion: None,
            diagnostic: None,
        })
    }
}
