//! Input, agent, Turn, message, and context Hook examples.

use async_trait::async_trait;
use extension::{
    AgentEndPoint, AgentSettledPoint, AgentStartPoint, BeforeAgentStartPoint,
    ContextPoint, ExtensionContext, ExtensionError, ExtensionHandler,
    ExtensionRegistrar, InputPoint, MessageEndPoint, MessageStartPoint,
    MessageUpdatePoint, TurnEndPoint, TurnStartPoint,
};
use protocol::{
    AgentEndEvent, AgentMessage, AgentSettledEvent, AgentStartEvent,
    BeforeAgentStartEvent, BeforeAgentStartResult, ContextEvent, ContextResult,
    InputEvent, InputResult, MessageEndEvent, MessageStartEvent,
    MessageUpdateEvent, TurnEndEvent, TurnStartEvent,
};

struct AgentPolicy;

#[async_trait]
impl ExtensionHandler<InputPoint> for AgentPolicy {
    /// Normalizes outer whitespace before prompt-template and skill expansion.
    async fn handle(
        &self,
        event: &InputEvent,
        _context: &ExtensionContext,
    ) -> Result<InputResult, ExtensionError> {
        let normalized = event.text.trim();
        Ok(if normalized == event.text {
            InputResult::Continue
        } else {
            InputResult::Transform {
                text: normalized.to_string(),
            }
        })
    }
}

#[async_trait]
impl ExtensionHandler<BeforeAgentStartPoint> for AgentPolicy {
    /// Appends one stable policy line to the assembled system prompt.
    async fn handle(
        &self,
        event: &BeforeAgentStartEvent,
        _context: &ExtensionContext,
    ) -> Result<BeforeAgentStartResult, ExtensionError> {
        Ok(BeforeAgentStartResult {
            messages: Vec::new(),
            system_prompt: Some(format!(
                "{}\n\nReport destructive operations before executing them.",
                event.system_prompt
            )),
        })
    }
}

#[async_trait]
impl ExtensionHandler<ContextPoint> for AgentPolicy {
    /// Demonstrates request-local context replacement without mutating persisted history.
    async fn handle(
        &self,
        event: &ContextEvent,
        _context: &ExtensionContext,
    ) -> Result<ContextResult, ExtensionError> {
        Ok(ContextResult {
            messages: Some(event.request.messages.clone()),
        })
    }
}

#[async_trait]
impl ExtensionHandler<MessageEndPoint> for AgentPolicy {
    /// Leaves the final message unchanged after a point where safe replacement is allowed.
    async fn handle(
        &self,
        event: &MessageEndEvent,
        _context: &ExtensionContext,
    ) -> Result<Option<AgentMessage>, ExtensionError> {
        let _identity = &event.message.identity;
        Ok(None)
    }
}

struct AgentObserver;

#[async_trait]
impl ExtensionHandler<AgentStartPoint> for AgentObserver {
    /// Observes the beginning of one agent loop.
    async fn handle(
        &self,
        _event: &AgentStartEvent,
        context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let _run = &context.invocation.run_id;
        Ok(())
    }
}

#[async_trait]
impl ExtensionHandler<AgentEndPoint> for AgentObserver {
    /// Observes every run-local message after the loop ends.
    async fn handle(
        &self,
        event: &AgentEndEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let _message_count = event.messages.len();
        Ok(())
    }
}

#[async_trait]
impl ExtensionHandler<AgentSettledPoint> for AgentObserver {
    /// Observes the point where no automatic continuation remains.
    async fn handle(
        &self,
        _event: &AgentSettledEvent,
        context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let _pending = context.snapshot.pending_follow_up;
        Ok(())
    }
}

#[async_trait]
impl ExtensionHandler<TurnStartPoint> for AgentObserver {
    /// Observes the zero-based index and timestamp of a model Turn.
    async fn handle(
        &self,
        event: &TurnStartEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let _turn = (event.turn_index, event.timestamp_ms);
        Ok(())
    }
}

#[async_trait]
impl ExtensionHandler<TurnEndPoint> for AgentObserver {
    /// Observes the settled Turn and its source-ordered tool results.
    async fn handle(
        &self,
        event: &TurnEndEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let _outcome = (&event.turn.outcome, event.tool_results.len());
        Ok(())
    }
}

#[async_trait]
impl ExtensionHandler<MessageStartPoint> for AgentObserver {
    /// Observes the initial snapshot of one streaming message.
    async fn handle(
        &self,
        event: &MessageStartEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let _message_id = &event.message.identity.message_id;
        Ok(())
    }
}

#[async_trait]
impl ExtensionHandler<MessageUpdatePoint> for AgentObserver {
    /// Observes typed text, reasoning, or tool-call updates.
    async fn handle(
        &self,
        event: &MessageUpdateEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let _update = &event.update;
        Ok(())
    }
}

/// Registers every input, agent, Turn, message, and context hook.
pub(super) fn register(
    registrar: &mut ExtensionRegistrar,
) -> Result<(), ExtensionError> {
    registrar.on::<InputPoint, _>(AgentPolicy)?;
    registrar.on::<BeforeAgentStartPoint, _>(AgentPolicy)?;
    registrar.on::<AgentStartPoint, _>(AgentObserver)?;
    registrar.on::<AgentEndPoint, _>(AgentObserver)?;
    registrar.on::<AgentSettledPoint, _>(AgentObserver)?;
    registrar.on::<TurnStartPoint, _>(AgentObserver)?;
    registrar.on::<TurnEndPoint, _>(AgentObserver)?;
    registrar.on::<MessageStartPoint, _>(AgentObserver)?;
    registrar.on::<MessageUpdatePoint, _>(AgentObserver)?;
    registrar.on::<MessageEndPoint, _>(AgentPolicy)?;
    registrar.on::<ContextPoint, _>(AgentPolicy)
}
