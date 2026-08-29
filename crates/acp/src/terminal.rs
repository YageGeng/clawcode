use agent_client_protocol::{
    JsonRpcMessage, JsonRpcNotification, UntypedMessage,
};
use protocol::{ProductIdentity, TerminalUpdateNotification};

/// Product-scoped realtime notification carrying one terminal lifecycle event.
#[derive(Debug, Clone)]
pub(crate) struct AcpTerminalUpdateNotification(
    pub(crate) TerminalUpdateNotification,
);

impl JsonRpcMessage for AcpTerminalUpdateNotification {
    /// Matches only the centralized product terminal update notification.
    fn matches_method(method: &str) -> bool {
        method == ProductIdentity::ACP_TERMINAL_UPDATE_NOTIFICATION
    }

    /// Returns the centralized product terminal notification method.
    fn method(&self) -> &'static str {
        ProductIdentity::ACP_TERMINAL_UPDATE_NOTIFICATION
    }

    /// Serializes the typed terminal payload through one untyped JSON-RPC envelope.
    fn to_untyped_message(
        &self,
    ) -> Result<UntypedMessage, agent_client_protocol::Error> {
        UntypedMessage::new(self.method(), &self.0)
    }

    /// Parses one typed terminal payload without accepting unknown protocol fields.
    fn parse_message(
        method: &str,
        params: &impl serde::Serialize,
    ) -> Result<Self, agent_client_protocol::Error> {
        if !Self::matches_method(method) {
            return Err(agent_client_protocol::Error::method_not_found());
        }
        let value = serde_json::to_value(params)
            .map_err(agent_client_protocol::Error::into_internal_error)?;
        serde_json::from_value(value).map(Self).map_err(|error| {
            agent_client_protocol::Error::invalid_params()
                .data(error.to_string())
        })
    }
}

impl JsonRpcNotification for AcpTerminalUpdateNotification {}

#[cfg(test)]
mod tests {
    use agent_client_protocol::JsonRpcMessage as _;
    use protocol::{SessionId, TerminalUpdateNotification};

    use super::AcpTerminalUpdateNotification;

    /// Terminal notifications preserve their method and typed event payload.
    #[test]
    fn terminal_notification_round_trips_through_json_rpc() {
        let notification =
            AcpTerminalUpdateNotification(TerminalUpdateNotification {
                session_id: SessionId::try_from("terminal-notification")
                    .expect("session id"),
                revision: 4,
            });
        let message = notification.to_untyped_message().expect("notification");
        assert_eq!(
            message.method,
            protocol::ProductIdentity::ACP_TERMINAL_UPDATE_NOTIFICATION
        );
        let reparsed = AcpTerminalUpdateNotification::parse_message(
            &message.method,
            &notification.0,
        )
        .expect("parse notification");
        assert_eq!(reparsed.0, notification.0);
    }
}
