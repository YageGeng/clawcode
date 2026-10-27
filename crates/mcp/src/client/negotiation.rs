//! Checks the initialize response before rmcp can acknowledge the connection.

use std::sync::{Arc, Mutex};

use protocol::McpProtocolVersion;
use rmcp::model::{
    ClientJsonRpcMessage, ClientRequest, RequestId, ServerJsonRpcMessage,
    ServerResult,
};
use rmcp::service::RoleClient;
use rmcp::transport::Transport;

/// Startup failures retained when closing an invalid transport response.
#[derive(Debug, Clone, thiserror::Error)]
pub(super) enum NegotiationFailure {
    #[error("unsupported server protocol: {0}")]
    UnsupportedVersion(String),
    #[error("initialize response has an unexpected request id")]
    UnexpectedResponseId,
    #[error("expected an initialize result")]
    InvalidInitializeResult,
}

/// Shared handshake outcome needed after the transport moves into the rmcp service.
#[derive(Debug)]
pub(super) enum NegotiationOutcome {
    Pending(Option<RequestId>),
    Accepted(McpProtocolVersion),
    Rejected(NegotiationFailure),
}

/// Delegates normal traffic while guarding the single initialize response.
pub(super) struct NegotiationTransport<T> {
    inner: T,
    outcome: Arc<Mutex<NegotiationOutcome>>,
}

impl<T> NegotiationTransport<T> {
    /// Returns the guarded transport and its retained startup result.
    pub(super) fn new(inner: T) -> (Self, Arc<Mutex<NegotiationOutcome>>) {
        let outcome = Arc::new(Mutex::new(NegotiationOutcome::Pending(None)));
        (
            Self {
                inner,
                outcome: Arc::clone(&outcome),
            },
            outcome,
        )
    }
}

impl<T: Transport<RoleClient>> Transport<RoleClient>
    for NegotiationTransport<T>
{
    type Error = T::Error;

    /// Registers the initialize id before dispatch while preserving the transport's send future.
    fn send(
        &mut self,
        message: ClientJsonRpcMessage,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        if let ClientJsonRpcMessage::Request(request) = &message
            && matches!(request.request, ClientRequest::InitializeRequest(_))
        {
            *self
                .outcome
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                NegotiationOutcome::Pending(Some(request.id.clone()));
        }
        self.inner.send(message)
    }

    /// Rejects unsupported or miscorrelated initialization before initialized can be emitted.
    async fn receive(&mut self) -> Option<ServerJsonRpcMessage> {
        let message = self.inner.receive().await?;
        let reject = {
            let mut outcome = self
                .outcome
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let NegotiationOutcome::Pending(Some(expected)) = &*outcome {
                let failure = match &message {
                    ServerJsonRpcMessage::Response(response)
                        if &response.id != expected =>
                    {
                        Some(NegotiationFailure::UnexpectedResponseId)
                    }
                    ServerJsonRpcMessage::Response(response) => match &response
                        .result
                    {
                        ServerResult::InitializeResult(result) => {
                            match result.protocol_version.as_str().parse() {
                                Ok(version) => {
                                    *outcome =
                                        NegotiationOutcome::Accepted(version);
                                    None
                                }
                                Err(_) => Some(
                                    NegotiationFailure::UnsupportedVersion(
                                        result.protocol_version.to_string(),
                                    ),
                                ),
                            }
                        }
                        _ => Some(NegotiationFailure::InvalidInitializeResult),
                    },
                    ServerJsonRpcMessage::Error(error)
                        if error.id.as_ref() != Some(expected) =>
                    {
                        Some(NegotiationFailure::UnexpectedResponseId)
                    }
                    _ => None,
                };
                if let Some(failure) = failure {
                    *outcome = NegotiationOutcome::Rejected(failure);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if reject {
            let _ = self.inner.close().await;
            None
        } else {
            Some(message)
        }
    }

    /// Forwards explicit shutdown without retaining a transport worker.
    async fn close(&mut self) -> Result<(), Self::Error> {
        self.inner.close().await
    }
}
