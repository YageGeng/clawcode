//! The actions on transcript messages (M4b spec §3): copy, Branch (edit an
//! earlier question and ask it again), and Fork (a new session from a
//! finished turn's last reply), and the marks deciding where they show.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use agent_client_protocol::v2::schema as wire;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    Disableable as _, Sizable as _, WindowExt as _, h_flex,
};
use gpui_kit::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, Window,
};

use super::{SessionView, SessionViewEvent};
use crate::backend::BackendError;
use crate::branch::MessageAuthor;
use crate::projection::{MessageRow, Role, RowKey, RunState, TranscriptRow};
use crate::session::SessionHandle;
use crate::ui::composer::attachments::Outgoing;

/// Which transcript messages carry a Fork action or a turn's change card,
/// worked out once per transcript change rather than once per row per frame.
#[derive(Default)]
pub struct MessageMarks {
    /// Replies a session can fork from.
    pub(super) fork_points: HashSet<wire::MessageId>,
    /// Each turn's last reply, with what its run changed.
    pub(super) turn_changes:
        HashMap<wire::MessageId, Arc<protocol::RunCheckpoint>>,
}

impl MessageMarks {
    /// The replies carrying a turn's change card.
    pub(super) fn card_replies(&self) -> HashSet<wire::MessageId> {
        self.turn_changes.keys().cloned().collect()
    }
}

impl From<&SessionHandle> for MessageMarks {
    /// The marks of a session's transcript and its runs.
    fn from(handle: &SessionHandle) -> Self {
        let projection = handle.projection();
        let turn_changes = projection
            .shown_runs(handle.checkpoints())
            .into_iter()
            .filter_map(|shown| {
                let anchor = shown.run.anchor_message_id.as_ref()?;
                Some((
                    wire::MessageId::new(anchor.to_string()),
                    Arc::clone(shown.run),
                ))
            })
            .collect();
        Self {
            fork_points: projection.fork_points(),
            turn_changes,
        }
    }
}

impl SessionView {
    /// Works the message marks out again from the session.
    pub(super) fn refresh_marks(&mut self, cx: &mut Context<'_, Self>) {
        self.marks = self
            .handle()
            .map(|handle| MessageMarks::from(handle.read(cx)))
            .unwrap_or_default();
    }

    /// The actions under a message (M4b spec §3): copy on every one, Branch
    /// on the user's questions, Fork on the last reply of a finished turn.
    /// They show while the pointer is over the message (`group`). Branch
    /// and Fork wait while a run is active, and a subagent shown in the
    /// panel offers copy only.
    pub(super) fn render_message_actions(
        &self,
        message: &MessageRow,
        group: &SharedString,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let id = message.message_id.clone();
        // A run, or an edit being branched, holds the session's history.
        let busy = self.branching
            || self.handle().is_some_and(|handle| {
                handle.read(cx).projection().run_state() != RunState::Idle
            });
        let busy_tip = rust_i18n::t!("Message.busy").into_owned();
        let copy = Clipboard::new(SharedString::from(format!("copy:{id}")))
            .value(SharedString::from(message.text.clone()));
        let branch =
            (!self.copy_only && message.role == Role::User).then(|| {
                let target = id.clone();
                Button::new(SharedString::from(format!("branch:{id}")))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Pencil)
                    .tooltip(if busy {
                        busy_tip.clone()
                    } else {
                        rust_i18n::t!("Message.branch").into_owned()
                    })
                    .disabled(busy)
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            this.begin_branch(&target, window, cx)
                        },
                    ))
            });
        let forkable = !self.copy_only
            && message.role == Role::Assistant
            && self.marks.fork_points.contains(&id);
        let fork = forkable.then(|| {
            let target = id.clone();
            Button::new(SharedString::from(format!("fork:{id}")))
                .ghost()
                .xsmall()
                .icon(IconName::GitFork)
                .tooltip(if busy {
                    busy_tip
                } else {
                    rust_i18n::t!("Message.fork").into_owned()
                })
                .loading(self.forking.contains(&id))
                .disabled(busy || self.forking.contains(&id))
                .on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.fork_from(target.clone(), window, cx)
                    },
                ))
        });
        h_flex()
            .gap_0p5()
            .opacity(0.)
            .group_hover(group.clone(), |style| style.opacity(1.))
            .child(copy)
            .children(branch)
            .children(fork)
            .into_any_element()
    }

    /// Starts editing the question `id` in the composer.
    fn begin_branch(
        &mut self,
        id: &wire::MessageId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.branching {
            return;
        }
        let row = self.handle().and_then(|handle| {
            let handle = handle.read(cx);
            let projection = handle.projection();
            match projection
                .row(projection.index_of(&RowKey::Message(id.clone()))?)
            {
                Some(TranscriptRow::Message(row)) => Some(row.clone()),
                _ => None,
            }
        });
        if let Some(row) = row {
            self.edit_message(&row, window, cx);
        }
    }

    /// Forks the session's history up to the reply `id` into a new session,
    /// which the shell then opens in its own tab.
    fn fork_from(
        &mut self,
        id: wire::MessageId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let (Some(handle), Some(client)) = (self.handle(), self.client.clone())
        else {
            return;
        };
        let (session, cwd) = {
            let handle = handle.read(cx);
            (handle.id().clone(), handle.cwd().to_path_buf())
        };
        if self.branching || !self.forking.insert(id.clone()) {
            return;
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let forked = async {
                let entry = client
                    .message_entry(
                        &session,
                        &id.to_string(),
                        MessageAuthor::Assistant,
                    )
                    .await?;
                client.fork(&session, &entry.entry_id, &cwd).await
            }
            .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.forking.remove(&id);
                match forked {
                    Ok(fork) => {
                        tracing::info!(
                            "forked session {} into {}",
                            session,
                            fork
                        );
                        cx.emit(SessionViewEvent::Forked(fork));
                    }
                    Err(error) => {
                        tracing::warn!(
                            "failed to fork session {}: {}",
                            session,
                            error
                        );
                        window.push_notification(
                            Notification::error(error.to_string()),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Starts editing an earlier question (M4b spec §3.3): the composer takes
    /// its text and images; nothing reaches the backend until it is sent.
    pub(in crate::ui) fn edit_message(
        &mut self,
        row: &MessageRow,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let message = Outgoing::from(row);
        let id = row.message_id.clone();
        self.composer.update(cx, |composer, cx| {
            composer.begin_edit(id, message, window, cx);
            composer.focus(window, cx);
        });
    }

    /// Sends an edit of an earlier question: branches to just before it,
    /// replays the transcript of the new branch, then sends `message` there.
    /// On failure the edit stays in the composer to try again.
    pub(in crate::ui) fn send_edit(
        &mut self,
        edited: wire::MessageId,
        message: Outgoing,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let (Some(handle), Some(client)) = (self.handle(), self.client.clone())
        else {
            self.composer.update(cx, |composer, cx| {
                composer.restore(message, window, cx)
            });
            return;
        };
        let id = handle.read(cx).id().clone();
        let handle = handle.downgrade();
        self.branching = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let branched = async {
                let entry = client
                    .message_entry(
                        &id,
                        &edited.to_string(),
                        MessageAuthor::User,
                    )
                    .await?;
                // Navigating to a question reopens it: the kernel moves the
                // branch to the question's parent itself.
                client.branch(&id, Some(&entry.entry_id)).await?;
                tracing::info!(
                    "branched session {} before message {}",
                    id,
                    edited
                );
                // Wait for the replay, or it would clear the new message's rows.
                let replay = handle
                    .update(cx, |handle, cx| handle.replay(client.clone(), cx))
                    .map_err(|closed| {
                        BackendError::Request(closed.to_string())
                    })?;
                replay.await
            }
            .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.branching = false;
                cx.notify();
                match branched {
                    Ok(()) => {
                        let kept = this.composer.update(cx, |composer, cx| {
                            composer.finish_edit(cx)
                        });
                        this.send_keeping(message, kept, window, cx);
                    }
                    Err(error) => {
                        tracing::warn!(
                            "failed to branch session {}: {}",
                            id,
                            error
                        );
                        this.composer.update(cx, |composer, cx| {
                            composer.restore(message, window, cx)
                        });
                        window.push_notification(
                            Notification::error(error.to_string()),
                            cx,
                        );
                    }
                }
            });
        })
        .detach();
    }
}
