//! Type conversions from clawcode internal types to ACP schema types.
//!
//! All conversions use the `From` trait with move semantics.
//! Since both `protocol` types and the ACP schema types are
//! foreign to the acp crate, these impls live here where the
//! protocol types are local (satisfying the orphan rule).

use acp::schema;
use agent_client_protocol as acp;

use crate::event::StopReason;
use crate::item::{FileChange, FileChangeStatus, TurnItem};
use crate::mcp::{McpServerConfig, McpTransportConfig};
use crate::permission::PermissionOptionKind;
use crate::plan::{PlanPriority, PlanStatus};
use crate::tool::ToolCallStatus;

// ── StopReason ──

impl From<StopReason> for schema::StopReason {
    fn from(r: StopReason) -> Self {
        match r {
            StopReason::EndTurn => Self::EndTurn,
            StopReason::Cancelled => Self::Cancelled,
            // ACP has no Error variant; map to Cancelled.
            StopReason::Error => Self::Cancelled,
        }
    }
}

// ── ToolCallStatus ──

impl From<ToolCallStatus> for schema::ToolCallStatus {
    fn from(s: ToolCallStatus) -> Self {
        match s {
            ToolCallStatus::Pending => Self::Pending,
            ToolCallStatus::InProgress => Self::InProgress,
            ToolCallStatus::Completed => Self::Completed,
            ToolCallStatus::Failed => Self::Failed,
        }
    }
}

// ── PlanPriority ──

impl From<PlanPriority> for schema::PlanEntryPriority {
    fn from(p: PlanPriority) -> Self {
        match p {
            PlanPriority::Low => Self::Low,
            PlanPriority::Medium => Self::Medium,
            PlanPriority::High => Self::High,
        }
    }
}

// ── PlanStatus ──

impl From<PlanStatus> for schema::PlanEntryStatus {
    fn from(s: PlanStatus) -> Self {
        match s {
            PlanStatus::Pending => Self::Pending,
            PlanStatus::InProgress => Self::InProgress,
            PlanStatus::Completed => Self::Completed,
        }
    }
}

// ── PermissionOptionKind ──

impl From<PermissionOptionKind> for schema::PermissionOptionKind {
    fn from(k: PermissionOptionKind) -> Self {
        match k {
            PermissionOptionKind::AllowOnce => Self::AllowOnce,
            PermissionOptionKind::AllowAlways => Self::AllowAlways,
            PermissionOptionKind::RejectOnce => Self::RejectOnce,
            PermissionOptionKind::RejectAlways => Self::RejectAlways,
        }
    }
}

/// Error returned when an ACP MCP server cannot map to a runtime MCP config.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AcpMcpServerConfigError {
    /// ACP SSE MCP transport is not supported by the runtime MCP client yet.
    #[error("ACP SSE MCP server '{server}' is not supported")]
    UnsupportedSse { server: String },
    /// Future ACP MCP transports require explicit runtime mapping before use.
    #[error("ACP MCP server transport is not supported")]
    UnsupportedTransport,
    /// Runtime MCP stdio command must be representable as UTF-8.
    #[error("ACP stdio MCP server '{server}' command is not valid UTF-8")]
    NonUtf8Command { server: String },
}

// ── McpServer ──

impl TryFrom<schema::McpServer> for McpServerConfig {
    type Error = AcpMcpServerConfigError;

    /// Convert an ACP MCP server config into a runtime MCP server config.
    fn try_from(server: schema::McpServer) -> Result<Self, Self::Error> {
        match server {
            schema::McpServer::Stdio(server) => server.try_into(),
            schema::McpServer::Http(server) => Ok(server.into()),
            schema::McpServer::Sse(server) => Err(AcpMcpServerConfigError::UnsupportedSse {
                server: server.name,
            }),
            _ => Err(AcpMcpServerConfigError::UnsupportedTransport),
        }
    }
}

impl TryFrom<schema::McpServerStdio> for McpServerConfig {
    type Error = AcpMcpServerConfigError;

    /// Convert an ACP stdio MCP server config into a runtime MCP server config.
    fn try_from(server: schema::McpServerStdio) -> Result<Self, Self::Error> {
        let command = server
            .command
            .into_os_string()
            .into_string()
            .map_err(|_e| AcpMcpServerConfigError::NonUtf8Command {
                server: server.name.clone(),
            })?;
        let env = server
            .env
            .into_iter()
            .map(|var| (var.name, var.value))
            .collect();

        Ok(McpServerConfig::builder()
            .name(server.name)
            .enabled(true)
            .external(true)
            .transport(McpTransportConfig::Stdio {
                command,
                args: server.args,
                env,
            })
            .build())
    }
}

impl From<schema::McpServerHttp> for McpServerConfig {
    /// Convert an ACP HTTP MCP server config into a runtime streamable HTTP config.
    fn from(server: schema::McpServerHttp) -> Self {
        let http_headers = server
            .headers
            .into_iter()
            .map(|header| (header.name, header.value))
            .collect();

        McpServerConfig::builder()
            .name(server.name)
            .enabled(true)
            .external(true)
            .transport(McpTransportConfig::StreamableHttp {
                url: server.url,
                bearer_token_env: None,
                http_headers,
            })
            .build()
    }
}

/// Converts structured turn-item lifecycle stages into ACP session updates.
pub trait TurnItemAcpExt {
    /// Convert an item-start event into an optional ACP session update.
    fn start(self) -> Option<schema::SessionUpdate>;

    /// Convert an item-completed event into an optional ACP session update.
    fn end(self) -> Option<schema::SessionUpdate>;
}

// ── FileChangeStatus ──

impl From<FileChangeStatus> for schema::ToolCallStatus {
    /// Convert file-change lifecycle status into ACP tool-call status.
    fn from(status: FileChangeStatus) -> Self {
        match status {
            FileChangeStatus::InProgress => Self::InProgress,
            FileChangeStatus::Completed => Self::Completed,
            FileChangeStatus::Failed | FileChangeStatus::Declined => Self::Failed,
        }
    }
}

// ── FileChange ──

impl From<FileChange> for schema::ToolCallContent {
    /// Convert one file-change final state into ACP diff content.
    fn from(change: FileChange) -> Self {
        // ACP renders file-change items through Diff content so clients can show
        // additions, updates, and deletions in a native edit tool cell.
        Self::Diff(schema::Diff::new(change.path, change.new_text).old_text(change.old_text))
    }
}

// ── TurnItem ──

impl TurnItemAcpExt for TurnItem {
    /// Convert an item-start event into an optional ACP session update.
    fn start(self) -> Option<schema::SessionUpdate> {
        match self {
            TurnItem::FileChange(item) => {
                let status = schema::ToolCallStatus::from(item.status);
                let fields = schema::ToolCallUpdateFields::new()
                    .kind(schema::ToolKind::Edit)
                    .status(status)
                    .title(item.title);
                Some(schema::SessionUpdate::ToolCallUpdate(
                    schema::ToolCallUpdate::new(schema::ToolCallId::new(item.id), fields),
                ))
            }
            TurnItem::McpToolCall(_) => None,
        }
    }

    /// Convert an item-completed event into an optional ACP session update.
    fn end(self) -> Option<schema::SessionUpdate> {
        match self {
            TurnItem::FileChange(item) => {
                let status = schema::ToolCallStatus::from(item.status);
                let mut fields = schema::ToolCallUpdateFields::new()
                    .kind(schema::ToolKind::Edit)
                    .status(status)
                    .title(item.title);
                if let Some(model_output) = item.model_output {
                    fields.raw_output = Some(serde_json::json!(model_output));
                }
                if !item.changes.is_empty() {
                    fields.content = Some(
                        item.changes
                            .into_iter()
                            .map(schema::ToolCallContent::from)
                            .collect(),
                    );
                }
                Some(schema::SessionUpdate::ToolCallUpdate(
                    schema::ToolCallUpdate::new(schema::ToolCallId::new(item.id), fields),
                ))
            }
            TurnItem::McpToolCall(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::item::{FileChange, FileChangeItem};
    use crate::mcp::McpTransportConfig;

    /// Verifies that file-change start events update the ACP tool cell as an edit.
    #[test]
    fn file_change_started_maps_to_acp_edit_update() {
        let item = TurnItem::FileChange(
            FileChangeItem::builder()
                .id("call-apply".to_string())
                .title("Apply patch".to_string())
                .changes(Vec::new())
                .status(FileChangeStatus::InProgress)
                .build(),
        );

        let update = item.start();
        let update = update.expect("file change should map to ACP");
        let schema::SessionUpdate::ToolCallUpdate(update) = update else {
            panic!("expected a tool call update");
        };

        assert_eq!(update.tool_call_id.to_string(), "call-apply");
        assert_eq!(update.fields.kind, Some(schema::ToolKind::Edit));
        assert_eq!(
            update.fields.status,
            Some(schema::ToolCallStatus::InProgress)
        );
        assert_eq!(update.fields.title, Some("Apply patch".to_string()));
        assert!(update.fields.content.is_none());
    }

    /// Verifies that file-change completion events map final states to ACP diffs.
    #[test]
    fn file_change_completed_maps_final_file_states_to_acp_diffs() {
        let item = TurnItem::FileChange(
            FileChangeItem::builder()
                .id("call-apply".to_string())
                .title("Apply patch".to_string())
                .changes(vec![
                    FileChange::builder()
                        .path(PathBuf::from("src/new.rs"))
                        .new_text("fn new() {}\n".to_string())
                        .build(),
                    FileChange::builder()
                        .path(PathBuf::from("src/existing.rs"))
                        .old_text("fn old() {}\n".to_string())
                        .new_text("fn new() {}\n".to_string())
                        .build(),
                ])
                .status(FileChangeStatus::Completed)
                .model_output("A src/new.rs\nM src/existing.rs".to_string())
                .build(),
        );

        let update = item.end();
        let update = update.expect("file change should map to ACP");
        let schema::SessionUpdate::ToolCallUpdate(update) = update else {
            panic!("expected a tool call update");
        };

        assert_eq!(update.tool_call_id.to_string(), "call-apply");
        assert_eq!(update.fields.kind, Some(schema::ToolKind::Edit));
        assert_eq!(
            update.fields.status,
            Some(schema::ToolCallStatus::Completed)
        );
        assert_eq!(update.fields.title, Some("Apply patch".to_string()));
        assert_eq!(
            update.fields.raw_output,
            Some(serde_json::json!("A src/new.rs\nM src/existing.rs"))
        );

        let content = update
            .fields
            .content
            .expect("diff content should be present");
        assert_eq!(content.len(), 2);

        let schema::ToolCallContent::Diff(added) = &content[0] else {
            panic!("expected added file to be represented as a diff");
        };
        assert_eq!(added.path, PathBuf::from("src/new.rs"));
        assert_eq!(added.old_text, None);
        assert_eq!(added.new_text, "fn new() {}\n");

        let schema::ToolCallContent::Diff(updated) = &content[1] else {
            panic!("expected updated file to be represented as a diff");
        };
        assert_eq!(updated.path, PathBuf::from("src/existing.rs"));
        assert_eq!(updated.old_text, Some("fn old() {}\n".to_string()));
        assert_eq!(updated.new_text, "fn new() {}\n");
    }

    /// Verifies that ACP stdio MCP config maps to the runtime stdio config.
    #[test]
    fn acp_stdio_mcp_server_maps_to_runtime_config() {
        let server = schema::McpServer::Stdio(
            schema::McpServerStdio::new("filesystem", "/usr/bin/mcp")
                .args(vec!["--root".to_string(), ".".to_string()])
                .env(vec![schema::EnvVariable::new("RUST_LOG", "debug")]),
        );

        let config = McpServerConfig::try_from(server).expect("stdio MCP config should convert");

        assert_eq!(config.name, "filesystem");
        assert!(config.enabled);
        assert!(config.external);
        let McpTransportConfig::Stdio { command, args, env } = config.transport else {
            panic!("expected stdio transport");
        };
        assert_eq!(command, "/usr/bin/mcp");
        assert_eq!(args, vec!["--root", "."]);
        assert_eq!(env.get("RUST_LOG"), Some(&"debug".to_string()));
    }

    /// Verifies that ACP HTTP MCP config maps to runtime streamable HTTP config.
    #[test]
    fn acp_http_mcp_server_maps_to_runtime_config() {
        let server = schema::McpServer::Http(
            schema::McpServerHttp::new("remote", "https://example.com/mcp").headers(vec![
                schema::HttpHeader::new("Authorization", "Bearer token"),
            ]),
        );

        let config = McpServerConfig::try_from(server).expect("HTTP MCP config should convert");

        assert_eq!(config.name, "remote");
        assert!(config.enabled);
        assert!(config.external);
        let McpTransportConfig::StreamableHttp {
            url,
            bearer_token_env,
            http_headers,
        } = config.transport
        else {
            panic!("expected streamable HTTP transport");
        };
        assert_eq!(url, "https://example.com/mcp");
        assert_eq!(bearer_token_env, None);
        assert_eq!(
            http_headers.get("Authorization"),
            Some(&"Bearer token".to_string())
        );
    }

    /// Verifies that ACP SSE MCP config is rejected until runtime support exists.
    #[test]
    fn acp_sse_mcp_server_is_rejected() {
        let server = schema::McpServer::Sse(schema::McpServerSse::new(
            "legacy-sse",
            "https://example.com/sse",
        ));

        let error = McpServerConfig::try_from(server).expect_err("SSE should be unsupported");

        assert_eq!(
            error,
            AcpMcpServerConfigError::UnsupportedSse {
                server: "legacy-sse".to_string()
            }
        );
    }
}
