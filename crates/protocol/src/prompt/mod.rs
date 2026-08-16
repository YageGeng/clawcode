//! Shared Prompt resource, rendering, and command contracts.

mod command;
mod policy;
mod source;
mod system;
mod template;

pub use command::{AvailableAgentCommand, AvailableAgentCommandKind};
pub use policy::{
    PromptContentSource, PromptPolicy, PromptResourceRequest,
    SkillResourceRequest,
};
pub use source::{
    ProjectInstruction, PromptCollision, PromptDiagnostic,
    PromptDiagnosticSeverity, PromptSourceInfo, PromptSourceKind,
    PromptSourceScope,
};
pub use system::{
    SystemPromptBuildOptions, SystemPromptTool, ToolPromptContribution,
};
pub use template::PromptTemplateInfo;
