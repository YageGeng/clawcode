use protocol::HookEventName;

/// Return matcher inputs with canonical tool name first.
pub(crate) fn matcher_inputs<'a>(
    tool_name: &'a str,
    matcher_aliases: &'a [String],
) -> Vec<&'a str> {
    // Keep the canonical name first so preview and execution preserve the
    // primary identity that hook stdin serializes.
    std::iter::once(tool_name)
        .chain(matcher_aliases.iter().map(String::as_str))
        .collect()
}

/// Return the matcher pattern that applies to an event.
pub(crate) fn matcher_pattern_for_event(
    event_name: HookEventName,
    matcher: Option<&str>,
) -> Option<&str> {
    match event_name {
        HookEventName::PreToolUse
        | HookEventName::PermissionRequest
        | HookEventName::PostToolUse
        | HookEventName::PreCompact
        | HookEventName::PostCompact
        | HookEventName::SessionStart
        | HookEventName::SubagentStart
        | HookEventName::SubagentStop => matcher,
        HookEventName::UserPromptSubmit | HookEventName::Stop => None,
    }
}

/// Validate a matcher pattern before discovery accepts a group.
pub(crate) fn validate_matcher_pattern(
    matcher: &str,
) -> Result<(), regex::Error> {
    if is_match_all_matcher(matcher) || is_exact_matcher(matcher) {
        Ok(())
    } else {
        regex::Regex::new(matcher).map(|_| ())
    }
}

/// Return whether a matcher selects one candidate input.
pub(crate) fn matches_matcher(
    matcher: Option<&str>,
    input: Option<&str>,
) -> bool {
    match matcher {
        None => true,
        Some(matcher) if is_match_all_matcher(matcher) => true,
        Some(matcher) if is_exact_matcher(matcher) => input
            .map(|input| matcher.split('|').any(|candidate| candidate == input))
            .unwrap_or(false),
        Some(matcher) => input
            .and_then(|input| {
                regex::Regex::new(matcher)
                    .ok()
                    .map(|regex| regex.is_match(input))
            })
            .unwrap_or(false),
    }
}

/// Return true for explicit match-all matcher forms.
fn is_match_all_matcher(matcher: &str) -> bool {
    matcher.is_empty() || matcher == "*"
}

/// Return true when a matcher should use exact matching instead of regex.
fn is_exact_matcher(matcher: &str) -> bool {
    matcher
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '|')
}
