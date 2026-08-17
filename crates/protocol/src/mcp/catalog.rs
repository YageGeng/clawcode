use serde::{Deserialize, Serialize};

use crate::{McpPromptRef, McpResourceRef, McpServerId, McpToolRef};

/// Capability gates negotiated before catalog discovery and invocation.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct McpServerCapabilities {
    /// Server declares Tool listing and invocation.
    #[builder(default)]
    pub tools: bool,
    /// Server declares Prompt listing and retrieval.
    #[builder(default)]
    pub prompts: bool,
    /// Server declares Resource listing and reading.
    #[builder(default)]
    pub resources: bool,
    /// Server declares argument Completion.
    #[builder(default)]
    pub completions: bool,
    /// Server declares Resource subscription support.
    #[builder(default)]
    pub resource_subscribe: bool,
    /// Tool catalog may change during the Session.
    #[builder(default)]
    pub tools_list_changed: bool,
    /// Prompt catalog may change during the Session.
    #[builder(default)]
    pub prompts_list_changed: bool,
    /// Resource catalog may change during the Session.
    #[builder(default)]
    pub resources_list_changed: bool,
    /// Server declares the MCP Tasks extension.
    #[builder(default)]
    pub tasks: bool,
}

/// One argument accepted by an MCP Prompt or Resource Template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpArgumentInfo {
    /// Remote argument name.
    pub name: String,
    /// Optional server-provided description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the argument must be supplied.
    pub required: bool,
}

/// Public description of one namespaced MCP Tool.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct McpToolInfo {
    /// Structured routing reference retained independently from the public name.
    pub reference: McpToolRef,
    /// Stable model-facing namespaced name.
    pub public_name: String,
    /// Optional server-provided description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub description: Option<String>,
    /// JSON Schema accepted by the remote Tool.
    pub input_schema: serde_json::Value,
}

/// Public description of one remotely discovered MCP Prompt.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct McpPromptInfo {
    /// Structured routing reference.
    pub reference: McpPromptRef,
    /// Optional display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub title: Option<String>,
    /// Optional server-provided description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub description: Option<String>,
    /// Ordered argument declarations.
    #[serde(default)]
    #[builder(default)]
    pub arguments: Vec<McpArgumentInfo>,
}

/// Public description of one remotely discovered MCP Resource.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceInfo {
    /// Structured routing reference preserving the original URI.
    pub reference: McpResourceRef,
    /// Server-provided Resource name.
    pub name: String,
    /// Optional display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub title: Option<String>,
    /// Optional server-provided description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub description: Option<String>,
    /// Optional declared MIME type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub mime_type: Option<String>,
    /// Optional declared byte size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub size: Option<u64>,
}

/// Public description of one remotely discovered MCP Resource Template.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceTemplateInfo {
    /// Server that owns the Template.
    pub server_id: McpServerId,
    /// Original URI template used for remote routing.
    pub uri_template: String,
    /// Server-provided Template name.
    pub name: String,
    /// Optional display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub title: Option<String>,
    /// Optional server-provided description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub description: Option<String>,
    /// Optional declared MIME type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub mime_type: Option<String>,
}

/// Complete immutable MCP capability catalog for one Server revision.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct McpCatalog {
    /// Fully paginated Tool catalog.
    #[serde(default)]
    #[builder(default)]
    pub tools: Vec<McpToolInfo>,
    /// Fully paginated Prompt catalog.
    #[serde(default)]
    #[builder(default)]
    pub prompts: Vec<McpPromptInfo>,
    /// Fully paginated Resource catalog.
    #[serde(default)]
    #[builder(default)]
    pub resources: Vec<McpResourceInfo>,
    /// Fully paginated Resource Template catalog.
    #[serde(default)]
    #[builder(default)]
    pub resource_templates: Vec<McpResourceTemplateInfo>,
}

impl McpCatalog {
    /// Counts each capability family in this exact immutable catalog.
    #[must_use]
    pub fn counts(&self) -> McpCapabilityCounts {
        McpCapabilityCounts {
            tools: self.tools.len(),
            prompts: self.prompts.len(),
            resources: self.resources.len(),
            resource_templates: self.resource_templates.len(),
        }
    }
}

/// Number of currently published entities in each MCP capability family.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub struct McpCapabilityCounts {
    /// Published Tool count.
    pub tools: usize,
    /// Published Prompt count.
    pub prompts: usize,
    /// Published Resource count.
    pub resources: usize,
    /// Published Resource Template count.
    pub resource_templates: usize,
}
