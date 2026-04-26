use std::fs;

use llm::completion::{Message, message::UserContent};
use skills::{
    SkillConfig, SkillMetadata, SkillsManager, build_skill_injections,
    collect_explicit_skill_mentions, render_skills_section,
};

/// Verifies a valid `SKILL.md` frontmatter block loads into skill metadata.
#[tokio::test]
async fn load_from_root_parses_skill_frontmatter() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let skill_dir = temp.path().join("rust-error-snafu");
    fs::create_dir_all(&skill_dir).expect("skill dir should be created");
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: rust-error-snafu\ndescription: Typed Rust errors.\n---\nBody\n",
    )
    .expect("skill file should be written");

    let manager = SkillsManager::new(SkillConfig {
        roots: vec![temp.path().to_path_buf()],
        cwd: None,
        enabled: true,
    });

    let outcome = manager.load().await;

    assert!(outcome.errors.is_empty());
    assert_eq!(outcome.skills.len(), 1);
    assert_eq!(outcome.skills[0].name, "rust-error-snafu");
    assert_eq!(outcome.skills[0].description, "Typed Rust errors.");
    assert_eq!(outcome.skills[0].path, skill_dir.join("SKILL.md"));
}

/// Verifies one invalid skill does not block valid skills from the same root.
#[tokio::test]
async fn invalid_skill_records_load_error_without_failing_root() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let invalid_dir = temp.path().join("invalid");
    let valid_dir = temp.path().join("valid");
    fs::create_dir_all(&invalid_dir).expect("invalid dir should be created");
    fs::create_dir_all(&valid_dir).expect("valid dir should be created");
    fs::write(invalid_dir.join("SKILL.md"), "missing frontmatter").expect("invalid file");
    fs::write(
        valid_dir.join("SKILL.md"),
        "---\nname: valid\ndescription: Valid skill.\n---\nBody\n",
    )
    .expect("valid file");

    let outcome = SkillsManager::new(SkillConfig {
        roots: vec![temp.path().to_path_buf()],
        cwd: None,
        enabled: true,
    })
    .load()
    .await;

    assert_eq!(outcome.skills.len(), 1);
    assert_eq!(outcome.skills[0].name, "valid");
    assert_eq!(outcome.errors.len(), 1);
    assert!(
        outcome.errors[0]
            .message
            .contains("missing YAML frontmatter")
    );
}

/// Verifies available skills render into the system prompt section.
#[test]
fn render_skills_section_lists_available_skills() {
    let skill = SkillMetadata {
        name: "rust-error-snafu".to_string(),
        description: "Typed Rust errors.".to_string(),
        path: "/tmp/skills/rust-error-snafu/SKILL.md".into(),
    };

    let rendered = render_skills_section(&[skill]).expect("section should render");

    assert!(rendered.contains("## Skills"));
    assert!(rendered.contains("- rust-error-snafu: Typed Rust errors."));
    assert!(rendered.contains("/tmp/skills/rust-error-snafu/SKILL.md"));
}

/// Verifies explicit name and linked path mentions select one unique skill.
#[test]
fn explicit_mentions_select_unique_matching_skills() {
    let skill = SkillMetadata {
        name: "rust-error-snafu".to_string(),
        description: "Typed Rust errors.".to_string(),
        path: "/tmp/skills/rust-error-snafu/SKILL.md".into(),
    };
    let other = SkillMetadata {
        name: "other".to_string(),
        description: "Other skill.".to_string(),
        path: "/tmp/skills/other/SKILL.md".into(),
    };

    let selected = collect_explicit_skill_mentions(
        "Use $rust-error-snafu and [$rust-error-snafu](skill:///tmp/skills/rust-error-snafu/SKILL.md)",
        &[skill.clone(), other],
    );

    assert_eq!(selected, vec![skill]);
}

/// Verifies selected skill files are wrapped as prompt-visible instruction messages.
#[tokio::test]
async fn build_skill_injections_wraps_selected_skill_contents() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let skill_path = temp.path().join("SKILL.md");
    fs::write(
        &skill_path,
        "---\nname: rust-error-snafu\ndescription: Typed Rust errors.\n---\nUse SNAFU context.\n",
    )
    .expect("skill file should be written");

    let messages = build_skill_injections(&[SkillMetadata {
        name: "rust-error-snafu".to_string(),
        description: "Typed Rust errors.".to_string(),
        path: skill_path.clone(),
    }])
    .await
    .expect("injection should succeed");

    let content = first_user_text(&messages[0]);
    assert_eq!(messages.len(), 1);
    assert!(content.contains("<skill_instructions"));
    assert!(content.contains("Use SNAFU context."));
    assert!(content.contains(skill_path.to_string_lossy().as_ref()));
}

/// Extracts the first user text block from a message created by skill injection.
fn first_user_text(message: &Message) -> &str {
    let Message::User { content } = message else {
        panic!("expected user message");
    };
    let UserContent::Text(text) = content.first_ref() else {
        panic!("expected text content");
    };
    text.text()
}
