//! Per-run workspace snapshots: taken as hidden Git refs without touching the
//! user's index, HEAD, or files; listed per run with their changes; diffed
//! per file; and removed with their session.

use std::path::Path;
use std::process::Command;

use acp::workspace::WorkspaceRoot;
use acp::workspace::checkpoint::GitCheckpoints;
use kernel::{CheckpointPoint, CheckpointStage, RunCheckpoints as _};
use protocol::{
    MessageId, RunId, SessionId, WorkspaceChangeKind, WorkspaceDiff,
};

/// Runs one git command in `dir` with a fixed identity and returns its stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8(output.stdout).expect("utf8")
}

/// A repository on branch `main` with one commit holding `a.rs` and a
/// `.gitignore` that ignores `*.log`.
fn git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("repository");
    git(dir.path(), &["init", "-q"]);
    std::fs::write(dir.path().join("a.rs"), "one\n").expect("file");
    std::fs::write(dir.path().join(".gitignore"), "*.log\n").expect("ignore");
    git(dir.path(), &["add", "a.rs", ".gitignore"]);
    git(dir.path(), &["commit", "-q", "-m", "first"]);
    dir
}

/// The ids a test uses.
fn ids(session: &str, run: &str) -> (SessionId, RunId) {
    (
        SessionId::try_from(session).expect("session"),
        RunId::try_from(run).expect("run"),
    )
}

/// Takes one snapshot of `cwd` for `run` of `session`.
async fn capture(
    store: &GitCheckpoints,
    cwd: &Path,
    session: &SessionId,
    run: &RunId,
    stage: CheckpointStage,
    anchor: Option<&MessageId>,
) {
    store
        .capture(
            CheckpointPoint::builder()
                .cwd(cwd)
                .session_id(session)
                .run_id(run)
                .stage(stage)
                .anchor(anchor)
                .build(),
        )
        .await
        .expect("capture");
}

/// A run's start and end snapshots list the files it changed, untracked ones
/// included and ignored ones left out, under its last reply.
#[tokio::test]
async fn checkpoints_list_a_run_with_its_changes() {
    let repo = git_repo();
    let store = GitCheckpoints::default();
    let (session, run) = ids("session-1", "run-1");
    let anchor = MessageId::try_from("message-1").expect("message");
    capture(
        &store,
        repo.path(),
        &session,
        &run,
        CheckpointStage::Start,
        None,
    )
    .await;
    std::fs::write(repo.path().join("a.rs"), "one\ntwo\n").expect("edit");
    std::fs::write(repo.path().join("n.rs"), "x\ny\nz\n").expect("new");
    std::fs::write(repo.path().join("ignored.log"), "noise\n").expect("log");
    capture(
        &store,
        repo.path(),
        &session,
        &run,
        CheckpointStage::End,
        Some(&anchor),
    )
    .await;

    let root = WorkspaceRoot::try_from(repo.path()).expect("root");
    let list = root.checkpoints(&session).await.expect("list");
    assert_eq!(list.runs.len(), 1);
    let listed = &list.runs[0];
    assert_eq!(listed.run_id, run);
    assert_eq!(listed.anchor_message_id.as_ref(), Some(&anchor));
    let files: Vec<_> = listed
        .changes
        .changes
        .iter()
        .map(|change| (change.path.as_str(), change.kind, change.insertions))
        .collect();
    assert_eq!(
        files,
        vec![
            ("a.rs", WorkspaceChangeKind::Modified, Some(1)),
            ("n.rs", WorkspaceChangeKind::Added, Some(3)),
        ]
    );
}

/// A run that changed nothing is not listed.
#[tokio::test]
async fn unchanged_runs_are_not_listed() {
    let repo = git_repo();
    let store = GitCheckpoints::default();
    let (session, run) = ids("session-1", "run-1");
    capture(
        &store,
        repo.path(),
        &session,
        &run,
        CheckpointStage::Start,
        None,
    )
    .await;
    capture(
        &store,
        repo.path(),
        &session,
        &run,
        CheckpointStage::End,
        None,
    )
    .await;

    let root = WorkspaceRoot::try_from(repo.path()).expect("root");
    assert!(
        root.checkpoints(&session)
            .await
            .expect("list")
            .runs
            .is_empty()
    );
}

/// Snapshots leave the user's index bytes, HEAD, and status as they were,
/// and no scratch index behind.
#[tokio::test]
async fn capture_leaves_the_user_index_and_head_alone() {
    let repo = git_repo();
    std::fs::write(repo.path().join("staged.rs"), "s\n").expect("new");
    git(repo.path(), &["add", "staged.rs"]);
    std::fs::write(repo.path().join("a.rs"), "loose\n").expect("edit");
    let index = std::fs::read(repo.path().join(".git/index")).expect("index");
    let head = git(repo.path(), &["rev-parse", "HEAD"]);
    let status = git(repo.path(), &["status", "--porcelain"]);

    let store = GitCheckpoints::default();
    let (session, run) = ids("session-1", "run-1");
    capture(
        &store,
        repo.path(),
        &session,
        &run,
        CheckpointStage::Start,
        None,
    )
    .await;
    capture(
        &store,
        repo.path(),
        &session,
        &run,
        CheckpointStage::End,
        None,
    )
    .await;

    assert_eq!(
        std::fs::read(repo.path().join(".git/index")).expect("index"),
        index
    );
    assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]), head);
    assert_eq!(git(repo.path(), &["status", "--porcelain"]), status);
    let leftovers: Vec<_> = std::fs::read_dir(repo.path().join(".git"))
        .expect("git dir")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("clawcode-checkpoint-index-")
        })
        .collect();
    assert!(leftovers.is_empty());
}

/// A repository without commits or an index can be snapshotted too.
#[tokio::test]
async fn capture_in_a_repository_without_commits() {
    let dir = tempfile::tempdir().expect("repository");
    git(dir.path(), &["init", "-q"]);
    let store = GitCheckpoints::default();
    let (session, run) = ids("session-1", "run-1");
    capture(
        &store,
        dir.path(),
        &session,
        &run,
        CheckpointStage::Start,
        None,
    )
    .await;
    std::fs::write(dir.path().join("first.rs"), "x\n").expect("new");
    capture(
        &store,
        dir.path(),
        &session,
        &run,
        CheckpointStage::End,
        None,
    )
    .await;

    let root = WorkspaceRoot::try_from(dir.path()).expect("root");
    let list = root.checkpoints(&session).await.expect("list");
    assert_eq!(list.runs.len(), 1);
    assert_eq!(list.runs[0].changes.changes[0].path, "first.rs");
}

/// A workspace inside a larger repository sees its own files, root-relative.
#[tokio::test]
async fn checkpoint_paths_are_relative_to_the_workspace() {
    let repo = git_repo();
    let sub = repo.path().join("sub");
    std::fs::create_dir(&sub).expect("sub");
    let store = GitCheckpoints::default();
    let (session, run) = ids("session-1", "run-1");
    capture(&store, &sub, &session, &run, CheckpointStage::Start, None).await;
    std::fs::write(sub.join("in.rs"), "x\n").expect("new");
    std::fs::write(repo.path().join("a.rs"), "outside\n").expect("edit");
    capture(&store, &sub, &session, &run, CheckpointStage::End, None).await;

    let root = WorkspaceRoot::try_from(sub.as_path()).expect("root");
    let list = root.checkpoints(&session).await.expect("list");
    let paths: Vec<_> = list.runs[0]
        .changes
        .changes
        .iter()
        .map(|change| change.path.as_str())
        .collect();
    assert_eq!(paths, vec!["in.rs"]);
    let diff = root
        .checkpoint_diff(&session, &run, "in.rs", None)
        .await
        .expect("diff");
    assert!(
        matches!(diff, WorkspaceDiff::Patch { patch, .. } if patch.contains("+x"))
    );
}

/// One file's change within a run; a file the run left alone has none.
#[tokio::test]
async fn checkpoint_diff_returns_one_file() {
    let repo = git_repo();
    std::fs::write(repo.path().join("b.rs"), "b\n").expect("file");
    git(repo.path(), &["add", "b.rs"]);
    git(repo.path(), &["commit", "-q", "-m", "b"]);
    let store = GitCheckpoints::default();
    let (session, run) = ids("session-1", "run-1");
    capture(
        &store,
        repo.path(),
        &session,
        &run,
        CheckpointStage::Start,
        None,
    )
    .await;
    std::fs::write(repo.path().join("a.rs"), "one\ntwo\n").expect("edit");
    capture(
        &store,
        repo.path(),
        &session,
        &run,
        CheckpointStage::End,
        None,
    )
    .await;

    let root = WorkspaceRoot::try_from(repo.path()).expect("root");
    let diff = root
        .checkpoint_diff(&session, &run, "a.rs", None)
        .await
        .expect("diff");
    assert!(
        matches!(&diff, WorkspaceDiff::Patch { patch, .. } if patch.contains("+two")),
        "{diff:?}"
    );
    assert_eq!(
        root.checkpoint_diff(&session, &run, "b.rs", None)
            .await
            .expect("diff"),
        WorkspaceDiff::Unchanged
    );
}

/// Removing a session drops all its refs and keeps other sessions'.
#[tokio::test]
async fn remove_deletes_every_ref_of_the_session() {
    let repo = git_repo();
    let store = GitCheckpoints::default();
    let (session, first) = ids("session-1", "run-1");
    let (other, other_run) = ids("session-2", "run-9");
    let second = RunId::try_from("run-2").expect("run");
    for run in [&first, &second] {
        capture(
            &store,
            repo.path(),
            &session,
            run,
            CheckpointStage::Start,
            None,
        )
        .await;
        capture(
            &store,
            repo.path(),
            &session,
            run,
            CheckpointStage::End,
            None,
        )
        .await;
    }
    capture(
        &store,
        repo.path(),
        &other,
        &other_run,
        CheckpointStage::Start,
        None,
    )
    .await;

    store.remove(repo.path(), &session).await.expect("remove");

    let refs = |prefix: &str| {
        git(
            repo.path(),
            &["for-each-ref", "--format=%(refname)", prefix],
        )
    };
    assert!(refs("refs/clawcode/checkpoints/session-1/").is_empty());
    assert!(!refs("refs/clawcode/checkpoints/session-2/").is_empty());
}

/// Outside Git, snapshots are skipped and nothing is listed.
#[tokio::test]
async fn outside_git_capture_is_skipped() {
    let dir = tempfile::tempdir().expect("plain directory");
    let store = GitCheckpoints::default();
    let (session, run) = ids("session-1", "run-1");
    capture(
        &store,
        dir.path(),
        &session,
        &run,
        CheckpointStage::Start,
        None,
    )
    .await;
    capture(
        &store,
        dir.path(),
        &session,
        &run,
        CheckpointStage::End,
        None,
    )
    .await;

    let root = WorkspaceRoot::try_from(dir.path()).expect("root");
    assert!(
        root.checkpoints(&session)
            .await
            .expect("list")
            .runs
            .is_empty()
    );
}

/// A file a run renamed diffs as a rename, not as a new file.
#[tokio::test]
async fn renamed_file_checkpoint_diff_shows_the_rename() {
    let repo = git_repo();
    let body: String = (0..50).map(|n| format!("line {n}\n")).collect();
    std::fs::write(repo.path().join("old.rs"), &body).expect("file");
    git(repo.path(), &["add", "old.rs"]);
    git(repo.path(), &["commit", "-q", "-m", "old"]);
    let store = GitCheckpoints::default();
    let (session, run) = ids("session-1", "run-1");
    capture(
        &store,
        repo.path(),
        &session,
        &run,
        CheckpointStage::Start,
        None,
    )
    .await;
    std::fs::rename(repo.path().join("old.rs"), repo.path().join("new.rs"))
        .expect("rename");
    std::fs::write(repo.path().join("new.rs"), format!("{body}added\n"))
        .expect("edit");
    capture(
        &store,
        repo.path(),
        &session,
        &run,
        CheckpointStage::End,
        None,
    )
    .await;

    let root = WorkspaceRoot::try_from(repo.path()).expect("root");
    let diff = root
        .checkpoint_diff(&session, &run, "new.rs", Some("old.rs"))
        .await
        .expect("diff");
    let WorkspaceDiff::Patch { patch, .. } = diff else {
        panic!("a patch: {diff:?}");
    };
    assert!(patch.contains("rename from old.rs"), "{patch}");
}

/// A run that touches thousands of files is still listed: its recorded
/// change list is far past the 128 KiB limit of one command-line argument.
#[tokio::test]
async fn a_run_changing_thousands_of_files_is_listed() {
    let repo = git_repo();
    let store = GitCheckpoints::default();
    let (session, run) = ids("session-1", "run-1");
    capture(
        &store,
        repo.path(),
        &session,
        &run,
        CheckpointStage::Start,
        None,
    )
    .await;
    for n in 0..3000 {
        std::fs::write(
            repo.path()
                .join(format!("generated-file-with-a-long-name-{n:05}.txt")),
            "x\n",
        )
        .expect("file");
    }
    capture(
        &store,
        repo.path(),
        &session,
        &run,
        CheckpointStage::End,
        None,
    )
    .await;

    let root = WorkspaceRoot::try_from(repo.path()).expect("root");
    let list = root.checkpoints(&session).await.expect("list");
    assert_eq!(list.runs.len(), 1);
    assert_eq!(list.runs[0].changes.changes.len(), 3000);
}
