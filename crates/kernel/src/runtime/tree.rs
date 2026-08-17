use store::SessionEntry;

use super::*;

const BRANCH_SUMMARY_INSTRUCTION: &str = "Summarize the abandoned conversation branch for another agent. Preserve user requirements, decisions, completed work, unresolved problems, exact identifiers, and important technical details. Return only the summary.";

/// Immutable topology and context captured before one tree navigation.
#[derive(typed_builder::TypedBuilder)]
struct TreeNavigationPlan {
    old_leaf_id: Option<EntryId>,
    target_id: EntryId,
    new_leaf_id: Option<EntryId>,
    common_ancestor_id: Option<EntryId>,
    entries_to_summarize: Vec<SessionEntry>,
}

impl TreeNavigationPlan {
    /// Resolves Pi navigation topology from one consistent Store snapshot.
    fn capture(
        store: &dyn SessionStore,
        lane: &LaneId,
        target_id: EntryId,
    ) -> Result<Self, KernelError> {
        let target = store.get_entry(&target_id).cloned().ok_or_else(|| {
            store::StoreError::EntryNotFound(target_id.clone())
        })?;
        let old_leaf_id = store.lane(lane).cloned();
        let old_branch = old_leaf_id
            .as_ref()
            .map(|leaf| store.branch(leaf))
            .transpose()?
            .unwrap_or_default();
        let target_branch = store.branch(&target_id)?;
        let common_length = old_branch
            .iter()
            .zip(&target_branch)
            .take_while(|(old, target)| old.id == target.id)
            .count();
        let common_ancestor_id = common_length
            .checked_sub(1)
            .and_then(|index| old_branch.get(index))
            .map(|entry| entry.id.clone());
        let entries_to_summarize = old_branch
            .into_iter()
            .skip(common_length)
            .collect::<Vec<_>>();
        // Pi reopens user input in the editor, so its parent becomes the new leaf.
        let new_leaf_id = if target.kind == EntryKind::Message
            && serde_json::from_value::<AgentMessage>(
                serde_json::Value::Object(target.payload.clone()),
            )
            .is_ok_and(|message| {
                matches!(
                    message.content,
                    MessageContent::User { .. }
                        | MessageContent::ExpandedUser { .. }
                )
            }) {
            target.parent_id
        } else {
            Some(target.id)
        };
        Ok(Self::builder()
            .old_leaf_id(old_leaf_id)
            .target_id(target_id)
            .new_leaf_id(new_leaf_id)
            .common_ancestor_id(common_ancestor_id)
            .entries_to_summarize(entries_to_summarize)
            .build())
    }

    /// Projects abandoned entries into the public extension tree shape.
    fn protocol_entries(&self) -> Vec<SessionTreeEntry> {
        self.entries_to_summarize
            .iter()
            .map(|entry| {
                SessionTreeEntry::builder()
                    .entry_id(entry.id.clone())
                    .parent_id(entry.parent_id.clone())
                    .kind(entry.kind.as_str().to_string())
                    .timestamp_ms(entry.timestamp_ms)
                    .payload(serde_json::Value::Object(entry.payload.clone()))
                    .build()
            })
            .collect()
    }

    /// Extracts model-facing messages from the abandoned branch in source order.
    fn messages(&self) -> Result<Vec<AgentMessage>, KernelError> {
        self.entries_to_summarize
            .iter()
            .filter(|entry| entry.kind == EntryKind::Message)
            .map(|entry| {
                serde_json::from_value(serde_json::Value::Object(
                    entry.payload.clone(),
                ))
                .map_err(KernelError::from)
            })
            .collect()
    }
}

impl Kernel {
    /// Moves the main lane without generating an abandoned-branch summary.
    pub async fn navigate_session(
        &self,
        session_id: &SessionId,
        target: Option<EntryId>,
    ) -> Result<SessionTreeSnapshot, KernelError> {
        match target {
            Some(target) => {
                self.navigate_session_with_summary(session_id, target, false)
                    .await
            }
            None => {
                let session = self.session(session_id)?;
                let _run_guard = session.acquire_operation().await?;
                {
                    let mut store = session
                        .store
                        .lock()
                        .map_err(|_poison_error| KernelError::Poisoned)?;
                    store.move_lane(&session.lane, None)?;
                }
                *session
                    .history
                    .lock()
                    .map_err(|_poison_error| KernelError::Poisoned)? =
                    Vec::new();
                let context = self.extension_context(&session, None, None)?;
                session
                    .extensions
                    .emit_session_tree(
                        &protocol::SessionTreeEvent::builder()
                            .old_leaf_id(context.snapshot.tree.leaf_id.clone())
                            .new_leaf_id(None)
                            .from_extension(false)
                            .build(),
                        &context,
                    )
                    .await;
                self.session_tree(session_id)
            }
        }
    }

    /// Navigates one tree target with optional Pi-compatible branch summarization.
    pub async fn navigate_session_with_summary(
        &self,
        session_id: &SessionId,
        target: EntryId,
        summarize: bool,
    ) -> Result<SessionTreeSnapshot, KernelError> {
        let session = self.session(session_id)?;
        let _run_guard = session.acquire_operation().await?;
        let plan = {
            let store = session
                .store
                .lock()
                .map_err(|_poison_error| KernelError::Poisoned)?;
            TreeNavigationPlan::capture(store.as_ref(), &session.lane, target)?
        };
        if plan.old_leaf_id.as_ref() == Some(&plan.target_id) {
            return self.session_tree(session_id);
        }
        let context = self.extension_context(&session, None, None)?;
        let decision = session
            .extensions
            .emit_session_before_tree(
                &protocol::SessionBeforeTreeEvent::builder()
                    .target_id(plan.target_id.clone())
                    .old_leaf_id(plan.old_leaf_id.clone())
                    .common_ancestor_id(plan.common_ancestor_id.clone())
                    .entries_to_summarize(plan.protocol_entries())
                    .user_wants_summary(summarize)
                    .custom_instructions(None)
                    .replace_instructions(false)
                    .label(None)
                    .build(),
                &context,
            )
            .await;
        if decision.cancel {
            return Err(KernelError::ExtensionBlocked(
                "session tree navigation cancelled".to_string(),
            ));
        }

        let extension_summary = summarize.then_some(decision.summary).flatten();
        let from_extension = extension_summary.is_some();
        let generated_summary = if summarize
            && extension_summary.is_none()
            && !plan.entries_to_summarize.is_empty()
        {
            let messages = plan.messages()?;
            if messages.is_empty() {
                None
            } else {
                let run_id =
                    RunId::try_from(self.id_generator.next(IdKind::Run))
                        .map_err(|error| {
                            KernelError::Protocol(error.to_string())
                        })?;
                let turn_id =
                    TurnId::try_from(self.id_generator.next(IdKind::Turn))
                        .map_err(|error| {
                            KernelError::Protocol(error.to_string())
                        })?;
                let cancellation = CancellationToken::new();
                let instruction = match decision.custom_instructions.as_deref()
                {
                    Some(custom)
                        if decision.replace_instructions == Some(true) =>
                    {
                        custom.to_string()
                    }
                    Some(custom) => {
                        format!("{BRANCH_SUMMARY_INSTRUCTION}\n\n{custom}")
                    }
                    None => BRANCH_SUMMARY_INSTRUCTION.to_string(),
                };
                Some(
                    self.generate_compaction_summary(
                        SummaryGeneration::builder()
                            .session(&session)
                            .run_id(&run_id)
                            .turn_id(&turn_id)
                            .timestamp(self.clock.now())
                            .messages(messages)
                            .cancellation(&cancellation)
                            .protocol(SummaryProtocol::Branch {
                                instruction: &instruction,
                            })
                            .build(),
                    )
                    .await?
                    .text,
                )
            }
        } else {
            None
        };
        let summary = extension_summary
            .as_ref()
            .map(|summary| summary.summary.clone())
            .or(generated_summary);
        let mut summary_entry = None;
        let history = {
            let mut store = session
                .store
                .lock()
                .map_err(|_poison_error| KernelError::Poisoned)?;
            store.move_lane(&session.lane, plan.new_leaf_id.clone())?;
            if let Some(summary) = summary {
                let entry = store.append_entry(
                    &session.lane,
                    NewEntry {
                        id: EntryId::try_from(self.id_generator.next(IdKind::Entry))
                            .map_err(|error| KernelError::Protocol(error.to_string()))?,
                        kind: EntryKind::BranchSummary,
                        payload: serde_json::json!({
                            "fromId": plan.new_leaf_id.as_ref().map(EntryId::as_str).unwrap_or("root"),
                            "summary": summary,
                            "details": extension_summary.as_ref().and_then(|summary| summary.details.clone()),
                            "usage": extension_summary.as_ref().and_then(|summary| summary.usage.clone()),
                            "fromHook": from_extension,
                        }),
                    },
                )?;
                if let Some(label) = decision.label.clone() {
                    store.set_label(entry.id.clone(), Some(label))?;
                }
                summary_entry = Some(
                    SessionTreeEntry::builder()
                        .entry_id(entry.id)
                        .parent_id(entry.parent_id)
                        .kind(entry.kind.as_str().to_string())
                        .timestamp_ms(entry.timestamp_ms)
                        .payload(serde_json::Value::Object(entry.payload))
                        .build(),
                );
            } else if let Some(label) = decision.label {
                store.set_label(plan.target_id.clone(), Some(label))?;
            }
            Self::history_from_store(store.as_ref(), &session.lane)?
        };
        *session
            .history
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)? = history;
        let new_leaf_id = {
            let store = session
                .store
                .lock()
                .map_err(|_poison_error| KernelError::Poisoned)?;
            store.lane(&session.lane).cloned()
        };
        let context = self.extension_context(&session, None, None)?;
        session
            .extensions
            .emit_session_tree(
                &protocol::SessionTreeEvent::builder()
                    .new_leaf_id(new_leaf_id)
                    .old_leaf_id(plan.old_leaf_id)
                    .summary_entry(summary_entry)
                    .from_extension(from_extension)
                    .build(),
                &context,
            )
            .await;
        self.session_tree(session_id)
    }
}
