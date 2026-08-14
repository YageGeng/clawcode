use std::fs;

use skill::{FilesystemSkillFactory, SkillFactory};

/// Recursive discovery reads SKILL.md frontmatter and explicit invocation returns the file.
#[test]
fn discovers_and_invokes_nested_skills() {
    let root = tempfile::tempdir().expect("temporary skill root");
    let skill_dir = root.path().join("nested").join("review");
    fs::create_dir_all(&skill_dir).expect("create skill directory");
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: code-review\ndescription: Review Rust code\n---\n\n# Steps\nInspect carefully.\n",
    )
    .expect("write skill");

    let catalog = FilesystemSkillFactory::new(vec![root.path().to_path_buf()])
        .create()
        .expect("skills should load");
    let descriptor = catalog
        .get("code-review")
        .expect("skill should be discovered");
    let content = catalog
        .invoke("code-review")
        .expect("skill should be invoked");

    assert_eq!(descriptor.description, "Review Rust code");
    assert!(content.contains("# Steps"));
    assert!(content.contains("Inspect carefully."));
}

/// A later root overrides an earlier skill with the same declared name.
#[test]
fn later_skill_roots_override_earlier_roots() {
    let user = tempfile::tempdir().expect("temporary user root");
    let project = tempfile::tempdir().expect("temporary project root");
    fs::write(
        user.path().join("SKILL.md"),
        "---\nname: shared\ndescription: User version\n---\nuser\n",
    )
    .expect("write user skill");
    fs::write(
        project.path().join("SKILL.md"),
        "---\nname: shared\ndescription: Project version\n---\nproject\n",
    )
    .expect("write project skill");

    let catalog = FilesystemSkillFactory::new(vec![
        user.path().to_path_buf(),
        project.path().to_path_buf(),
    ])
    .create()
    .expect("skills should load");

    assert_eq!(
        catalog.get("shared").expect("shared skill").description,
        "Project version"
    );
    assert!(
        catalog
            .invoke("shared")
            .expect("invoke skill")
            .contains("project")
    );
}
