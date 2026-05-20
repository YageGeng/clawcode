//! Structured turn item types for display-only lifecycle events.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Stable identifier for one model turn within a session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TurnId(pub String);

impl fmt::Display for TurnId {
    /// Render the wrapped turn id for persistence and logging boundaries.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<TurnId> for String {
    /// Convert a turn id into its serialized string representation.
    fn from(turn_id: TurnId) -> Self {
        turn_id.0
    }
}

impl From<String> for TurnId {
    /// Wrap a generated or restored turn id string.
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for TurnId {
    /// Wrap a borrowed turn id string.
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<&TurnId> for String {
    /// Clone a turn id into its serialized string representation.
    fn from(turn_id: &TurnId) -> Self {
        turn_id.0.clone()
    }
}

/// Structured item emitted during a turn for rich frontend display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnItem {
    /// A file-changing tool invocation, such as apply_patch or edit.
    FileChange(FileChangeItem),
    /// An MCP tool invocation with MCP-specific identity and result fields.
    McpToolCall(McpToolCallItem),
    /// A shell/exec command invocation with streaming lifecycle.
    ExecCommand(ExecCommandItem),
}

/// File-change lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeStatus {
    /// The file-changing tool is currently running.
    InProgress,
    /// The file-changing tool completed successfully.
    Completed,
    /// The file-changing tool failed before completing the requested change.
    Failed,
    /// The file-changing tool was declined before execution.
    Declined,
}

/// Final before/after state for one changed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct FileChange {
    /// Path displayed to the frontend.
    pub path: PathBuf,
    /// Content before the tool ran; `None` means the file was newly created.
    #[builder(default, setter(strip_option))]
    pub old_text: Option<String>,
    /// Content after the tool ran; an empty string represents a deleted file.
    pub new_text: String,
}

/// Partial file-change preview parsed from streamed apply_patch arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PatchPreviewChange {
    /// Preview for a file that is being added by the patch.
    Add {
        /// Path displayed to the frontend.
        path: PathBuf,
        /// Partial or complete new file content parsed from the patch stream.
        content: String,
    },
    /// Preview for a file that is being deleted by the patch.
    Delete {
        /// Path displayed to the frontend.
        path: PathBuf,
    },
    /// Preview for a file update using local old/new hunk text.
    Update {
        /// Source path from the patch.
        path: PathBuf,
        /// Optional destination path when the update also moves the file.
        #[serde(skip_serializing_if = "Option::is_none")]
        move_path: Option<PathBuf>,
        /// Local hunk text before the previewed update.
        old_text: String,
        /// Local hunk text after the previewed update.
        new_text: String,
    },
}

/// Turn item describing the lifecycle and result of a file-changing tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct FileChangeItem {
    /// Tool call id shared with existing tool-call events.
    pub id: String,
    /// Human-facing title for the file change.
    pub title: String,
    /// Final file states produced by the tool.
    pub changes: Vec<FileChange>,
    /// Current file-change lifecycle status.
    pub status: FileChangeStatus,
    /// Short text result returned to the model.
    #[builder(default, setter(strip_option))]
    pub model_output: Option<String>,
}

/// MCP tool-call lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpToolCallStatus {
    /// The MCP call is currently running.
    InProgress,
    /// The MCP call completed successfully.
    Completed,
    /// The MCP call failed or returned an error result.
    Failed,
}

/// Turn item describing an MCP tool-call lifecycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct McpToolCallItem {
    /// Tool call id shared with existing tool-call events.
    pub id: String,
    /// MCP server name from configuration.
    pub server: String,
    /// MCP tool name provided by the server.
    pub tool: String,
    /// JSON arguments passed to the MCP tool.
    pub arguments: serde_json::Value,
    /// Current MCP lifecycle status.
    pub status: McpToolCallStatus,
    /// Structured MCP result, retained as JSON until the MCP crate exposes a stable type.
    #[builder(default, setter(strip_option))]
    pub result: Option<serde_json::Value>,
    /// Human-readable MCP error.
    #[builder(default, setter(strip_option))]
    pub error: Option<String>,
    /// Duration in milliseconds when the call has completed.
    #[builder(default, setter(strip_option))]
    pub duration_ms: Option<u64>,
}

/// shell/exec command lifecycle item emitted via ItemStarted/ItemCompleted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct ExecCommandItem {
    /// Tool call id shared with existing tool-call events.
    pub id: String,
    /// Command-line argument array, first element is the program.
    pub command: Vec<String>,
    /// Working directory for the command.
    pub cwd: PathBuf,
    /// Current lifecycle status.
    pub status: ExecCommandStatus,
    /// Full stdout, populated in terminal states.
    #[builder(default, setter(strip_option))]
    pub stdout: Option<String>,
    /// Full stderr, populated in terminal states.
    #[builder(default, setter(strip_option))]
    pub stderr: Option<String>,
    /// Exit code, populated in terminal states.
    #[builder(default, setter(strip_option))]
    pub exit_code: Option<i32>,
    /// Wall-clock duration in milliseconds, populated in terminal states.
    #[builder(default, setter(strip_option))]
    pub duration_ms: Option<u64>,
}

/// ExecCommand lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandStatus {
    /// Command has started execution.
    InProgress,
    /// Command completed successfully (exit_code == 0).
    Completed,
    /// Command failed (non-zero exit code or abnormal termination).
    Failed,
    /// Command was declined by the user.
    Declined,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that TurnId keeps a string-only wire representation.
    #[test]
    fn turn_id_serializes_as_string_newtype() {
        let turn_id = TurnId("turn-1".to_string());

        let encoded = serde_json::to_string(&turn_id).expect("serialize turn id");
        let decoded: TurnId = serde_json::from_str(&encoded).expect("deserialize turn id");

        assert_eq!(encoded, "\"turn-1\"");
        assert_eq!(decoded, turn_id);
    }

    /// Verifies that file-change items preserve add, update, and delete states.
    #[test]
    fn file_change_item_roundtrips_all_file_states() {
        let item = TurnItem::FileChange(
            FileChangeItem::builder()
                .id("call-1".to_string())
                .title("Apply patch".to_string())
                .changes(vec![
                    FileChange::builder()
                        .path(PathBuf::from("added.txt"))
                        .new_text("new\n".to_string())
                        .build(),
                    FileChange::builder()
                        .path(PathBuf::from("updated.txt"))
                        .old_text("old\n".to_string())
                        .new_text("new\n".to_string())
                        .build(),
                    FileChange::builder()
                        .path(PathBuf::from("deleted.txt"))
                        .old_text("old\n".to_string())
                        .new_text(String::new())
                        .build(),
                ])
                .status(FileChangeStatus::Completed)
                .model_output("A added.txt\nM updated.txt\nD deleted.txt".to_string())
                .build(),
        );

        let encoded = serde_json::to_string(&item).expect("serialize file change item");
        let decoded: TurnItem = serde_json::from_str(&encoded).expect("deserialize item");

        assert_eq!(decoded, item);
    }

    /// Verifies that patch preview changes preserve partial update old/new text.
    #[test]
    fn patch_preview_change_roundtrips_update_state() {
        let change = PatchPreviewChange::Update {
            path: PathBuf::from("src/lib.rs"),
            move_path: Some(PathBuf::from("src/main.rs")),
            old_text: "fn old() {}\n".to_string(),
            new_text: "fn new() {}\n".to_string(),
        };

        let encoded = serde_json::to_string(&change).expect("serialize patch preview change");
        let decoded: PatchPreviewChange =
            serde_json::from_str(&encoded).expect("deserialize patch preview change");

        assert_eq!(decoded, change);
    }

    /// Verifies that MCP tool-call items preserve optional result metadata.
    #[test]
    fn mcp_tool_call_item_roundtrips_optional_fields() {
        let item = TurnItem::McpToolCall(
            McpToolCallItem::builder()
                .id("call-2".to_string())
                .server("filesystem".to_string())
                .tool("read".to_string())
                .arguments(serde_json::json!({ "path": "README.md" }))
                .status(McpToolCallStatus::Completed)
                .result(serde_json::json!({ "ok": true }))
                .duration_ms(42)
                .build(),
        );

        let encoded = serde_json::to_string(&item).expect("serialize mcp item");
        let decoded: TurnItem = serde_json::from_str(&encoded).expect("deserialize mcp item");

        assert_eq!(decoded, item);
    }
}
