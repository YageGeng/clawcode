use std::path::PathBuf;

use protocol::{
    AcpNavigateParameters, AcpSessionParameters, AcpWorkingDirectory, IdKind,
    SessionId,
};

/// Verifies that shared identifier categories expose stable persisted prefixes.
#[test]
fn id_kind_prefixes_are_stable() {
    assert_eq!(IdKind::Session.prefix(), "session");
    assert_eq!(IdKind::Turn.prefix(), "turn");
    assert_eq!(IdKind::Queue.prefix(), "queue");
}

/// Verifies that ACP request DTOs deserialize directly into protocol identifier types.
#[test]
fn acp_parameters_use_domain_identifiers() {
    let session: AcpSessionParameters =
        serde_json::from_value(serde_json::json!({ "sessionId": "session-1" }))
            .expect("session parameters");
    assert_eq!(
        session.session_id,
        SessionId::try_from("session-1").expect("valid session identifier")
    );

    let navigation: AcpNavigateParameters =
        serde_json::from_value(serde_json::json!({
            "sessionId": "session-1",
            "entryId": "entry-1"
        }))
        .expect("navigation parameters");
    assert_eq!(
        navigation.entry_id.expect("entry identifier").as_str(),
        "entry-1"
    );
}

/// Verifies that the shared ACP working-directory type rejects relative paths.
#[test]
fn acp_working_directory_requires_an_absolute_directory() {
    let error = AcpWorkingDirectory::try_from(PathBuf::from("relative/path"))
        .expect_err("relative path must fail");
    assert_eq!(error.to_string(), "working directory must be absolute");

    let current = std::env::current_dir().expect("current directory");
    let directory =
        AcpWorkingDirectory::try_from(current.clone()).expect("current dir");
    assert_eq!(directory.into_inner(), current);
}
