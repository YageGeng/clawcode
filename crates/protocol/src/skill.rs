use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Origin mechanism used to discover one Skill resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSourceKind {
    /// Explicit path supplied by immutable application configuration.
    Configured,
    /// Pi-compatible `.pi/skills` or user configuration directory.
    Pi,
    /// Agent Skills compatible `.agents/skills` directory.
    Agents,
    /// Path contributed by a registered Extension.
    Extension,
}

/// Ownership and trust scope associated with one Skill source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSourceScope {
    /// Explicit configuration whose file provenance is intentionally opaque.
    Configured,
    /// Project-owned resource subject to the project trust decision.
    Project,
    /// User-owned resource available independently of project trust.
    User,
    /// Resource supplied by an Extension after its own trust handling.
    Extension,
}

/// Directory traversal semantics used for one Skill source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillDiscoveryMode {
    /// Accept root Markdown files and nested `SKILL.md` resources.
    Pi,
    /// Accept only nested `SKILL.md` resources.
    Agents,
}

/// Complete provenance and traversal policy for one Skill source.
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
pub struct SkillSource {
    /// Mechanism that introduced the source.
    pub kind: SkillSourceKind,
    /// Ownership and trust scope of the source.
    pub scope: SkillSourceScope,
    /// File or directory supplied to Skill discovery.
    pub root: PathBuf,
    /// Directory used to resolve relative source configuration.
    pub origin_base_dir: PathBuf,
    /// Traversal behavior applied below the source root.
    pub discovery_mode: SkillDiscoveryMode,
}

/// Read-only metadata for one effective discovered Skill.
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
pub struct SkillInfo {
    /// Conflict-resolved invocation name.
    pub name: String,
    /// Human-readable purpose from the Skill frontmatter.
    pub description: String,
    /// Source Markdown path used for invocation and display.
    pub path: PathBuf,
    /// Directory used to resolve references inside the Skill body.
    pub reference_dir: PathBuf,
    /// Provenance of the winning resource.
    pub source: SkillSource,
    /// Whether the Skill is available only through explicit user invocation.
    #[serde(default)]
    #[builder(default)]
    pub disable_model_invocation: bool,
}

/// Ordered enable or disable rule targeting one Skill by path or name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SkillSelectionRule {
    /// Optional exact Skill Markdown path selector.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// Optional declared Skill name selector.
    #[serde(default)]
    pub name: Option<String>,
    /// Whether matching Skills remain in the effective catalog.
    pub enabled: bool,
}

/// Severity assigned to one recoverable Skill resource diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillDiagnosticSeverity {
    /// Resource loading continued with a degraded or skipped input.
    Warning,
    /// A requested resource could not satisfy its explicit contract.
    Error,
}

/// Stable machine-readable cause for one Skill diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillDiagnosticCode {
    /// An explicit Skill path does not exist.
    PathNotFound,
    /// A Skill path is neither a directory nor a Markdown file.
    UnsupportedPath,
    /// File metadata could not be read.
    FileInfoFailed,
    /// A Skill directory could not be traversed.
    DirectoryReadFailed,
    /// A candidate Skill file could not be read.
    FileReadFailed,
    /// YAML frontmatter could not be parsed.
    FrontmatterInvalid,
    /// Parsed Skill metadata violated a semantic constraint.
    MetadataInvalid,
    /// A later Skill reused an already selected invocation name.
    NameCollision,
    /// A selection rule did not specify exactly one selector.
    SelectionRuleInvalid,
}

/// Winner and loser paths retained for one Skill name collision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCollision {
    /// Invocation name shared by both resources.
    pub name: String,
    /// Higher-priority path retained in the effective catalog.
    pub winner_path: PathBuf,
    /// Lower-priority path ignored by the catalog.
    pub loser_path: PathBuf,
}

/// Structured, serializable diagnostic produced while loading Skills.
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
pub struct SkillDiagnostic {
    /// Human-facing severity.
    pub severity: SkillDiagnosticSeverity,
    /// Stable programmatic cause.
    pub code: SkillDiagnosticCode,
    /// Concise human-readable explanation.
    pub message: String,
    /// Primary path associated with the diagnostic when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub path: Option<PathBuf>,
    /// Source provenance when discovery reached a concrete source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub source: Option<SkillSource>,
    /// Collision details for `NameCollision` diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub collision: Option<SkillCollision>,
}

/// Complete Session-scoped Skill snapshot returned by Kernel and ACP.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillListResult {
    /// Effective Skills after precedence, collision, and rule handling.
    #[serde(default)]
    pub skills: Vec<SkillInfo>,
    /// Recoverable discovery and validation diagnostics.
    #[serde(default)]
    pub diagnostics: Vec<SkillDiagnostic>,
}
