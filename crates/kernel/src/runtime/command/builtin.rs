use std::sync::Arc;

use protocol::{
    ContentBlock, SessionTitle, SlashCommandAliasKind, SlashCommandDefinition,
    SlashCommandOutcome, SlashCommandOutput, SlashCommandSource,
    SlashCommandStatus,
};

use super::super::{AgentEventPayload, EventEmitter, Kernel, KernelError};
use super::execution::SlashCommandExecution;
use super::session_stats::SessionStatistics;
use tokio_util::sync::CancellationToken;

/// Kernel-owned commands available without an Extension or resource catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runtime) enum BuiltinSlashCommand {
    /// Manually compacts the active Session context.
    Compact,
    /// Sets or reports the persisted Session display name.
    Name,
    /// Reports persisted Session information and usage.
    Session,
}

impl BuiltinSlashCommand {
    /// Returns definitions for every server-owned command in lexical order.
    pub(super) fn definitions() -> [SlashCommandDefinition; 3] {
        [
            Self::Compact.definition(
                "Manually compact the session context",
                Some("[custom instructions]"),
            ),
            Self::Name.definition(
                "Set or show the session display name",
                Some("[name]"),
            ),
            Self::Session
                .definition("Show session information and statistics", None),
        ]
    }

    /// Resolves one reserved Builtin name without allocating aliases.
    pub(super) fn resolve(name: &str) -> Option<Self> {
        match name {
            "compact" => Some(Self::Compact),
            "name" => Some(Self::Name),
            "session" => Some(Self::Session),
            _ => None,
        }
    }

    /// Builds the complete protocol definition for one Builtin command.
    fn definition(
        self,
        description: &str,
        argument_hint: Option<&str>,
    ) -> SlashCommandDefinition {
        SlashCommandDefinition::builder()
            .name(self.name().to_string())
            .description(description.to_string())
            .argument_hint(argument_hint.map(str::to_string))
            .source(SlashCommandSource::Builtin)
            .qualified_name(None)
            .alias_kind(SlashCommandAliasKind::Canonical)
            .build()
    }

    /// Returns the reserved protocol name for this Builtin command.
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Name => "name",
            Self::Session => "session",
        }
    }

    /// Builds one terminal Builtin output using the shared protocol message shape.
    fn output(
        self,
        status: SlashCommandStatus,
        text: String,
    ) -> SlashCommandOutcome {
        SlashCommandOutcome::Handled {
            output: Some(
                SlashCommandOutput::builder()
                    .command(self.name().to_string())
                    .source(SlashCommandSource::Builtin)
                    .status(status)
                    .blocks(vec![ContentBlock::Text { text }])
                    .build(),
            ),
        }
    }
}

impl Kernel {
    /// Executes one Kernel-owned command under the direct command operation gate.
    pub(super) async fn execute_builtin_slash_command(
        &self,
        command: BuiltinSlashCommand,
        execution: &SlashCommandExecution<'_>,
        emitter: &EventEmitter,
        cancellation: &CancellationToken,
    ) -> Result<SlashCommandOutcome, KernelError> {
        match command {
            BuiltinSlashCommand::Compact => {
                self.perform_compaction(
                    super::super::CompactionExecution::builder()
                        .session_id(execution.session_id)
                        .session(execution.session)
                        .sink(Arc::clone(&execution.sink))
                        .reason(protocol::CompactionReason::Manual)
                        .cancellation(cancellation)
                        .identity(Some(super::super::CompactionIdentity::new(
                            execution.run_id.clone(),
                            execution.turn_id.clone(),
                            execution.started_at,
                        )))
                        .instruction(
                            (!execution.invocation.arguments.is_empty()).then(
                                || execution.invocation.arguments.clone(),
                            ),
                        )
                        .build(),
                )
                .await?;
                // The persisted compaction card is the single success result;
                // emitting a command output would duplicate it in replay.
                Ok(SlashCommandOutcome::Handled { output: None })
            }
            BuiltinSlashCommand::Name => {
                if execution.invocation.arguments.is_empty() {
                    let name = execution.session.transcript.name()?;
                    return Ok(name.map_or_else(
                        || {
                            command.output(
                                SlashCommandStatus::Failed,
                                "Usage: /name <name>".to_string(),
                            )
                        },
                        |name| {
                            command.output(
                                SlashCommandStatus::Succeeded,
                                format!("Session name: {name}"),
                            )
                        },
                    ));
                }
                let title = match SessionTitle::try_from(
                    execution.invocation.arguments.clone(),
                ) {
                    Ok(title) => title,
                    Err(_error) => {
                        return Ok(SlashCommandOutcome::Rejected(
                            protocol::SlashCommandError::InvalidArguments {
                                command: command.name().to_string(),
                                usage: "/name <name> (maximum 120 characters)"
                                    .to_string(),
                            },
                        ));
                    }
                };
                self.rename_session(execution.session_id, title.clone())
                    .await?;
                emitter
                    .emit(
                        execution.turn_id.clone(),
                        AgentEventPayload::SessionTitleChanged {
                            title: title.as_str().to_string(),
                        },
                    )
                    .await?;
                Ok(command.output(
                    SlashCommandStatus::Succeeded,
                    format!("Session name set: {}", title.as_str()),
                ))
            }
            BuiltinSlashCommand::Session => {
                if !execution.invocation.arguments.is_empty() {
                    return Ok(SlashCommandOutcome::Rejected(
                        protocol::SlashCommandError::InvalidArguments {
                            command: command.name().to_string(),
                            usage: "/session".to_string(),
                        },
                    ));
                }
                let name = execution.session.transcript.name()?;
                let transcript =
                    self.session_transcript(execution.session_id)?;
                Ok(command.output(
                    SlashCommandStatus::Succeeded,
                    SessionStatistics::from_transcript(
                        execution.session_id.clone(),
                        name,
                        &transcript,
                    )
                    .to_string(),
                ))
            }
        }
    }
}
