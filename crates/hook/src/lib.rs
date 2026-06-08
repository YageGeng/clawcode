//! Configurable lifecycle command hooks.

mod engine;
mod events;

pub use engine::{ConfiguredHandler, DiscoveryConfig, HookEngine};
pub use events::{
    PostToolUseOutcome, PostToolUseRequest, PreToolUseHandlerResult,
    PreToolUseOutcome, PreToolUseRequest, fold_pre_tool_use_results,
};

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use protocol::{HookEventName, HookSource};

    use super::*;

    /// Invalid regex matchers are skipped during discovery instead of matching every call.
    #[test]
    fn discovery_skips_invalid_regex_matchers() {
        let temp = tempfile::tempdir().expect("temp dir");
        let project = temp.path().join("project");
        std::fs::create_dir_all(project.join(".claw"))
            .expect("create hook dir");
        std::fs::write(
            project.join(".claw/hooks.json"),
            r#"{
                "hooks": {
                    "PreToolUse": [{
                        "matcher": "[",
                        "hooks": [{ "type": "command", "command": "echo bad" }]
                    }]
                }
            }"#,
        )
        .expect("write hooks");

        let engine = HookEngine::discover(
            DiscoveryConfig::builder().project_cwd(project).build(),
        );

        assert!(
            engine
                .preview_pre_tool_use(&pre_tool_request("shell"))
                .is_empty()
        );
        assert_eq!(engine.warnings().len(), 1);
    }

    /// Duplicate matcher aliases select the same handler at most once.
    #[test]
    fn preview_deduplicates_alias_matches() {
        let handler = ConfiguredHandler::builder()
            .event_name(HookEventName::PreToolUse)
            .matcher(Some("shell|Bash".to_string()))
            .command("echo ok".to_string())
            .timeout_sec(5)
            .source_path(PathBuf::from("/tmp/hooks.json"))
            .source(HookSource::Project)
            .display_order(0)
            .build();
        let engine = HookEngine::from_handlers_for_test(vec![handler]);
        let request = pre_tool_request("shell")
            .with_matcher_aliases(vec!["Bash".to_string()]);

        let previews = engine.preview_pre_tool_use(&request);

        assert_eq!(previews.len(), 1);
    }

    /// A denying PreToolUse hook discards any updated input from other hooks.
    #[test]
    fn pre_tool_use_deny_discards_updated_input() {
        let outcome = fold_pre_tool_use_results(vec![
            PreToolUseHandlerResult::builder()
                .should_block(false)
                .updated_input(Some(serde_json::json!({"command": "pwd"})))
                .completion_order(0)
                .build(),
            PreToolUseHandlerResult::builder()
                .should_block(true)
                .block_reason(Some("blocked".to_string()))
                .completion_order(1)
                .build(),
        ]);

        assert!(outcome.should_block);
        assert_eq!(outcome.block_reason.as_deref(), Some("blocked"));
        assert!(outcome.updated_input.is_none());
    }

    /// Discovery loads project hooks before user hooks, matching Codex precedence order.
    #[test]
    fn discovery_orders_project_hooks_before_user_hooks() {
        let temp = tempfile::tempdir().expect("temp dir");
        let project = temp.path().join("project");
        let user = temp.path().join("user");
        std::fs::create_dir_all(project.join(".claw"))
            .expect("project hook dir");
        std::fs::create_dir_all(user.join(".claw")).expect("user hook dir");
        write_hooks_file(
            project.join(".claw/hooks.json"),
            "PreToolUse",
            "echo project",
        );
        write_hooks_file(
            user.join(".claw/hooks.json"),
            "PreToolUse",
            "echo user",
        );

        let engine = HookEngine::discover(
            DiscoveryConfig::builder()
                .project_cwd(project)
                .user_home(Some(user))
                .build(),
        );

        let previews = engine.preview_pre_tool_use(&pre_tool_request("shell"));

        assert_eq!(previews.len(), 2);
        assert!(previews[0].source_path.contains("project"));
        assert!(previews[1].source_path.contains("user"));
    }

    /// The latest PreToolUse rewrite is chosen by completion order, not declaration order.
    #[tokio::test]
    async fn pre_tool_use_uses_last_completed_updated_input() {
        let temp = tempfile::tempdir().expect("temp dir");
        let engine = HookEngine::from_handlers_for_test(vec![
            test_handler(
                HookEventName::PreToolUse,
                "sleep 0.15; printf '%s' '{\"hookSpecificOutput\":{\"permissionDecision\":\"allow\",\"updatedInput\":{\"message\":\"slow\"}}}'",
                0,
            ),
            test_handler(
                HookEventName::PreToolUse,
                "printf '%s' '{\"hookSpecificOutput\":{\"permissionDecision\":\"allow\",\"updatedInput\":{\"message\":\"fast\"}}}'",
                1,
            ),
        ]);
        let mut request = pre_tool_request("shell");
        request.cwd = temp.path().to_path_buf();

        let outcome = engine.run_pre_tool_use(request).await;

        assert_eq!(
            outcome.updated_input,
            Some(serde_json::json!({ "message": "slow" }))
        );
    }

    /// Builds a minimal PreToolUse request for hook engine tests.
    fn pre_tool_request(tool_name: &str) -> PreToolUseRequest {
        PreToolUseRequest::builder()
            .session_id(protocol::SessionId::from("session-1"))
            .turn_id(protocol::TurnId::from("turn-1"))
            .cwd(PathBuf::from("/repo"))
            .model("test-model".to_string())
            .permission_mode("default".to_string())
            .tool_name(tool_name.to_string())
            .tool_use_id("call-1".to_string())
            .tool_input(serde_json::json!({}))
            .build()
    }

    /// Write one simple command hook config for discovery tests.
    fn write_hooks_file(path: PathBuf, event_name: &str, command: &str) {
        std::fs::write(
            path,
            serde_json::json!({
                "hooks": {
                    event_name: [{
                        "hooks": [{
                            "type": "command",
                            "command": command,
                            "timeout": 2
                        }]
                    }]
                }
            })
            .to_string(),
        )
        .expect("write hooks file");
    }

    /// Build one configured handler for execution tests.
    fn test_handler(
        event_name: HookEventName,
        command: &str,
        display_order: i64,
    ) -> ConfiguredHandler {
        ConfiguredHandler::builder()
            .event_name(event_name)
            .command(command.to_string())
            .timeout_sec(2)
            .source_path(PathBuf::from("/tmp/hooks.json"))
            .source(HookSource::Project)
            .display_order(display_order)
            .build()
    }

    trait PreToolUseRequestTestExt {
        /// Return a copy with matcher aliases set for tests.
        fn with_matcher_aliases(self, matcher_aliases: Vec<String>) -> Self;
    }

    impl PreToolUseRequestTestExt for PreToolUseRequest {
        /// Return a copy with matcher aliases set for tests.
        fn with_matcher_aliases(
            mut self,
            matcher_aliases: Vec<String>,
        ) -> Self {
            self.matcher_aliases = matcher_aliases;
            self
        }
    }
}
