//! Shared Prompt resource, rendering, and command contracts.

mod policy;
mod source;
mod system;
mod template;

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
