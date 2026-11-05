use std::sync::Arc;

use protocol::{
    AgentEventPayload, ExtensionInvocation, SessionId, SessionInfoChangedEvent,
    TurnId,
};

use super::{EventEmitter, Kernel, KernelError, Session};

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
