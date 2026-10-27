//! Model-facing resource adapters over immutable local catalogs and cancellable remote reads.

use std::io::{self, Write};
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use base64::{Engine, prelude::BASE64_URL_SAFE_NO_PAD};
use protocol::{
    ContentBlock, McpResourceInfo, McpResourcePageKind, McpResourceRef,
    McpResourceRequest, McpResourceTemplateInfo, McpServerId,
    McpSessionSnapshot, SessionId, ToolCall, ToolCallId, ToolDefinition,
    ToolMetadata, ToolOutputPolicy, ToolResult,
};
use serde::{Deserialize, Serialize, ser::SerializeMap};
use tools::{AgentTool, TOOL_TEXT_BYTES, ToolError, ToolExecutionContext};

use crate::McpSession;

/// Three resource operations share routing, validation and one Session owner.
#[derive(Clone, Copy)]
pub(crate) enum McpResourceOperation {
    ListResources,
    ListTemplates,
    Read,
}

impl McpResourceOperation {
    /// Returns the stable model-facing resource tool name.
    fn name(self) -> &'static str {
        match self {
            Self::ListResources => "list_mcp_resources",
            Self::ListTemplates => "list_mcp_resource_templates",
            Self::Read => "read_mcp_resource",
        }
    }

    /// Attaches operation identity without logging opaque URIs or server content.
    fn error(self, message: impl ToString) -> ToolError {
        ToolError::Execution {
            tool: self.name().into(),
            message: message.to_string(),
        }
    }
}

/// Validated snapshot list arguments; absent server means all allowed resource servers.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceListArguments {
    server: Option<McpServerId>,
    cursor: Option<String>,
    limit: Option<usize>,
}

impl TryFrom<&ToolCall> for ResourceListArguments {
    type Error = ToolError;

    /// Rejects fractional, zero or excessive limits before reading the catalog.
    fn try_from(call: &ToolCall) -> Result<Self, Self::Error> {
        // Explicit null is not omission: keep preflight consistent with the advertised JSON Schema.
        if ["server", "cursor"].iter().any(|key| {
            call.arguments
                .get(key)
                .is_some_and(|value| !value.is_string())
        }) || call
            .arguments
            .get("limit")
            .is_some_and(|value| !value.is_u64())
        {
            return Err(ToolError::InvalidArguments {
                tool: call.name.clone(),
                message:
                    "server/cursor must be strings and limit must be an integer"
                        .into(),
            });
        }
        let arguments =
            Self::deserialize(&call.arguments).map_err(|error| {
                ToolError::InvalidArguments {
                    tool: call.name.clone(),
                    message: error.to_string(),
                }
            })?;
        if !(1..=200).contains(&arguments.limit.unwrap_or(50)) {
            return Err(ToolError::InvalidArguments {
                tool: call.name.clone(),
                message: "limit must be an integer from 1 to 200".into(),
            });
        }
        Ok(arguments)
    }
}

/// Opaque URI and explicit route for a remote resource read.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceReadArguments {
    server: McpServerId,
    uri: String,
}

impl TryFrom<&ToolCall> for ResourceReadArguments {
    type Error = ToolError;

    /// Preserves URI bytes while rejecting missing or empty route arguments.
    fn try_from(call: &ToolCall) -> Result<Self, Self::Error> {
        let arguments =
            Self::deserialize(&call.arguments).map_err(|error| {
                ToolError::InvalidArguments {
                    tool: call.name.clone(),
                    message: error.to_string(),
                }
            })?;
        if arguments.uri.is_empty() {
            return Err(ToolError::InvalidArguments {
                tool: call.name.clone(),
                message: "uri must not be empty".into(),
            });
        }
        Ok(arguments)
    }
}

/// Context-bound continuation, deliberately unrelated to the remote MCP cursor protocol.
#[derive(Serialize, Deserialize, typed_builder::TypedBuilder)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpResourceCursor {
    version: u8,
    session_id: SessionId,
    catalog_revision: u64,
    kind: McpResourcePageKind,
    #[builder(default)]
    server: Option<McpServerId>,
    next_index: usize,
}

/// Borrowed sorted records; serializing one page never copies the entire catalog.
enum ResourceItems<'a> {
    Resources(Vec<&'a McpResourceInfo>),
    Templates(Vec<&'a McpResourceTemplateInfo>),
}

impl ResourceItems<'_> {
    /// Returns the count used to validate continuation offsets.
    fn len(&self) -> usize {
        match self {
            Self::Resources(items) => items.len(),
            Self::Templates(items) => items.len(),
        }
    }
}

/// Serialization view over the selected range and its exact continuation token.
struct ResourceSlice<'a> {
    items: &'a ResourceItems<'a>,
    range: std::ops::Range<usize>,
    cursor: Option<String>,
}

impl Serialize for ResourceSlice<'_> {
    /// Emits complete JSON with an array key that identifies the operation family.
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        match self.items {
            ResourceItems::Resources(items) => map.serialize_entry(
                "resources",
                items.get(self.range.clone()).unwrap_or_default(),
            )?,
            ResourceItems::Templates(items) => map.serialize_entry(
                "resourceTemplates",
                items.get(self.range.clone()).unwrap_or_default(),
            )?,
        }
        if let Some(cursor) = &self.cursor {
            map.serialize_entry("nextCursor", cursor)?;
        }
        map.end()
    }
}

/// Stops JSON serialization before one malicious record allocates an unbounded page string.
#[derive(Default)]
struct PageBuffer(Vec<u8>);

impl Write for PageBuffer {
    /// Enforces the final page byte budget while retaining valid complete writes only.
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > TOOL_TEXT_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::other("resource page exceeds text budget"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    /// An in-memory page does not require a flush.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Borrowed listing context used for filtering, cursor validation and bounded pagination.
#[derive(typed_builder::TypedBuilder)]
struct ResourcePage<'a> {
    session: &'a McpSession,
    snapshot: &'a McpSessionSnapshot,
    kind: McpResourcePageKind,
    arguments: &'a ResourceListArguments,
}

impl ResourcePage<'_> {
    /// Returns the largest complete page fitting both count and final JSON byte limits.
    fn result(&self, call_id: ToolCallId) -> Result<ToolResult, ToolError> {
        let operation = match self.kind {
            McpResourcePageKind::Resources => {
                McpResourceOperation::ListResources
            }
            McpResourcePageKind::Templates => {
                McpResourceOperation::ListTemplates
            }
        };
        if let Some(server) = &self.arguments.server {
            self.session
                .ensure_resource_access(server)
                .map_err(|error| operation.error(error))?;
        }
        let allowed = |server: &McpServerId| {
            self.arguments
                .server
                .as_ref()
                .is_none_or(|filter| filter == server)
                && self.session.ensure_resource_access(server).is_ok()
        };
        let items = match self.kind {
            McpResourcePageKind::Resources => {
                let mut items: Vec<_> = self
                    .snapshot
                    .catalog
                    .resources
                    .iter()
                    .filter(|item| allowed(&item.reference.server_id))
                    .collect();
                items.sort_by(|left, right| {
                    (&left.reference.server_id, &left.reference.remote_uri).cmp(
                        &(
                            &right.reference.server_id,
                            &right.reference.remote_uri,
                        ),
                    )
                });
                ResourceItems::Resources(items)
            }
            McpResourcePageKind::Templates => {
                let mut items: Vec<_> = self
                    .snapshot
                    .catalog
                    .resource_templates
                    .iter()
                    .filter(|item| allowed(&item.server_id))
                    .collect();
                items.sort_by(|left, right| {
                    (&left.server_id, &left.uri_template)
                        .cmp(&(&right.server_id, &right.uri_template))
                });
                ResourceItems::Templates(items)
            }
        };
        let start = if let Some(cursor) = &self.arguments.cursor {
            // Decoding uses the encoder's page bound so long valid filters cannot produce unusable cursors.
            if cursor.len() > TOOL_TEXT_BYTES {
                return Err(
                    operation.error("resource cursor exceeds its size limit")
                );
            }
            let bytes = BASE64_URL_SAFE_NO_PAD
                .decode(cursor)
                .map_err(|error| operation.error(error))?;
            let cursor: McpResourceCursor = serde_json::from_slice(&bytes)
                .map_err(|error| operation.error(error))?;
            if cursor.version != 1
                || cursor.session_id != *self.session.session_id()
                || cursor.catalog_revision != self.snapshot.revision
                || cursor.kind != self.kind
                || cursor.server != self.arguments.server
                || cursor.next_index == 0
                || cursor.next_index >= items.len()
            {
                return Err(operation.error("resource cursor is stale or does not match this Session, catalog, kind or filter"));
            }
            cursor.next_index
        } else {
            0
        };
        let maximum =
            (start + self.arguments.limit.unwrap_or(50)).min(items.len());
        let mut lower = start;
        let mut upper = maximum;
        let mut best = None;
        // A complete final page drops its cursor and can be smaller than a prefix; test it before bisecting.
        let mut end = maximum;
        loop {
            let cursor = if end < items.len() {
                let cursor = McpResourceCursor::builder()
                    .version(1)
                    .session_id(self.session.session_id().clone())
                    .catalog_revision(self.snapshot.revision)
                    .kind(self.kind)
                    .server(self.arguments.server.clone())
                    .next_index(end)
                    .build();
                Some(
                    BASE64_URL_SAFE_NO_PAD.encode(
                        serde_json::to_vec(&cursor)
                            .map_err(|error| operation.error(error))?,
                    ),
                )
            } else {
                None
            };
            let mut buffer = PageBuffer::default();
            if serde_json::to_writer(
                &mut buffer,
                &ResourceSlice {
                    items: &items,
                    range: start..end,
                    cursor,
                },
            )
            .is_ok()
            {
                if end > start || items.len() == 0 {
                    best = Some(buffer.0);
                }
                if end == maximum {
                    break;
                }
                lower = end + 1;
            } else {
                if end == start {
                    break;
                }
                upper = end - 1;
            }
            if lower > upper {
                break;
            }
            end = lower + (upper - lower) / 2;
        }
        let bytes = best.ok_or_else(|| {
            operation.error(format!(
                "resource at index {start} cannot fit in one page"
            ))
        })?;
        let text =
            String::from_utf8(bytes).map_err(|error| operation.error(error))?;
        Ok(ToolResult::builder()
            .tool_call_id(call_id)
            .blocks(vec![ContentBlock::Text { text }])
            .is_error(false)
            .build())
    }
}

/// One adapter implementation covers the three fixed model resource operations.
pub(crate) struct McpResourceTool {
    session: Arc<McpSession>,
    operation: McpResourceOperation,
}

impl McpResourceTool {
    /// Retains the Session connection owner without introducing a reverse registry reference.
    pub(crate) fn new(
        session: Arc<McpSession>,
        operation: McpResourceOperation,
    ) -> Self {
        Self { session, operation }
    }
}

#[async_trait]
impl AgentTool for McpResourceTool {
    /// Describes the fixed resource API without injecting remote resource names into the prompt.
    fn definition(&self) -> ToolDefinition {
        let (description, parameters) = match self.operation {
            McpResourceOperation::Read => (
                "Read an opaque URI through its MCP server, including template URIs and unlisted resource links.",
                serde_json::json!({"type":"object","properties":{"server":{"type":"string"},"uri":{"type":"string"}},"required":["server","uri"],"additionalProperties":false}),
            ),
            _ => (
                "List a bounded page from the current MCP catalog snapshot. Continue with nextCursor; restart after catalog changes.",
                serde_json::json!({"type":"object","properties":{"server":{"type":"string"},"cursor":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":200}},"additionalProperties":false}),
            ),
        };
        ToolDefinition {
            name: self.operation.name().into(),
            description: description.into(),
            parameters,
        }
    }

    /// Selects the post-hook page validator or ordinary rich-result budget.
    fn metadata(&self) -> &ToolMetadata {
        static RESOURCES: LazyLock<ToolMetadata> = LazyLock::new(|| {
            ToolMetadata::builder()
                .output_policy(ToolOutputPolicy::ResourcePage(
                    McpResourcePageKind::Resources,
                ))
                .build()
        });
        static TEMPLATES: LazyLock<ToolMetadata> = LazyLock::new(|| {
            ToolMetadata::builder()
                .output_policy(ToolOutputPolicy::ResourcePage(
                    McpResourcePageKind::Templates,
                ))
                .build()
        });
        static READ: LazyLock<ToolMetadata> = LazyLock::new(|| {
            ToolMetadata::builder()
                .output_policy(ToolOutputPolicy::Bounded)
                .build()
        });
        match self.operation {
            McpResourceOperation::ListResources => &RESOURCES,
            McpResourceOperation::ListTemplates => &TEMPLATES,
            McpResourceOperation::Read => &READ,
        }
    }

    /// Rejects invalid arguments at the same preflight boundary as other tools.
    fn validate(&self, call: &ToolCall) -> Result<(), ToolError> {
        if call.name != self.operation.name() {
            return Err(ToolError::NotFound(call.name.clone()));
        }
        match self.operation {
            McpResourceOperation::Read => {
                ResourceReadArguments::try_from(call).map(|_| ())
            }
            _ => ResourceListArguments::try_from(call).map(|_| ()),
        }
    }

    /// Uses the immutable catalog for lists and the actual remote transport for reads.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        context.ensure_active()?;
        self.validate(&call)?;
        match self.operation {
            McpResourceOperation::Read => {
                let arguments = ResourceReadArguments::try_from(&call)?;
                let result = self
                    .session
                    .read_resource(
                        McpResourceRequest {
                            reference: McpResourceRef {
                                server_id: arguments.server,
                                remote_uri: arguments.uri,
                            },
                        },
                        &context.cancellation,
                    )
                    .await
                    .map_err(|error| self.operation.error(error))?;
                Ok(ToolResult::builder()
                    .tool_call_id(call.tool_call_id)
                    .blocks(result.contents)
                    .is_error(false)
                    .build())
            }
            operation => {
                let arguments = ResourceListArguments::try_from(&call)?;
                let snapshot = self.session.snapshot();
                ResourcePage::builder()
                    .session(&self.session)
                    .snapshot(&snapshot)
                    .kind(match operation {
                        McpResourceOperation::ListResources => {
                            McpResourcePageKind::Resources
                        }
                        _ => McpResourcePageKind::Templates,
                    })
                    .arguments(&arguments)
                    .build()
                    .result(call.tool_call_id)
            }
        }
    }
}
