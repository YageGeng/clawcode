//! Hook configuration types loaded from hooks.json files.

use serde::{Deserialize, Serialize};

/// Top-level hooks.json document.
#[derive(
    Debug,
    Default,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct HooksFile {
    /// Hook event groups keyed by lifecycle event name.
    #[serde(default)]
    pub hooks: HookEventsToml,
}

/// Configured hook matcher groups for every supported event.
#[derive(
    Debug,
    Default,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct HookEventsToml {
    /// Groups selected before a tool executes.
    #[builder(default)]
    #[serde(rename = "PreToolUse", default)]
    pub pre_tool_use: Vec<MatcherGroup>,
    /// Groups selected when a tool requests permission.
    #[builder(default)]
    #[serde(rename = "PermissionRequest", default)]
    pub permission_request: Vec<MatcherGroup>,
    /// Groups selected after a tool finishes.
    #[builder(default)]
    #[serde(rename = "PostToolUse", default)]
    pub post_tool_use: Vec<MatcherGroup>,
    /// Groups selected before compaction starts.
    #[builder(default)]
    #[serde(rename = "PreCompact", default)]
    pub pre_compact: Vec<MatcherGroup>,
    /// Groups selected after compaction completes.
    #[builder(default)]
    #[serde(rename = "PostCompact", default)]
    pub post_compact: Vec<MatcherGroup>,
    /// Groups selected when a session starts or resumes.
    #[builder(default)]
    #[serde(rename = "SessionStart", default)]
    pub session_start: Vec<MatcherGroup>,
    /// Groups selected before a user prompt is accepted.
    #[builder(default)]
    #[serde(rename = "UserPromptSubmit", default)]
    pub user_prompt_submit: Vec<MatcherGroup>,
    /// Groups selected when a user-visible subagent starts.
    #[builder(default)]
    #[serde(rename = "SubagentStart", default)]
    pub subagent_start: Vec<MatcherGroup>,
    /// Groups selected when a user-visible subagent stops.
    #[builder(default)]
    #[serde(rename = "SubagentStop", default)]
    pub subagent_stop: Vec<MatcherGroup>,
    /// Groups selected at a root turn stop boundary.
    #[builder(default)]
    #[serde(rename = "Stop", default)]
    pub stop: Vec<MatcherGroup>,
}

impl HookEventsToml {
    /// Returns true when no event contains hook handlers.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pre_tool_use.is_empty()
            && self.permission_request.is_empty()
            && self.post_tool_use.is_empty()
            && self.pre_compact.is_empty()
            && self.post_compact.is_empty()
            && self.session_start.is_empty()
            && self.user_prompt_submit.is_empty()
            && self.subagent_start.is_empty()
            && self.subagent_stop.is_empty()
            && self.stop.is_empty()
    }
}

/// One matcher and the handlers it selects.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatcherGroup {
    /// Regex or exact matcher. None means all matcher inputs for that event.
    #[serde(default)]
    pub matcher: Option<String>,
    /// Handlers run when the matcher selects the event.
    #[serde(default)]
    pub hooks: Vec<HookHandlerConfig>,
}

/// Supported hook handler declarations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HookHandlerConfig {
    /// Command handler executed as a child process.
    #[serde(rename = "command")]
    Command {
        /// Shell command executed for the hook.
        command: String,
        /// Windows-specific command override parsed for compatibility.
        #[serde(default, rename = "commandWindows", alias = "command_windows")]
        command_windows: Option<String>,
        /// Timeout in seconds before the hook is treated as failed.
        #[serde(default, rename = "timeout", alias = "timeout_sec")]
        timeout_sec: Option<u64>,
        /// Async hooks are parsed for compatibility but skipped at discovery.
        #[serde(default)]
        r#async: bool,
        /// Optional status message rendered while this hook runs.
        #[serde(default, rename = "statusMessage", alias = "status_message")]
        status_message: Option<String>,
    },
    /// Prompt handlers are parsed for compatibility but skipped at discovery.
    #[serde(rename = "prompt")]
    Prompt {},
    /// Agent handlers are parsed for compatibility but skipped at discovery.
    #[serde(rename = "agent")]
    Agent {},
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses command handlers with the tagged JSON shape used by hook configs.
    #[test]
    fn parses_command_hook_config() {
        let parsed: HooksFile = serde_json::from_str(
            r#"{
              "hooks": {
                "PreToolUse": [{
                  "matcher": "shell",
                  "hooks": [{
                    "type": "command",
                    "command": "python3 .claw/hooks/pre_shell.py",
                    "commandWindows": "py .claw\\hooks\\pre_shell.py",
                    "timeout": 60,
                    "async": false,
                    "statusMessage": "checking shell"
                  }]
                }]
              }
            }"#,
        )
        .expect("parse hooks file");

        assert_eq!(parsed.hooks.pre_tool_use.len(), 1);
        let group = &parsed.hooks.pre_tool_use[0];
        assert_eq!(group.matcher.as_deref(), Some("shell"));
        assert_eq!(group.hooks.len(), 1);
        let HookHandlerConfig::Command {
            command,
            command_windows,
            timeout_sec,
            r#async,
            status_message,
        } = &group.hooks[0]
        else {
            panic!("expected command handler");
        };
        assert_eq!(command, "python3 .claw/hooks/pre_shell.py");
        assert_eq!(
            command_windows.as_deref(),
            Some("py .claw\\hooks\\pre_shell.py")
        );
        assert_eq!(*timeout_sec, Some(60));
        assert!(!*r#async);
        assert_eq!(status_message.as_deref(), Some("checking shell"));
    }

    /// Parses unsupported handler variants so discovery can warn and skip them.
    #[test]
    fn parses_unsupported_handler_variants() {
        let parsed: HooksFile = serde_json::from_str(
            r#"{
              "hooks": {
                "SessionStart": [{
                  "hooks": [{ "type": "prompt" }, { "type": "agent" }]
                }]
              }
            }"#,
        )
        .expect("parse hooks file");

        assert!(matches!(
            parsed.hooks.session_start[0].hooks[0],
            HookHandlerConfig::Prompt {}
        ));
        assert!(matches!(
            parsed.hooks.session_start[0].hooks[1],
            HookHandlerConfig::Agent {}
        ));
    }
}
