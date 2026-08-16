use super::*;

/// Result of checking raw input against the current Extension command snapshot.
pub(super) enum CommandInputDisposition {
    /// An Extension command completed without starting an Agent Run.
    Handled,
    /// Input was not an Extension command and continues through normal processing.
    Continue(String),
}

impl Kernel {
    /// Dispatches a recognized slash command before acquiring the Session run gate.
    pub(super) async fn dispatch_extension_command_input(
        &self,
        session: &Arc<SessionRuntime>,
        input: &str,
    ) -> Result<CommandInputDisposition, KernelError> {
        let invocation =
            match ::extension::ExtensionCommandInvocation::try_from(input) {
                Ok(invocation) => invocation,
                Err(
                    ::extension::DynamicRegistryError::InvalidCommandInvocation(
                        _,
                    ),
                ) => {
                    return Ok(CommandInputDisposition::Continue(
                        input.to_string(),
                    ));
                }
                Err(error) => {
                    return Err(KernelError::ExtensionBlocked(
                        error.to_string(),
                    ));
                }
            };
        let command = match session
            .commands
            .snapshot()
            .map_err(|error| KernelError::ExtensionBlocked(error.to_string()))?
            .resolve(&invocation.name)
        {
            Ok(command) => command,
            Err(::extension::DynamicRegistryError::CommandNotFound(_)) => {
                return Ok(CommandInputDisposition::Continue(
                    input.to_string(),
                ));
            }
            Err(::extension::DynamicRegistryError::AmbiguousCommand(name)) => {
                return Err(KernelError::ExtensionCommandAmbiguous(name));
            }
            Err(error) => {
                return Err(KernelError::ExtensionBlocked(error.to_string()));
            }
        };
        let context = self
            .extension_context(session, None, None)?
            .for_extension(&command.extension_id);
        command
            .handler
            .handle(
                &invocation.arguments,
                &serde_json::Value::Null,
                &::extension::ExtensionCommandContext { event: context },
            )
            .await?;
        Ok(CommandInputDisposition::Handled)
    }
}

impl SessionRuntime {
    /// Expands one transformed input through Session Skill and Template snapshots.
    pub(super) fn expand_prompt_input(
        &self,
        session_id: &SessionId,
        input: &str,
    ) -> Result<String, KernelError> {
        let skill_expanded = match self.skill_catalog()? {
            Some(skills) => match skills.expand_command(input) {
                Ok(Some(expanded)) => expanded,
                Ok(None) => input.to_string(),
                Err(error) => {
                    tracing::warn!(
                        "failed to expand Skill command for session {}: {}",
                        session_id,
                        error
                    );
                    input.to_string()
                }
            },
            None => input.to_string(),
        };
        Ok(self.prompt_session()?.expand_template(&skill_expanded))
    }
}
