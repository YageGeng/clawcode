//! Transport helpers for building MCP connections.

use crate::error::McpError;

/// Build a tokio Command from stdio config.
pub(crate) fn build_stdio_command(
    command: &str,
    args: &[String],
    env: &std::collections::HashMap<String, String>,
) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd
}

/// Build headers for StreamableHTTP transport.
pub(crate) fn build_http_headers(
    bearer_token_env: &Option<String>,
    http_headers: &std::collections::HashMap<String, String>,
) -> Result<reqwest::header::HeaderMap, McpError> {
    let mut headers = reqwest::header::HeaderMap::new();

    if let Some(env_var) = bearer_token_env
        && let Ok(token) = std::env::var(env_var)
    {
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_e| McpError::Transport("bad bearer token".into()))?,
        );
    }

    for (k, v) in http_headers {
        let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
            .map_err(|_e| McpError::Transport(format!("bad header name '{k}'")))?;
        let value = reqwest::header::HeaderValue::from_str(v)
            .map_err(|_e| McpError::Transport(format!("bad header value '{v}'")))?;
        headers.insert(name, value);
    }

    Ok(headers)
}
