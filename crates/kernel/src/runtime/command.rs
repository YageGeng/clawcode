use std::collections::{BTreeMap, BTreeSet};

use super::*;

mod builtin;
mod execution;
mod session_stats;

pub(super) use builtin::BuiltinSlashCommand;
pub(super) use execution::SlashCommandExecution;

/// Direct command selected before Extension input hooks and prompt expansion.
pub(super) enum DirectSlashCommand {
    /// Kernel-owned command with a reserved name.
    Builtin(BuiltinSlashCommand),
    /// Session-local command owned by one Extension.
    Extension(::extension::RegisteredCommand),
}

impl DirectSlashCommand {
    /// Returns the runtime source represented by this resolved command.
    pub(in crate::runtime) const fn source(
        &self,
    ) -> protocol::SlashCommandSource {
        match self {
            Self::Builtin(_) => protocol::SlashCommandSource::Builtin,
            Self::Extension(_) => protocol::SlashCommandSource::Extension,
        }
    }
}

/// Complete result of resolving the direct command precedence tiers.
pub(super) enum DirectSlashCommandResolution {
    /// One Builtin or Extension command is executable.
    Command(DirectSlashCommand),
    /// A recognized command produced a stable rejection before execution.
    Rejected {
        /// Runtime source that owns the rejected name.
        source: protocol::SlashCommandSource,
        /// Client-safe command-domain failure.
        error: protocol::SlashCommandError,
    },
    /// No direct command matched and normal input processing must continue.
    NotFound,
}

impl DirectSlashCommandResolution {
    /// Returns the recognized source or `None` when normal input must continue.
    const fn source(&self) -> Option<protocol::SlashCommandSource> {
        match self {
            Self::Command(command) => Some(command.source()),
            Self::Rejected { source, .. } => Some(*source),
            Self::NotFound => None,
        }
    }
}

impl Session {
    /// Resolves direct commands in Builtin then Extension precedence order.
    pub(super) fn resolve_direct_slash_command(
        &self,
        invocation: &protocol::SlashCommandInvocation,
    ) -> Result<DirectSlashCommandResolution, KernelError> {
        if let Some(command) = BuiltinSlashCommand::resolve(&invocation.name) {
            return Ok(DirectSlashCommandResolution::Command(
                DirectSlashCommand::Builtin(command),
            ));
        }
        match self
            .extensions
            .command_snapshot()
            .map_err(|error| KernelError::ExtensionBlocked(error.to_string()))?
            .resolve(&invocation.name)
        {
            Ok(command) => Ok(DirectSlashCommandResolution::Command(
                DirectSlashCommand::Extension(command),
            )),
            Err(::extension::DynamicRegistryError::CommandNotFound(_)) => {
                Ok(DirectSlashCommandResolution::NotFound)
            }
            Err(::extension::DynamicRegistryError::AmbiguousCommand {
                name,
                candidates,
            }) => Ok(DirectSlashCommandResolution::Rejected {
                source: protocol::SlashCommandSource::Extension,
                error: protocol::SlashCommandError::Ambiguous {
                    command: name,
                    candidates,
                },
            }),
            Err(error) => Err(KernelError::ExtensionBlocked(error.to_string())),
        }
    }
}

impl Kernel {
    /// Projects the effective Session command snapshot in runtime precedence order.
    pub fn available_commands(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<protocol::SlashCommandDefinition>, KernelError> {
        let session = self.session(session_id)?;
        let registered = session
            .extensions
            .command_snapshot()
            .map_err(|error| KernelError::ExtensionBlocked(error.to_string()))?
            .commands();
        let mut short_counts = BTreeMap::<String, usize>::new();
        for command in &registered {
            *short_counts
                .entry(command.definition.name.clone())
                .or_default() += 1;
        }

        let mut commands = BuiltinSlashCommand::definitions().to_vec();
        let mut used_names = commands
            .iter()
            .map(|command| command.name.clone())
            .collect::<BTreeSet<_>>();
        let mut extension_commands = registered
            .iter()
            .map(|command| {
                let name = command.qualified_name();
                used_names.insert(name.clone());
                protocol::SlashCommandDefinition::builder()
                    .name(name)
                    .description(
                        command
                            .definition
                            .description
                            .clone()
                            .unwrap_or_default(),
                    )
                    .argument_hint(command.definition.argument_hint.clone())
                    .source(protocol::SlashCommandSource::Extension)
                    .qualified_name(None)
                    .alias_kind(protocol::SlashCommandAliasKind::Canonical)
                    .build()
            })
            .collect::<Vec<_>>();
        extension_commands.extend(registered.iter().filter_map(|command| {
            let name = command.definition.name.clone();
            (short_counts.get(&name) == Some(&1)
                && used_names.insert(name.clone()))
            .then(|| {
                protocol::SlashCommandDefinition::builder()
                    .name(name)
                    .description(
                        command
                            .definition
                            .description
                            .clone()
                            .unwrap_or_default(),
                    )
                    .argument_hint(command.definition.argument_hint.clone())
                    .source(protocol::SlashCommandSource::Extension)
                    .qualified_name(Some(command.qualified_name()))
                    .alias_kind(protocol::SlashCommandAliasKind::Short)
                    .build()
            })
        }));
        // Ambiguous Extension short names are not advertised as aliases, but
        // runtime dispatch still rejects them before Skill or Template lookup.
        // Reserve those names so the ACP snapshot cannot advertise an
        // unreachable lower-priority command.
        used_names.extend(
            short_counts
                .iter()
                .filter(|(_name, count)| **count > 1)
                .map(|(name, _count)| name.clone()),
        );
        extension_commands.sort_by(|left, right| left.name.cmp(&right.name));

        let mut skill_commands = session
            .skill_catalog()?
            .map_or_else(Vec::new, |catalog| catalog.descriptors())
            .into_iter()
            .filter_map(|skill| {
                let name = format!("skill:{}", skill.name);
                used_names.insert(name.clone()).then(|| {
                    protocol::SlashCommandDefinition::builder()
                        .name(name)
                        .description(skill.description)
                        .argument_hint(Some("[arguments]".to_string()))
                        .source(protocol::SlashCommandSource::Skill)
                        .qualified_name(None)
                        .alias_kind(protocol::SlashCommandAliasKind::Canonical)
                        .build()
                })
            })
            .collect::<Vec<_>>();
        skill_commands.sort_by(|left, right| left.name.cmp(&right.name));

        let mut template_commands = session
            .prompt_session()?
            .templates()
            .into_iter()
            .filter_map(|template| {
                used_names.insert(template.name.clone()).then(|| {
                    protocol::SlashCommandDefinition::builder()
                        .name(template.name)
                        .description(template.description)
                        .argument_hint(template.argument_hint)
                        .source(protocol::SlashCommandSource::PromptTemplate)
                        .qualified_name(None)
                        .alias_kind(protocol::SlashCommandAliasKind::Canonical)
                        .build()
                })
            })
            .collect::<Vec<_>>();
        template_commands.sort_by(|left, right| left.name.cmp(&right.name));

        commands.extend(extension_commands);
        commands.extend(skill_commands);
        commands.extend(template_commands);
        Ok(commands)
    }

    /// Builds one correlated complete command snapshot event for ACP delivery.
    pub fn available_commands_event(
        &self,
        session_id: &SessionId,
    ) -> Result<AgentEvent, KernelError> {
        let session = self.session(session_id)?;
        Ok(AgentEvent {
            metadata: EventMetadata {
                turn_id: TurnId::try_from(self.id_generator.next(IdKind::Turn))
                    .map_err(|error| {
                        KernelError::Protocol(error.to_string())
                    })?,
                timestamp_ms: self.clock.now(),
                sequence: session.execution.next_sequence()?,
            },
            payload: AgentEventPayload::AvailableCommandsChanged {
                commands: self.available_commands(session_id)?,
            },
        })
    }
}
