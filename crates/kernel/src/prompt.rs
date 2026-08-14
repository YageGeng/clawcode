//! Pi-style system prompt construction and immutable project context loading.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use protocol::{
    AgentMessage, ContentBlock, MessageContent, MessageId, MessageIdentity,
    MessageTiming, ProductIdentity, SkillInfo, TimestampMs, ToolDefinition,
    TurnId,
};

/// Context filenames in the same per-directory precedence used by pi v4.
const PROJECT_CONTEXT_CANDIDATES: [&str; 5] = [
    "AGENTS.override.md",
    "AGENTS.md",
    "AGENTS.MD",
    "CLAUDE.md",
    "CLAUDE.MD",
];

/// One loaded project instruction document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectInstruction {
    /// Absolute path of the selected instruction file.
    pub path: PathBuf,
    /// Complete instruction file contents loaded once for the session.
    pub content: String,
}

/// Immutable ordered project instruction snapshot for one session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectContext {
    documents: Vec<ProjectInstruction>,
}

impl ProjectContext {
    /// Discovers global and ancestor project instructions using pi precedence.
    pub fn discover(
        cwd: &Path,
        global_root: &Path,
    ) -> Result<Self, PromptError> {
        Self::discover_with_global(cwd, Some(global_root))
    }

    /// Discovers ancestor project instructions without a global instruction directory.
    pub fn discover_ancestors(cwd: &Path) -> Result<Self, PromptError> {
        Self::discover_with_global(cwd, None)
    }

    /// Implements ordered discovery for optional global and ancestor scopes.
    fn discover_with_global(
        cwd: &Path,
        global_root: Option<&Path>,
    ) -> Result<Self, PromptError> {
        let cwd = std::fs::canonicalize(cwd).map_err(|source| {
            PromptError::ProjectContext {
                path: cwd.to_path_buf(),
                source,
            }
        })?;
        let global_root = global_root
            .filter(|global_root| global_root.exists())
            .map(std::fs::canonicalize)
            .transpose()
            .map_err(|source| PromptError::ProjectContext {
                path: global_root.unwrap_or(cwd.as_path()).to_path_buf(),
                source,
            })?;
        let mut directories = Vec::new();
        if let Some(global_root) = global_root {
            directories.push(global_root);
        }
        let mut ancestors =
            cwd.ancestors().map(Path::to_path_buf).collect::<Vec<_>>();
        ancestors.reverse();
        directories.extend(ancestors);

        let mut seen_directories = HashSet::new();
        let mut documents = Vec::new();
        for directory in directories {
            if !seen_directories.insert(directory.clone()) {
                continue;
            }
            for candidate in PROJECT_CONTEXT_CANDIDATES {
                let path = directory.join(candidate);
                if !path.is_file() {
                    continue;
                }
                let content =
                    std::fs::read_to_string(&path).map_err(|source| {
                        PromptError::ProjectContext {
                            path: path.clone(),
                            source,
                        }
                    })?;
                documents.push(ProjectInstruction { path, content });
                break;
            }
        }

        Ok(Self { documents })
    }

    /// Returns loaded instruction documents in effective application order.
    #[must_use]
    pub fn documents(&self) -> &[ProjectInstruction] {
        &self.documents
    }
}

/// Complete immutable input for one system prompt message.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct SystemPromptContext {
    /// Session working directory.
    pub cwd: PathBuf,
    /// Turn receiving the generated system message.
    pub turn_id: TurnId,
    /// Shared complete-message timestamp in Unix milliseconds.
    pub timestamp_ms: TimestampMs,
    /// Actual model-facing tools registered for this session.
    pub tools: Vec<ToolDefinition>,
    /// Effective discovered skill descriptors.
    pub skills: Vec<SkillInfo>,
    /// Project instructions loaded once for this session.
    pub project: ProjectContext,
}

/// Failures produced while loading context or building a system message.
#[derive(Debug, thiserror::Error)]
pub enum PromptError {
    /// An instruction path could not be resolved or read.
    #[error("project context I/O failed for {path}: {source}")]
    ProjectContext {
        /// Context path that failed.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// Generated protocol identity or timing data was invalid.
    #[error("system prompt protocol error: {0}")]
    Protocol(String),
}

/// Factory interface used by the kernel to create per-turn system messages.
pub trait SystemPromptFactory: Send + Sync {
    /// Loads one immutable project context snapshot for a new session.
    fn project_context(
        &self,
        cwd: &Path,
    ) -> Result<ProjectContext, PromptError>;

    /// Creates a complete system message from immutable session and turn data.
    fn create(
        &self,
        context: SystemPromptContext,
    ) -> Result<AgentMessage, PromptError>;
}

/// Pi-compatible system prompt implementation with an optional global context root.
#[derive(Debug, Clone, Default)]
pub struct PiSystemPromptFactory {
    global_root: Option<PathBuf>,
}

impl PiSystemPromptFactory {
    /// Creates a pi prompt factory with a product-level instruction directory.
    #[must_use]
    pub fn new(global_root: PathBuf) -> Self {
        Self {
            global_root: Some(global_root),
        }
    }

    /// Escapes text inserted into prompt XML sections.
    fn escape_xml(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }
}

impl SystemPromptFactory for PiSystemPromptFactory {
    /// Loads global and ancestor project instructions once for the session.
    fn project_context(
        &self,
        cwd: &Path,
    ) -> Result<ProjectContext, PromptError> {
        match &self.global_root {
            Some(global_root) => ProjectContext::discover(cwd, global_root),
            None => ProjectContext::discover_ancestors(cwd),
        }
    }

    /// Creates a concise coding-agent prompt from actual runtime capabilities.
    fn create(
        &self,
        context: SystemPromptContext,
    ) -> Result<AgentMessage, PromptError> {
        let mut prompt = format!(
            "You are an expert coding assistant operating inside {}, an agent harness.\n\nAvailable tools:\n",
            ProductIdentity::NAME
        );
        if context.tools.is_empty() {
            prompt.push_str("(none)\n");
        } else {
            for tool in &context.tools {
                prompt.push_str(&format!(
                    "- {}: {}\n",
                    tool.name, tool.description
                ));
            }
        }
        prompt.push_str(
            "\nGuidelines:\n- Be concise in your responses\n- Show file paths clearly when working with files\n",
        );

        if !context.project.documents.is_empty() {
            prompt.push_str(
                "\n<project_context>\nProject-specific instructions and guidelines:\n\n",
            );
            for document in &context.project.documents {
                prompt.push_str(&format!(
                    "<project_instructions path=\"{}\">\n{}\n</project_instructions>\n\n",
                    Self::escape_xml(document.path.to_string_lossy().as_ref()),
                    document.content
                ));
            }
            prompt.push_str("</project_context>\n");
        }

        if context.tools.iter().any(|tool| tool.name == "read")
            && !context.skills.is_empty()
        {
            prompt.push_str(
                "\nThe following skills provide specialized instructions for specific tasks.\nUse the read tool to load a skill's file when the task matches its description.\n\n<available_skills>\n",
            );
            for skill in &context.skills {
                prompt.push_str(&format!(
                    "  <skill>\n    <name>{}</name>\n    <description>{}</description>\n    <location>{}</location>\n  </skill>\n",
                    Self::escape_xml(&skill.name),
                    Self::escape_xml(&skill.description),
                    Self::escape_xml(skill.path.to_string_lossy().as_ref()),
                ));
            }
            prompt.push_str("</available_skills>\n");
        }

        prompt.push_str(&format!(
            "\nCurrent working directory: {}",
            context.cwd.to_string_lossy().replace('\\', "/")
        ));
        let timing = MessageTiming::try_from((
            context.timestamp_ms,
            context.timestamp_ms,
            context.timestamp_ms,
        ))
        .map_err(|error| PromptError::Protocol(error.to_string()))?;
        let message_id =
            MessageId::try_from(format!("system-{}", context.turn_id))
                .map_err(|error| PromptError::Protocol(error.to_string()))?;

        Ok(AgentMessage {
            identity: MessageIdentity {
                message_id,
                turn_id: context.turn_id,
            },
            timing,
            content: MessageContent::System {
                blocks: vec![ContentBlock::Text { text: prompt }],
            },
        })
    }
}
