use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Semantic role of one discovered Prompt resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptSourceKind {
    /// Complete System Prompt replacement.
    System,
    /// Content appended to the selected System Prompt.
    AppendSystem,
    /// Project or user instruction content.
    Instruction,
    /// Slash-command Prompt Template.
    Template,
}

/// Priority scope from which a Prompt resource originated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptSourceScope {
    /// User-global Prompt resource.
    User,
    /// Project-owned Prompt resource.
    Project,
    /// Explicit configuration resource.
    Config,
    /// Extension-contributed resource.
    Extension,
}

/// Stable source metadata attached to resources and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PromptSourceInfo {
    /// Semantic role of the source.
    pub kind: PromptSourceKind,
    /// Discovery scope that established source priority.
    pub scope: PromptSourceScope,
    /// Filesystem path when the source is file-backed.
    pub path: Option<PathBuf>,
}

/// Instruction file loaded into one session Prompt snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ProjectInstruction {
    /// Canonical instruction file path used for display and deduplication.
    pub path: PathBuf,
    /// Complete instruction body preserved without semantic rewriting.
    pub content: String,
    /// Discovery metadata for the instruction.
    pub source: PromptSourceInfo,
}

/// Severity category for non-fatal Prompt discovery diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptDiagnosticSeverity {
    /// A resource could not be loaded but discovery continued.
    Warning,
    /// A lower-priority named resource lost to an earlier source.
    Collision,
}

/// Structured duplicate-name details for deterministic resource resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PromptCollision {
    /// Conflicting public resource name.
    pub name: String,
    /// Earlier source retained by priority order.
    pub winner: PromptSourceInfo,
    /// Later source excluded from the effective catalog.
    pub loser: PromptSourceInfo,
}

/// Non-fatal issue encountered while discovering Prompt resources.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "snake_case")]
pub struct PromptDiagnostic {
    /// Diagnostic category used by clients for presentation.
    pub severity: PromptDiagnosticSeverity,
    /// Human-readable summary that excludes Prompt body content.
    pub message: String,
    /// Source associated with the diagnostic.
    pub source: PromptSourceInfo,
    /// Collision metadata when the diagnostic reports a duplicate name.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default, setter(strip_option))]
    pub collision: Option<PromptCollision>,
}
