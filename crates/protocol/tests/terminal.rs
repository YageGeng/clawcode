use std::path::PathBuf;

use protocol::{
    SessionId, TerminalId, TerminalListResult, TerminalSnapshot,
    TerminalStatus, TerminalUpdateNotification, TimestampMs,
};

/// Terminal snapshots serialize stable host-facing field names.
#[test]
fn terminal_snapshot_uses_host_facing_camel_case_fields() {
    let snapshot = TerminalSnapshot::builder()
        .terminal_id(TerminalId::try_from(7_319_u32).expect("terminal id"))
        .command("python3 -i".to_string())
        .cwd(PathBuf::from("/tmp/work"))
        .tty(true)
        .status(TerminalStatus::Running)
        .started_at(TimestampMs::from(100_u64))
        .last_activity_at(TimestampMs::from(200_u64))
        .build();

    let json = serde_json::to_value(snapshot).expect("terminal snapshot json");
    assert_eq!(json["terminalId"], 7_319);
    assert_eq!(json["status"], "running");
    assert_eq!(json["tty"], true);
    assert_eq!(json.get("exitCode"), Some(&serde_json::Value::Null));
}

/// Terminal identifiers reject values outside the model-facing numeric range.
#[test]
fn terminal_id_enforces_the_session_local_range() {
    TerminalId::try_from(999_u32).expect_err("below minimum");
    TerminalId::try_from(1_000_u32).expect("minimum");
    TerminalId::try_from(99_999_u32).expect("maximum");
    TerminalId::try_from(100_000_u32).expect_err("above maximum");
}

/// Terminal synchronization carries only a monotonic snapshot revision.
#[test]
fn terminal_sync_payloads_use_revision_invalidation() {
    let list = TerminalListResult {
        revision: 17,
        terminals: Vec::new(),
    };
    let update = TerminalUpdateNotification {
        session_id: SessionId::try_from("terminal-sync").expect("session id"),
        revision: 18,
    };

    assert_eq!(
        serde_json::to_value(list).expect("terminal list JSON"),
        serde_json::json!({ "revision": 17, "terminals": [] })
    );
    assert_eq!(
        serde_json::to_value(update).expect("terminal update JSON"),
        serde_json::json!({ "sessionId": "terminal-sync", "revision": 18 })
    );
}
