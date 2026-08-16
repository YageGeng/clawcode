use std::collections::{BTreeMap, BTreeSet};

use super::*;

impl Kernel {
    /// Projects the effective Session command snapshot in runtime precedence order.
    pub fn available_commands(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<protocol::AvailableAgentCommand>, KernelError> {
        let session = self.session(session_id)?;
        let registered = session
            .commands
            .snapshot()
            .map_err(|error| KernelError::ExtensionBlocked(error.to_string()))?
            .commands();
        let mut short_counts = BTreeMap::<String, usize>::new();
        for command in &registered {
            *short_counts
                .entry(command.definition.name.clone())
                .or_default() += 1;
        }

        let mut used_names = BTreeSet::new();
        let mut extension_commands = registered
            .iter()
            .map(|command| {
                let name = command.qualified_name();
                used_names.insert(name.clone());
                protocol::AvailableAgentCommand::builder()
                    .name(name)
                    .description(
                        command
                            .definition
                            .description
                            .clone()
                            .unwrap_or_default(),
                    )
                    .argument_hint(command.definition.argument_hint.clone())
                    .kind(protocol::AvailableAgentCommandKind::Extension)
                    .build()
            })
            .collect::<Vec<_>>();
        extension_commands.extend(registered.iter().filter_map(|command| {
            let name = command.definition.name.clone();
            (short_counts.get(&name) == Some(&1)
                && used_names.insert(name.clone()))
            .then(|| {
                protocol::AvailableAgentCommand::builder()
                    .name(name)
                    .description(
                        command
                            .definition
                            .description
                            .clone()
                            .unwrap_or_default(),
                    )
                    .argument_hint(command.definition.argument_hint.clone())
                    .kind(protocol::AvailableAgentCommandKind::Extension)
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
                    protocol::AvailableAgentCommand::builder()
                        .name(name)
                        .description(skill.description)
                        .argument_hint(Some("[arguments]".to_string()))
                        .kind(protocol::AvailableAgentCommandKind::Skill)
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
                    protocol::AvailableAgentCommand::builder()
                        .name(template.name)
                        .description(template.description)
                        .argument_hint(template.argument_hint)
                        .kind(
                            protocol::AvailableAgentCommandKind::PromptTemplate,
                        )
                        .build()
                })
            })
            .collect::<Vec<_>>();
        template_commands.sort_by(|left, right| left.name.cmp(&right.name));

        extension_commands.extend(skill_commands);
        extension_commands.extend(template_commands);
        Ok(extension_commands)
    }

    /// Builds one correlated complete command snapshot event for ACP delivery.
    pub fn available_commands_event(
        &self,
        session_id: &SessionId,
    ) -> Result<AgentEvent, KernelError> {
        let session = self.session(session_id)?;
        let raw_sequence = session
            .event_sequence
            .fetch_add(1, Ordering::Relaxed)
            .checked_add(1)
            .ok_or_else(|| {
                KernelError::Protocol("event sequence overflow".to_string())
            })?;
        Ok(AgentEvent {
            metadata: EventMetadata {
                turn_id: TurnId::try_from(self.id_generator.next(IdKind::Turn))
                    .map_err(|error| {
                        KernelError::Protocol(error.to_string())
                    })?,
                timestamp_ms: self.clock.now(),
                sequence: Sequence::try_from(raw_sequence).map_err(
                    |error| KernelError::Protocol(error.to_string()),
                )?,
            },
            payload: AgentEventPayload::AvailableCommandsChanged {
                commands: self.available_commands(session_id)?,
            },
        })
    }
}
