use agent_client_protocol::v2::schema as wire;
use protocol::ToolLocation;

/// Borrows a tool name for its ACP tool kind, shared by live start events and replay.
pub(crate) struct ToolKindOf<'a>(pub(crate) &'a str);

impl From<ToolKindOf<'_>> for wire::ToolKind {
    /// Classifies built-in tools; everything else, including MCP tools, is `other`.
    fn from(name: ToolKindOf<'_>) -> Self {
        match name.0 {
            "read" => Self::Read,
            "write" | "edit" => Self::Edit,
            protocol::EXEC_COMMAND_TOOL | protocol::WRITE_STDIN_TOOL => {
                Self::Execute
            }
            "grep" | "tool_search" => Self::Search,
            // The approval card is a mode switch, so clients can render it as one.
            protocol::EXIT_PLAN_MODE_TOOL => Self::SwitchMode,
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
