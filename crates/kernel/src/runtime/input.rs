use super::*;

/// Server-expanded text or preserved multimodal prompt content.
pub(super) enum PromptInputExpansion {
    /// Text input after optional Skill or Prompt Template expansion.
    Text {
        /// Exact text projected into the Model.
        text: String,
        /// Frozen Skill or Prompt Template projection retained for replay.
        expansion: Option<Box<protocol::SlashCommandExpansion>>,
        /// Skill read failure emitted by the active Kernel Run when present.
        diagnostic: Option<Box<protocol::SkillDiagnostic>>,
    },
    /// Ordered non-command content plus its text-only extension projection.
    Blocks {
        /// Original model-facing content blocks.
        blocks: Vec<ContentBlock>,
        /// Text blocks joined for the BeforeAgentStart compatibility event.
        text_projection: String,
    },
}

impl PromptInputExpansion {
    /// Preserves multimodal blocks without invoking text-only expansion hooks.
    pub(super) fn from_blocks(blocks: Vec<ContentBlock>) -> Self {
        let text_projection = blocks
            .iter()
            .filter_map(ContentBlock::text)
            .collect::<Vec<_>>()
            .join("\n\n");
        Self::Blocks {
            blocks,
            text_projection,
        }
    }

    /// Returns the text projection exposed to pre-Agent extension events.
    pub(super) fn text(&self) -> &str {
        match self {
            Self::Text { text, .. } => text,
            Self::Blocks {
                text_projection, ..
            } => text_projection,
        }
    }

    /// Returns a recoverable Skill diagnostic for text expansion failures.
    pub(super) fn diagnostic(&self) -> Option<&protocol::SkillDiagnostic> {
        match self {
            Self::Text { diagnostic, .. } => diagnostic.as_deref(),
            Self::Blocks { .. } => None,
        }
    }

    /// Materializes the persisted user message content for this expansion result.
    pub(super) fn message_content(self) -> MessageContent {
        match self {
            Self::Text {
                text, expansion, ..
            } => match expansion {
                Some(expansion) => MessageContent::ExpandedUser {
                    expansion: *expansion,
                },
                None => MessageContent::User {
                    blocks: vec![ContentBlock::Text { text }],
                },
            },
            Self::Blocks { blocks, .. } => MessageContent::User { blocks },
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
                    return Ok(PromptInputExpansion::Text {
                        expansion: invocation.map(|invocation| {
                            Box::new(
                                protocol::SlashCommandExpansion::builder()
                                    .invocation(invocation)
                                    .source(protocol::SlashCommandSource::Skill)
                                    .model_blocks(vec![ContentBlock::Text {
                                        text: text.clone(),
                                    }])
                                    .build(),
                            )
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
                    return Ok(PromptInputExpansion::Text {
                        text: original,
                        expansion: None,
                        diagnostic: Some(diagnostic),
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
            return Ok(PromptInputExpansion::Text {
                expansion: invocation.map(|invocation| {
                    Box::new(
                        protocol::SlashCommandExpansion::builder()
                            .invocation(invocation)
                            .source(
                                protocol::SlashCommandSource::PromptTemplate,
                            )
                            .model_blocks(vec![ContentBlock::Text {
                                text: text.clone(),
                            }])
                            .build(),
                    )
                }),
                text,
                diagnostic: None,
            });
        }

        Ok(PromptInputExpansion::Text {
            text: input.to_string(),
            expansion: None,
            diagnostic: None,
        })
    }
}
