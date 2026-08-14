use std::path::PathBuf;

use kernel::{
    PiSystemPromptFactory, ProjectContext, SystemPromptContext,
    SystemPromptFactory,
};
use protocol::SkillInfo;
use protocol::{MessageContent, TimestampMs, ToolDefinition, TurnId};

/// Builds a model-facing tool definition with deterministic schema.
fn tool_definition(name: &str, description: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {}
        }),
    }
}

/// The prompt declares only registered tools and includes ancestor instructions.
#[test]
fn prompt_declares_only_registered_tools_and_ancestor_context() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("AGENTS.md"), "root rules")
        .expect("root rules");
    let cwd = root.path().join("repo/src");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let project =
        ProjectContext::discover(&cwd, root.path()).expect("project context");
    let context = SystemPromptContext::builder()
        .cwd(cwd.clone())
        .turn_id(TurnId::try_from("turn-prompt").expect("turn id"))
        .timestamp_ms(TimestampMs::from(1_700_000_000_001))
        .tools(vec![
            tool_definition("read", "Read a file"),
            tool_definition("write", "Write a file"),
        ])
        .skills(Vec::new())
        .project(project)
        .build();

    let message = PiSystemPromptFactory::default()
        .create(context)
        .expect("system prompt");
    let text = message.content.text_content().expect("system text");

    assert!(matches!(message.content, MessageContent::System { .. }));
    assert!(text.contains("read"));
    assert!(text.contains("write"));
    assert!(!text.contains("- bash:"));
    assert!(text.contains("root rules"));
    assert!(text.contains(cwd.to_string_lossy().as_ref()));
    assert_eq!(message.identity.turn_id.as_str(), "turn-prompt");
    assert_eq!(message.timing.timestamp_ms.to_string(), "1700000000001");
}

/// Skills are model-visible only when the registered tool set includes read.
#[test]
fn prompt_injects_skills_only_when_read_is_registered() {
    let root = tempfile::tempdir().expect("root");
    let cwd = root.path().to_path_buf();
    let skill = SkillInfo {
        name: "rust-patterns".to_string(),
        description: "Use idiomatic Rust patterns".to_string(),
        path: PathBuf::from("/skills/rust-patterns/SKILL.md"),
    };
    let base = SystemPromptContext::builder()
        .cwd(cwd.clone())
        .turn_id(TurnId::try_from("turn-without-read").expect("turn id"))
        .timestamp_ms(TimestampMs::from(10))
        .tools(vec![tool_definition("bash", "Run a shell command")])
        .skills(vec![skill.clone()])
        .project(ProjectContext::default())
        .build();
    let without_read = PiSystemPromptFactory::default()
        .create(base)
        .expect("prompt without read")
        .content
        .text_content()
        .expect("system text");
    let with_read = PiSystemPromptFactory::default()
        .create(
            SystemPromptContext::builder()
                .cwd(cwd)
                .turn_id(TurnId::try_from("turn-with-read").expect("turn id"))
                .timestamp_ms(TimestampMs::from(11))
                .tools(vec![tool_definition("read", "Read a file")])
                .skills(vec![skill])
                .project(ProjectContext::default())
                .build(),
        )
        .expect("prompt with read")
        .content
        .text_content()
        .expect("system text");

    assert!(!without_read.contains("<available_skills>"));
    assert!(with_read.contains("<available_skills>"));
    assert!(with_read.contains("rust-patterns"));
    assert!(with_read.contains("/skills/rust-patterns/SKILL.md"));
}

/// Project discovery applies candidate precedence and ancestor-to-child order.
#[test]
fn project_context_uses_pi_candidate_precedence_and_scope_order() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("AGENTS.md"), "root agents")
        .expect("root agents");
    std::fs::write(root.path().join("CLAUDE.md"), "ignored root claude")
        .expect("root claude");
    let child = root.path().join("project");
    std::fs::create_dir_all(&child).expect("child");
    std::fs::write(child.join("AGENTS.md"), "ignored child agents")
        .expect("child agents");
    std::fs::write(child.join("AGENTS.override.md"), "child override")
        .expect("child override");

    let context =
        ProjectContext::discover(&child, root.path()).expect("project context");
    let documents = context.documents();

    assert_eq!(documents.len(), 2);
    assert_eq!(documents[0].content, "root agents");
    assert_eq!(documents[1].content, "child override");
}
