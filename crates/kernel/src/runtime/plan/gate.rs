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
    /// `exec_command` runs only read-only commands and `write_stdin` may only
    /// poll or interrupt, although both are the kernel's; other kernel tools
    /// run because their own behavior respects plan mode; anything else needs
    /// a read-only hint.
    pub(in crate::runtime) fn check(
        &self,
        call: &ToolCall,
        metadata: Option<&ToolMetadata>,
    ) -> Result<(), String> {
        if self.mode == SessionMode::Default {
            return Ok(());
        }
        // Shell tools are checked by name first: being the kernel's says
        // nothing about what the command does.
        let argument = |name: &str| {
            call.arguments.get(name).and_then(serde_json::Value::as_str)
        };
        match call.name.as_str() {
            protocol::EXEC_COMMAND_TOOL => {
                return match argument("cmd") {
                    Some(command) => ShellCommandLine::read_only(command)
                        .map_err(|violation| violation.to_string()),
                    None => Err("plan mode needs a command to check".into()),
                };
            }
            protocol::WRITE_STDIN_TOOL => {
                return match argument("chars").unwrap_or_default() {
                    "" | "\u{3}" => Ok(()),
                    _ => Err("Plan mode is active: write_stdin may only poll or interrupt.".into()),
                };
            }
            _ => {}
        }
        if self.kernel_owned.contains(&call.name) {
            return Ok(());
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
