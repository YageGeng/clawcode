use std::sync::Arc;

use extension::{ExtensionContext, ExtensionHostError};
use protocol::{
    ExtensionId, ExtensionInvocation, ExtensionMessageDraft, ExtensionSnapshot,
    IdGenerator, IdKind, MessageIdentity, ProductIdentity, RunId, TurnId,
};

use super::super::{Kernel, KernelError, Session};
use super::host::SessionExtensionHost;

/// Assigns durable message identity and complete non-streaming timing.
pub(super) struct ExtensionMessageMaterializer {
    clock: Arc<dyn store::Clock>,
    id_generator: Arc<dyn IdGenerator>,
}

impl ExtensionMessageMaterializer {
    /// Creates a materializer from the Kernel's identity dependencies.
    pub(super) fn new(
        clock: Arc<dyn store::Clock>,
        id_generator: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            clock,
            id_generator,
        }
    }

    /// Converts one draft into a complete message and optionally verifies its author.
    pub(super) fn materialize(
        &self,
        turn_id: TurnId,
        expected_extension_id: Option<&ExtensionId>,
        draft: ExtensionMessageDraft,
    ) -> Result<protocol::AgentMessage, ExtensionHostError> {
        if expected_extension_id
            .is_some_and(|expected| expected != &draft.extension_id)
        {
            return Err(ExtensionHostError::Operation(
                "extension message identity does not match handler".to_string(),
            ));
        }
        let timestamp = self.clock.now();
        Ok(protocol::AgentMessage {
            identity: MessageIdentity {
                message_id: protocol::MessageId::try_from(
                    self.id_generator.next(IdKind::Message),
                )
                .map_err(|error| {
                    ExtensionHostError::Operation(error.to_string())
                })?,
                turn_id,
            },
            timing: protocol::MessageTiming::try_from((
                timestamp, timestamp, timestamp,
            ))
            .map_err(|error| {
                ExtensionHostError::Operation(error.to_string())
            })?,
            content: protocol::MessageContent::Extension {
                extension: protocol::ExtensionMessage {
                    extension_id: draft.extension_id,
                    custom_type: draft.custom_type,
                    blocks: draft.blocks,
                    display: draft.display,
                    include_in_context: draft.include_in_context,
                    details: draft.details,
                },
            },
        })
    }
}

impl Kernel {
    /// Assigns durable identity and complete timing to one extension message draft.
    pub(in crate::runtime) fn materialize_extension_message(
        &self,
        turn_id: &TurnId,
        draft: ExtensionMessageDraft,
    ) -> Result<protocol::AgentMessage, KernelError> {
        ExtensionMessageMaterializer::new(
            Arc::clone(&self.clock),
            Arc::clone(&self.id_generator),
        )
        .materialize(turn_id.clone(), None, draft)
        .map_err(|error| KernelError::Protocol(error.to_string()))
    }

    /// Captures one lock-free extension snapshot before asynchronous handler execution.
    pub(in crate::runtime) fn extension_context(
        &self,
        session: &Arc<Session>,
        run_id: Option<&RunId>,
        turn_id: Option<&TurnId>,
    ) -> Result<ExtensionContext, KernelError> {
        self.extension_context_with_event_sink(session, run_id, turn_id, None)
    }

    /// Captures an extension context with an operation-local event destination.
    pub(in crate::runtime) fn extension_context_with_event_sink(
        &self,
        session: &Arc<Session>,
        run_id: Option<&RunId>,
        turn_id: Option<&TurnId>,
        event_sink: Option<Arc<dyn crate::EventSink>>,
    ) -> Result<ExtensionContext, KernelError> {
        let snapshot = session.extension_snapshot()?;
        let extension_id = ExtensionId::try_from(ProductIdentity::SLUG)
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        let host = SessionExtensionHost::builder()
            .session(Arc::clone(session))
            .kernel(self.clone())
            .clock(Arc::clone(&self.clock))
            .id_generator(Arc::clone(&self.id_generator))
            .event_sink(event_sink)
            .build();

        Ok(ExtensionContext::builder()
            .invocation(
                ExtensionInvocation::builder()
                    .extension_id(extension_id)
                    .session_id(snapshot.tree.session_id.clone())
                    .run_id(run_id.cloned())
                    .turn_id(turn_id.cloned())
                    .timestamp_ms(self.clock.now())
                    .cwd(session.cwd.clone())
                    .build(),
            )
            .snapshot(snapshot)
            .host(Arc::new(host))
            .generation(session.extensions.runtime_ref().generation())
            .build())
    }
}

impl Session {
    /// Captures one complete extension-visible state snapshot without retaining locks.
    pub(in crate::runtime) fn extension_snapshot(
        &self,
    ) -> Result<ExtensionSnapshot, KernelError> {
        let messages = self.transcript.history()?;
        let pending = self.execution.pending_snapshot()?;
        let tree = self.transcript.tree_snapshot()?;
        let tools = self.tools.snapshot()?;
        // Read both selections under one lock so extension snapshots cannot mix
        // values from two concurrent setting updates.
        let (model, thinking_level) = self.model.snapshot()?;
        let model_profile = model.profile().clone();
        Ok(ExtensionSnapshot::builder()
            .active_model(model_profile)
            .thinking_level(thinking_level)
            .models(self.model.profiles())
            .messages(messages)
            .tree(tree)
            .pending_steer(pending.steering.len())
            .pending_follow_up(pending.follow_up.len())
            .active_tools(tools.active_names())
            .all_tools(tools.available_names())
            .cancelled(self.execution.cancellation()?.is_cancelled())
            .build())
    }
}
