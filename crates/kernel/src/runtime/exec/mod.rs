//! Unified exec wiring: per-Session process managers, their Kernel tools,
//! lifecycle events, and the queries clients use to list and stop processes.

mod sessions;
mod tools;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, PoisonError, Weak};

use ::tools::exec::{ExecEventSink, ExecOutputUpdate};
use async_trait::async_trait;
use protocol::{
    AgentEventPayload, AgentMessage, EntryId, ExecExit, ExecProcessInfo,
    IdKind, MessageContent, SessionId, ToolCallId, ToolResultDetails, TurnId,
};
use store::{EntryKind, NewEntry};

pub(in crate::runtime) use sessions::CommandSessionsNote;

use super::session::ClawcodeEntry;
use super::{Kernel, KernelError, Session};

/// One persisted background exit, stored as a clawcode custom entry that
/// never enters model context and closes the terminal card on replay.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub(in crate::runtime) struct ExecExitEntry(pub(in crate::runtime) ExecExit);

impl ClawcodeEntry for ExecExitEntry {
    const CUSTOM_TYPE: &'static str = "exec_exit";
}

/// Registers `exec_command` and `write_stdin` when the Session has a process manager.
pub(in crate::runtime) fn register(
    runtime: &Arc<Session>,
    kernel: &Kernel,
) -> Result<(), KernelError> {
    if runtime.exec.is_none() {
        return Ok(());
    }
    runtime
        .tools
        .register_kernel(Arc::new(tools::ExecCommandTool {
            session: Arc::downgrade(runtime),
            kernel: kernel.clone(),
        }))?;
    runtime
        .tools
        .register_kernel(Arc::new(tools::WriteStdinTool {
            session: Arc::downgrade(runtime),
        }))?;
    Ok(())
}

/// Routes one Session's process reports into its event stream.
pub(in crate::runtime) struct SessionExecSink {
    session: Weak<Session>,
    kernel: Kernel,
    session_id: SessionId,
}

impl SessionExecSink {
    /// Emits on the active Run's sink when one exists, else the Session's.
    async fn emit(&self, turn_id: TurnId, payload: AgentEventPayload) {
        let Some(session) = self.session.upgrade() else {
            tracing::debug!(
                "dropped exec event for released session {}",
                self.session_id
            );
            return;
        };
        match self.kernel.observer_emitter(session, &self.session_id) {
            Ok((_active_turn, emitter)) => {
                if let Err(error) = emitter.emit(turn_id, payload).await {
                    tracing::warn!(
                        "failed to emit exec event for session {}: {}",
                        self.session_id,
                        error
                    );
                }
            }
            Err(error) => tracing::warn!(
                "could not route exec event for session {}: {}",
                self.session_id,
                error
            ),
        }
    }
}

#[async_trait]
impl ExecEventSink for SessionExecSink {
    /// Publishes a transcript snapshot on the process's own turn.
    async fn output(&self, update: ExecOutputUpdate) {
        self.emit(
            update.turn_id,
            AgentEventPayload::ExecOutput {
                tool_call_id: update.tool_call_id,
                process_id: update.process_id,
                output: update.output,
            },
        )
        .await;
    }

    /// Stores, then announces, a background exit on the process's own turn.
    async fn exited(&self, exit: ExecExit) {
        if let Some(session) = self.session.upgrade()
            && let Err(error) = self.kernel.persist_exec_exit(&session, &exit)
        {
            tracing::warn!(
                "failed to persist exec exit of process {} in session {}: {}",
                exit.process_id,
                self.session_id,
                error
            );
        }
        self.emit(exit.turn_id.clone(), AgentEventPayload::ExecExited { exit })
            .await;
    }
}

impl Kernel {
    /// Appends one exit record without changing model context.
    fn persist_exec_exit(
        &self,
        session: &Session,
        exit: &ExecExit,
    ) -> Result<(), KernelError> {
        let id = EntryId::try_from(self.id_generator.next(IdKind::Entry))
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        session.transcript.append_store_only_entry(NewEntry {
            id,
            kind: EntryKind::Custom,
            payload: ExecExitEntry(exit.clone()).payload()?,
        })?;
        session.sync_store()
    }

    /// Records as lost every backgrounded process on the active branch that
    /// has no exit record, so a reopened Session never shows it running.
    pub(in crate::runtime) fn reconcile_lost_exec(
        &self,
        session: &Session,
    ) -> Result<(), KernelError> {
        let mut open: BTreeMap<ToolCallId, (TurnId, i32)> = BTreeMap::new();
        let mut closed: BTreeSet<ToolCallId> = BTreeSet::new();
        for entry in session.transcript.active_branch()? {
            if let Some(ExecExitEntry(exit)) = ExecExitEntry::from_entry(&entry)
            {
                closed.insert(exit.tool_call_id);
                continue;
            }
            // Decoding every message is slow on long branches; only exec
            // results, tagged `exec` in their details, can open a terminal.
            let exec_result = entry.kind == EntryKind::Message
                && entry
                    .payload
                    .get("content")
                    .and_then(|content| content.pointer("/details/type"))
                    .and_then(serde_json::Value::as_str)
                    == Some("exec");
            if !exec_result {
                continue;
            }
            let Ok(message) = serde_json::from_value::<AgentMessage>(
                serde_json::Value::Object(entry.payload),
            ) else {
                continue;
            };
            if let MessageContent::ToolResult {
                details:
                    Some(ToolResultDetails::Exec {
                        process_id: Some(process_id),
                        terminal_call_id,
                        ..
                    }),
                ..
            } = &message.content
            {
                // The first result is the starting call, whose turn owns the card.
                open.entry(terminal_call_id.clone())
                    .or_insert((message.identity.turn_id.clone(), *process_id));
            }
        }
        for (tool_call_id, (turn_id, process_id)) in open {
            if closed.contains(&tool_call_id) {
                continue;
            }
            tracing::info!(
                "recorded exec process {} of call {} as lost",
                process_id,
                tool_call_id
            );
            self.persist_exec_exit(
                session,
                &ExecExit::builder()
                    .tool_call_id(tool_call_id)
                    .turn_id(turn_id)
                    .process_id(process_id)
                    .output(String::new())
                    .wall_time_ms(0)
                    .lost(true)
                    .build(),
            )?;
        }
        Ok(())
    }

    /// Lists a Session's running command sessions.
    pub async fn exec_processes(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<ExecProcessInfo>, KernelError> {
        let session = self.session(session_id)?;
        Ok(match &session.exec {
            Some(exec) => exec.list().await,
            None => Vec::new(),
        })
    }

    /// Kills one command session; `false` when the id is unknown.
    pub async fn terminate_exec_process(
        &self,
        session_id: &SessionId,
        process_id: i32,
    ) -> Result<bool, KernelError> {
        let session = self.session(session_id)?;
        Ok(match &session.exec {
            Some(exec) => exec.terminate(process_id).await,
            None => false,
        })
    }

    /// Kills every command session of a Session and returns how many ran.
    pub async fn clean_exec_processes(
        &self,
        session_id: &SessionId,
    ) -> Result<usize, KernelError> {
        let session = self.session(session_id)?;
        Ok(match &session.exec {
            Some(exec) => exec.terminate_all().await,
            None => 0,
        })
    }

    /// Kills the command sessions of every live Session, for application exit.
    pub async fn terminate_exec_processes(&self) -> usize {
        let sessions: Vec<Arc<Session>> = self
            .sessions
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(Arc::clone)
            .collect();
        // Sessions stop concurrently, so each one's report wait overlaps.
        let terminated: usize = futures::future::join_all(
            sessions
                .iter()
                .filter_map(|session| session.exec.as_ref())
                .map(|exec| exec.terminate_all()),
        )
        .await
        .into_iter()
        .sum();
        tracing::info!("terminated {} exec processes at shutdown", terminated);
        terminated
    }
}
