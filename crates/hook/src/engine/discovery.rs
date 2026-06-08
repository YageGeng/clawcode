use std::fs;
use std::path::PathBuf;

use config::{HookEventsToml, HookHandlerConfig, HooksFile, MatcherGroup};
use protocol::HookEventName;
use protocol::HookSource;

use super::ConfiguredHandler;
use crate::events::common::{
    matcher_pattern_for_event, validate_matcher_pattern,
};

/// File-system roots used for hook discovery.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct DiscoveryConfig {
    /// Project working directory searched for `.claw/hooks.json`.
    pub project_cwd: PathBuf,
    /// Optional home directory searched for `.claw/hooks.json`.
    #[builder(default)]
    pub user_home: Option<PathBuf>,
}

/// Discover handlers from project hooks first, then user hooks.
pub(crate) fn discover_handlers(
    config: DiscoveryConfig,
) -> (Vec<ConfiguredHandler>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut handlers = Vec::new();
    let mut display_order = 0_i64;

    append_hooks_file(
        &mut handlers,
        &mut warnings,
        &mut display_order,
        config.project_cwd.join(".claw/hooks.json"),
        HookSource::Project,
    );
    if let Some(user_home) = config.user_home {
        append_hooks_file(
            &mut handlers,
            &mut warnings,
            &mut display_order,
            user_home.join(".claw/hooks.json"),
            HookSource::User,
        );
    }

    (handlers, warnings)
}

/// Append handlers declared by one hook config file.
fn append_hooks_file(
    handlers: &mut Vec<ConfiguredHandler>,
    warnings: &mut Vec<String>,
    display_order: &mut i64,
    source_path: PathBuf,
    source: HookSource,
) {
    if !source_path.is_file() {
        return;
    }
    let contents = match fs::read_to_string(&source_path) {
        Ok(contents) => contents,
        Err(error) => {
            warnings.push(format!(
                "failed to read hooks config {}: {error}",
                source_path.display()
            ));
            return;
        }
    };
    let parsed: HooksFile = match serde_json::from_str(&contents) {
        Ok(parsed) => parsed,
        Err(error) => {
            warnings.push(format!(
                "failed to parse hooks config {}: {error}",
                source_path.display()
            ));
            return;
        }
    };
    append_hook_events(
        handlers,
        warnings,
        display_order,
        source_path,
        source,
        parsed.hooks,
    );
}

/// Append all event groups from one parsed hook config.
fn append_hook_events(
    handlers: &mut Vec<ConfiguredHandler>,
    warnings: &mut Vec<String>,
    display_order: &mut i64,
    source_path: PathBuf,
    source: HookSource,
    events: HookEventsToml,
) {
    let groups = [
        (HookEventName::PreToolUse, events.pre_tool_use),
        (HookEventName::PermissionRequest, events.permission_request),
        (HookEventName::PostToolUse, events.post_tool_use),
        (HookEventName::PreCompact, events.pre_compact),
        (HookEventName::PostCompact, events.post_compact),
        (HookEventName::SessionStart, events.session_start),
        (HookEventName::UserPromptSubmit, events.user_prompt_submit),
        (HookEventName::SubagentStart, events.subagent_start),
        (HookEventName::SubagentStop, events.subagent_stop),
        (HookEventName::Stop, events.stop),
    ];
    for (event_name, groups) in groups {
        for group in groups {
            append_matcher_group(
                handlers,
                warnings,
                display_order,
                source_path.clone(),
                source,
                event_name,
                group,
            );
        }
    }
}

/// Append all supported command handlers from one matcher group.
#[allow(clippy::too_many_arguments)]
fn append_matcher_group(
    handlers: &mut Vec<ConfiguredHandler>,
    warnings: &mut Vec<String>,
    display_order: &mut i64,
    source_path: PathBuf,
    source: HookSource,
    event_name: HookEventName,
    group: MatcherGroup,
) {
    let matcher =
        matcher_pattern_for_event(event_name, group.matcher.as_deref());
    if let Some(matcher) = matcher
        && let Err(error) = validate_matcher_pattern(matcher)
    {
        warnings.push(format!(
            "invalid matcher {matcher:?} in {}: {error}",
            source_path.display()
        ));
        return;
    }

    for handler in group.hooks {
        let HookHandlerConfig::Command {
            command,
            command_windows,
            timeout_sec,
            r#async,
            status_message,
        } = handler
        else {
            warnings.push(format!(
                "skipping unsupported hook handler in {}",
                source_path.display()
            ));
            continue;
        };
        if r#async {
            warnings.push(format!(
                "skipping async hook in {}: async hooks are not supported yet",
                source_path.display()
            ));
            continue;
        }
        let command = if cfg!(windows) {
            command_windows.unwrap_or(command)
        } else {
            command
        };
        if command.trim().is_empty() {
            warnings.push(format!(
                "skipping empty hook command in {}",
                source_path.display()
            ));
            continue;
        }
        handlers.push(
            ConfiguredHandler::builder()
                .event_name(event_name)
                .matcher(matcher.map(ToOwned::to_owned))
                .command(command)
                .timeout_sec(timeout_sec.unwrap_or(600).max(1))
                .status_message(status_message)
                .source_path(source_path.clone())
                .source(source)
                .display_order(*display_order)
                .build(),
        );
        *display_order += 1;
    }
}
