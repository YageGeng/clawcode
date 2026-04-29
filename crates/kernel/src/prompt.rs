use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
};

use chrono::Utc;

use crate::Result;
use skills::SkillMetadata;
use tools::ToolRouter;

const SYSTEM_FILE_NAME: &str = "SYSTEM.md";
const AGENTS_FILE_NAME: &str = "AGENTS.md";
const CLAUDE_FILE_NAME: &str = "CLAUDE.md";

/// Carries runtime prompt overrides into the system-prompt builder.
#[derive(Debug, Clone, Default)]
pub struct SystemPromptOverrides {
    pub custom_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub cwd: Option<PathBuf>,
    pub current_date: Option<String>,
}

/// Bundles rendered prompt sections so final assembly keeps a compact function signature.
struct RenderInputs {
    cwd: PathBuf,
    current_date: String,
    prompt_files: PromptFiles,
    context_files: Vec<ContextPromptFile>,
    tools_section: String,
    guidelines_section: String,
    docs_section: String,
    skills_section: Option<String>,
}

/// Builds the full system prompt following the documented prompt spec.
pub fn build_system_prompt(
    router: &ToolRouter,
    skills: &[SkillMetadata],
    overrides: &SystemPromptOverrides,
) -> Result<Option<String>> {
    let cwd = resolve_cwd(overrides.cwd.clone())?;
    let current_date = overrides
        .current_date
        .clone()
        .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());
    let prompt_files = PromptFiles::load(&cwd, overrides)?;
    let context_files = collect_context_files(&cwd)?;
    let project_root = find_project_root(&cwd);
    let docs_section = render_docs_section(&project_root)?;
    let tools_section = render_tools_section(router);
    let guidelines_section = render_guidelines_section(router);
    let skills_section = render_visible_skills_section(router, skills);

    Ok(Some(render_system_prompt(RenderInputs {
        cwd,
        current_date,
        prompt_files,
        context_files,
        tools_section,
        guidelines_section,
        docs_section,
        skills_section,
    })))
}

/// Stores every discovered prompt-file input before rendering.
struct PromptFiles {
    custom_prompt: Option<String>,
    append_prompts: Vec<String>,
}

impl PromptFiles {
    /// Loads file-backed and override-backed prompt inputs using the configured precedence rules.
    fn load(cwd: &Path, overrides: &SystemPromptOverrides) -> Result<Self> {
        let custom_prompt = match normalize_optional_text(overrides.custom_prompt.clone()) {
            Some(prompt) => Some(prompt),
            None => read_first_existing_file(find_upward_file(cwd, SYSTEM_FILE_NAME))?,
        };
        let mut append_prompts = Vec::new();
        if let Some(append_override) =
            normalize_optional_text(overrides.append_system_prompt.clone())
        {
            append_prompts.push(append_override);
        }

        Ok(Self {
            custom_prompt,
            append_prompts,
        })
    }
}

/// Represents one rendered project-context file with its absolute path and body.
struct ContextPromptFile {
    path: PathBuf,
    content: String,
}

/// Resolves the working directory used for file discovery and prompt rendering.
fn resolve_cwd(cwd: Option<PathBuf>) -> Result<PathBuf> {
    let cwd = match cwd {
        Some(cwd) => cwd,
        None => env::current_dir().map_err(|error| crate::Error::Runtime {
            message: format!("failed to read current working directory: {error}"),
            stage: "prompt-current-dir".to_string(),
            inflight_snapshot: None,
        })?,
    };

    cwd.canonicalize().map_err(|error| crate::Error::Runtime {
        message: format!(
            "failed to canonicalize prompt working directory `{}`: {error}",
            cwd.display()
        ),
        stage: "prompt-canonicalize-cwd".to_string(),
        inflight_snapshot: None,
    })
}

/// Finds the closest existing file while walking from cwd toward the filesystem root.
fn find_upward_file(start: &Path, file_name: &str) -> Vec<PathBuf> {
    let mut matches = Vec::new();
    for dir in start.ancestors() {
        let candidate = dir.join(file_name);
        if candidate.exists() {
            matches.push(candidate);
        }
    }
    matches
}

/// Reads the first existing file from a discovered list.
fn read_first_existing_file(paths: Vec<PathBuf>) -> Result<Option<String>> {
    match paths.into_iter().next() {
        Some(path) => read_existing_file(&path),
        None => Ok(None),
    }
}

/// Reads one optional file and normalizes empty content away.
fn read_existing_file(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(path).map_err(|error| crate::Error::Runtime {
        message: format!("failed to read prompt file `{}`: {error}", path.display()),
        stage: "prompt-read-file".to_string(),
        inflight_snapshot: None,
    })?;
    Ok(normalize_optional_text(Some(content)))
}

/// Collects AGENTS.md / CLAUDE.md files from cwd ancestors.
fn collect_context_files(cwd: &Path) -> Result<Vec<ContextPromptFile>> {
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for dir in cwd.ancestors() {
        for file_name in [AGENTS_FILE_NAME, CLAUDE_FILE_NAME] {
            let path = dir.join(file_name);
            if let Some(file) = read_context_file(&path, &mut seen)? {
                files.push(file);
            }
        }
    }
    Ok(files)
}

/// Reads one context file if it exists and has not already been included.
fn read_context_file(
    path: &Path,
    seen: &mut HashSet<PathBuf>,
) -> Result<Option<ContextPromptFile>> {
    if !path.exists() {
        return Ok(None);
    }

    let canonical = path.canonicalize().map_err(|error| crate::Error::Runtime {
        message: format!(
            "failed to canonicalize context file `{}`: {error}",
            path.display()
        ),
        stage: "prompt-canonicalize-context-file".to_string(),
        inflight_snapshot: None,
    })?;
    if !seen.insert(canonical.clone()) {
        return Ok(None);
    }

    let content = fs::read_to_string(&canonical).map_err(|error| crate::Error::Runtime {
        message: format!(
            "failed to read context file `{}`: {error}",
            canonical.display()
        ),
        stage: "prompt-read-context-file".to_string(),
        inflight_snapshot: None,
    })?;

    Ok(Some(ContextPromptFile {
        path: canonical,
        content,
    }))
}

/// Renders the final prompt body after every input has been discovered.
fn render_system_prompt(inputs: RenderInputs) -> String {
    let RenderInputs {
        cwd,
        current_date,
        prompt_files,
        context_files,
        tools_section,
        guidelines_section,
        docs_section,
        skills_section,
    } = inputs;
    let mut sections = Vec::new();
    if let Some(custom_prompt) = prompt_files.custom_prompt {
        sections.push(custom_prompt);
    } else {
        sections.push(default_role_section());
        sections.push(tools_section);
        sections.push(guidelines_section);
        sections.push(docs_section);
    }
    if !prompt_files.append_prompts.is_empty() {
        sections.push(prompt_files.append_prompts.join("\n\n"));
    }
    sections.push(render_project_context_section(context_files));
    if let Some(skills_section) = skills_section {
        sections.push(skills_section);
    }
    sections.push(format!("Current date: {current_date}"));
    sections.push(format!("Current working directory: {}", cwd.display()));
    sections.join("\n\n")
}

/// Renders the static default role declaration used when no custom system prompt exists.
fn default_role_section() -> String {
    [
        "You are an expert coding assistant operating inside pi, a coding agent harness.",
        "You help users by reading files, executing commands, editing code, and writing new files.",
    ]
    .join("\n")
}

/// Renders the model-visible tool list from prompt snippets.
fn render_tools_section(router: &ToolRouter) -> String {
    let mut lines = vec!["Available tools:".to_string()];
    let mut snippets = router
        .specs()
        .iter()
        .filter_map(|configured| {
            configured
                .spec
                .prompt_metadata
                .prompt_snippet
                .as_ref()
                .map(|snippet| format!("- {}: {}", configured.spec.name(), snippet))
        })
        .collect::<Vec<_>>();
    if snippets.is_empty() {
        snippets.push("- (none)".to_string());
    }
    lines.extend(snippets);
    lines.push(
        "In addition to the tools above, you may have access to other custom tools depending on the project."
            .to_string(),
    );
    lines.join("\n")
}

/// Renders the merged default prompt guidelines, preserving first-seen order.
fn render_guidelines_section(router: &ToolRouter) -> String {
    let mut guidelines = derive_guidelines_from_tools(router);
    for configured in router.specs() {
        for guideline in &configured.spec.prompt_metadata.prompt_guidelines {
            push_unique_line(&mut guidelines, guideline.clone());
        }
    }
    push_unique_line(&mut guidelines, "Be concise in your responses".to_string());
    push_unique_line(
        &mut guidelines,
        "Show file paths clearly when working with files".to_string(),
    );

    let mut lines = vec!["Guidelines:".to_string()];
    lines.extend(
        guidelines
            .into_iter()
            .map(|guideline| format!("- {guideline}")),
    );
    lines.join("\n")
}

/// Derives default guidelines from the currently visible tool set.
fn derive_guidelines_from_tools(router: &ToolRouter) -> Vec<String> {
    let tool_names = router
        .specs()
        .iter()
        .map(|configured| configured.spec.name().to_string())
        .collect::<HashSet<_>>();
    let mut guidelines = Vec::new();
    if tool_names.contains("exec_command") {
        push_unique_line(
            &mut guidelines,
            "When searching for text or files, prefer `rg` or `rg --files` when shell access is available.".to_string(),
        );
    }
    if tool_names.contains("fs/read_text_file")
        && (tool_names.contains("apply_patch") || tool_names.contains("fs/write_text_file"))
    {
        push_unique_line(
            &mut guidelines,
            "Read relevant files before editing them.".to_string(),
        );
    }
    if tool_names.contains("apply_patch") {
        push_unique_line(
            &mut guidelines,
            "Prefer `apply_patch` for focused edits to existing files.".to_string(),
        );
    }
    guidelines
}

/// Finds the best project root used for documentation pointers and project-relative discovery.
fn find_project_root(cwd: &Path) -> PathBuf {
    let mut selected = cwd.to_path_buf();
    for dir in cwd.ancestors() {
        if has_project_marker(dir) {
            selected = dir.to_path_buf();
        }
    }
    selected
}

/// Checks whether the directory looks like a project root worth using for doc pointers.
fn has_project_marker(dir: &Path) -> bool {
    [".git", "Cargo.toml", "package.json", "pyproject.toml"]
        .into_iter()
        .any(|marker| dir.join(marker).exists())
}

/// Renders README/docs/examples pointers rooted at the resolved project root.
fn render_docs_section(project_root: &Path) -> Result<String> {
    let mut entries = Vec::new();
    for readme_name in ["README.md", "README_CN.md"] {
        let path = project_root.join(readme_name);
        if path.exists() {
            entries.push(("README".to_string(), path));
        }
    }
    entries.extend(collect_doc_entries(project_root, "docs")?);
    entries.extend(collect_doc_entries(project_root, "examples")?);

    let mut lines = vec!["ClawCode documentation pointers:".to_string()];
    if entries.is_empty() {
        lines.push("- (none found)".to_string());
    } else {
        for (topic, path) in entries {
            lines.push(format!("- {topic}: {}", path.display()));
        }
    }
    Ok(lines.join("\n"))
}

/// Collects top-level documentation file pointers from one named directory.
fn collect_doc_entries(project_root: &Path, dir_name: &str) -> Result<Vec<(String, PathBuf)>> {
    let dir = project_root.join(dir_name);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|error| crate::Error::Runtime {
        message: format!(
            "failed to read documentation directory `{}`: {error}",
            dir.display()
        ),
        stage: "prompt-read-doc-directory".to_string(),
        inflight_snapshot: None,
    })? {
        let entry = entry.map_err(|error| crate::Error::Runtime {
            message: format!(
                "failed to enumerate documentation directory `{}`: {error}",
                dir.display()
            ),
            stage: "prompt-enumerate-doc-directory".to_string(),
            inflight_snapshot: None,
        })?;
        let path = entry.path();
        if path.is_file() {
            let topic = path
                .strip_prefix(project_root)
                .ok()
                .map(|relative| relative.display().to_string())
                .unwrap_or_else(|| path.display().to_string());
            entries.push((topic, path));
        }
    }
    entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    Ok(entries)
}

/// Renders the collected AGENTS.md / CLAUDE.md context section.
fn render_project_context_section(context_files: Vec<ContextPromptFile>) -> String {
    let mut lines = vec![
        "# Project Context".to_string(),
        "Project-specific instructions and guidelines:".to_string(),
    ];
    if context_files.is_empty() {
        lines.push("(none)".to_string());
        return lines.join("\n");
    }

    for file in context_files {
        lines.push(String::new());
        lines.push(format!("## {}", file.path.display()));
        lines.push(file.content);
    }
    lines.join("\n")
}

/// Renders the skills XML section only when a visible read tool exists.
fn render_visible_skills_section(router: &ToolRouter, skills: &[SkillMetadata]) -> Option<String> {
    router.find_spec("fs/read_text_file")?;
    skills::render_skills_section(skills)
}

/// Normalizes optional prompt text by trimming pure whitespace to `None`.
fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value.and_then(|text| {
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    })
}

/// Pushes one line only if the vector does not already contain it.
fn push_unique_line(lines: &mut Vec<String>, line: String) {
    if !lines.iter().any(|existing| existing == &line) {
        lines.push(line);
    }
}

#[cfg(test)]
mod tests {
    use super::{SystemPromptOverrides, build_system_prompt};
    use std::{fs, sync::Arc};

    use async_trait::async_trait;
    use tools::{
        Result as ToolResult, ToolInvocation, ToolOutput, handler::ToolHandler,
        registry::ToolRegistryBuilder,
    };

    /// Renders the default system prompt with tool metadata, project context, and skill XML.
    #[test]
    fn build_system_prompt_renders_default_path() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        fs::write(temp.path().join("AGENTS.md"), "agent instructions").expect("AGENTS.md");
        fs::create_dir_all(temp.path().join("docs")).expect("docs dir");
        fs::write(temp.path().join("docs/guide.md"), "guide").expect("guide");
        let mut builder = ToolRegistryBuilder::new();
        builder.push_handler_spec(Arc::new(PromptTestTool));
        let router = builder.build_router();
        let skills = vec![skills::SkillMetadata {
            name: "alpha".to_string(),
            description: "Alpha skill.".to_string(),
            path: temp.path().join(".agents/skills/alpha/SKILL.md"),
            disable_model_invocation: false,
        }];

        let prompt = build_system_prompt(
            &router,
            &skills,
            &SystemPromptOverrides {
                cwd: Some(temp.path().to_path_buf()),
                current_date: Some("2026-04-29".to_string()),
                ..SystemPromptOverrides::default()
            },
        )
        .expect("prompt should build")
        .expect("default prompt should exist");

        assert!(prompt.contains("You are an expert coding assistant operating inside pi"));
        assert!(prompt.contains("Available tools:"));
        assert!(prompt.contains("fs/read_text_file: Prompt test snippet."));
        assert!(prompt.contains("# Project Context"));
        assert!(prompt.contains("AGENTS.md"));
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("Current date: 2026-04-29"));
        assert!(prompt.contains(temp.path().to_string_lossy().as_ref()));
    }

    /// Renders the custom system prompt path while still appending project context and dynamic metadata.
    #[test]
    fn build_system_prompt_prefers_custom_prompt_path() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        fs::write(temp.path().join("AGENTS.md"), "agent instructions").expect("AGENTS.md");
        let router = ToolRegistryBuilder::new().build_router();

        let prompt = build_system_prompt(
            &router,
            &[],
            &SystemPromptOverrides {
                custom_prompt: Some("custom system".to_string()),
                append_system_prompt: Some("append prompt".to_string()),
                cwd: Some(temp.path().to_path_buf()),
                current_date: Some("2026-04-29".to_string()),
            },
        )
        .expect("prompt should build")
        .expect("custom prompt should exist");

        assert!(prompt.starts_with("custom system"));
        assert!(prompt.contains("append prompt"));
        assert!(!prompt.contains("Available tools:"));
        assert!(prompt.contains("# Project Context"));
    }

    /// Hidden skills stay available to runtime code but do not render into the model-visible prompt section.
    #[test]
    fn build_system_prompt_skips_hidden_skills() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let mut builder = ToolRegistryBuilder::new();
        builder.push_handler_spec(Arc::new(PromptTestTool));
        let router = builder.build_router();
        let skills = vec![skills::SkillMetadata {
            name: "hidden".to_string(),
            description: "Hidden skill.".to_string(),
            path: temp.path().join("hidden/SKILL.md"),
            disable_model_invocation: true,
        }];

        let prompt = build_system_prompt(
            &router,
            &skills,
            &SystemPromptOverrides {
                custom_prompt: None,
                append_system_prompt: None,
                cwd: Some(temp.path().to_path_buf()),
                current_date: Some("2026-04-29".to_string()),
            },
        )
        .expect("prompt should build")
        .expect("prompt should exist");

        assert!(!prompt.contains("<available_skills>"));
        assert!(!prompt.contains("hidden"));
    }

    /// Legacy append and global prompt directories no longer participate in system prompt assembly.
    #[test]
    fn build_system_prompt_ignores_legacy_prompt_directories() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let home_dir = temp.path().join("home");
        let global_dir = home_dir.join(".pi/agent");
        let legacy_append = temp.path().join(".pi/APPEND_SYSTEM.md");

        fs::create_dir_all(legacy_append.parent().expect("legacy dir")).expect("legacy dir");
        fs::create_dir_all(&global_dir).expect("global dir");
        fs::write(temp.path().join("AGENTS.md"), "project instructions").expect("AGENTS.md");
        fs::write(&legacy_append, "legacy append prompt").expect("legacy append");
        fs::write(global_dir.join("APPEND_SYSTEM.md"), "global append prompt")
            .expect("global append");
        fs::write(global_dir.join("AGENTS.md"), "global agent instructions")
            .expect("global agents");

        let router = ToolRegistryBuilder::new().build_router();
        let prompt = build_system_prompt(
            &router,
            &[],
            &SystemPromptOverrides {
                cwd: Some(temp.path().to_path_buf()),
                current_date: Some("2026-04-29".to_string()),
                ..SystemPromptOverrides::default()
            },
        )
        .expect("prompt should build")
        .expect("prompt should exist");

        assert!(prompt.contains("project instructions"));
        assert!(!prompt.contains("legacy append prompt"));
        assert!(!prompt.contains("global append prompt"));
        assert!(!prompt.contains("global agent instructions"));
    }

    /// Tiny test tool that contributes prompt metadata and a visible read-tool name.
    struct PromptTestTool;

    #[async_trait]
    impl ToolHandler for PromptTestTool {
        fn name(&self) -> &'static str {
            "fs/read_text_file"
        }

        fn description(&self) -> &'static str {
            "Prompt test tool."
        }

        fn prompt_snippet(&self) -> Option<String> {
            Some("Prompt test snippet.".to_string())
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {},
            })
        }

        async fn handle(&self, _invocation: ToolInvocation) -> ToolResult<ToolOutput> {
            Ok(ToolOutput::text("ok"))
        }
    }
}
