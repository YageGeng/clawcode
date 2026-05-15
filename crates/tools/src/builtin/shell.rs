//! Shell command execution tool.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;
use tokio::time::timeout;

use crate::Tool;

const OUTPUT_MAX_LEN: usize = 4096;
const SHELL_TIMEOUT_SECS: u64 = 30;

/// Executes arbitrary shell commands.
pub struct ShellCommand;

impl ShellCommand {
    /// Create a new shell tool instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ShellCommand {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ShellCommand {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return stdout, stderr, and exit code"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional working directory for the command"
                }
            },
            "required": ["command"]
        })
    }

    fn needs_approval(&self, _: &serde_json::Value, _ctx: &crate::ToolContext) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &crate::ToolContext,
    ) -> Result<String, String> {
        let command = arguments
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or("missing 'command' argument")?
            .to_string();

        let work_dir = arguments
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(Path::new)
            .unwrap_or(&ctx.cwd);

        let result = timeout(
            Duration::from_secs(SHELL_TIMEOUT_SECS),
            Command::new("/bin/sh")
                .arg("-c")
                .arg(&command)
                .current_dir(work_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let mut formatted = format!(
                    "exit code: {}\nstdout:\n{}\nstderr:\n{}",
                    output.status.code().unwrap_or(-1),
                    truncate(&stdout),
                    truncate(&stderr),
                );
                if formatted.len() > OUTPUT_MAX_LEN {
                    formatted.truncate(OUTPUT_MAX_LEN);
                    formatted.push_str("\n... (output truncated)");
                }
                Ok(formatted)
            }
            Ok(Err(e)) => Err(format!("command execution failed: {e}")),
            Err(_) => Err(format!("command timed out after {SHELL_TIMEOUT_SECS}s")),
        }
    }
}

/// Truncate command output to the per-stream display budget.
///
/// Uses [`str::floor_char_boundary`] to avoid panicking when the byte limit
/// falls in the middle of a multi-byte UTF-8 character.
#[allow(clippy::string_slice)]
fn truncate(s: &str) -> &str {
    if s.len() > OUTPUT_MAX_LEN / 2 {
        let boundary = s.floor_char_boundary(OUTPUT_MAX_LEN / 2);
        &s[..boundary]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;

    #[tokio::test]
    async fn shell_echo_hello() {
        let tool = ShellCommand::new();
        let result = tool
            .execute(
                serde_json::json!({"command": "echo hello"}),
                &ToolContext::for_test(Path::new(".")),
            )
            .await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("hello"));
        assert!(output.contains("exit code: 0"));
    }

    #[tokio::test]
    async fn shell_missing_command() {
        let tool = ShellCommand::new();
        let result = tool
            .execute(
                serde_json::json!({}),
                &ToolContext::for_test(Path::new(".")),
            )
            .await;
        result.unwrap_err();
    }

    #[tokio::test]
    async fn shell_needs_approval() {
        let tool = ShellCommand::new();
        assert!(tool.needs_approval(
            &serde_json::json!({"command": "ls"}),
            &ToolContext::for_test(Path::new("."))
        ));
    }

    #[test]
    fn truncate_utf8_boundary_does_not_panic() {
        // 2047 ASCII bytes + CJK chars: byte 2048 lands mid-char.
        let s = format!("{}{}", "a".repeat(2047), "你好世界");
        let result = truncate(&s);
        assert!(
            result.len() <= OUTPUT_MAX_LEN / 2,
            "len {} > budget {}",
            result.len(),
            OUTPUT_MAX_LEN / 2
        );
        assert!(
            s.is_char_boundary(result.len()) || result == s,
            "slice ends at non-char-boundary"
        );
    }

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello"), "hello");
    }

    #[test]
    fn truncate_exact_boundary_ascii() {
        let s = "a".repeat(OUTPUT_MAX_LEN);
        let result = truncate(&s);
        assert_eq!(result.len(), OUTPUT_MAX_LEN / 2);
    }
}
