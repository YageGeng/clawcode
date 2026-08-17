//! Shared standards-compliant MCP Server used by both transport examples.

use std::borrow::Cow;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult,
    CompleteRequestParams, CompleteResult, CompletionInfo,
    GetPromptRequestParams, GetPromptResponse, GetPromptResult, Implementation,
    ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult,
    ListToolsResult, PaginatedRequestParams, Prompt, PromptArgument,
    PromptMessage, ProtocolVersion, ReadResourceRequestParams,
    ReadResourceResponse, ReadResourceResult, Resource, ResourceContents,
    ResourceTemplate, Role, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler};

const TOOL_NAME: &str = "inspect";
const PROMPT_NAME: &str = "explain";
const RESOURCE_URI: &str = "reference://guide";
const RESOURCE_TEMPLATE: &str = "reference://topic/{name}";

/// Exact lifecycle exposed by one reference Server instance.
#[derive(Debug, Clone, Copy)]
#[allow(
    dead_code,
    reason = "each transport example compiles only its selected exact protocol"
)]
enum ReferenceProtocol {
    Legacy,
    Modern,
}

impl ReferenceProtocol {
    /// Returns the single protocol version accepted by this Server instance.
    fn supported_versions(self) -> Cow<'static, [ProtocolVersion]> {
        match self {
            Self::Legacy => Cow::Borrowed(&[ProtocolVersion::V_2025_11_25]),
            Self::Modern => Cow::Borrowed(&[ProtocolVersion::V_2026_07_28]),
        }
    }
}

/// Small standards-compliant MCP Server covering every baseline catalog family.
#[derive(Debug, Clone, Copy)]
pub struct ReferenceServer {
    protocol: ReferenceProtocol,
}

#[allow(
    dead_code,
    reason = "each transport example compiles only its selected exact protocol"
)]
impl ReferenceServer {
    /// Creates a Server that only accepts the Legacy 2025-11-25 lifecycle.
    #[must_use]
    pub fn legacy() -> Self {
        Self {
            protocol: ReferenceProtocol::Legacy,
        }
    }

    /// Creates a Server that only accepts the Modern 2026-07-28 lifecycle.
    #[must_use]
    pub fn modern() -> Self {
        Self {
            protocol: ReferenceProtocol::Modern,
        }
    }
}

impl ServerHandler for ReferenceServer {
    /// Advertises all baseline capabilities exercised by the agent and WebUI.
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_completions()
                .enable_prompts()
                .enable_resources()
                .enable_tools()
                .build(),
        )
        .with_server_info(Implementation::new("mcp-reference-server", "1.0.0"))
        .with_instructions("Reference server for MCP interoperability checks")
    }

    /// Restricts negotiation to the exact protocol selected at construction.
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        self.protocol.supported_versions()
    }

    /// Lists the structured inspection Tool.
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let input_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "value": { "type": "string" }
            },
            "required": ["value"],
            "additionalProperties": false
        })
        .as_object()
        .cloned()
        .unwrap_or_default();
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            TOOL_NAME,
            "Returns the supplied value as structured MCP content",
            input_schema,
        )]))
    }

    /// Executes the inspection Tool with protocol-level argument validation.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if request.name.as_ref() != TOOL_NAME {
            return Err(ErrorData::invalid_params(
                "unknown reference Tool",
                None,
            ));
        }
        let value = request
            .arguments
            .as_ref()
            .and_then(|arguments| arguments.get("value"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                ErrorData::invalid_params("value must be a string", None)
            })?;
        Ok(
            CallToolResult::structured(serde_json::json!({ "value": value }))
                .into(),
        )
    }

    /// Lists one Prompt whose argument also supports Completion.
    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(vec![
            Prompt::new(
                PROMPT_NAME,
                Some("Explains a requested topic"),
                Some(vec![
                    PromptArgument::new("topic")
                        .with_description("Topic to explain")
                        .with_required(true),
                ]),
            )
            .with_title("Explain a topic"),
        ]))
    }

    /// Renders one user message from the requested Prompt arguments.
    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        if request.name != PROMPT_NAME {
            return Err(ErrorData::invalid_params(
                "unknown reference Prompt",
                None,
            ));
        }
        let topic = request
            .arguments
            .as_ref()
            .and_then(|arguments| arguments.get("topic"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                ErrorData::invalid_params("topic must be a string", None)
            })?;
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
            Role::User,
            format!("Explain {topic} with one concrete example."),
        )])
        .with_description("Reference Prompt result")
        .into())
    }

    /// Lists one static Resource.
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new(RESOURCE_URI, "reference-guide")
                .with_title("Reference Guide")
                .with_description("MCP interoperability reference content")
                .with_mime_type("text/plain"),
        ]))
    }

    /// Lists one RFC 6570 Resource Template.
    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            ResourceTemplate::new(RESOURCE_TEMPLATE, "reference-topic")
                .with_title("Reference Topic")
                .with_description("Reads one named reference topic")
                .with_mime_type("text/plain"),
        ]))
    }

    /// Reads static or templated reference content without filesystem access.
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let text = if request.uri == RESOURCE_URI {
            "This is the MCP interoperability reference guide.".to_string()
        } else if let Some(topic) =
            request.uri.strip_prefix("reference://topic/")
        {
            format!("Reference topic: {topic}")
        } else {
            return Err(ErrorData::invalid_params(
                "unknown reference Resource",
                None,
            ));
        };
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, request.uri)
                .with_mime_type("text/plain"),
        ])
        .into())
    }

    /// Completes Prompt topics and Resource Template names from a fixed safe vocabulary.
    async fn complete(
        &self,
        request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        let candidates = ["Rust", "Runtime", "Routing"];
        let values = candidates
            .into_iter()
            .filter(|candidate| {
                candidate
                    .to_lowercase()
                    .starts_with(&request.argument.value.to_lowercase())
            })
            .map(str::to_string)
            .collect();
        let completion = CompletionInfo::with_all_values(values)
            .map_err(|error| ErrorData::invalid_params(error, None))?;
        Ok(CompleteResult::new(completion))
    }
}
