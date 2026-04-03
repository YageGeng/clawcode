use std::process::Command;

#[test]
fn exits_with_usage_when_prompt_is_missing() {
    let output = Command::new(env!("CARGO_BIN_EXE_agent-cli"))
        .output()
        .expect("agent-cli binary should run");

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "usage: cargo run -p agent-cli -- \"your prompt\""
    );
}

#[test]
fn emits_startup_log_when_tracing_is_enabled() {
    let output = Command::new(env!("CARGO_BIN_EXE_agent-cli"))
        .env("RUST_LOG", "info")
        .output()
        .expect("agent-cli binary should run");

    assert_eq!(output.status.code(), Some(2));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("agent-cli invoked without prompt"));
    assert!(stderr.contains("usage: cargo run -p agent-cli -- \"your prompt\""));
}
