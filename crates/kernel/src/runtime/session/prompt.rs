use std::sync::Arc;

use ::tools::ToolRegistry;
use protocol::{
    AgentMessage, ContentBlock, MessageContent, MessageId, MessageIdentity,
    MessageTiming, SessionId, TimestampMs, TurnId,
};

use super::resources::SessionResources;
use crate::runtime::{Kernel, KernelError, Session};

impl Session {
    /// Returns the initialized immutable Prompt snapshot for this Session.
    pub(in crate::runtime) fn prompt_session(
        &self,
    ) -> Result<&Arc<::prompt::PromptSession>, KernelError> {
        self.resources
            .get()
            .map(SessionResources::prompt)
            .ok_or_else(|| {
                KernelError::Protocol(
                    "session Prompt resources are not initialized".to_string(),
                )
            })
    }

    /// Returns the initialized optional Skill snapshot for this Session.
    pub(in crate::runtime) fn skill_catalog(
        &self,
    ) -> Result<Option<&Arc<skill::SkillCatalog>>, KernelError> {
        self.resources
            .get()
            .map(SessionResources::skills)
            .ok_or_else(|| {
                KernelError::Protocol(
                    "session Skill resources are not initialized".to_string(),
                )
            })
    }

    /// Builds the current Turn System Prompt from one active Tool snapshot.
    pub(in crate::runtime) fn build_system_prompt(
        &self,
        tools: &ToolRegistry,
        include_skill_instructions: bool,
    ) -> Result<::prompt::BuiltSystemPrompt, KernelError> {
        let skills = self
            .skill_catalog()?
            .map_or_else(Vec::new, |catalog| catalog.descriptors());
        Ok(self.prompt_session()?.build_system_prompt(
            ::prompt::SystemPromptTurnInput {
                tools: tools.prompt_tools(),
                skills,
                include_skill_instructions,
            },
        ))
    }
}

impl Kernel {
    /// Materializes one rendered System Prompt with Turn identity and timing.
    pub(in crate::runtime) fn system_prompt_message(
        &self,
        session: &Session,
        turn_id: TurnId,
        timestamp_ms: TimestampMs,
        tools: &ToolRegistry,
    ) -> Result<AgentMessage, KernelError> {
        let built = session
            .build_system_prompt(tools, self.include_skill_instructions)?;
        let timing =
            MessageTiming::try_from((timestamp_ms, timestamp_ms, timestamp_ms))
                .map_err(|error| KernelError::Protocol(error.to_string()))?;
        let message_id = MessageId::try_from(format!("system-{turn_id}"))
            .map_err(|error| KernelError::Protocol(error.to_string()))?;

        Ok(AgentMessage {
            identity: MessageIdentity {
                message_id,
                turn_id,
            },
            timing,
            content: MessageContent::System {
                blocks: vec![ContentBlock::Text { text: built.text }],
            },
        })
    }

    /// Returns startup Prompt diagnostics without exposing resource bodies.
    pub fn prompt_diagnostics(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<protocol::PromptDiagnostic>, KernelError> {
        Ok(self
            .session(session_id)?
            .prompt_session()?
            .diagnostics()
            .to_vec())
    }
}
