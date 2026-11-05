use std::sync::Arc;

use protocol::{
    AgentEventPayload, ExtensionInvocation, SessionId, SessionInfoChangedEvent,
    SessionTitle, TurnId,
};

use crate::runtime::{EventEmitter, Kernel, KernelError, Session};

/// Borrows one live Session for a name change.
///
/// Client titles, `/name`, extension calls, and owned child creation all rename
/// through here, so each persists the name, runs `session_info_changed`
/// observers, and decides explicitly whether clients hear about it.
#[derive(typed_builder::TypedBuilder)]
pub(in crate::runtime) struct SessionRename<'a> {
    kernel: &'a Kernel,
    session_id: &'a SessionId,
    session: &'a Arc<Session>,
    /// Extension call that made the change; `None` for client and internal renames.
    #[builder(default, setter(strip_option))]
    invocation: Option<&'a ExtensionInvocation>,
}

/// Whether a rename replaces the name or only fills a Session never named.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NameWrite {
    Replace,
    DefaultOnly,
}

/// Where a stored name is announced.
enum Announcement<'e> {
    /// Internal renames, such as owned child creation, stay silent.
    Silent,
    /// The Session's active Turn, or a fresh Turn on its sink, carries the event.
    Observers,
    /// A caller's emitter and Turn carry the event, ordered with its output.
    On(&'e EventEmitter, TurnId),
}

impl SessionRename<'_> {
    /// Persists `name` without telling clients, for internal renames such as
    /// owned child creation that no client asked to see.
    pub(in crate::runtime) async fn silent(
        &self,
        name: Option<String>,
    ) -> Result<(), KernelError> {
        self.apply(name, Announcement::Silent, NameWrite::Replace)
            .await
    }

    /// Persists `name` and announces it on the Session's active Turn, or on a
    /// fresh Turn when the Session is idle.
    pub(in crate::runtime) async fn announced(
        &self,
        name: Option<String>,
    ) -> Result<(), KernelError> {
        self.apply(name, Announcement::Observers, NameWrite::Replace)
            .await
    }

    /// Persists `name` and announces it on a caller's Turn, so a command's or
    /// extension call's title event stays ordered with its other output.
    pub(in crate::runtime) async fn announced_on(
        &self,
        name: Option<String>,
        emitter: &EventEmitter,
        turn_id: TurnId,
    ) -> Result<(), KernelError> {
        self.apply(name, Announcement::On(emitter, turn_id), NameWrite::Replace)
            .await
    }

    /// Stores `title` as the default name when the Session was never named,
    /// then announces it on the caller's Turn and runs observers like any
    /// rename; a Session that already has or cleared a name is left alone.
    pub(in crate::runtime) async fn default_on(
        &self,
        title: String,
        emitter: &EventEmitter,
        turn_id: TurnId,
    ) -> Result<(), KernelError> {
        self.apply(
            Some(title),
            Announcement::On(emitter, turn_id),
            NameWrite::DefaultOnly,
        )
        .await
    }

    /// Stores the name and announces it under the Session's rename lock, then
    /// runs observers unlocked so one that renames again cannot deadlock.
    ///
    /// Returns `SessionClosing` when the runtime finished closing while this
    /// rename waited: its sealed transcript refuses the write, and the caller
    /// renames through the Store instead.
    async fn apply(
        &self,
        name: Option<String>,
        announcement: Announcement<'_>,
        write: NameWrite,
    ) -> Result<(), KernelError> {
        {
            // Overlapping renames announce in the order they were stored, so
            // observers end on the title the Store holds.
            let _serial = self.session.name_changes.lock().await;
            let transcript = &self.session.transcript;
            let stored = match (write, &name) {
                (NameWrite::Replace, _) => {
                    transcript.set_name(name.clone())?;
                    true
                }
                (NameWrite::DefaultOnly, Some(title)) => {
                    transcript.set_default_name(title.clone())?.is_some()
                }
                (NameWrite::DefaultOnly, None) => false,
            };
            if !stored {
                return Ok(());
            }
            self.session.sync_store()?;
            // Chosen under the lock, so the event joins the Run active now.
            match announcement {
                Announcement::Silent => {}
                Announcement::Observers => match self
                    .kernel
                    .observer_emitter(Arc::clone(self.session), self.session_id)
                {
                    Ok((turn_id, emitter)) => {
                        self.announce(&emitter, turn_id, &name).await;
                    }
                    Err(error) => tracing::warn!(
                        "stored title for Session {} but could not route its announcement: {}",
                        self.session_id,
                        error
                    ),
                },
                Announcement::On(emitter, turn_id) => {
                    self.announce(emitter, turn_id, &name).await;
                }
            }
        }
        let context = self.kernel.extension_context(
            self.session,
            self.invocation
                .and_then(|invocation| invocation.run_id.as_ref()),
            self.invocation
                .and_then(|invocation| invocation.turn_id.as_ref()),
        )?;
        self.session
            .extensions
            .runtime_ref()
            .emit_session_info_changed(
                &SessionInfoChangedEvent { name },
                &context,
            )
            .await;
        Ok(())
    }

    /// Reports a stored name; the name is durable, so a failed announcement
    /// only leaves observers stale until their next refresh.
    async fn announce(
        &self,
        emitter: &EventEmitter,
        turn_id: TurnId,
        name: &Option<String>,
    ) {
        let event = AgentEventPayload::SessionTitleChanged {
            title: name.clone(),
        };
        if let Err(error) = emitter.emit(turn_id, event).await {
            tracing::warn!(
                "stored title for Session {} but failed to announce it: {}",
                self.session_id,
                error
            );
        }
    }
}

impl Kernel {
    /// Stores a title on a live Session without announcing it, for internal
    /// callers such as owned child creation that no client asked to see.
    pub async fn rename_session(
        &self,
        session_id: &SessionId,
        title: SessionTitle,
    ) -> Result<(), KernelError> {
        let session = self.session(session_id)?;
        SessionRename::builder()
            .kernel(self)
            .session_id(session_id)
            .session(&session)
            .build()
            .silent(Some(title.as_str().to_string()))
            .await
    }

    /// Stores a client-chosen title and announces it to every Session observer.
    pub async fn set_session_title(
        &self,
        session_id: &SessionId,
        title: SessionTitle,
    ) -> Result<(), KernelError> {
        // Any listed Session may be retitled. A live runtime costs one map
        // lookup; only a Session without one touches its persisted Store.
        loop {
            match self.session(session_id) {
                // One runtime both stores and announces the title, so the event
                // follows the runtime that actually took the name.
                Ok(session) => {
                    match SessionRename::builder()
                        .kernel(self)
                        .session_id(session_id)
                        .session(&session)
                        .build()
                        .announced(Some(title.as_str().to_string()))
                        .await
                    {
                        // The runtime closed while the rename waited for its
                        // lock; its sealed transcript refused the write, so
                        // retry and let the Store take the title.
                        Err(
                            KernelError::SessionNotFound(_)
                            | KernelError::SessionClosing,
                        ) => continue,
                        result => return result,
                    }
                }
                Err(
                    KernelError::SessionNotFound(_)
                    | KernelError::SessionStarting,
                ) => {}
                // Shutdown gives the id back when it ends; the Store, or the
                // runtime restored by an aborted close, then takes the title.
                Err(KernelError::SessionClosing) => {
                    if self.wait_for_session_close(session_id).await? {
                        continue;
                    }
                    return Err(KernelError::SessionClosing);
                }
                Err(error) => return Err(error),
            }
            match self.reserve_session_build(session_id) {
                Ok(_reservation) => {
                    // The build reservation keeps Resume from opening the same
                    // file. Looking the file up under it reports a deletion that
                    // finished first as SessionNotFound, not a raw I/O error.
                    // Nothing observes an unloaded Session, so no event.
                    let header =
                        self.store_factory.header(session_id)?.ok_or_else(
                            || KernelError::SessionNotFound(session_id.clone()),
                        )?;
                    // One appended fact, not a replay, keeps the reservation short.
                    self.store_factory.set_name_at(
                        &header.path,
                        Some(title.as_str().to_string()),
                    )?;
                    tracing::info!(
                        "renamed persisted Session {} without loading it",
                        session_id
                    );
                    return Ok(());
                }
                // Construction or deletion owns the id; retry once it ends.
                Err(KernelError::DuplicateSession(_)) => {
                    if !self.wait_for_session_reservation(session_id).await? {
                        // A cancelled startup can leave a Starting runtime with
                        // no reservation to wait for; report its lifecycle error.
                        match self.session(session_id) {
                            Ok(_)
                            | Err(
                                KernelError::SessionNotFound(_)
                                | KernelError::SessionClosing,
                            ) => {}
                            Err(error) => return Err(error),
                        }
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }
}
