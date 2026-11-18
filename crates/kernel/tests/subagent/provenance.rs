//! Creation-time git and product provenance recorded in Session headers.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use store::{JsonlStoreFactory, SystemClock};

use super::fixture::{Route, TestKernel};

/// Runs one git command in `root` and returns its trimmed stdout.
fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf8")
        .trim()
        .to_owned()
}

/// Reads the header line of the only Session log under the fixture root.
fn only_header(test: &TestKernel) -> serde_json::Value {
    let factory = JsonlStoreFactory::new(
        test.root.path().join("sessions"),
        Arc::new(SystemClock),
    );
    let paths = factory.session_paths().expect("paths");
    assert_eq!(paths.len(), 1);
    let content =
        std::fs::read_to_string(paths.first().expect("log path")).expect("log");
    serde_json::from_str(content.lines().next().expect("header"))
        .expect("header json")
}

/// A Session created inside a repository records its commit, branch, and origin.
#[tokio::test]
async fn session_header_records_git_provenance() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let root = test.root.path();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["commit", "-q", "--allow-empty", "-m", "init"]);
    git(
        root,
        &["remote", "add", "origin", "https://example.com/o/r.git"],
    );
    let sha = git(root, &["rev-parse", "HEAD"]);

    test.create("session-git").await;

    let header = only_header(&test);
    assert_eq!(header["git"]["sha"], sha.as_str());
    assert_eq!(header["git"]["branch"], "main");
    assert_eq!(header["git"]["originUrl"], "https://example.com/o/r.git");
    assert_eq!(header["appVersion"], env!("CARGO_PKG_VERSION"));
}

/// Credentials in the origin URL and a detached HEAD never reach the header.
#[tokio::test]
async fn session_header_drops_origin_credentials_and_detached_head() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let root = test.root.path();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["commit", "-q", "--allow-empty", "-m", "init"]);
    git(root, &["checkout", "-q", "--detach"]);
    git(
        root,
        &[
            "remote",
            "add",
            "origin",
            "https://user:token@example.com/o/r.git",
        ],
    );

    test.create("session-secret").await;

    let header = only_header(&test);
    assert_eq!(header["git"]["originUrl"], "https://example.com/o/r.git");
    let git = header["git"].as_object().expect("git object");
    assert!(!git.contains_key("branch"));
}

/// Outside a repository the header has no git field but still names the version.
#[tokio::test]
async fn session_outside_git_has_no_git_field() {
    let test = TestKernel::new(|_| Route::answer("ok"));

    test.create("session-plain").await;

    let header = only_header(&test);
    let object = header.as_object().expect("header object");
    assert!(!object.contains_key("git"));
    assert_eq!(header["appVersion"], env!("CARGO_PKG_VERSION"));
}
