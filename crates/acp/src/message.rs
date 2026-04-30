use std::collections::HashMap;

use agent_client_protocol::schema as official_acp;
use agent_client_protocol::{JsonRpcNotification, jsonrpcmsg, util::json_cast};
use llm::completion::{
    Message,
    message::{AssistantContent, ReasoningContent, ToolResultContent, UserContent},
};
use serde::Serialize;
use serde_json::Value;
use tools::StructuredToolOutput;

/// Builder for official ACP session updates used by the agent adapter.
pub(crate) struct AcpMessage;

impl AcpMessage {
    /// Builds a user text chunk update.
    pub(crate) fn user_text(text: impl Into<String>) -> official_acp::SessionUpdate {
        official_acp::SessionUpdate::UserMessageChunk(text_chunk(text))
    }

    /// Builds an assistant-visible text chunk update.
    pub(crate) fn agent_text(text: impl Into<String>) -> official_acp::SessionUpdate {
        official_acp::SessionUpdate::AgentMessageChunk(text_chunk(text))
    }

    /// Builds an assistant reasoning text chunk update.
    pub(crate) fn thought_text(text: impl Into<String>) -> official_acp::SessionUpdate {
        official_acp::SessionUpdate::AgentThoughtChunk(text_chunk(text))
    }

    /// Builds an ACP tool-call start update with the raw tool input attached.
    pub(crate) fn tool_started(
        handle_id: impl Into<official_acp::ToolCallId>,
        name: impl Into<String>,
        arguments: Value,
    ) -> official_acp::SessionUpdate {
        official_acp::SessionUpdate::ToolCall(
            official_acp::ToolCall::new(handle_id, name)
                .status(official_acp::ToolCallStatus::InProgress)
                .raw_input(arguments),
        )
    }

    /// Builds an ACP tool-call completion update with optional structured output.
    pub(crate) fn tool_completed(
        handle_id: impl Into<official_acp::ToolCallId>,
        name: impl Into<String>,
        output: impl Into<String>,
        structured_output: Option<StructuredToolOutput>,
    ) -> official_acp::SessionUpdate {
        official_acp::SessionUpdate::ToolCallUpdate(official_acp::ToolCallUpdate::new(
            handle_id,
            official_acp::ToolCallUpdateFields::new()
                .title(name.into())
                .status(official_acp::ToolCallStatus::Completed)
                .content(vec![official_acp::ToolCallContent::Content(
                    official_acp::Content::new(output.into()),
                )])
                .raw_output(structured_output.map(|value| value.to_serde_value())),
        ))
    }

    /// Builds an ACP tool-call failure update with the rendered error text attached.
    pub(crate) fn tool_failed(
        handle_id: impl Into<official_acp::ToolCallId>,
        name: impl Into<String>,
        error_message: impl Into<String>,
        structured_output: Option<StructuredToolOutput>,
    ) -> official_acp::SessionUpdate {
        official_acp::SessionUpdate::ToolCallUpdate(official_acp::ToolCallUpdate::new(
            handle_id,
            official_acp::ToolCallUpdateFields::new()
                .title(name.into())
                .status(official_acp::ToolCallStatus::Failed)
                .content(vec![official_acp::ToolCallContent::Content(
                    official_acp::Content::new(error_message.into()),
                )])
                .raw_output(structured_output.map(|value| value.to_serde_value())),
        ))
    }
}

/// Extension trait that converts stored messages into ACP session updates
/// for history replay.
pub(crate) trait MessageHistoryExt {
    fn to_acp(self) -> Vec<official_acp::SessionUpdate>;
}

impl MessageHistoryExt for Vec<Message> {
    fn to_acp(self) -> Vec<official_acp::SessionUpdate> {
        let mut updates = Vec::new();
        let mut tool_calls: HashMap<String, (String, String)> = HashMap::new();

        for msg in self {
            match msg {
                Message::User { content } => {
                    for item in content.into_iter() {
                        match item {
                            UserContent::Text(t) => {
                                updates.push(AcpMessage::user_text(t.text));
                            }
                            UserContent::ToolResult(tr) => {
                                let call_id = tr.call_id.as_deref().unwrap_or(&tr.id);
                                let (handle_id, name) = tool_calls
                                    .get(call_id)
                                    .cloned()
                                    .unwrap_or_else(|| (tr.id, "unknown".into()));

                                let output = tr
                                    .content
                                    .into_iter()
                                    .filter_map(|c| match c {
                                        ToolResultContent::Text(t) => Some(t.text),
                                        _ => None,
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");

                                updates.push(AcpMessage::tool_completed(
                                    handle_id, name, output, None,
                                ));
                            }
                            _ => {}
                        }
                    }
                }
                Message::Assistant { content, .. } => {
                    for item in content.into_iter() {
                        match item {
                            AssistantContent::Text(t) => {
                                updates.push(AcpMessage::agent_text(t.text + "\n"));
                            }
                            AssistantContent::Reasoning(r) => {
                                let text: String = r
                                    .content
                                    .into_iter()
                                    .filter_map(|rc| match rc {
                                        ReasoningContent::Text { text, .. } => Some(text),
                                        _ => None,
                                    })
                                    .collect::<Vec<_>>()
                                    .join("");

                                if !text.is_empty() {
                                    updates.push(AcpMessage::thought_text(text));
                                }
                            }

                            AssistantContent::ToolCall(tc) => {
                                let handle_id = tc.call_id.clone().unwrap_or_else(|| tc.id.clone());
                                let call_id = tc.call_id.unwrap_or_else(|| tc.id.clone());

                                tool_calls
                                    .insert(call_id, (handle_id.clone(), tc.function.name.clone()));

                                updates.push(AcpMessage::tool_started(
                                    handle_id,
                                    tc.function.name,
                                    tc.function.arguments,
                                ));
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        updates
    }
}

/// Builder for the JSON-RPC envelopes still used by the local ACP transport.
pub(crate) struct SessionRpc;

impl SessionRpc {
    /// Wraps one ACP session update in a JSON-RPC notification envelope.
    pub(crate) fn session_update(session_id: &str, update: official_acp::SessionUpdate) -> Value {
        Self::notification(official_acp::SessionNotification::new(
            session_id.to_string(),
            update,
        ))
    }

    /// Builds a JSON-RPC notification envelope from an official ACP notification type.
    pub(crate) fn notification<N>(notification: N) -> Value
    where
        N: JsonRpcNotification,
    {
        let message = notification
            .to_untyped_message()
            .expect("official ACP notification conversion should not fail");
        let params = json_cast(message.params)
            .expect("official ACP notification params should be JSON-RPC params");
        let request = jsonrpcmsg::Request::new_v2(message.method, params, None);
        Self::value(&request)
    }

    /// Builds a successful JSON-RPC response envelope.
    pub(crate) fn response(id: Value, result: Value) -> Value {
        let response = jsonrpcmsg::Response::success_v2(result, Some(Self::id(id)));
        Self::value(&response)
    }

    /// Builds a JSON-RPC error response envelope.
    pub(crate) fn error(id: Value, error: Value) -> Value {
        let error = serde_json::from_value(error).expect("JSON-RPC error object should be valid");
        let response = jsonrpcmsg::Response::error_v2(error, Some(Self::id(id)));
        Self::value(&response)
    }

    /// Serializes official ACP schema values for JSON-RPC envelopes.
    pub(crate) fn value<T>(value: &T) -> Value
    where
        T: Serialize,
    {
        serde_json::to_value(value).expect("official ACP schema serialization should not fail")
    }

    /// Converts a dynamic JSON-RPC id value into the SDK JSON-RPC id representation.
    fn id(id: Value) -> jsonrpcmsg::Id {
        serde_json::from_value(id).unwrap_or(jsonrpcmsg::Id::Null)
    }
}

/// Builds a text content chunk from plain text.
fn text_chunk(text: impl Into<String>) -> official_acp::ContentChunk {
    official_acp::ContentChunk::new(official_acp::ContentBlock::from(text.into()))
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema as official_acp;
    use serde_json::json;

    use super::{AcpMessage, SessionRpc};

    /// Verifies text update helpers preserve official ACP session update shapes.
    #[test]
    fn update_helpers_build_typed_text_chunks() {
        let user = AcpMessage::user_text("hello");
        let agent = AcpMessage::agent_text("world");
        let thought = AcpMessage::thought_text("thinking");

        assert!(matches!(
            user,
            official_acp::SessionUpdate::UserMessageChunk(_)
        ));
        assert!(matches!(
            agent,
            official_acp::SessionUpdate::AgentMessageChunk(_)
        ));
        assert!(matches!(
            thought,
            official_acp::SessionUpdate::AgentThoughtChunk(_)
        ));
    }

    /// Verifies tool helpers omit optional raw output when no structured result is available.
    #[test]
    fn update_helpers_omit_missing_tool_raw_output() {
        let update = AcpMessage::tool_completed("call-1", "exec_command", "ok", None);
        let value = serde_json::to_value(update).expect("update should serialize");

        assert_eq!(value["sessionUpdate"], json!("tool_call_update"));
        assert_eq!(value["toolCallId"], json!("call-1"));
        assert_eq!(value["status"], json!("completed"));
        assert!(value.get("rawOutput").is_none());
    }

    /// Verifies failed tool updates carry `failed` status and the rendered error text.
    #[test]
    fn update_helpers_build_failed_tool_updates() {
        let update = AcpMessage::tool_failed("call-1", "apply_patch", "bad patch", None);
        let value = serde_json::to_value(update).expect("update should serialize");

        assert_eq!(value["sessionUpdate"], json!("tool_call_update"));
        assert_eq!(value["toolCallId"], json!("call-1"));
        assert_eq!(value["status"], json!("failed"));
        assert_eq!(value["content"][0]["content"]["text"], json!("bad patch"));
        assert!(value.get("rawOutput").is_none());
    }

    /// Verifies JSON-RPC helpers keep envelope details out of ACP business logic.
    #[test]
    fn rpc_helpers_build_protocol_envelopes() {
        let notification =
            SessionRpc::session_update("session-1", AcpMessage::agent_text("answer"));
        let response = SessionRpc::response(json!(7), json!({ "ok": true }));
        let error = SessionRpc::error(json!(8), json!({ "code": -32602, "message": "bad params" }));

        assert_eq!(notification["jsonrpc"], json!("2.0"));
        assert_eq!(notification["method"], json!("session/update"));
        assert_eq!(notification["params"]["sessionId"], json!("session-1"));
        assert_eq!(response["id"], json!(7));
        assert_eq!(response["result"]["ok"], json!(true));
        assert_eq!(error["id"], json!(8));
        assert_eq!(error["error"]["code"], json!(-32602));
    }
}
