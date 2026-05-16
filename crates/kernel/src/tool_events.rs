//! Tool lifecycle emitters for structured display events.

use tokio::sync::mpsc;

use protocol::{Event, FileChange, FileChangeItem, FileChangeStatus, SessionId, TurnId, TurnItem};

/// Minimal context required to emit tool lifecycle events.
pub(crate) struct ToolEventCtx<'a> {
    /// Session that owns the tool call.
    pub session_id: &'a SessionId,
    /// Turn that owns the tool call.
    pub turn_id: &'a TurnId,
    /// Stable tool call id.
    pub call_id: &'a str,
    /// Kernel event stream sender.
    pub tx_event: &'a mpsc::UnboundedSender<Event>,
}

/// Emits structured lifecycle events for tools with rich display payloads.
pub(crate) enum ToolEmitter {
    /// File-changing tool lifecycle, such as apply_patch.
    FileChange { title: String },
}

impl ToolEmitter {
    /// Create a file-change emitter with a human-facing title.
    pub(crate) fn file_change(title: impl Into<String>) -> Self {
        Self::FileChange {
            title: title.into(),
        }
    }

    /// Emit the started event for this tool lifecycle.
    pub(crate) fn begin(&self, ctx: &ToolEventCtx<'_>) {
        match self {
            Self::FileChange { title } => {
                let item = file_change_item(
                    ctx.call_id,
                    title.clone(),
                    Vec::new(),
                    FileChangeStatus::InProgress,
                    None,
                );
                let _ = ctx.tx_event.send(Event::item_started(
                    ctx.session_id.clone(),
                    ctx.turn_id.clone(),
                    TurnItem::FileChange(item),
                ));
            }
        }
    }

    /// Emit the completed event for a file-changing tool.
    pub(crate) fn complete_file_change(
        &self,
        ctx: &ToolEventCtx<'_>,
        changes: Vec<FileChange>,
        model_output: String,
    ) {
        match self {
            Self::FileChange { title } => {
                let item = file_change_item(
                    ctx.call_id,
                    title.clone(),
                    changes,
                    FileChangeStatus::Completed,
                    Some(model_output),
                );
                let _ = ctx.tx_event.send(Event::item_completed(
                    ctx.session_id.clone(),
                    ctx.turn_id.clone(),
                    TurnItem::FileChange(item),
                ));
            }
        }
    }

    /// Emit the failed event for a file-changing tool.
    pub(crate) fn fail_file_change(&self, ctx: &ToolEventCtx<'_>, model_output: String) {
        match self {
            Self::FileChange { title } => {
                let item = file_change_item(
                    ctx.call_id,
                    title.clone(),
                    Vec::new(),
                    FileChangeStatus::Failed,
                    Some(model_output),
                );
                let _ = ctx.tx_event.send(Event::item_completed(
                    ctx.session_id.clone(),
                    ctx.turn_id.clone(),
                    TurnItem::FileChange(item),
                ));
            }
        }
    }
}

/// Build a typed file-change item while keeping builder calls localized.
fn file_change_item(
    call_id: &str,
    title: String,
    changes: Vec<FileChange>,
    status: FileChangeStatus,
    model_output: Option<String>,
) -> FileChangeItem {
    if let Some(model_output) = model_output {
        FileChangeItem::builder()
            .id(call_id.to_string())
            .title(title)
            .changes(changes)
            .status(status)
            .model_output(model_output)
            .build()
    } else {
        FileChangeItem::builder()
            .id(call_id.to_string())
            .title(title)
            .changes(changes)
            .status(status)
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_change_emitter_preserves_turn_id() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let session_id = SessionId("session-1".to_string());
        let turn_id = TurnId("turn-1".to_string());
        let ctx = ToolEventCtx {
            session_id: &session_id,
            turn_id: &turn_id,
            call_id: "call-1",
            tx_event: &tx,
        };

        ToolEmitter::file_change("Apply patch").begin(&ctx);

        let event = rx.try_recv().expect("emitted item event");
        assert!(matches!(
            event,
            Event::ItemStarted { turn_id: decoded, .. } if decoded == turn_id
        ));
    }
}
