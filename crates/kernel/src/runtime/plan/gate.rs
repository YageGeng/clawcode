//! Plan-mode tool gate captured with each Turn's tool snapshot.
//!
//! Tool definitions stay the same in every mode so the cached prompt prefix
//! survives a switch; plan mode refuses calls here instead of hiding tools.

use std::collections::BTreeSet;

use protocol::{SessionMode, ToolCall, ToolMetadata};
use tools::ShellCommandLine;

/// What one Turn may run, fixed when the Turn takes its tool snapshot.
pub(in crate::runtime) struct PlanModeGate {
    mode: SessionMode,
    /// Names whose implementation is the kernel's, which plan mode trusts.
    kernel_owned: BTreeSet<String>,
}

impl PlanModeGate {
    /// Captures the mode and the kernel's tool names for one Turn.
    pub(in crate::runtime) fn new(
        mode: SessionMode,
        kernel_owned: BTreeSet<String>,
    ) -> Self {
        Self { mode, kernel_owned }
    }

    /// Lets a call run, or returns the reason plan mode refuses it.
    ///
    /// Kernel tools run because their own behavior respects plan mode; `bash`
    /// runs only read-only commands; anything else needs a read-only hint.
    pub(in crate::runtime) fn check(
        &self,
        call: &ToolCall,
        metadata: Option<&ToolMetadata>,
    ) -> Result<(), String> {
        if self.mode == SessionMode::Default
            || self.kernel_owned.contains(&call.name)
        {
            return Ok(());
        }
        if call.name == "bash" {
            return match call
                .arguments
                .get("command")
                .and_then(serde_json::Value::as_str)
            {
                Some(command) => ShellCommandLine::read_only(command)
                    .map_err(|violation| violation.to_string()),
                None => Err("plan mode needs a bash command to check".into()),
            };
        }
        if metadata.and_then(|metadata| metadata.annotations.read_only_hint)
            == Some(true)
        {
            return Ok(());
        }
        Err(format!(
            "Plan mode is active: {} is not allowed. Finish the plan and call exit_plan_mode.",
            call.name
        ))
    }
}
