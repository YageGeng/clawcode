use std::io::Write;

use protocol::{EntryId, IdGenerator, IdKind, ToolExposure, ToolSelection};
use store::{EntryKind, NewEntry, SessionEntry};

use super::state::{SessionToolSnapshot, SessionToolState};
use crate::KernelError;
use crate::runtime::transcript::SessionTranscript;

/// Actual outcome of an incremental search load, generated after commit-time validation.
#[derive(Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::runtime) struct ToolLoadReport {
    pub loaded: Vec<String>,
    pub already_loaded: Vec<String>,
    pub skipped: Vec<ToolLoadSkip>,
}

/// A candidate that could not be loaded from the current catalog.
#[derive(serde::Serialize)]
pub(in crate::runtime) struct ToolLoadSkip {
    pub name: String,
    pub reason: ToolLoadSkipReason,
}

/// Stable reasons that avoid leaking hidden descriptions or serialized schemas.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::runtime) enum ToolLoadSkipReason {
    Unavailable,
    Hidden,
    Disabled,
    NotDeferred,
    DefinitionTooLarge,
    BudgetExceeded,
}

/// Counts a definition without allocating a second serialized schema.
struct DefinitionBytes(usize);

impl Write for DefinitionBytes {
    /// Stops serialization as soon as this definition exceeds the entire per-load budget.
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        if self.0 > 32 * 1024 {
            return Err(std::io::Error::other(
                "definition exceeds load budget",
            ));
        }
        Ok(bytes.len())
    }

    /// Counting requires no flush operation.
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Borrows exactly the state, transcript and identifier source needed for one selection commit.
pub(in crate::runtime) struct ToolSelectionUpdate<'a> {
    tools: &'a SessionToolState,
    transcript: &'a SessionTranscript,
    ids: &'a dyn IdGenerator,
}

impl<'a> ToolSelectionUpdate<'a> {
    /// Persists an inherited policy including disabled tools not yet registered.
    pub(in crate::runtime) fn adopt(
        &self,
        selection: ToolSelection,
    ) -> Result<(), KernelError> {
        self.tools.update(|next| {
            next.selection = selection;
            self.persist(next)
        })
    }

    /// Binds a synchronous selection operation without acquiring the Session Run gate.
    pub(in crate::runtime) fn new(
        tools: &'a SessionToolState,
        transcript: &'a SessionTranscript,
        ids: &'a dyn IdGenerator,
    ) -> Self {
        Self {
            tools,
            transcript,
            ids,
        }
    }

    /// Commits a complete selection only for candidates visible in the observed snapshot.
    pub(in crate::runtime) fn replace(
        &self,
        observed: &SessionToolSnapshot,
        names: Vec<String>,
    ) -> Result<(), KernelError> {
        self.tools.update(|next| {
            let previous = next.selection.clone();
            next.replace_selection(observed, names)?;
            if next.selection != previous {
                self.persist(next)?;
            }
            Ok(())
        })
    }

    /// Merges ranked candidates against the latest policy and a per-call schema byte budget.
    pub(in crate::runtime) fn load(
        &self,
        names: &[String],
    ) -> Result<ToolLoadReport, KernelError> {
        self.tools.update(|next| {
            let available = next.available_registry();
            let mut report = ToolLoadReport::default();
            let mut used = 0;
            let mut seen = std::collections::BTreeSet::new();
            for name in names {
                if !seen.insert(name) {
                    continue;
                }
                let tool = available.tool(name);
                let reason = match &tool {
                    None => Some(ToolLoadSkipReason::Unavailable),
                    Some(tool)
                        if tool.metadata().exposure == ToolExposure::Hidden =>
                    {
                        Some(ToolLoadSkipReason::Hidden)
                    }
                    Some(_) if next.selection.disabled.contains(name) => {
                        Some(ToolLoadSkipReason::Disabled)
                    }
                    Some(tool)
                        if tool.metadata().exposure
                            != ToolExposure::Deferred =>
                    {
                        Some(ToolLoadSkipReason::NotDeferred)
                    }
                    _ => None,
                };
                if let Some(reason) = reason {
                    report.skipped.push(ToolLoadSkip {
                        // A candidate hidden during ranking must not leak its identity in the report.
                        name: if matches!(&reason, ToolLoadSkipReason::Hidden) {
                            "<hidden>".into()
                        } else {
                            name.clone()
                        },
                        reason,
                    });
                    continue;
                }
                if next.selection.loaded.contains(name) {
                    report.already_loaded.push(name.clone());
                    continue;
                }
                let mut size = DefinitionBytes(0);
                let definition =
                    tool.expect("validated candidate").definition();
                if serde_json::to_writer(&mut size, &definition).is_err() {
                    report.skipped.push(ToolLoadSkip {
                        name: name.clone(),
                        reason: ToolLoadSkipReason::DefinitionTooLarge,
                    });
                } else if used + size.0 > 32 * 1024 {
                    report.skipped.push(ToolLoadSkip {
                        name: name.clone(),
                        reason: ToolLoadSkipReason::BudgetExceeded,
                    });
                } else {
                    used += size.0;
                    next.selection.loaded.insert(name.clone());
                    report.loaded.push(name.clone());
                }
            }
            if !report.loaded.is_empty() {
                self.persist(next)?;
            }
            Ok(report)
        })
    }

    /// Appends and synchronizes compatibility and authoritative projections before publication.
    fn persist(
        &self,
        snapshot: &SessionToolSnapshot,
    ) -> Result<(), KernelError> {
        let id = EntryId::try_from(self.ids.next(IdKind::Entry))
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        self.transcript.commit_setting(NewEntry { id, kind: EntryKind::ActiveToolsChange, payload: serde_json::json!({ "activeTools": snapshot.active_names(), "selection": snapshot.selection }) })
    }

    /// Replays full branch settings, including legacy explicit selections and empty roots.
    pub(in crate::runtime) fn restore(
        entries: &[SessionEntry],
        catalog: &SessionToolSnapshot,
    ) -> Result<ToolSelection, KernelError> {
        let mut selection = ToolSelection::default();
        for entry in entries
            .iter()
            .filter(|entry| entry.kind == EntryKind::ActiveToolsChange)
        {
            if let Some(value) = entry.payload.get("selection") {
                selection = serde_json::from_value(value.clone())?;
                if selection.version != 1
                    || !selection.loaded.is_disjoint(&selection.disabled)
                {
                    return Err(KernelError::Protocol(
                        "unsupported or overlapping tool selection".into(),
                    ));
                }
            } else {
                let names: Vec<String> = serde_json::from_value(
                    entry.payload.get("activeTools").cloned().ok_or_else(
                        || {
                            KernelError::Protocol(
                                "active-tools change has no selection".into(),
                            )
                        },
                    )?,
                )?;
                selection = names.into();
                catalog.migrate_selection(&mut selection);
                selection.disabled = catalog
                    .available_names()
                    .into_iter()
                    .filter(|name| !selection.loaded.contains(name))
                    .collect();
            }
        }
        catalog.migrate_selection(&mut selection);
        Ok(selection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::tools::{AgentTool, ToolError, ToolExecutionContext, ToolRegistry};
    use async_trait::async_trait;
    use protocol::{ToolCall, ToolDefinition, ToolMetadata, ToolResult};
    use std::sync::Arc;

    /// A deferred test tool with a complete schema and immutable host metadata.
    struct DeferredTool(&'static str);

    #[async_trait]
    impl AgentTool for DeferredTool {
        /// Supplies a small schema so concurrent loads exercise selection rather than size limits.
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.0.into(),
                description: "Deferred tool".into(),
                parameters: if self.0 == "huge" {
                    serde_json::json!({"type":"object","description":"x".repeat(40 * 1024)})
                } else {
                    serde_json::json!({"type":"object"})
                },
            }
        }
        /// Exposes the same deferred policy as the MCP adapter.
        fn metadata(&self) -> &ToolMetadata {
            static METADATA: std::sync::LazyLock<ToolMetadata> =
                std::sync::LazyLock::new(|| {
                    ToolMetadata::builder()
                        .exposure(ToolExposure::Deferred)
                        .build()
                });
            &METADATA
        }
        /// Selection tests must never invoke remote work.
        async fn execute(
            &self,
            _call: ToolCall,
            _context: &ToolExecutionContext,
        ) -> Result<ToolResult, ToolError> {
            unreachable!("loading must not execute")
        }
    }

    /// Concurrent incremental updates persist the union and already-loaded matches produce no entry.
    #[test]
    fn concurrent_loads_merge_and_replay() {
        let root = tempfile::tempdir().expect("directory");
        let factory = store::JsonlStoreFactory::new(
            root.path(),
            Arc::new(store::SystemClock),
        );
        let store = store::StoreFactory::create(
            &factory,
            store::SessionCreateOptions {
                session_id: protocol::SessionId::try_from("loads").expect("id"),
                cwd: root.path().into(),
                parent_session_id: None,
            },
        )
        .expect("store");
        let transcript = SessionTranscript::new(
            store,
            protocol::LaneId::try_from("main").expect("lane"),
            Vec::new(),
        );
        let mut registry = ToolRegistry::default();
        for name in ["first", "second", "huge"] {
            registry
                .register(Arc::new(DeferredTool(name)))
                .expect("unique");
        }
        let tools = SessionToolState::new(
            ToolRegistry::default(),
            ToolRegistry::default(),
            registry,
            ToolSelection::default(),
        )
        .expect("tools");
        let ids = crate::NanoidIdGenerator;
        let index_before =
            Arc::clone(&tools.snapshot().expect("snapshot").search_index);
        std::thread::scope(|scope| {
            for name in ["first", "second"] {
                let tools = &tools;
                let transcript = &transcript;
                let ids = &ids;
                scope.spawn(move || {
                    let report =
                        ToolSelectionUpdate::new(tools, transcript, ids)
                            .load(&[name.into()])
                            .expect("load");
                    assert_eq!(report.loaded, vec![name]);
                });
            }
        });
        let snapshot = tools.snapshot().expect("snapshot");
        assert!(Arc::ptr_eq(&index_before, &snapshot.search_index));
        assert_eq!(snapshot.active_names(), vec!["first", "second"]);
        let branch = transcript.active_branch().expect("branch");
        assert_eq!(
            ToolSelectionUpdate::restore(&branch, &snapshot).expect("restore"),
            snapshot.selection
        );
        assert_eq!(
            ToolSelectionUpdate::restore(&[], &snapshot).expect("empty root"),
            ToolSelection::default()
        );
        let report = ToolSelectionUpdate::new(&tools, &transcript, &ids)
            .load(&["first".into()])
            .expect("reload");
        assert_eq!(report.already_loaded, vec!["first"]);
        assert_eq!(
            transcript.active_branch().expect("branch").len(),
            branch.len()
        );
        let report = ToolSelectionUpdate::new(&tools, &transcript, &ids)
            .load(&["huge".into()])
            .expect("oversized schema");
        assert!(report.loaded.is_empty());
        assert!(matches!(
            report.skipped.first().map(|skip| &skip.reason),
            Some(ToolLoadSkipReason::DefinitionTooLarge)
        ));
        assert_eq!(
            transcript.active_branch().expect("branch").len(),
            branch.len()
        );
        tools
            .replace_mcp_partition(ToolRegistry::default())
            .expect("catalog withdrawn after ranking");
        let report = ToolSelectionUpdate::new(&tools, &transcript, &ids)
            .load(&["first".into()])
            .expect("revalidate stale candidate");
        assert!(report.loaded.is_empty());
        assert!(matches!(
            report.skipped.first().map(|skip| &skip.reason),
            Some(ToolLoadSkipReason::Unavailable)
        ));
        assert_eq!(
            transcript.active_branch().expect("branch").len(),
            branch.len()
        );
        assert!(!Arc::ptr_eq(
            &index_before,
            &tools.snapshot().expect("snapshot").search_index
        ));
    }
}
