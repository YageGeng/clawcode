//! Integration tests for Session-scoped pi-compatible Skill discovery.

use std::fs;
use std::path::{Path, PathBuf};

use prompt::{FilesystemPromptFactory, PromptFactory, SystemPromptTurnInput};
use protocol::{
    PromptPolicy, PromptResourceRequest, SkillDiagnosticCode,
    SkillResourceRequest, SkillSelectionRule, SystemPromptTool,
    ToolPromptContribution,
};
use skill::{FilesystemSkillFactory, SkillCommandExpansion, SkillFactory};

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

/// Builds a production Skill Factory with an isolated unused home directory.
fn factory(
    global_root: PathBuf,
    rules: Vec<SkillSelectionRule>,
) -> FilesystemSkillFactory {
    let user_home = global_root.join("test-home");
    FilesystemSkillFactory::builder()
        .global_root(global_root)
        .user_home(user_home)
        .rules(rules)
        .build()
}

/// Recursive discovery reads metadata and explicit invocation returns the expanded body.
#[test]
fn discovers_and_invokes_nested_skills() {
    let workspace = tempfile::tempdir().expect("workspace");
    let global = workspace.path().join("global");
    let skill_file = global.join("skills/nested/review/SKILL.md");
    write_skill(
        &skill_file,
        "---\nname: \"code-review\"\ndescription: >-\n  Review Rust code with\n  ownership awareness\n---\n\n# Steps\nInspect carefully.\n",
    );
    let skill_file =
        fs::canonicalize(skill_file).expect("canonical nested Skill");

    let catalog = factory(global, Vec::new())
        .create(request(&workspace.path().join("project")))
        .expect("Skills should load");
    let descriptor = catalog
        .get("code-review")
        .expect("Skill should be discovered");
    let content = catalog
        .invoke("code-review")
        .expect("Skill should be invoked");
    let expected = format!(
        "<skill name=\"code-review\" location=\"{}\">\nReferences are relative to {}.\n\n# Steps\nInspect carefully.\n</skill>",
        skill_file.display(),
        skill_file.parent().expect("Skill directory").display()
    );

    assert_eq!(
        descriptor.description,
        "Review Rust code with ownership awareness"
    );
    assert_eq!(content, expected);
    assert!(!content.contains("description:"));
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

    let catalog = factory(global, Vec::new())
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

    let catalog = factory(global, Vec::new())
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
    let catalog = factory(global.clone(), Vec::new())
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

    let catalog = factory(global, Vec::new())
        .create(request(&cwd))
        .expect("invalid automatic Skill should not block Session resources");

    assert!(catalog.get("valid").is_some());
    assert_eq!(catalog.descriptors().len(), 1);
    assert!(catalog.diagnostics().iter().any(|diagnostic| {
        diagnostic.code == SkillDiagnosticCode::FrontmatterInvalid
            && diagnostic
                .path
                .as_deref()
                .is_some_and(|path| path.ends_with("invalid/SKILL.md"))
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

    let catalog = factory(global, Vec::new())
        .create(resource_request)
        .expect("Skills should load");

    assert_eq!(
        catalog.get("shared").expect("shared Skill").description,
        "Project version"
    );
    assert!(
        catalog
            .invoke("shared")
            .expect("invoke Skill")
            .contains("project")
    );
    assert_eq!(
        catalog
            .diagnostics()
            .iter()
            .filter(|diagnostic| {
                diagnostic.code == SkillDiagnosticCode::NameCollision
            })
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

    let catalog = factory(global, Vec::new())
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

    let catalog = factory(global, rules)
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
    let skill_file =
        fs::canonicalize(skill_file).expect("canonical command Skill");
    let catalog = factory(global, Vec::new())
        .create(request(&cwd))
        .expect("Skills should load");
    let expected = format!(
        "<skill name=\"rust-patterns\" location=\"{}\">\nReferences are relative to {}.\n\nUse ownership carefully.\n</skill>\n\nfix borrow",
        skill_file.display(),
        skill_file.parent().expect("Skill directory").display()
    );

    assert_eq!(
        catalog.expand_command("/skill:rust-patterns fix borrow"),
        SkillCommandExpansion::Expanded(expected)
    );
    assert_eq!(
        catalog.expand_command("/skill:rust-patterns\tfix"),
        SkillCommandExpansion::NotSkillCommand
    );
    assert_eq!(
        catalog.expand_command("/skill:rust-patterns\nfix"),
        SkillCommandExpansion::NotSkillCommand
    );
    assert_eq!(
        catalog.expand_command("/skill:missing argument"),
        SkillCommandExpansion::NotSkillCommand
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
    let catalog = factory(global, Vec::new())
        .create(request(&cwd))
        .expect("Skills should load");
    fs::remove_file(skill_file).expect("remove Skill after discovery");

    let expansion = catalog.expand_command("/skill:removed");
    let SkillCommandExpansion::Failed {
        original,
        diagnostic,
    } = expansion
    else {
        panic!("removed Skill should return a typed expansion failure");
    };
    assert_eq!(original, "/skill:removed");
    assert_eq!(diagnostic.code, SkillDiagnosticCode::FileReadFailed);
}
