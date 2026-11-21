//! Why a backend operation failed.

/// Why a backend operation failed.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// The application or the ACP connection could not be started.
    #[error("{0}")]
    Startup(String),
    /// The agent rejected a request or answered with an unexpected shape.
    #[error("{0}")]
    Request(String),
    /// The backend runtime is gone.
    #[error("the backend has stopped")]
    Stopped,
    /// The agent cancelled the request (JSON-RPC `-32800`), for example a
    /// queued prompt that was removed or whose run was stopped.
    #[error("the request was cancelled")]
    Cancelled,
    /// The chosen model needs its provider signed in first; carries the auth method id.
    #[error("sign-in required ({0})")]
    AuthRequired(String),
    /// The question was answered or skipped elsewhere already.
    #[error("the question is closed")]
    QuestionClosed,
}

impl From<agent_client_protocol::Error> for BackendError {
    /// Keeps cancellation distinguishable from real failures.
    fn from(error: agent_client_protocol::Error) -> Self {
        let data = error.data.as_ref();
        let field = |name: &str| {
            data.and_then(|data| data.get(name))
                .and_then(serde_json::Value::as_str)
        };
        if error.code == agent_client_protocol::ErrorCode::REQUEST_CANCELLED {
            BackendError::Cancelled
        } else if field("reason") == Some("questionClosed") {
            BackendError::QuestionClosed
        } else if let Some(method) = field("methodId") {
            BackendError::AuthRequired(method.to_owned())
        } else {
            BackendError::Request(error.to_string())
        }
    }
}
