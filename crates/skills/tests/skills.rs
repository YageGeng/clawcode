use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
};

use llm::completion::{Message, message::UserContent};
use skills::{
    SkillConfig, SkillInput, SkillMentionOptions, SkillMetadata, SkillsManager,
    build_skill_injections, collect_explicit_skill_mentions, render_skills_section,
};

/// Builds default mention options for tests that do not need disabled paths or connector conflicts.
fn mention_options() -> SkillMentionOptions {
    SkillMentionOptions::default()
}

/// Builds skill metadata with predictable fields for mention-selection tests.
fn test_skill(name: &str, path: &str) -> SkillMetadata {
    SkillMetadata {
        name: name.to_string(),
        description: format!("{name} skill"),
        path: PathBuf::from(path),
        disable_model_invocation: false,
    }
}

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
        disable_model_invocation: false,
    };

    let rendered = render_skills_section(&[skill]).expect("section should render");

    assert!(rendered.contains("<available_skills>"));
    assert!(rendered.contains("<name>rust-error-snafu</name>"));
    assert!(rendered.contains("<description>Typed Rust errors.</description>"));
    assert!(rendered.contains("/tmp/skills/rust-error-snafu/SKILL.md"));
}

/// Verifies explicit name and linked path mentions select one unique skill.
#[test]
fn explicit_mentions_select_unique_matching_skills() {
    let skill = SkillMetadata {
        name: "rust-error-snafu".to_string(),
        description: "Typed Rust errors.".to_string(),
        path: "/tmp/skills/rust-error-snafu/SKILL.md".into(),
        disable_model_invocation: false,
    };
    let other = SkillMetadata {
        name: "other".to_string(),
        description: "Other skill.".to_string(),
        path: "/tmp/skills/other/SKILL.md".into(),
        disable_model_invocation: false,
    };

    let inputs = vec![SkillInput::text(
        "Use $rust-error-snafu and [$rust-error-snafu](skill:///tmp/skills/rust-error-snafu/SKILL.md)",
    )];
    let selected =
        collect_explicit_skill_mentions(&inputs, &[skill.clone(), other], &mention_options());

    assert_eq!(selected, vec![skill]);
}

/// Verifies structured skill input selects by exact path without requiring text mention.
#[test]
fn structured_skill_input_selects_by_path() {
    let alpha = test_skill("alpha-skill", "/tmp/alpha/SKILL.md");
    let inputs = vec![SkillInput::skill("alpha-skill", "/tmp/alpha/SKILL.md")];

    let selected =
        collect_explicit_skill_mentions(&inputs, std::slice::from_ref(&alpha), &mention_options());

    assert_eq!(selected, vec![alpha]);
}

/// Verifies missing structured path blocks same-name plain mention fallback.
#[test]
fn structured_missing_path_blocks_plain_name_fallback() {
    let alpha = test_skill("alpha-skill", "/tmp/alpha/SKILL.md");
    let inputs = vec![
        SkillInput::skill("alpha-skill", "/tmp/missing/SKILL.md"),
        SkillInput::text("use $alpha-skill"),
    ];

    let selected = collect_explicit_skill_mentions(&inputs, &[alpha], &mention_options());

    assert!(selected.is_empty());
}

/// Verifies disabled structured path blocks same-name plain mention fallback.
#[test]
fn structured_disabled_path_blocks_plain_name_fallback() {
    let alpha = test_skill("alpha-skill", "/tmp/alpha/SKILL.md");
    let inputs = vec![
        SkillInput::skill("alpha-skill", "/tmp/alpha/SKILL.md"),
        SkillInput::text("use $alpha-skill"),
    ];
    let options = SkillMentionOptions {
        disabled_paths: HashSet::from([PathBuf::from("/tmp/alpha/SKILL.md")]),
        connector_slug_counts: HashMap::new(),
    };

    let selected = collect_explicit_skill_mentions(&inputs, &[alpha], &options);

    assert!(selected.is_empty());
}

/// Verifies linked paths resolve ambiguous skill names by exact path.
#[test]
fn linked_path_selects_when_plain_name_is_ambiguous() {
    let alpha = test_skill("demo-skill", "/tmp/alpha/SKILL.md");
    let beta = test_skill("demo-skill", "/tmp/beta/SKILL.md");
    let inputs = vec![SkillInput::text(
        "use $demo-skill and [$demo-skill](skill:///tmp/beta/SKILL.md)",
    )];

    let selected =
        collect_explicit_skill_mentions(&inputs, &[alpha, beta.clone()], &mention_options());

    assert_eq!(selected, vec![beta]);
}

/// Verifies plain ambiguous names select nothing.
#[test]
fn plain_ambiguous_name_selects_nothing() {
    let alpha = test_skill("demo-skill", "/tmp/alpha/SKILL.md");
    let beta = test_skill("demo-skill", "/tmp/beta/SKILL.md");
    let inputs = vec![SkillInput::text("use $demo-skill")];

    let selected = collect_explicit_skill_mentions(&inputs, &[alpha, beta], &mention_options());

    assert!(selected.is_empty());
}

/// Verifies connector slug conflicts suppress plain-name skill matching.
#[test]
fn connector_slug_conflict_suppresses_plain_name() {
    let alpha = test_skill("alpha-skill", "/tmp/alpha/SKILL.md");
    let inputs = vec![SkillInput::text("use $alpha-skill")];
    let options = SkillMentionOptions {
        disabled_paths: HashSet::new(),
        connector_slug_counts: HashMap::from([("alpha-skill".to_string(), 1)]),
    };

    let selected = collect_explicit_skill_mentions(&inputs, &[alpha], &options);

    assert!(selected.is_empty());
}

/// Verifies linked path wins even when connector slug conflicts with the skill name.
#[test]
fn linked_path_ignores_connector_slug_conflict() {
    let alpha = test_skill("alpha-skill", "/tmp/alpha/SKILL.md");
    let inputs = vec![SkillInput::text("use [$alpha-skill](/tmp/alpha/SKILL.md)")];
    let options = SkillMentionOptions {
        disabled_paths: HashSet::new(),
        connector_slug_counts: HashMap::from([("alpha-skill".to_string(), 1)]),
    };

    let selected = collect_explicit_skill_mentions(&inputs, std::slice::from_ref(&alpha), &options);

    assert_eq!(selected, vec![alpha]);
}

/// Verifies structured paths match skills when callers use an absolute spelling for a relative skill path.
#[test]
fn structured_path_matches_relative_loaded_skill_path() {
    let relative_path = PathBuf::from("target/skill-test/alpha/SKILL.md");
    let absolute_path = std::env::current_dir()
        .expect("current dir should be readable")
        .join(&relative_path);
    let alpha = SkillMetadata {
        name: "alpha-skill".to_string(),
        description: "Alpha skill".to_string(),
        path: relative_path,
        disable_model_invocation: false,
    };
    let inputs = vec![SkillInput::skill("alpha-skill", absolute_path)];

    let selected =
        collect_explicit_skill_mentions(&inputs, std::slice::from_ref(&alpha), &mention_options());

    assert_eq!(selected, vec![alpha]);
}

/// Verifies linked paths match skills when callers use an absolute spelling for a relative skill path.
#[test]
fn linked_path_matches_relative_loaded_skill_path() {
    let relative_path = PathBuf::from("target/skill-test/beta/SKILL.md");
    let absolute_path = std::env::current_dir()
        .expect("current dir should be readable")
        .join(&relative_path);
    let beta = SkillMetadata {
        name: "beta-skill".to_string(),
        description: "Beta skill".to_string(),
        path: relative_path,
        disable_model_invocation: false,
    };
    let inputs = vec![SkillInput::text(format!(
        "use [$beta-skill]({})",
        absolute_path.display()
    ))];

    let selected =
        collect_explicit_skill_mentions(&inputs, std::slice::from_ref(&beta), &mention_options());

    assert_eq!(selected, vec![beta]);
}

/// Verifies disabled paths use the same normalization as structured and linked mention paths.
#[test]
fn disabled_path_matches_relative_loaded_skill_path() {
    let relative_path = PathBuf::from("target/skill-test/gamma/SKILL.md");
    let absolute_path = std::env::current_dir()
        .expect("current dir should be readable")
        .join(&relative_path);
    let gamma = SkillMetadata {
        name: "gamma-skill".to_string(),
        description: "Gamma skill".to_string(),
        path: relative_path,
        disable_model_invocation: false,
    };
    let inputs = vec![SkillInput::text("use $gamma-skill")];
    let options = SkillMentionOptions {
        disabled_paths: HashSet::from([absolute_path]),
        connector_slug_counts: HashMap::new(),
    };

    let selected = collect_explicit_skill_mentions(&inputs, &[gamma], &options);

    assert!(selected.is_empty());
}

/// Verifies common shell environment variables are not treated as skill mentions.
#[test]
fn common_env_vars_are_ignored() {
    let path_skill = test_skill("PATH", "/tmp/path/SKILL.md");
    let alpha = test_skill("alpha-skill", "/tmp/alpha/SKILL.md");
    let inputs = vec![SkillInput::text(
        "use $PATH and $XDG_CONFIG_HOME and $alpha-skill",
    )];

    let selected =
        collect_explicit_skill_mentions(&inputs, &[path_skill, alpha.clone()], &mention_options());

    assert_eq!(selected, vec![alpha]);
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
        disable_model_invocation: false,
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
