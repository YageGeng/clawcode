//! Integration tests for pi-compatible Prompt resource discovery.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use prompt::{FilesystemPromptFactory, PromptFactory};
use protocol::{
    PromptContentSource, PromptPolicy, PromptResourceRequest, PromptSourceKind,
};

/// Writes one resource file after creating its parent directory.
fn write_resource(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("resource parent"))
        .expect("create resource directory");
    fs::write(path, content).expect("write resource");
}

/// Creates one Prompt session from the supplied factory and trust decision.
fn create_session(
    factory: &FilesystemPromptFactory,
    cwd: &Path,
    project_resources_allowed: bool,
    extension_prompt_paths: Vec<PathBuf>,
) -> prompt::PromptSession {
    factory
        .create(PromptResourceRequest {
            cwd: cwd.to_path_buf(),
            project_resources_allowed,
            extension_prompt_paths,
        })
        .expect("create Prompt session")
}

/// Runs a Git command and exposes environment failures as an explicit test failure.
fn run_git(cwd: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(cwd)
        .output()
        .expect("Git must be installed for linked worktree coverage");

    assert!(
        output.status.success(),
        "git {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Configured, project, global, and default System Prompt sources use pi priority.
#[test]
fn discovery_selects_system_and_append_sources_by_priority() {
    let workspace = tempfile::tempdir().expect("workspace");
    let global = workspace.path().join("global");
    let cwd = workspace.path().join("project");
    write_resource(&global.join("SYSTEM.md"), "global-system");
    write_resource(&global.join("APPEND_SYSTEM.md"), "global-append");
    write_resource(&cwd.join(".pi/SYSTEM.md"), "project-system");
    write_resource(&cwd.join(".pi/APPEND_SYSTEM.md"), "project-append");

    let configured_policy = PromptPolicy::builder()
        .system_prompt(Some(PromptContentSource::Text(
            "configured-system".to_string(),
        )))
        .append_system_prompts(vec![
            PromptContentSource::Text("first-append".to_string()),
            PromptContentSource::Text("second-append".to_string()),
        ])
        .build();
    let configured = create_session(
        &FilesystemPromptFactory::new(global.clone(), configured_policy),
        &cwd,
        true,
        Vec::new(),
    );
    assert_eq!(configured.custom_prompt(), Some("configured-system"));
    assert_eq!(
        configured.append_system_prompt(),
        Some("first-append\n\nsecond-append")
    );

    let discovered_factory =
        FilesystemPromptFactory::new(global.clone(), PromptPolicy::default());
    let project = create_session(&discovered_factory, &cwd, true, Vec::new());
    assert_eq!(project.custom_prompt(), Some("project-system"));
    assert_eq!(project.append_system_prompt(), Some("project-append"));

    let untrusted =
        create_session(&discovered_factory, &cwd, false, Vec::new());
    assert_eq!(untrusted.custom_prompt(), Some("global-system"));
    assert_eq!(untrusted.append_system_prompt(), Some("global-append"));

    let empty_global = workspace.path().join("empty-global");
    let empty_project = workspace.path().join("empty-project");
    fs::create_dir_all(&empty_global).expect("create empty global root");
    fs::create_dir_all(&empty_project).expect("create empty project root");
    let defaults = create_session(
        &FilesystemPromptFactory::new(empty_global, PromptPolicy::default()),
        &empty_project,
        false,
        Vec::new(),
    );
    assert_eq!(defaults.custom_prompt(), None);
    assert_eq!(defaults.append_system_prompt(), None);
}

/// Template roots preserve global, project, config, and Extension priority.
#[test]
fn discovery_orders_template_roots_and_honors_disabled_defaults() {
    let workspace = tempfile::tempdir().expect("workspace");
    let global = workspace.path().join("global");
    let cwd = workspace.path().join("project");
    let extension = workspace.path().join("extension");
    write_resource(&global.join("prompts/shared.md"), "global $1");
    write_resource(&global.join("prompts/user.md"), "user");
    write_resource(&cwd.join(".pi/prompts/shared.md"), "project $1");
    write_resource(&cwd.join(".pi/prompts/project.md"), "project");
    write_resource(&cwd.join("configured/shared.md"), "config $1");
    write_resource(&cwd.join("configured/config.md"), "config");
    write_resource(&extension.join("shared.md"), "extension $1");
    write_resource(&extension.join("extension.md"), "extension");

    let policy = PromptPolicy::builder()
        .template_paths(vec![PathBuf::from("configured")])
        .build();
    let factory = FilesystemPromptFactory::new(global.clone(), policy);
    let session = create_session(&factory, &cwd, true, vec![extension.clone()]);
    let names: Vec<_> = session
        .templates()
        .into_iter()
        .map(|template| template.name)
        .collect();

    assert_eq!(
        names,
        vec!["shared", "user", "project", "config", "extension"]
    );
    assert_eq!(session.expand_template("/shared file"), "global file");
    assert_eq!(
        session
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.collision.is_some())
            .count(),
        3
    );

    let explicit_only_policy = PromptPolicy::builder()
        .load_templates(false)
        .template_paths(vec![PathBuf::from("configured")])
        .build();
    let explicit_only = create_session(
        &FilesystemPromptFactory::new(global, explicit_only_policy),
        &cwd,
        true,
        vec![extension],
    );
    assert_eq!(explicit_only.expand_template("/user"), "/user");
    assert_eq!(explicit_only.expand_template("/project"), "/project");
    assert_eq!(explicit_only.expand_template("/config"), "config");
    assert_eq!(explicit_only.expand_template("/extension"), "extension");
}

/// Instructions load one candidate per directory from global then root to cwd.
#[test]
fn discovery_orders_instructions_and_respects_project_policy() {
    let workspace = tempfile::tempdir().expect("workspace");
    let global = workspace.path().join("global");
    let project = workspace.path().join("project");
    let child = project.join("child");
    let cwd = child.join("current");
    write_resource(&global.join("AGENTS.md"), "global");
    write_resource(&project.join("AGENTS.md"), "project");
    write_resource(&child.join("CLAUDE.md"), "child");
    write_resource(&cwd.join("AGENTS.override.md"), "override");
    write_resource(&cwd.join("AGENTS.md"), "lower-priority");

    let factory =
        FilesystemPromptFactory::new(global.clone(), PromptPolicy::default());
    let session = create_session(&factory, &cwd, true, Vec::new());
    let content: Vec<_> = session
        .instructions()
        .iter()
        .map(|instruction| instruction.content.as_str())
        .collect();
    assert_eq!(content, vec!["global", "project", "child", "override"]);

    let disabled_policy = PromptPolicy::builder()
        .load_project_instructions(false)
        .build();
    let disabled = create_session(
        &FilesystemPromptFactory::new(global.clone(), disabled_policy),
        &cwd,
        true,
        Vec::new(),
    );
    assert_eq!(disabled.instructions().len(), 1);
    assert_eq!(disabled.instructions()[0].content, "global");

    let untrusted = create_session(&factory, &cwd, false, Vec::new());
    assert_eq!(untrusted.instructions().len(), 1);
    assert_eq!(untrusted.instructions()[0].content, "global");
}

/// Configured paths resolve from the Session cwd and fail with their exact path.
#[test]
fn configured_content_paths_are_required() {
    let workspace = tempfile::tempdir().expect("workspace");
    let global = workspace.path().join("global");
    let cwd = workspace.path().join("project");
    write_resource(&cwd.join("custom/SYSTEM.md"), "configured-path");

    let valid_policy = PromptPolicy::builder()
        .system_prompt(Some(PromptContentSource::Path(PathBuf::from(
            "custom/SYSTEM.md",
        ))))
        .build();
    let valid = create_session(
        &FilesystemPromptFactory::new(global.clone(), valid_policy),
        &cwd,
        true,
        Vec::new(),
    );
    assert_eq!(valid.custom_prompt(), Some("configured-path"));

    let missing_policy = PromptPolicy::builder()
        .system_prompt(Some(PromptContentSource::Path(PathBuf::from(
            "missing.md",
        ))))
        .build();
    let error = FilesystemPromptFactory::new(global, missing_policy)
        .create(PromptResourceRequest {
            cwd: cwd.clone(),
            project_resources_allowed: true,
            extension_prompt_paths: Vec::new(),
        })
        .expect_err("missing configured path must fail")
        .to_string();

    assert!(error.contains(cwd.join("missing.md").to_string_lossy().as_ref()));
}

/// Broken automatic project files warn and fall back to readable global files.
#[test]
fn automatic_source_failures_warn_and_continue() {
    let workspace = tempfile::tempdir().expect("workspace");
    let global = workspace.path().join("global");
    let cwd = workspace.path().join("project");
    write_resource(&global.join("SYSTEM.md"), "global-system");
    fs::create_dir_all(cwd.join(".pi/SYSTEM.md"))
        .expect("create invalid project system path");

    let session = create_session(
        &FilesystemPromptFactory::new(global, PromptPolicy::default()),
        &cwd,
        true,
        Vec::new(),
    );

    assert_eq!(session.custom_prompt(), Some("global-system"));
    assert!(session.diagnostics().iter().any(|diagnostic| {
        diagnostic.source.kind == PromptSourceKind::System
            && diagnostic
                .source
                .path
                .as_ref()
                .is_some_and(|path| path.ends_with(".pi/SYSTEM.md"))
    }));
}

/// A nested linked worktree shadows the main worktree instruction at the same scope.
#[test]
fn linked_worktree_instructions_shadow_main_worktree_copy() {
    let workspace = tempfile::tempdir().expect("workspace");
    let main = workspace.path().join("main");
    fs::create_dir_all(&main).expect("create main worktree");
    run_git(&main, &["init"]);
    run_git(&main, &["config", "user.email", "prompt@example.invalid"]);
    run_git(&main, &["config", "user.name", "Prompt Test"]);
    write_resource(&main.join("AGENTS.md"), "main");
    write_resource(&main.join("tracked.txt"), "tracked");
    run_git(&main, &["add", "AGENTS.md", "tracked.txt"]);
    run_git(&main, &["commit", "-m", "initial"]);
    run_git(&main, &["worktree", "add", "-b", "prompt-nested", "nested"]);

    let linked = main.join("nested");
    let cwd = linked.join("src");
    write_resource(&linked.join("AGENTS.md"), "linked");
    fs::create_dir_all(&cwd).expect("create linked cwd");
    let session = create_session(
        &FilesystemPromptFactory::new(
            workspace.path().join("global"),
            PromptPolicy::default(),
        ),
        &cwd,
        true,
        Vec::new(),
    );
    let content: Vec<_> = session
        .instructions()
        .iter()
        .map(|instruction| instruction.content.as_str())
        .collect();

    assert_eq!(content, vec!["linked"]);
}
