//! Integration tests for Session-scoped pi-compatible Skill discovery.

use std::fs;
use std::path::{Path, PathBuf};

use prompt::{FilesystemPromptFactory, PromptFactory, SystemPromptTurnInput};
use protocol::{
    PromptPolicy, PromptResourceRequest, SkillResourceRequest,
    SkillSelectionRule, SystemPromptTool, ToolPromptContribution,
};
use skill::{FilesystemSkillFactory, SkillError, SkillFactory};

/// Writes one SKILL.md document and creates its parent directory.
fn write_skill(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("Skill parent"))
        .expect("create Skill directory");
    fs::write(path, content).expect("write Skill");
}

/// Builds one Session resource request rooted at the supplied cwd.
fn request(cwd: &Path) -> SkillResourceRequest {
    fs::create_dir_all(cwd).expect("create Session cwd");
    SkillResourceRequest {
        cwd: cwd.to_path_buf(),
        project_resources_allowed: true,
        extension_skill_paths: Vec::new(),
    }
}

/// Recursive discovery reads YAML metadata and explicit invocation returns the file.
#[test]
fn discovers_and_invokes_nested_skills() {
    let workspace = tempfile::tempdir().expect("workspace");
    let global = workspace.path().join("global");
    let skill_file = global.join("skills/nested/review/SKILL.md");
    write_skill(
        &skill_file,
        "---\nname: \"code-review\"\ndescription: >-\n  Review Rust code with\n  ownership awareness\n---\n\n# Steps\nInspect carefully.\n",
    );

    let catalog = FilesystemSkillFactory::new(global, Vec::new())
        .create(request(&workspace.path().join("project")))
        .expect("Skills should load");
    let descriptor = catalog
        .get("code-review")
        .expect("Skill should be discovered");
    let content = catalog
        .invoke("code-review")
        .expect("Skill should be invoked");

    assert_eq!(
        descriptor.description,
        "Review Rust code with ownership awareness"
    );
    assert!(content.contains("# Steps"));
    assert!(content.contains("Inspect carefully."));
}

/// Pi-compatible roots load direct Markdown files as standalone Skills.
#[test]
fn discovers_direct_markdown_skills_at_each_root() {
    let workspace = tempfile::tempdir().expect("workspace");
    let global = workspace.path().join("global");
    let cwd = workspace.path().join("project");
    let skill_file = global.join("skills/review.md");
    write_skill(
        &skill_file,
        "---\nname: review\ndescription: Review direct Markdown\n---\nreview\n",
    );

    let catalog = FilesystemSkillFactory::new(global, Vec::new())
        .create(request(&cwd))
        .expect("direct Markdown Skill should load");

    assert_eq!(
        catalog.get("review").expect("review Skill").path,
        fs::canonicalize(skill_file).expect("canonical Skill path")
    );
}

/// Skill discovery honors pi's supported ignore files before descending.
#[test]
fn ignored_skill_directories_are_not_discovered() {
    let workspace = tempfile::tempdir().expect("workspace");
    let global = workspace.path().join("global");
    let cwd = workspace.path().join("project");
    let skills = global.join("skills");
    write_skill(
        &skills.join("visible/SKILL.md"),
        "---\nname: visible\ndescription: Visible Skill\n---\nvisible\n",
    );
    write_skill(
        &skills.join("git-ignored/SKILL.md"),
        "---\nname: git-ignored\ndescription: Git ignored Skill\n---\nignored\n",
    );
    write_skill(
        &skills.join("plain-ignored/SKILL.md"),
        "---\nname: plain-ignored\ndescription: Plain ignored Skill\n---\nignored\n",
    );
    write_skill(
        &skills.join("fd-ignored/SKILL.md"),
        "---\nname: fd-ignored\ndescription: Fd ignored Skill\n---\nignored\n",
    );
    fs::write(skills.join(".gitignore"), "git-ignored/\n")
        .expect("write Skill ignore file");
    fs::write(skills.join(".ignore"), "plain-ignored/\n")
        .expect("write plain Skill ignore file");
    fs::write(skills.join(".fdignore"), "fd-ignored/\n")
        .expect("write fd Skill ignore file");

    let catalog = FilesystemSkillFactory::new(global, Vec::new())
        .create(request(&cwd))
        .expect("Skill discovery");

    assert!(catalog.get("visible").is_some());
    assert!(catalog.get("git-ignored").is_none());
    assert!(catalog.get("plain-ignored").is_none());
    assert!(catalog.get("fd-ignored").is_none());
}

/// Manual-only Skills remain invokable without being advertised to the model.
#[test]
fn disabled_model_invocation_skills_stay_out_of_system_prompt() {
    let workspace = tempfile::tempdir().expect("workspace");
    let global = workspace.path().join("global");
    let cwd = workspace.path().join("project");
    write_skill(
        &global.join("skills/manual/SKILL.md"),
        "---\nname: manual\ndescription: Manual only\ndisable-model-invocation: true\n---\nmanual\n",
    );
    let catalog = FilesystemSkillFactory::new(global.clone(), Vec::new())
        .create(request(&cwd))
        .expect("Skill discovery");
    assert!(catalog.get("manual").is_some());
    let prompt = FilesystemPromptFactory::new(global, PromptPolicy::default())
        .create(PromptResourceRequest {
            cwd,
            project_resources_allowed: true,
            extension_prompt_paths: Vec::new(),
        })
        .expect("Prompt resources");

    let built = prompt.build_system_prompt(SystemPromptTurnInput {
        tools: vec![SystemPromptTool {
            name: "read".to_string(),
            contribution: ToolPromptContribution::default(),
        }],
        skills: catalog.descriptors(),
        include_skill_instructions: true,
    });

    assert!(!built.text.contains("<available_skills>"));
}

/// Invalid automatically discovered Skills warn without blocking valid siblings.
#[test]
fn invalid_skills_are_skipped_with_diagnostics() {
    let workspace = tempfile::tempdir().expect("workspace");
    let global = workspace.path().join("global");
    let cwd = workspace.path().join("project");
    write_skill(
        &global.join("skills/valid/SKILL.md"),
        "---\nname: valid\ndescription: Valid Skill\n---\nvalid\n",
    );
    write_skill(
        &global.join("skills/invalid/SKILL.md"),
        "---\nname: [\n---\ninvalid\n",
    );

    let catalog = FilesystemSkillFactory::new(global, Vec::new())
        .create(request(&cwd))
        .expect("invalid automatic Skill should not block Session resources");

    assert!(catalog.get("valid").is_some());
    assert_eq!(catalog.descriptors().len(), 1);
    assert!(catalog.diagnostics().iter().any(|diagnostic| {
        diagnostic.contains("invalid/SKILL.md")
            && diagnostic.contains("invalid Skill")
    }));
}

/// Earlier Skill roots win declared-name collisions and report later sources.
#[test]
fn earlier_session_skill_roots_win_name_collisions() {
    let workspace = tempfile::tempdir().expect("workspace");
    let global = workspace.path().join("global");
    let cwd = workspace.path().join("project");
    let extension = workspace.path().join("extension");
    write_skill(
        &global.join("skills/shared/SKILL.md"),
        "---\nname: shared\ndescription: User version\n---\nuser\n",
    );
    write_skill(
        &cwd.join(".pi/skills/shared/SKILL.md"),
        "---\nname: shared\ndescription: Project version\n---\nproject\n",
    );
    write_skill(
        &extension.join("shared/SKILL.md"),
        "---\nname: shared\ndescription: Extension version\n---\nextension\n",
    );
    let mut resource_request = request(&cwd);
    resource_request.extension_skill_paths = vec![extension];

    let catalog = FilesystemSkillFactory::new(global, Vec::new())
        .create(resource_request)
        .expect("Skills should load");

    assert_eq!(
        catalog.get("shared").expect("shared Skill").description,
        "User version"
    );
    assert!(
        catalog
            .invoke("shared")
            .expect("invoke Skill")
            .contains("user")
    );
    assert_eq!(
        catalog
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.contains("collision"))
            .count(),
        2
    );
}

/// Explicit project denial excludes both default project Skill directories.
#[test]
fn project_trust_denial_keeps_global_skills_only() {
    let workspace = tempfile::tempdir().expect("workspace");
    let global = workspace.path().join("global");
    let cwd = workspace.path().join("project");
    write_skill(
        &global.join("skills/global/SKILL.md"),
        "---\nname: global\ndescription: Global Skill\n---\nglobal\n",
    );
    write_skill(
        &cwd.join(".pi/skills/project/SKILL.md"),
        "---\nname: project\ndescription: Project Skill\n---\nproject\n",
    );
    write_skill(
        &cwd.join(".agents/skills/agent/SKILL.md"),
        "---\nname: agent\ndescription: Agent Skill\n---\nagent\n",
    );
    let mut resource_request = request(&cwd);
    resource_request.project_resources_allowed = false;

    let catalog = FilesystemSkillFactory::new(global, Vec::new())
        .create(resource_request)
        .expect("Skills should load");
    let names: Vec<_> = catalog
        .descriptors()
        .into_iter()
        .map(|skill| skill.name)
        .collect();

    assert_eq!(names, vec!["global"]);
}

/// Later valid name and path rules override earlier matches while invalid rules warn.
#[test]
fn skill_rules_apply_in_order_and_report_invalid_selectors() {
    let workspace = tempfile::tempdir().expect("workspace");
    let global = workspace.path().join("global");
    let cwd = workspace.path().join("project");
    let alpha = global.join("skills/alpha/SKILL.md");
    let beta = global.join("skills/beta/SKILL.md");
    write_skill(&alpha, "---\nname: alpha\ndescription: Alpha\n---\nalpha\n");
    write_skill(&beta, "---\nname: beta\ndescription: Beta\n---\nbeta\n");
    let rules = vec![
        SkillSelectionRule {
            path: None,
            name: Some("alpha".to_string()),
            enabled: false,
        },
        SkillSelectionRule {
            path: Some(alpha.clone()),
            name: None,
            enabled: true,
        },
        SkillSelectionRule {
            path: Some(beta),
            name: None,
            enabled: true,
        },
        SkillSelectionRule {
            path: None,
            name: Some("beta".to_string()),
            enabled: false,
        },
        SkillSelectionRule {
            path: Some(PathBuf::from("invalid")),
            name: Some("invalid".to_string()),
            enabled: false,
        },
        SkillSelectionRule {
            path: None,
            name: None,
            enabled: false,
        },
    ];

    let catalog = FilesystemSkillFactory::new(global, rules)
        .create(request(&cwd))
        .expect("Skills should load");
    let names: Vec<_> = catalog
        .descriptors()
        .into_iter()
        .map(|skill| skill.name)
        .collect();

    assert_eq!(names, vec!["alpha"]);
    assert_eq!(catalog.diagnostics().len(), 2);
}

/// Skill slash commands strip frontmatter and split only on a literal space.
#[test]
fn skill_commands_expand_with_pi_literal_space_semantics() {
    let workspace = tempfile::tempdir().expect("workspace");
    let global = workspace.path().join("global");
    let cwd = workspace.path().join("project");
    let skill_file = global.join("skills/rust-patterns/SKILL.md");
    write_skill(
        &skill_file,
        "---\nname: rust-patterns\ndescription: Rust patterns\n---\n\nUse ownership carefully.\n",
    );
    let catalog = FilesystemSkillFactory::new(global, Vec::new())
        .create(request(&cwd))
        .expect("Skills should load");
    let expected = format!(
        "<skill name=\"rust-patterns\" location=\"{}\">\nReferences are relative to {}.\n\nUse ownership carefully.\n</skill>\n\nfix borrow",
        skill_file.display(),
        skill_file.parent().expect("Skill directory").display()
    );

    assert_eq!(
        catalog
            .expand_command("/skill:rust-patterns fix borrow")
            .expect("expand Skill"),
        Some(expected)
    );
    assert_eq!(
        catalog
            .expand_command("/skill:rust-patterns\tfix")
            .expect("unknown tab command"),
        None
    );
    assert_eq!(
        catalog
            .expand_command("/skill:rust-patterns\nfix")
            .expect("unknown newline command"),
        None
    );
    assert_eq!(
        catalog
            .expand_command("/skill:missing argument")
            .expect("unknown Skill"),
        None
    );
}

/// Invocation read failures remain observable after the immutable catalog is built.
#[test]
fn skill_command_reports_unreadable_skill_files() {
    let workspace = tempfile::tempdir().expect("workspace");
    let global = workspace.path().join("global");
    let cwd = workspace.path().join("project");
    let skill_file = global.join("skills/removed/SKILL.md");
    write_skill(
        &skill_file,
        "---\nname: removed\ndescription: Removed Skill\n---\nbody\n",
    );
    let catalog = FilesystemSkillFactory::new(global, Vec::new())
        .create(request(&cwd))
        .expect("Skills should load");
    fs::remove_file(skill_file).expect("remove Skill after discovery");

    assert!(matches!(
        catalog.expand_command("/skill:removed"),
        Err(SkillError::Io(_))
    ));
}
