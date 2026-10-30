use agent_client_protocol::schema::v2 as wire;
use protocol::{ToolCall, ToolLocation};

/// Borrows a Kernel tool call for its ACP tool kind.
pub(crate) struct ToolKindOf<'a>(pub(crate) &'a ToolCall);

impl From<ToolKindOf<'_>> for wire::ToolKind {
    /// Classifies built-in tools; everything else, including MCP tools, is `other`.
    fn from(call: ToolKindOf<'_>) -> Self {
        match call.0.name.as_str() {
            "read" => Self::Read,
            "write" | "edit" => Self::Edit,
            "bash" => Self::Execute,
            "tool_search" => Self::Search,
            _ => Self::Other,
        }
    }
}

/// Borrows the file locations a tool reported for their ACP form.
///
/// Paths arrive already resolved by the tool, so ACP never re-derives them.
pub(crate) struct ToolLocationsOf<'a>(pub(crate) &'a [ToolLocation]);

impl From<ToolLocationsOf<'_>> for Vec<wire::ToolCallLocation> {
    /// Keeps each path and its one-based line.
    fn from(locations: ToolLocationsOf<'_>) -> Self {
        locations
            .0
            .iter()
            .map(|location| {
                wire::ToolCallLocation::new(location.path.clone())
                    .line(location.line)
            })
            .collect()
    }
}

/// Terminal flags of one tool result, ordered so cancellation wins over failure.
pub(crate) struct ToolStatusOf {
    pub(crate) cancelled: bool,
    pub(crate) is_error: bool,
}

impl From<ToolStatusOf> for wire::ToolCallStatus {
    /// Keeps live end events, persisted messages, and replay on one terminal status.
    fn from(status: ToolStatusOf) -> Self {
        if status.cancelled {
            Self::Cancelled
        } else if status.is_error {
            Self::Failed
        } else {
            Self::Completed
        }
    }
}
