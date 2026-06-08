/// Parse stdout as a JSON object, distinguishing empty output from invalid JSON.
pub(crate) fn parse_json_object(
    stdout: &str,
) -> Result<Option<serde_json::Value>, String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if !looks_like_json(trimmed) {
        return Ok(None);
    }
    let value: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|error| {
            format!("hook returned invalid JSON output: {error}")
        })?;
    if value.is_object() {
        Ok(Some(value))
    } else {
        Err("hook returned non-object JSON output".to_string())
    }
}

/// Validate event-specific `PreToolUse` output rules from Codex.
pub(crate) fn validate_pre_tool_use_output(
    value: &serde_json::Value,
) -> Option<String> {
    validate_pre_tool_use_universal(value).or_else(|| {
        let hook_specific = value.get("hookSpecificOutput");
        if hook_specific.is_some_and(|output| {
            output.get("permissionDecision").is_some()
                || output.get("permissionDecisionReason").is_some()
                || output.get("updatedInput").is_some()
        }) {
            hook_specific.and_then(validate_pre_tool_use_hook_specific_output)
        } else {
            validate_pre_tool_use_legacy_output(value)
        }
    })
}

/// Validate event-specific `PostToolUse` output rules from Codex.
pub(crate) fn validate_post_tool_use_output(
    value: &serde_json::Value,
) -> Option<String> {
    if value
        .get("suppressOutput")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        return Some(
            "PostToolUse hook returned unsupported suppressOutput".to_string(),
        );
    }
    if value
        .get("hookSpecificOutput")
        .and_then(|output| output.get("updatedMCPToolOutput"))
        .is_some()
    {
        return Some(
            "PostToolUse hook returned unsupported updatedMCPToolOutput"
                .to_string(),
        );
    }
    let should_block =
        value.get("decision").and_then(serde_json::Value::as_str)
            == Some("block");
    if should_block && block_reason(value).is_none() {
        Some(
            "PostToolUse hook returned decision:block without a non-empty reason"
                .to_string(),
        )
    } else if !should_block
        && value.get("continue").and_then(serde_json::Value::as_bool)
            != Some(false)
        && value.get("reason").is_some()
    {
        Some("PostToolUse hook returned reason without decision".to_string())
    } else {
        None
    }
}

/// Extract hook-specific additional context from parsed JSON output.
pub(crate) fn additional_context(value: &serde_json::Value) -> Option<String> {
    value
        .get("hookSpecificOutput")
        .and_then(|output| output.get("additionalContext"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

/// Extract a PreToolUse deny reason from parsed JSON output.
pub(crate) fn pre_tool_use_deny_reason(
    value: &serde_json::Value,
) -> Option<String> {
    value
        .get("hookSpecificOutput")
        .and_then(|output| {
            (output
                .get("permissionDecision")
                .and_then(serde_json::Value::as_str)
                == Some("deny"))
            .then(|| output.get("permissionDecisionReason"))
            .flatten()
        })
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| block_reason(value))
}

/// Extract a legacy block reason from parsed JSON output.
pub(crate) fn block_reason(value: &serde_json::Value) -> Option<String> {
    (value.get("decision").and_then(serde_json::Value::as_str) == Some("block"))
        .then(|| value.get("reason"))
        .flatten()
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

/// Extract a top-level reason field when it is non-empty.
pub(crate) fn top_level_reason(value: &serde_json::Value) -> Option<String> {
    value
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

/// Join model-facing hook feedback chunks in configured handler order.
pub(crate) fn join_feedback_messages(
    existing: Option<String>,
    next: Option<String>,
) -> Option<String> {
    match (existing, next) {
        (Some(existing), Some(next)) => Some(format!("{existing}\n\n{next}")),
        (None, Some(next)) => Some(next),
        (Some(existing), None) => Some(existing),
        (None, None) => None,
    }
}

/// Return true when hook stdout starts with a JSON container marker.
fn looks_like_json(stdout: &str) -> bool {
    let trimmed = stdout.trim_start();
    trimmed.starts_with('{') || trimmed.starts_with('[')
}

/// Validate universal fields unsupported by `PreToolUse`.
fn validate_pre_tool_use_universal(
    value: &serde_json::Value,
) -> Option<String> {
    if value.get("continue").and_then(serde_json::Value::as_bool) == Some(false)
    {
        Some("PreToolUse hook returned unsupported continue:false".to_string())
    } else if value.get("stopReason").is_some() {
        Some("PreToolUse hook returned unsupported stopReason".to_string())
    } else if value
        .get("suppressOutput")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        Some("PreToolUse hook returned unsupported suppressOutput".to_string())
    } else {
        None
    }
}

/// Validate modern hook-specific `PreToolUse` decision fields.
fn validate_pre_tool_use_hook_specific_output(
    output: &serde_json::Value,
) -> Option<String> {
    let decision = output
        .get("permissionDecision")
        .and_then(serde_json::Value::as_str);
    let has_updated_input = output.get("updatedInput").is_some();
    let reason = output
        .get("permissionDecisionReason")
        .and_then(serde_json::Value::as_str)
        .and_then(trimmed_non_empty);

    match decision {
        Some("allow") if has_updated_input => None,
        Some("allow") => {
            Some("PreToolUse hook returned unsupported permissionDecision:allow".to_string())
        }
        Some("deny") if reason.is_some() => None,
        Some("deny") => Some(
            "PreToolUse hook returned permissionDecision:deny without a non-empty permissionDecisionReason"
                .to_string(),
        ),
        Some("ask") => {
            Some("PreToolUse hook returned unsupported permissionDecision:ask".to_string())
        }
        Some(other) => Some(format!(
            "PreToolUse hook returned unsupported permissionDecision:{other}"
        )),
        None if has_updated_input => Some(
            "PreToolUse hook returned updatedInput without permissionDecision:allow".to_string(),
        ),
        None if output.get("permissionDecisionReason").is_some() => Some(
            "PreToolUse hook returned permissionDecisionReason without permissionDecision".to_string(),
        ),
        None => None,
    }
}

/// Validate legacy `PreToolUse` decision fields.
fn validate_pre_tool_use_legacy_output(
    value: &serde_json::Value,
) -> Option<String> {
    match value.get("decision").and_then(serde_json::Value::as_str) {
        Some("approve") => {
            Some("PreToolUse hook returned unsupported decision:approve".to_string())
        }
        Some("block") if block_reason(value).is_some() => None,
        Some("block") => Some(
            "PreToolUse hook returned decision:block without a non-empty reason"
                .to_string(),
        ),
        Some(other) => {
            Some(format!("PreToolUse hook returned unsupported decision:{other}"))
        }
        None if value.get("reason").is_some() => {
            Some("PreToolUse hook returned reason without decision".to_string())
        }
        None => None,
    }
}

/// Return a trimmed non-empty string.
fn trimmed_non_empty(reason: &str) -> Option<String> {
    let trimmed = reason.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
