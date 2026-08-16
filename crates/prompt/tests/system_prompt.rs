//! Integration tests for dynamic pi-compatible System Prompt construction.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use prompt::{FilesystemPromptFactory, PromptFactory, SystemPromptTurnInput};
use protocol::{
    ProductIdentity, PromptContentSource, PromptPolicy, PromptResourceRequest,
    SkillDiscoveryMode, SkillInfo, SkillSource, SkillSourceKind,
    SkillSourceScope, SystemPromptTool, ToolPromptContribution,
};

/// Creates one Session snapshot with optional configured System and Append text.
fn prompt_session(
    global_root: &Path,
    cwd: &Path,
    custom_prompt: Option<&str>,
    append_system_prompt: Option<&str>,
) -> prompt::PromptSession {
    fs::create_dir_all(global_root).expect("create global root");
    fs::create_dir_all(cwd).expect("create cwd");
    let policy = PromptPolicy::builder()
        .system_prompt(
            custom_prompt
                .map(|content| PromptContentSource::Text(content.to_string())),
        )
        .append_system_prompts(
            append_system_prompt
                .map(|content| {
                    vec![PromptContentSource::Text(content.to_string())]
                })
                .unwrap_or_default(),
        )
        .build();

    FilesystemPromptFactory::new(global_root.to_path_buf(), policy)
        .create(PromptResourceRequest {
            cwd: cwd.to_path_buf(),
            project_resources_allowed: true,
            extension_prompt_paths: Vec::new(),
        })
        .expect("create Prompt session")
}

/// Creates one named tool contribution for System Prompt tests.
fn prompt_tool(
    name: &str,
    snippet: Option<&str>,
    guidelines: &[&str],
) -> SystemPromptTool {
    SystemPromptTool {
        name: name.to_string(),
        contribution: ToolPromptContribution {
            snippet: snippet.map(str::to_string),
            guidelines: guidelines
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
        },
    }
}

/// Creates complete configured Skill metadata for System Prompt assertions.
fn skill_info(name: &str, description: &str, path: PathBuf) -> SkillInfo {
    let reference_dir = path
        .parent()
        .expect("Skill reference directory")
        .to_path_buf();
    SkillInfo::builder()
        .name(name.to_string())
        .description(description.to_string())
        .path(path.clone())
        .reference_dir(reference_dir.clone())
        .source(
            SkillSource::builder()
                .kind(SkillSourceKind::Configured)
                .scope(SkillSourceScope::Configured)
                .root(path)
                .origin_base_dir(reference_dir)
                .discovery_mode(SkillDiscoveryMode::Pi)
                .build(),
        )
        .build()
}

/// Default prompts use shared identity and expose only normalized non-empty snippets.
#[test]
fn default_prompt_uses_identity_and_visible_tool_contributions() {
    let workspace = tempfile::tempdir().expect("workspace");
    let session = prompt_session(
        &workspace.path().join("global"),
        &workspace.path().join("project"),
        None,
        None,
    );
    let built = session.build_system_prompt(SystemPromptTurnInput {
        tools: vec![
            prompt_tool("read", Some("  Inspect\nthe   service  "), &[]),
            prompt_tool("custom", None, &[]),
            prompt_tool("blank", Some("  \n  "), &[]),
        ],
        skills: Vec::new(),
        include_skill_instructions: true,
    });

    assert!(built.text.contains(ProductIdentity::NAME));
    assert!(built.text.contains("- read: Inspect the service"));
    assert!(!built.text.contains("- custom:"));
    assert!(!built.text.contains("- blank:"));
    assert!(built.text.contains(
        "In addition to the tools above, you may have access to other custom tools depending on the project."
    ));
    assert_eq!(
        built.options.selected_tools,
        vec!["read", "custom", "blank"]
    );
    assert_eq!(
        built.options.tool_contributions,
        BTreeMap::from([
            (
                "blank".to_string(),
                ToolPromptContribution {
                    snippet: Some("  \n  ".to_string()),
                    guidelines: Vec::new(),
                }
            ),
            ("custom".to_string(), ToolPromptContribution::default()),
            (
                "read".to_string(),
                ToolPromptContribution {
                    snippet: Some("  Inspect\nthe   service  ".to_string()),
                    guidelines: Vec::new(),
                }
            ),
        ])
    );

    let empty = session.build_system_prompt(SystemPromptTurnInput {
        tools: Vec::new(),
        skills: Vec::new(),
        include_skill_instructions: true,
    });
    assert!(empty.text.contains("Available tools:\n(none)"));
}

/// Guidelines are trimmed, stably deduplicated, and adapt to bash-only file access.
#[test]
fn default_prompt_normalizes_guidelines_and_bash_fallback() {
    let workspace = tempfile::tempdir().expect("workspace");
    let session = prompt_session(
        &workspace.path().join("global"),
        &workspace.path().join("project"),
        None,
        None,
    );
    let bash_only = session.build_system_prompt(SystemPromptTurnInput {
        tools: vec![
            prompt_tool(
                "bash",
                Some("Execute commands"),
                &["  Keep output focused  ", "Keep output focused", "   "],
            ),
            prompt_tool("read", Some("Read files"), &["Keep output focused"]),
        ],
        skills: Vec::new(),
        include_skill_instructions: true,
    });

    assert!(
        bash_only
            .text
            .contains("- Use bash for file operations like ls, rg, find")
    );
    assert_eq!(bash_only.text.matches("- Keep output focused").count(), 1);
    assert_eq!(
        bash_only
            .text
            .matches("- Be concise in your responses")
            .count(),
        1
    );

    let with_grep = session.build_system_prompt(SystemPromptTurnInput {
        tools: vec![
            prompt_tool("bash", Some("Execute commands"), &[]),
            prompt_tool("grep", Some("Search text"), &[]),
        ],
        skills: Vec::new(),
        include_skill_instructions: true,
    });
    assert!(
        !with_grep
            .text
            .contains("Use bash for file operations like ls, rg, find")
    );
}

/// Custom prompts still append context, Skills, and cwd in their fixed order.
#[test]
fn custom_prompt_preserves_append_context_skill_and_cwd_order() {
    let workspace = tempfile::tempdir().expect("workspace");
    let global = workspace.path().join("global&\"scope");
    let cwd = workspace.path().join("project");
    fs::create_dir_all(&global).expect("create global root");
    fs::write(global.join("AGENTS.md"), "Keep <raw> & unchanged.")
        .expect("write global instructions");
    let session =
        prompt_session(&global, &cwd, Some("custom-body"), Some("append-body"));
    let skill_path = workspace.path().join("skills/rust/SKILL.md");
    let built = session.build_system_prompt(SystemPromptTurnInput {
        tools: vec![prompt_tool("read", Some("Read files"), &[])],
        skills: vec![skill_info(
            "rust&review",
            "Review <Rust> safely",
            skill_path.clone(),
        )],
        include_skill_instructions: true,
    });

    let append = built.text.find("append-body").expect("append section");
    let context = built
        .text
        .find("<project_context>")
        .expect("context section");
    let skills = built
        .text
        .find("<available_skills>")
        .expect("skills section");
    let current = built
        .text
        .find("Current working directory:")
        .expect("cwd section");
    assert!(append < context && context < skills && skills < current);
    assert!(built.text.contains("Keep <raw> & unchanged."));
    assert!(built.text.contains("global&amp;&quot;scope"));
    assert!(built.text.contains("<name>rust&amp;review</name>"));
    assert!(
        built
            .text
            .contains("<description>Review &lt;Rust&gt; safely</description>")
    );
    assert!(
        built.text.contains(&format!(
            "<location>{}</location>",
            skill_path.display()
        ))
    );
}

/// Skill instructions require both policy permission and the read tool.
#[test]
fn skill_catalog_requires_read_and_enabled_policy() {
    let workspace = tempfile::tempdir().expect("workspace");
    let session = prompt_session(
        &workspace.path().join("global"),
        &workspace.path().join("project"),
        None,
        None,
    );
    let skill = skill_info(
        "review",
        "Review code",
        PathBuf::from("/skills/review/SKILL.md"),
    );
    let without_read = session.build_system_prompt(SystemPromptTurnInput {
        tools: vec![prompt_tool("bash", Some("Execute commands"), &[])],
        skills: vec![skill.clone()],
        include_skill_instructions: true,
    });
    let disabled = session.build_system_prompt(SystemPromptTurnInput {
        tools: vec![prompt_tool("read", Some("Read files"), &[])],
        skills: vec![skill],
        include_skill_instructions: false,
    });

    assert!(!without_read.text.contains("<available_skills>"));
    assert!(!disabled.text.contains("<available_skills>"));
}
