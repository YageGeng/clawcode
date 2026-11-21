//! Attached sessions: each keeps a live projection while a tab shows it or a run is active.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use agent_client_protocol::v2::schema as wire;
use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use futures::future::{FutureExt as _, LocalBoxFuture, Shared};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, Task, WeakEntity,
};
use typed_builder::TypedBuilder;

use crate::backend::{
    AcpClient, AgentRequest, BackendEvent, SessionId, SessionStart,
};
use crate::catalog::SessionSummary;
use crate::projection::{ProjectionChange, RunState, SessionProjection};

/// Progress of attaching a session's transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachPhase {
    /// Querying the run state and replaying history.
    Attaching,
    /// The transcript is complete and follows live updates.
    Attached,
    /// Attaching failed; carries the backend's message.
    Failed(String),
}

/// What the sidebar and tabs show for a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// Nothing is running.
    Idle,
    /// A run is in progress.
    Running,
    /// The run waits for the user.
    Waiting,
    /// The session could not be attached.
    Failed,
}

/// One attached session: its projection and attach state.
#[derive(TypedBuilder)]
pub struct SessionHandle {
    id: SessionId,
    cwd: PathBuf,
    #[builder(default)]
    projection: SessionProjection,
    #[builder(default = AttachPhase::Attaching)]
    phase: AttachPhase,
    /// Set when the tab closed during a run; the session closes once idle.
    #[builder(default)]
    close_when_idle: bool,
}

impl EventEmitter<ProjectionChange> for SessionHandle {}

impl SessionHandle {
    /// Session identifier.
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// Working directory.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The transcript so far.
    pub fn projection(&self) -> &SessionProjection {
        &self.projection
    }

    /// Attach progress.
    pub fn phase(&self) -> &AttachPhase {
        &self.phase
    }

    /// Status shown next to the session.
    pub fn status(&self) -> SessionStatus {
        if matches!(self.phase, AttachPhase::Failed(_)) {
            return SessionStatus::Failed;
        }
        // An unanswered request or question waits for the user, even while the run goes on.
        if !self.projection.controls().requests().is_empty()
            || !self.projection.open_questions().is_empty()
        {
            return SessionStatus::Waiting;
        }
        match self.projection.run_state() {
            RunState::Running => SessionStatus::Running,
            RunState::RequiresAction => SessionStatus::Waiting,
            RunState::Idle => SessionStatus::Idle,
        }
    }

    /// Mutable control state, for answering requests and questions from the UI.
    pub fn controls_mut(&mut self) -> &mut crate::projection::SessionControls {
        self.projection.controls_mut()
    }

    /// Whether a run is active or blocked on a request, so the session must stay attached.
    ///
    /// Open async questions alone do not count: their run has already
    /// settled, and the questions come back on the next attach.
    fn is_busy(&self) -> bool {
        self.projection.run_state() != RunState::Idle
            || !self.projection.controls().requests().is_empty()
    }
}

/// Something the shell reacts to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryEvent {
    /// The backend connection ended; carries its error, if any.
    BackendStopped(Option<String>),
    /// A session was closed in the backend and dropped from the registry.
    Released(SessionId),
    /// A session's title or run state settled, so the session catalog is stale.
    SessionChanged(SessionId),
    /// A live notice for the user, such as a failed extension handler.
    Notice {
        /// Short headline.
        title: String,
        /// Optional detail.
        description: Option<String>,
    },
}

/// Every attached session, fed by one backend event stream.
pub struct SessionRegistry {
    client: AcpClient,
    handles: HashMap<SessionId, Entity<SessionHandle>>,
    /// `session/close` requests still in flight; a reopen waits for them so it never races the close.
    closing: HashMap<SessionId, Shared<LocalBoxFuture<'static, ()>>>,
}

impl EventEmitter<RegistryEvent> for SessionRegistry {}

impl SessionRegistry {
    /// Creates an empty registry that talks to `client`.
    pub fn new(client: AcpClient) -> Self {
        Self {
            client,
            handles: HashMap::new(),
            closing: HashMap::new(),
        }
    }

    /// The attached session with this id.
    pub fn handle(&self, id: &SessionId) -> Option<Entity<SessionHandle>> {
        self.handles.get(id).cloned()
    }

    /// Number of attached sessions with an active run, the ones quitting would stop.
    pub fn running_count(&self, cx: &App) -> usize {
        self.handles
            .values()
            .filter(|handle| handle.read(cx).is_busy())
            .count()
    }

    /// Registers a session just created with `session/new`: the server already
    /// streams its events to this connection, so there is nothing to replay.
    pub fn adopt(
        &mut self,
        start: SessionStart,
        cwd: &Path,
        cx: &mut Context<'_, Self>,
    ) -> Entity<SessionHandle> {
        let id = start.id;
        let handle = cx.new(|_| {
            let mut handle = SessionHandle::builder()
                .id(id.clone())
                .cwd(cwd.to_path_buf())
                .phase(AttachPhase::Attached)
                .build();
            handle
                .projection
                .controls_mut()
                .set_config(start.config_options);
            handle
        });
        self.handles.insert(id, handle.clone());
        handle
    }

    /// Returns the attached session, attaching it first when needed.
    ///
    /// Attaching reads the run state, clears the projection, and replays the
    /// whole transcript through `session/resume`; live updates keep flowing in
    /// through [`Self::apply_batch`] meanwhile and merge by row identity.
    pub fn attach(
        &mut self,
        summary: &SessionSummary,
        cx: &mut Context<'_, Self>,
    ) -> Entity<SessionHandle> {
        if let Some(handle) = self.handles.get(&summary.session_id) {
            // Reopened before its run ended: the session stays attached after all.
            handle.update(cx, |handle, _| handle.close_when_idle = false);
            return handle.clone();
        }
        let handle = cx.new(|_| {
            SessionHandle::builder()
                .id(summary.session_id.clone())
                .cwd(summary.cwd.clone())
                .build()
        });
        self.handles
            .insert(summary.session_id.clone(), handle.clone());
        let client = self.client.clone();
        let id = summary.session_id.clone();
        let cwd = summary.cwd.clone();
        let target = handle.downgrade();
        let pending_close = self.closing.get(&summary.session_id).cloned();
        tracing::info!("attaching session {}", summary.session_id);
        cx.spawn(async move |_, cx| {
            // A close of this session still in flight would make the resume fail or be undone.
            if let Some(pending_close) = pending_close {
                pending_close.await;
            }
            let outcome: Result<(), crate::backend::BackendError> = async {
                let running = client.session_running(&id).await?;
                let alive = target
                    .update(cx, |handle, cx| {
                        handle.projection.clear();
                        let state = if running {
                            RunState::Running
                        } else {
                            RunState::Idle
                        };
                        handle.projection.set_run_state(state);
                        cx.notify();
                    })
                    .is_ok();
                // The tab closed while attaching; there is nothing left to replay into.
                if !alive {
                    return Ok(());
                }
                // The queue is a nicety: failing to read it must not fail the attach.
                match client.pending_messages(&id).await {
                    Ok(pending) => {
                        let _ = target.update(cx, |handle, cx| {
                            if handle.projection.set_pending(pending) {
                                cx.emit(
                                    ProjectionChange::builder()
                                        .queue(true)
                                        .build(),
                                );
                                cx.notify();
                            }
                        });
                    }
                    Err(error) => tracing::warn!(
                        "failed to load pending messages of session {}: {}",
                        id,
                        error
                    ),
                }
                let start = client.resume_with_replay(&id, &cwd).await?;
                let _ = target.update(cx, |handle, cx| {
                    handle
                        .projection
                        .controls_mut()
                        .set_config(start.config_options);
                    cx.emit(ProjectionChange::builder().controls(true).build());
                    cx.notify();
                });
                Ok(())
            }
            .await;
            let phase = match outcome {
                // Success is reported by the server's `replay_complete` (see `apply_batch`);
                // only a failed resume decides the phase here.
                Ok(()) => return,
                Err(error) => {
                    tracing::warn!(
                        "failed to attach session {}: {}",
                        id,
                        error
                    );
                    AttachPhase::Failed(error.to_string())
                }
            };
            // The handle may be gone if the tab closed while attaching.
            let _ = target.update(cx, |handle, cx| {
                handle.phase = phase;
                cx.notify();
            });
        })
        .detach();
        handle
    }

    /// Routes one batch of backend events; each touched session is notified once.
    pub fn apply_batch(
        &mut self,
        events: Vec<BackendEvent>,
        cx: &mut Context<'_, Self>,
    ) {
        let mut changes: HashMap<SessionId, ProjectionChange> = HashMap::new();
        for event in events {
            match event {
                BackendEvent::Update(notification) => {
                    // Notices are live-only and global: they become toasts, not rows.
                    if let wire::SessionUpdate::Notice(notice) =
                        &notification.update
                    {
                        cx.emit(RegistryEvent::Notice {
                            title: notice.title.clone(),
                            description: notice.description.clone(),
                        });
                        continue;
                    }
                    // Updates for sessions nobody attached are not projected.
                    let Some(handle) =
                        self.handles.get(&notification.session_id)
                    else {
                        continue;
                    };
                    let change = handle.update(cx, |handle, _| {
                        // The server marks the end of replayed history: the transcript is complete.
                        if matches!(
                            notification.update,
                            wire::SessionUpdate::ReplayComplete(_)
                        ) && handle.phase == AttachPhase::Attaching
                        {
                            handle.phase = AttachPhase::Attached;
                            tracing::info!("attached session {}", handle.id);
                        }
                        handle.projection.apply(&notification.update)
                    });
                    changes
                        .entry(notification.session_id.clone())
                        .or_default()
                        .extend(change);
                }
                BackendEvent::Stopped(reason) => {
                    cx.emit(RegistryEvent::BackendStopped(reason))
                }
                BackendEvent::Request(request) => {
                    let handle = request.session_id().and_then(|id| {
                        self.handles.get(id).map(|h| (id.clone(), h.clone()))
                    });
                    match handle {
                        // A released tab waiting to close shows nothing either.
                        Some((id, handle))
                            if !handle.read(cx).close_when_idle =>
                        {
                            handle.update(cx, |handle, _| {
                                handle
                                    .projection
                                    .controls_mut()
                                    .push_request(*request)
                            });
                            changes.entry(id).or_default().controls = true;
                        }
                        // Nobody shows this session, so nobody can answer: decline it.
                        _ => self.decline(&request),
                    }
                }
                BackendEvent::RequestCancelled(request_id) => {
                    for (id, handle) in &self.handles {
                        let removed = handle.update(cx, |handle, _| {
                            handle
                                .projection
                                .controls_mut()
                                .remove_request(request_id)
                        });
                        if removed {
                            changes.entry(id.clone()).or_default().controls =
                                true;
                        }
                    }
                }
            }
        }
        for (id, change) in changes {
            if let Some(handle) = self.handles.get(&id)
                && !change.is_empty()
            {
                // A new title or a settled run makes the catalog (title, sort order) stale.
                let catalog_stale = change.title
                    || (change.run_state
                        && handle.read(cx).projection.run_state()
                            == RunState::Idle);
                handle.update(cx, |_, cx| {
                    cx.emit(change);
                    cx.notify();
                });
                if catalog_stale {
                    cx.emit(RegistryEvent::SessionChanged(id));
                }
            }
        }
        let finished: Vec<SessionId> = self
            .handles
            .iter()
            .filter(|(_, handle)| {
                let handle = handle.read(cx);
                handle.close_when_idle && !handle.is_busy()
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in finished {
            self.close(&id, cx);
        }
    }

    /// Declines a request no attached session can show: permissions are
    /// cancelled and elicitations answered with `cancel`.
    fn decline(&self, request: &AgentRequest) {
        let result = match request {
            AgentRequest::Permission { id, .. } => {
                self.client.answer_permission(*id, None)
            }
            AgentRequest::Elicitation { id, .. } => {
                self.client.answer_elicitation(
                    *id,
                    agent_client_protocol::v2::schema::CreateElicitationResponse::new(
                        agent_client_protocol::v2::schema::ElicitationAction::Cancel,
                    ),
                )
            }
        };
        if let Err(error) = result {
            tracing::warn!(
                "failed to decline request {}: {}",
                request.id().0,
                error
            );
        }
    }

    /// A tab stopped showing the session: close it now, or once its run ends.
    pub fn release(&mut self, id: &SessionId, cx: &mut Context<'_, Self>) {
        let Some(handle) = self.handles.get(id).cloned() else {
            return;
        };
        // No view can answer the session's requests any more; declining them
        // unblocks the run so it can end and the session close.
        let pending = handle.update(cx, |handle, cx| {
            let pending = handle.controls_mut().take_requests();
            if !pending.is_empty() {
                cx.notify();
            }
            pending
        });
        for request in &pending {
            self.decline(request);
        }
        if handle.read(cx).is_busy() {
            handle.update(cx, |handle, _| handle.close_when_idle = true);
            return;
        }
        self.close(id, cx);
    }

    /// Drops a session's handle without closing it, for a session about to be deleted.
    pub fn forget(&mut self, id: &SessionId) {
        self.handles.remove(id);
    }

    /// Drops the handle and closes the session in the backend.
    fn close(&mut self, id: &SessionId, cx: &mut Context<'_, Self>) {
        if self.handles.remove(id).is_none() {
            return;
        }
        let client = self.client.clone();
        let session = id.clone();
        let close = async move {
            match client.close_session(&session).await {
                Ok(()) => tracing::info!("closed session {}", session),
                Err(error) => tracing::warn!(
                    "failed to close session {}: {}",
                    session,
                    error
                ),
            }
        }
        .boxed_local()
        .shared();
        self.closing.insert(id.clone(), close.clone());
        let session = id.clone();
        cx.spawn(async move |this, cx| {
            close.await;
            let _ = this.update(cx, |this, _| this.closing.remove(&session));
        })
        .detach();
        cx.emit(RegistryEvent::Released(id.clone()));
    }

    /// Feeds backend events into the registry in batches until either side goes away.
    pub fn pump(
        registry: WeakEntity<Self>,
        mut events: UnboundedReceiver<BackendEvent>,
        cx: &mut App,
    ) -> Task<()> {
        cx.spawn(async move |cx| {
            let mut stopped = false;
            while let Some(first) = events.next().await {
                let mut batch = vec![first];
                // Take everything already queued so one frame applies it at once.
                while let Ok(event) = events.try_recv() {
                    batch.push(event);
                }
                stopped |= batch
                    .iter()
                    .any(|event| matches!(event, BackendEvent::Stopped(_)));
                if registry
                    .update(cx, |registry, cx| registry.apply_batch(batch, cx))
                    .is_err()
                {
                    return;
                }
            }
            // The stream ended without a stop report (the connection task died): report it anyway.
            if !stopped {
                tracing::warn!(
                    "backend event stream ended without a stop report"
                );
                let _ = registry.update(cx, |_, cx| {
                    cx.emit(RegistryEvent::BackendStopped(None))
                });
            }
        })
    }
}
