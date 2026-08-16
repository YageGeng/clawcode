//! Integration tests for Skill frontmatter validation and structured diagnostics.

use std::fs;
use std::path::Path;

use protocol::{SkillDiagnosticCode, SkillResourceRequest, SkillSelectionRule};
use skill::{FilesystemSkillFactory, SkillFactory};

/// Writes one Skill fixture and creates its parent directory.
fn write_skill(path: &Path, frontmatter: &str) {
    fs::create_dir_all(path.parent().expect("Skill parent"))
        .expect("create Skill parent");
    fs::write(path, format!("---\n{frontmatter}\n---\nbody\n"))
        .expect("write Skill");
}

/// Missing frontmatter is distinguished from semantically invalid parsed metadata.
#[test]
fn metadata_missing_frontmatter_reports_frontmatter_invalid() {
    let workspace = tempfile::tempdir().expect("Skill workspace");
    let global = workspace.path().join("config");
    let home = workspace.path().join("home");
    let cwd = workspace.path().join("project");
    let skill = global.join("skills/missing/SKILL.md");
    fs::create_dir_all(skill.parent().expect("Skill parent"))
        .expect("create Skill parent");
    fs::create_dir_all(&cwd).expect("create Session cwd");
    fs::write(&skill, "No frontmatter\n").expect("write invalid Skill");

    let catalog = FilesystemSkillFactory::builder()
        .global_root(global)
        .user_home(home)
        .build()
        .create(SkillResourceRequest {
            cwd,
            project_resources_allowed: true,
            extension_skill_paths: Vec::new(),
        })
        .expect("discover Skills");

    assert!(catalog.get("missing").is_none());
    assert_eq!(catalog.diagnostics().len(), 1);
    assert_eq!(
        catalog.diagnostics()[0].code,
        SkillDiagnosticCode::FrontmatterInvalid
    );
}

/// Relative selection-rule paths resolve from the Session cwd before comparison.
#[test]
fn selection_rule_relative_path_uses_session_cwd() {
    let workspace = tempfile::tempdir().expect("Skill workspace");
    let global = workspace.path().join("config");
    let home = workspace.path().join("home");
    let cwd = workspace.path().join("project");
    let skill = cwd.join(".pi/skills/review/SKILL.md");
    fs::create_dir_all(skill.parent().expect("Skill parent"))
        .expect("create Skill parent");
    fs::write(
        &skill,
        "---\nname: review\ndescription: Review changes\n---\nreview\n",
    )
    .expect("write Skill");

    let catalog = FilesystemSkillFactory::builder()
        .global_root(global)
        .user_home(home)
        .rules(vec![SkillSelectionRule {
            path: Some(".pi/skills/review/SKILL.md".into()),
            name: None,
            enabled: false,
        }])
        .build()
        .create(SkillResourceRequest {
            cwd,
            project_resources_allowed: true,
            extension_skill_paths: Vec::new(),
        })
        .expect("discover Skills");

    assert!(catalog.get("review").is_none());
}

/// Pi validation warns for invalid names and long descriptions without dropping loadable Skills.
#[test]
fn metadata_validation_preserves_loadable_skills() {
    let workspace = tempfile::tempdir().expect("Skill workspace");
    let global = workspace.path().join("config");
    let home = workspace.path().join("home");
    let cwd = workspace.path().join("project");
    fs::create_dir_all(&cwd).expect("create Session cwd");
    let skills = global.join("skills");
    write_skill(
        &skills.join("valid/SKILL.md"),
        &format!("name: {}\ndescription: valid", "a".repeat(64)),
    );
    write_skill(
        &skills.join("long-name/SKILL.md"),
        &format!("name: {}\ndescription: long name", "b".repeat(65)),
    );
    write_skill(
        &skills.join("uppercase/SKILL.md"),
        "name: Uppercase\ndescription: uppercase",
    );
    write_skill(
        &skills.join("hyphens/SKILL.md"),
        "name: double--hyphen\ndescription: consecutive hyphens",
    );
    write_skill(
        &skills.join("different-directory/SKILL.md"),
        "name: declared-name\ndescription: name may differ\nunknown-field: retained",
    );
    write_skill(
        &skills.join("long-description/SKILL.md"),
        &format!("name: long-description\ndescription: {}", "x".repeat(1_025)),
    );
    write_skill(
        &skills.join("missing-description/SKILL.md"),
        "name: missing-description",
    );

    let catalog = FilesystemSkillFactory::builder()
        .global_root(global)
        .user_home(home)
        .build()
        .create(SkillResourceRequest {
            cwd,
            project_resources_allowed: true,
            extension_skill_paths: Vec::new(),
        })
        .expect("discover Skills");

    assert!(catalog.get(&"a".repeat(64)).is_some());
    assert!(catalog.get(&"b".repeat(65)).is_some());
    assert!(catalog.get("Uppercase").is_some());
    assert!(catalog.get("double--hyphen").is_some());
    assert!(catalog.get("declared-name").is_some());
    assert!(catalog.get("long-description").is_some());
    assert!(catalog.get("missing-description").is_none());
    assert_eq!(
        catalog
            .diagnostics()
            .iter()
            .filter(|diagnostic| {
                diagnostic.code == SkillDiagnosticCode::MetadataInvalid
            })
            .count(),
        5
    );
}
