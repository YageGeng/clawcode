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
    /// A sign-in the request needed did not complete; the reason is logged.
    #[error("the sign-in did not complete")]
    SignInFailed,
    /// The question was answered or skipped elsewhere already.
    #[error("the question is closed")]
    QuestionClosed,
    /// The history database is off, still backfilling, or repairing.
    #[error("the session history is not available yet")]
    HistoryUnavailable,
}

/// Why a session could not be started with its options.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The session could not be created.
    #[error("{0}")]
    Create(BackendError),
    /// An option could not be applied; the session was deleted again.
    #[error("{0}")]
    Configure(BackendError),
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
        } else if field("reason") == Some("historyUnavailable") {
            BackendError::HistoryUnavailable
        } else if let Some(method) = field("methodId") {
            BackendError::AuthRequired(method.to_owned())
        } else {
            BackendError::Request(error.to_string())
        }
    }
}
