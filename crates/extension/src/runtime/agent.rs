use protocol::{
    AgentEndEvent, AgentSettledEvent, AgentStartEvent, BeforeAgentStartEvent,
    BeforeAgentStartResult, ContextEvent, MessageEndEvent, MessageStartEvent,
    MessageUpdateEvent, ModelRequest, TurnEndEvent, TurnStartEvent,
};

use crate::{
    AgentEndPoint, AgentSettledPoint, AgentStartPoint, BeforeAgentStartPoint,
    ContextPoint, ExtensionContext, ExtensionError, ExtensionRuntime,
    MessageEndPoint, MessageStartPoint, MessageUpdatePoint, TurnEndPoint,
    TurnStartPoint,
};

impl ExtensionRuntime {
    /// Chains system-prompt replacements and accumulates custom message drafts.
    pub async fn emit_before_agent_start(
        &self,
        mut event: BeforeAgentStartEvent,
        context: &ExtensionContext,
    ) -> BeforeAgentStartResult {
        let mut messages = Vec::new();
        let mut changed_prompt = false;
        for registered in self.handlers::<BeforeAgentStartPoint>() {
            let handler_context = context.for_extension(registered.source());
            match registered.handler.handle(&event, &handler_context).await {
                Ok(result) => {
                    messages.extend(result.messages);
                    if let Some(system_prompt) = result.system_prompt {
                        event.system_prompt = system_prompt;
                        changed_prompt = true;
                    }
                }
                Err(error) => {
                    self.report::<BeforeAgentStartPoint>(registered, &error)
                        .await;
                }
            }
        }
        BeforeAgentStartResult {
            messages,
            system_prompt: changed_prompt.then_some(event.system_prompt),
        }
    }

    /// Chains model-context replacements while leaving persisted history untouched.
    ///
    /// The request moves through every handler and back to the caller, so a
    /// Turn's history is never copied, even when no handler is registered.
    pub async fn emit_context(
        &self,
        mut event: ContextEvent,
        context: &ExtensionContext,
    ) -> ModelRequest {
        for registered in self.handlers::<ContextPoint>() {
            let handler_context = context.for_extension(registered.source());
            match registered.handler.handle(&event, &handler_context).await {
                Ok(result) => {
                    if let Some(messages) = result.messages {
                        event.request.messages = messages;
                    }
                }
                Err(error) => {
                    self.report::<ContextPoint>(registered, &error).await;
                }
            }
        }
        event.request
    }

    /// Chains final message replacements, restoring a changed identity and
    /// discarding a changed role. Both are reported and the Run continues, because
    /// failing it would return a queued message to its queue for every later Run.
    pub async fn emit_message_end(
        &self,
        mut event: MessageEndEvent,
        context: &ExtensionContext,
    ) -> protocol::AgentMessage {
        for registered in self.handlers::<MessageEndPoint>() {
            let handler_context = context.for_extension(registered.source());
            match registered.handler.handle(&event, &handler_context).await {
                Ok(Some(mut message)) => {
                    if !message.content.same_role(&event.message.content) {
                        self.report::<MessageEndPoint>(
                            registered,
                            &ExtensionError::MessageRoleChanged,
                        )
                        .await;
                        continue;
                    }
                    // Announced events and queue entries correlate on the original
                    // identity. The content stays, because a hook that builds a
                    // fresh message may be redacting it.
                    if message.identity != event.message.identity {
                        message.identity = event.message.identity.clone();
                        self.report::<MessageEndPoint>(
                            registered,
                            &ExtensionError::MessageIdentityChanged,
                        )
                        .await;
                    }
                    event.message = message;
                }
                Ok(None) => {}
                Err(error) => {
                    self.report::<MessageEndPoint>(registered, &error).await;
                }
            }
        }
        event.message
    }

    /// Notifies extensions that an agent run started.
    pub async fn emit_agent_start(
        &self,
        event: &AgentStartEvent,
        context: &ExtensionContext,
    ) {
        self.observe::<AgentStartPoint>(event, context).await;
    }

    /// Notifies extensions that an agent run ended.
    pub async fn emit_agent_end(
        &self,
        event: &AgentEndEvent,
        context: &ExtensionContext,
    ) {
        self.observe::<AgentEndPoint>(event, context).await;
    }

    /// Notifies extensions that all automatic agent work settled.
    pub async fn emit_agent_settled(
        &self,
        event: &AgentSettledEvent,
        context: &ExtensionContext,
    ) {
        self.observe::<AgentSettledPoint>(event, context).await;
    }

    /// Notifies extensions that one model turn started.
    pub async fn emit_turn_start(
        &self,
        event: &TurnStartEvent,
        context: &ExtensionContext,
    ) {
        self.observe::<TurnStartPoint>(event, context).await;
    }

    /// Notifies extensions that one model turn ended.
    pub async fn emit_turn_end(
        &self,
        event: &TurnEndEvent,
        context: &ExtensionContext,
    ) {
        self.observe::<TurnEndPoint>(event, context).await;
    }

    /// Notifies extensions that one complete message entered streaming.
    pub async fn emit_message_start(
        &self,
        event: &MessageStartEvent,
        context: &ExtensionContext,
    ) {
        self.observe::<MessageStartPoint>(event, context).await;
    }

    /// Notifies extensions about one assistant streaming update.
    pub async fn emit_message_update(
        &self,
        event: &MessageUpdateEvent,
        context: &ExtensionContext,
    ) {
        self.observe::<MessageUpdatePoint>(event, context).await;
    }
}
