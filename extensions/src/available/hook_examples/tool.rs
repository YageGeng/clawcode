//! Tool result and execution lifecycle Hook examples.

use async_trait::async_trait;
use extension::{
    ExtensionContext, ExtensionError, ExtensionHandler, ExtensionRegistrar,
    ToolExecutionEndPoint, ToolExecutionStartPoint, ToolExecutionUpdatePoint,
    ToolResultPoint,
};
use protocol::{
    ToolExecutionEndEvent, ToolExecutionStartEvent, ToolExecutionUpdateEvent,
    ToolResultEvent, ToolResultPatch,
};

struct ToolPolicy;

#[async_trait]
impl ExtensionHandler<ToolResultPoint> for ToolPolicy {
    /// Leaves typed tool results unchanged while exposing the patch boundary.
    async fn handle(
        &self,
        event: &ToolResultEvent,
        _context: &ExtensionContext,
    ) -> Result<ToolResultPatch, ExtensionError> {
        let _is_error = event.result.is_error;
        Ok(ToolResultPatch::default())
    }
}

struct ToolObserver;

#[async_trait]
impl ExtensionHandler<ToolExecutionStartPoint> for ToolObserver {
    /// Observes the final parsed call immediately before execution.
    async fn handle(
        &self,
        event: &ToolExecutionStartEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let _call = &event.call;
        Ok(())
    }
}

#[async_trait]
impl ExtensionHandler<ToolExecutionUpdatePoint> for ToolObserver {
    /// Observes replaceable streaming snapshots from a running tool.
    async fn handle(
        &self,
        event: &ToolExecutionUpdateEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let _partial = &event.partial_result;
        Ok(())
    }
}

#[async_trait]
impl ExtensionHandler<ToolExecutionEndPoint> for ToolObserver {
    /// Observes the transformed final result after tool settlement.
    async fn handle(
        &self,
        event: &ToolExecutionEndEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let _final_result = &event.result;
        Ok(())
    }
}

/// Registers tool-result transformation and execution observation examples.
pub(super) fn register(
    registrar: &mut ExtensionRegistrar,
) -> Result<(), ExtensionError> {
    registrar.on::<ToolExecutionStartPoint, _>(ToolObserver)?;
    registrar.on::<ToolExecutionUpdatePoint, _>(ToolObserver)?;
    registrar.on::<ToolExecutionEndPoint, _>(ToolObserver)?;
    registrar.on::<ToolResultPoint, _>(ToolPolicy)
}
