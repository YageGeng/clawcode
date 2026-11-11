//! Codex-compatible agent addressing, nicknames, and inter-agent mail.

use std::collections::HashSet;

use protocol::{
    AgentMail, AgentName, AgentNickname, AgentPath, ExtensionMessage, MailKind,
};

/// Agent names follow Codex `validate_agent_name`, including its error text.
#[test]
fn agent_names_follow_codex_rules() {
    assert_eq!(
        AgentName::try_from("worker_1").map(String::from),
        Ok("worker_1".to_string())
    );
    let error = |name: &str| AgentName::try_from(name).expect_err(name);
    assert_eq!(error(""), "agent_name must not be empty");
    assert_eq!(error("root"), "agent_name `root` is reserved");
    assert_eq!(error("."), "agent_name `.` is reserved");
    assert_eq!(error(".."), "agent_name `..` is reserved");
    assert_eq!(error("a/b"), "agent_name must not contain `/`");
    for name in ["Reader", "代码审查", "has space"] {
        assert_eq!(
            error(name),
            "agent_name must use only lowercase letters, digits, and underscores"
        );
    }
}

/// References resolve against `/root` exactly one level deep.
#[test]
fn paths_resolve_against_root() {
    let root = AgentPath::root();
    assert!(root.is_root());
    assert_eq!(root.name(), "root");
    let child = AgentPath::resolve("x").expect("relative");
    assert_eq!(child.as_ref(), "/root/x");
    assert_eq!(child.name(), "x");
    assert!(!child.is_root());
    assert!(AgentPath::resolve("/root").expect("root").is_root());
    assert_eq!(
        AgentPath::resolve("/root/y").expect("absolute").as_ref(),
        "/root/y"
    );
    assert_eq!(
        AgentPath::resolve("root").expect_err("bare root"),
        "agent_name `root` is reserved"
    );
    assert_eq!(
        AgentPath::resolve("/root/a/b").expect_err("deep"),
        "agent path `/root/a/b` is deeper than one level"
    );
    assert_eq!(
        AgentPath::resolve("/other").expect_err("foreign"),
        "absolute agent paths must start with `/root`"
    );
    assert_eq!(
        AgentPath::child(&AgentName::try_from("x").expect("name")),
        child
    );
    let name: Result<AgentName, _> =
        serde_json::from_value(serde_json::json!("Bad"));
    assert!(name.is_err(), "names are validated when read");
    let parsed: AgentPath =
        serde_json::from_value(serde_json::json!("/root/x")).expect("serde");
    assert_eq!(parsed, child);
    assert_eq!(
        serde_json::to_value(&child).expect("serialize"),
        serde_json::json!("/root/x")
    );
}

/// Allocation is a pure function of the used set and index, rolling over with ordinals.
#[test]
fn nickname_allocation_is_deterministic_and_rolls_over() {
    let mut used = HashSet::new();
    let mut picks = Vec::new();
    for _ in 0..2021 {
        let nickname = AgentNickname::allocate(&used, 0).into_inner();
        used.insert(nickname.clone());
        picks.push(nickname);
    }
    assert_eq!(picks[0], "Euclid");
    assert_eq!(picks[101], "Euclid the 2nd");
    assert_eq!(picks[1010], "Euclid the 11th");
    assert_eq!(picks[1111], "Euclid the 12th");
    assert_eq!(picks[2020], "Euclid the 21st");
    let fresh = HashSet::new();
    assert_eq!(
        AgentNickname::allocate(&fresh, 7).into_inner(),
        AgentNickname::allocate(&fresh, 7).into_inner()
    );
    assert_eq!(
        AgentNickname::allocate(&fresh, 1).into_inner(),
        "Archimedes"
    );
}

/// Mail renders Codex's inter-agent text and persists as a visible context Extension.
#[test]
fn mail_renders_codex_text_and_extension() {
    let mail = AgentMail::builder()
        .author(AgentPath::root())
        .recipient(AgentPath::resolve("x").expect("path"))
        .kind(MailKind::NewTask)
        .payload("Do it.".to_string())
        .build();
    assert_eq!(
        mail.model_text(),
        "Message Type: new_task\nTask name: /root/x\nSender: /root\nPayload:\nDo it."
    );
    let extension = ExtensionMessage::try_from(mail).expect("extension");
    assert_eq!(extension.extension_id.as_str(), "clawcode");
    assert_eq!(extension.custom_type, protocol::AGENT_MESSAGE_TYPE);
    assert_eq!(extension.custom_type, "agent_message");
    assert!(extension.display);
    assert!(extension.include_in_context);
    assert_eq!(
        extension.details,
        Some(serde_json::json!({
            "kind": "new_task",
            "author": "/root",
            "recipient": "/root/x"
        }))
    );
    assert!(MailKind::NewTask.triggers_turn());
    assert!(!MailKind::Message.triggers_turn());
    assert!(!MailKind::Result.triggers_turn());
    assert_eq!(MailKind::Result.as_str(), "result");
}
