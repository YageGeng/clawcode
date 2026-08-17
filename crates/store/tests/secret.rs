use std::sync::Arc;

use protocol::McpServerId;
use store::{
    FileSecretStore, SecretKey, SecretStore, SecretStoreError, SecretValue,
};

/// File storage survives reconstruction while public formatting remains redacted.
#[test]
fn secret_round_trip_is_stable_and_redacted() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let key = SecretKey::new(
        McpServerId::try_from("oauth-server").expect("Server id"),
        "default-user",
    )
    .expect("Secret key");
    let value =
        SecretValue::new(br#"{"access_token":"access-secret"}"#.to_vec())
            .expect("Secret value");
    let store = FileSecretStore::new(directory.path()).expect("Secret Store");
    store.store(&key, &value).expect("store Secret");

    let reopened =
        FileSecretStore::new(directory.path()).expect("reopened Secret Store");
    let loaded = reopened
        .load(&key)
        .expect("load Secret")
        .expect("stored Secret");
    assert_eq!(loaded.expose(), value.expose());
    assert!(!format!("{value:?}").contains("access-secret"));
    assert!(!value.to_string().contains("access-secret"));
    assert!(
        !serde_json::to_string(&value)
            .expect("redacted Secret JSON")
            .contains("access-secret")
    );
}

/// Secret directories and files use owner-only permissions on Unix.
#[cfg(unix)]
#[test]
fn secret_storage_uses_restricted_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("temporary directory");
    let root = directory.path().join("credentials");
    let key = SecretKey::new(
        McpServerId::try_from("permission-server").expect("Server id"),
        "subject",
    )
    .expect("Secret key");
    let store = FileSecretStore::new(&root).expect("Secret Store");
    store
        .store(
            &key,
            &SecretValue::new(b"secret".to_vec()).expect("Secret value"),
        )
        .expect("store Secret");

    assert_eq!(
        std::fs::metadata(&root)
            .expect("directory metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let entry = std::fs::read_dir(&root)
        .expect("credential directory")
        .next()
        .expect("credential file")
        .expect("credential entry");
    assert_eq!(
        entry
            .metadata()
            .expect("file metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

/// Atomic replacements expose only complete old or new values to concurrent readers.
#[test]
fn concurrent_reads_never_observe_partial_secret() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let store =
        Arc::new(FileSecretStore::new(directory.path()).expect("Secret Store"));
    let key = SecretKey::new(
        McpServerId::try_from("concurrent-server").expect("Server id"),
        "subject",
    )
    .expect("Secret key");
    let first = SecretValue::new(vec![b'a'; 8_192]).expect("first Secret");
    let second = SecretValue::new(vec![b'b'; 8_192]).expect("second Secret");
    store.store(&key, &first).expect("initial Secret");

    let writer_store = Arc::clone(&store);
    let writer_key = key.clone();
    let writer = std::thread::spawn(move || {
        for index in 0..32 {
            let value = if index % 2 == 0 { &second } else { &first };
            writer_store
                .store(&writer_key, value)
                .expect("atomic Secret replacement");
        }
    });
    for _index in 0..64 {
        let loaded = store
            .load(&key)
            .expect("concurrent load")
            .expect("stored Secret");
        assert!(
            loaded.expose().iter().all(|byte| *byte == b'a')
                || loaded.expose().iter().all(|byte| *byte == b'b')
        );
    }
    writer.join().expect("Secret writer");
}

/// Corrupt credential envelopes fail explicitly and deletion is idempotent.
#[test]
fn corrupt_secret_is_rejected_and_delete_is_idempotent() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let key = SecretKey::new(
        McpServerId::try_from("corrupt-server").expect("Server id"),
        "subject",
    )
    .expect("Secret key");
    let store = FileSecretStore::new(directory.path()).expect("Secret Store");
    store
        .store(
            &key,
            &SecretValue::new(b"secret".to_vec()).expect("Secret value"),
        )
        .expect("store Secret");
    let credential = std::fs::read_dir(directory.path())
        .expect("credential directory")
        .next()
        .expect("credential file")
        .expect("credential entry")
        .path();
    std::fs::write(credential, b"not-json").expect("corrupt credential");

    assert!(matches!(
        store.load(&key),
        Err(SecretStoreError::Corrupt(_))
    ));
    store.delete(&key).expect("delete corrupt Secret");
    store.delete(&key).expect("idempotent delete");
    assert!(store.load(&key).expect("load missing Secret").is_none());
}
