//! Integration tests for pi-compatible Skill source planning and precedence.

use std::fs;
use std::path::{Path, PathBuf};

use protocol::{
    SkillDiagnosticCode, SkillResourceRequest, SkillSourceKind,
    SkillSourceScope,
};
use skill::{FilesystemSkillFactory, SkillFactory};

/// Isolated filesystem roots used to exercise production Skill discovery.
struct SkillWorkspace {
    _root: tempfile::TempDir,
    global: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
}

impl SkillWorkspace {
    /// Creates isolated user, project, and global configuration roots.
    fn new() -> Self {
        let root = tempfile::tempdir().expect("Skill workspace");
        let global = root.path().join("config");
        let home = root.path().join("home");
        let cwd = root.path().join("repo/project");
        fs::create_dir_all(&cwd).expect("create project cwd");
        fs::create_dir_all(root.path().join("repo/.git"))
            .expect("create Git marker");
        Self {
            _root: root,
            global,
            home,
            cwd,
        }
    }

    /// Writes one valid Skill document at a path relative to the workspace root.
    fn write_skill(
        &self,
        path: impl AsRef<Path>,
        name: &str,
        description: &str,
    ) {
        let path = self._root.path().join(path);
        fs::create_dir_all(path.parent().expect("Skill parent"))
            .expect("create Skill parent");
        fs::write(
            path,
            format!(
                "---\nname: {name}\ndescription: {description}\n---\n{description}\n"
            ),
        )
        .expect("write Skill");
    }

    /// Builds the production Factory with isolated user roots and explicit paths.
    fn factory(
        &self,
        configured_paths: Vec<PathBuf>,
    ) -> FilesystemSkillFactory {
        FilesystemSkillFactory::builder()
            .global_root(self.global.clone())
            .user_home(self.home.clone())
            .configured_paths(configured_paths)
            .build()
    }

    /// Creates one trusted Session request with optional Extension sources.
    fn request(
        &self,
        extension_skill_paths: Vec<PathBuf>,
    ) -> SkillResourceRequest {
        SkillResourceRequest {
            cwd: self.cwd.clone(),
            project_resources_allowed: true,
            extension_skill_paths,
        }
    }
}

/// Every adjacent source pair follows the fixed pi-compatible winner order.
#[test]
fn source_precedence_prefers_configured_project_user_then_extension() {
    let workspace = SkillWorkspace::new();
    workspace.write_skill(
        "configured/explicit/SKILL.md",
        "configured-over-project",
        "configured winner",
    );
    workspace.write_skill(
        "repo/project/.pi/skills/configured-over-project/SKILL.md",
        "configured-over-project",
        "project loser",
    );
    workspace.write_skill(
        "repo/project/.pi/skills/pi-over-agents/SKILL.md",
        "pi-over-agents",
        "pi winner",
    );
    workspace.write_skill(
        "repo/project/.agents/skills/pi-over-agents/SKILL.md",
        "pi-over-agents",
        "agents loser",
    );
    workspace.write_skill(
        "repo/project/.agents/skills/current-over-parent/SKILL.md",
        "current-over-parent",
        "current winner",
    );
    workspace.write_skill(
        "repo/.agents/skills/current-over-parent/SKILL.md",
        "current-over-parent",
        "parent loser",
    );
    workspace.write_skill(
        "repo/.agents/skills/project-over-user/SKILL.md",
        "project-over-user",
        "project winner",
    );
    workspace.write_skill(
        "config/skills/project-over-user/SKILL.md",
        "project-over-user",
        "user loser",
    );
    workspace.write_skill(
        "config/skills/pi-user-over-agents-user/SKILL.md",
        "pi-user-over-agents-user",
        "pi user winner",
    );
    workspace.write_skill(
        "home/.agents/skills/pi-user-over-agents-user/SKILL.md",
        "pi-user-over-agents-user",
        "agents user loser",
    );
    workspace.write_skill(
        "home/.agents/skills/user-over-extension/SKILL.md",
        "user-over-extension",
        "user winner",
    );
    workspace.write_skill(
        "extension/user-over-extension/SKILL.md",
        "user-over-extension",
        "extension loser",
    );

    let explicit = workspace._root.path().join("configured/explicit/SKILL.md");
    let extension = workspace._root.path().join("extension");
    let catalog = workspace
        .factory(vec![explicit])
        .create(workspace.request(vec![extension]))
        .expect("discover ordered Skill sources");

    let expected = [
        ("configured-over-project", "configured winner"),
        ("pi-over-agents", "pi winner"),
        ("current-over-parent", "current winner"),
        ("project-over-user", "project winner"),
        ("pi-user-over-agents-user", "pi user winner"),
        ("user-over-extension", "user winner"),
    ];
    for (name, description) in expected {
        assert_eq!(
            catalog.get(name).expect("precedence winner").description,
            description
        );
    }
    let configured = catalog
        .get("configured-over-project")
        .expect("configured Skill");
    assert_eq!(configured.source.kind, SkillSourceKind::Configured);
    assert_eq!(configured.source.scope, SkillSourceScope::Configured);
}

/// Ancestor `.agents/skills` discovery stops at the nearest Git repository root.
#[test]
fn source_discovery_stops_at_git_root() {
    let workspace = SkillWorkspace::new();
    workspace.write_skill(
        "repo/.agents/skills/inside/SKILL.md",
        "inside",
        "inside repository",
    );
    workspace.write_skill(
        ".agents/skills/outside/SKILL.md",
        "outside",
        "outside repository",
    );

    let catalog = workspace
        .factory(Vec::new())
        .create(workspace.request(Vec::new()))
        .expect("discover ancestor Skills");

    assert!(catalog.get("inside").is_some());
    assert!(catalog.get("outside").is_none());
}

/// Project denial excludes project sources without suppressing user Skills.
#[test]
fn source_discovery_respects_project_trust() {
    let workspace = SkillWorkspace::new();
    workspace.write_skill(
        "repo/project/.pi/skills/project/SKILL.md",
        "project",
        "project Skill",
    );
    workspace.write_skill(
        "repo/.agents/skills/ancestor/SKILL.md",
        "ancestor",
        "ancestor Skill",
    );
    workspace.write_skill("config/skills/user/SKILL.md", "user", "user Skill");
    workspace.write_skill(
        "home/.agents/skills/portable/SKILL.md",
        "portable",
        "portable Skill",
    );
    let mut request = workspace.request(Vec::new());
    request.project_resources_allowed = false;

    let catalog = workspace
        .factory(Vec::new())
        .create(request)
        .expect("discover trusted user Skills");

    assert!(catalog.get("project").is_none());
    assert!(catalog.get("ancestor").is_none());
    assert!(catalog.get("user").is_some());
    assert!(catalog.get("portable").is_some());
}

/// Agents mode ignores root Markdown while retaining nested `SKILL.md` files.
#[test]
fn source_agents_mode_ignores_root_markdown() {
    let workspace = SkillWorkspace::new();
    workspace.write_skill(
        "repo/project/.agents/skills/root.md",
        "root-markdown",
        "must be ignored",
    );
    workspace.write_skill(
        "repo/project/.agents/skills/nested/SKILL.md",
        "nested",
        "must be loaded",
    );

    let catalog = workspace
        .factory(Vec::new())
        .create(workspace.request(Vec::new()))
        .expect("discover Agents mode Skills");

    assert!(catalog.get("root-markdown").is_none());
    assert!(catalog.get("nested").is_some());
}

/// A root `SKILL.md` turns the configured directory into one Skill source.
#[test]
fn source_root_skill_stops_further_directory_scanning() {
    let workspace = SkillWorkspace::new();
    workspace.write_skill("configured/root/SKILL.md", "root", "root Skill");
    workspace.write_skill(
        "configured/root/sibling.md",
        "sibling",
        "must not be loaded",
    );
    workspace.write_skill(
        "configured/root/nested/SKILL.md",
        "nested",
        "must not be loaded",
    );
    let configured_root = workspace._root.path().join("configured/root");

    let catalog = workspace
        .factory(vec![configured_root])
        .create(workspace.request(Vec::new()))
        .expect("discover root Skill only");

    assert_eq!(
        catalog
            .descriptors()
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        vec!["root"]
    );
}

/// Configured paths expand `~` and report missing or unsupported inputs without aborting the Session.
#[test]
fn source_configured_paths_expand_home_and_report_invalid_inputs() {
    let workspace = SkillWorkspace::new();
    workspace.write_skill(
        "home/custom/review/SKILL.md",
        "review",
        "home-expanded Skill",
    );
    let unsupported = workspace._root.path().join("unsupported.txt");
    fs::write(&unsupported, "not markdown").expect("write unsupported path");
    let catalog = workspace
        .factory(vec![
            PathBuf::from("~/custom/review/SKILL.md"),
            PathBuf::from("missing/SKILL.md"),
            unsupported,
        ])
        .create(workspace.request(Vec::new()))
        .expect("discover configured Skill paths");

    assert_eq!(
        catalog.get("review").expect("home Skill").description,
        "home-expanded Skill"
    );
    assert!(catalog.diagnostics().iter().any(|diagnostic| {
        diagnostic.code == SkillDiagnosticCode::PathNotFound
    }));
    assert!(catalog.diagnostics().iter().any(|diagnostic| {
        diagnostic.code == SkillDiagnosticCode::UnsupportedPath
    }));
}

/// The same canonical Skill file loaded through a symlink is silently de-duplicated.
#[cfg(unix)]
#[test]
fn source_symlink_alias_is_not_a_name_collision() {
    use std::os::unix::fs::symlink;

    let workspace = SkillWorkspace::new();
    workspace.write_skill("shared/review/SKILL.md", "review", "shared Skill");
    let original = workspace._root.path().join("shared/review/SKILL.md");
    let alias = workspace._root.path().join("alias.md");
    symlink(&original, &alias).expect("create Skill symlink");

    let catalog = workspace
        .factory(vec![original, alias])
        .create(workspace.request(Vec::new()))
        .expect("discover aliased Skill");

    assert_eq!(catalog.descriptors().len(), 1);
    assert!(!catalog.diagnostics().iter().any(|diagnostic| {
        diagnostic.code == SkillDiagnosticCode::NameCollision
    }));
}
