//! Workspace tools behind `_clawcode/workspace/*`: listings, previews, Git status, and diffs.

use std::path::Path;
use std::process::Command;

use acp::workspace::{WorkspaceError, WorkspaceRoot};
use protocol::{
    WORKSPACE_ENTRY_LIMIT, WorkspaceDiff, WorkspaceDiffScope,
    WorkspaceDirListing, WorkspaceFileContent, WorkspaceGitStatus,
};

/// File names of a listing, in order.
fn names(listing: WorkspaceDirListing) -> Vec<String> {
    listing
        .entries
        .into_iter()
        .map(|entry| entry.name)
        .collect()
}

/// Runs one git command in `dir` with a fixed identity, failing the test if it fails.
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
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
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?}");
}

/// A repository on branch `main` with one commit holding `a.rs`.
fn git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("repository");
    git(dir.path(), &["init", "-q"]);
    std::fs::write(dir.path().join("a.rs"), "original\n").expect("file");
    git(dir.path(), &["add", "a.rs"]);
    git(dir.path(), &["commit", "-q", "-m", "first"]);
    dir
}

#[test]
fn listing_sorts_directories_first_and_hides_dotfiles() {
    let dir = tempfile::tempdir().expect("workspace");
    std::fs::create_dir(dir.path().join("src")).expect("dir");
    std::fs::write(dir.path().join("b.txt"), "b").expect("file");
    std::fs::write(dir.path().join("A.txt"), "a").expect("file");
    std::fs::write(dir.path().join(".env"), "x").expect("file");
    let root = WorkspaceRoot::try_from(dir.path()).expect("root");

    assert_eq!(
        names(root.list_dir("", false).expect("list")),
        ["src", "A.txt", "b.txt"]
    );
    assert_eq!(
        names(root.list_dir("", true).expect("list")),
        ["src", ".env", "A.txt", "b.txt"]
    );
}

#[test]
fn listing_respects_gitignore_and_never_shows_git() {
    let dir = tempfile::tempdir().expect("workspace");
    git(dir.path(), &["init", "-q"]);
    std::fs::write(dir.path().join(".gitignore"), "target/\n").expect("ignore");
    std::fs::create_dir(dir.path().join("target")).expect("dir");
    std::fs::write(dir.path().join("main.rs"), "").expect("file");
    let root = WorkspaceRoot::try_from(dir.path()).expect("root");

    assert_eq!(
        names(root.list_dir("", true).expect("list")),
        [".gitignore", "main.rs"]
    );
}

#[test]
fn listing_a_subdirectory_uses_relative_paths() {
    let dir = tempfile::tempdir().expect("workspace");
    std::fs::create_dir_all(dir.path().join("src/ui")).expect("dirs");
    std::fs::write(dir.path().join("src/lib.rs"), "").expect("file");
    let root = WorkspaceRoot::try_from(dir.path()).expect("root");

    let listing = root.list_dir("src", false).expect("list");
    let paths: Vec<_> = listing
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["src/ui", "src/lib.rs"]);
}

#[test]
fn paths_outside_the_root_are_rejected() {
    let outside = tempfile::tempdir().expect("outside");
    let dir = tempfile::tempdir().expect("workspace");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("escape"))
        .expect("symlink");
    let root = WorkspaceRoot::try_from(dir.path()).expect("root");

    for path in ["../x", "/etc/passwd", "a/../../x", "escape"] {
        assert!(
            matches!(
                root.list_dir(path, false),
                Err(WorkspaceError::OutsideRoot)
            ),
            "{path}"
        );
    }
}

#[test]
fn large_and_binary_files_are_not_returned() {
    let dir = tempfile::tempdir().expect("workspace");
    std::fs::write(dir.path().join("big.log"), vec![b'a'; 1024 * 1024 + 1])
        .expect("big");
    std::fs::write(dir.path().join("pic.bin"), [0x89, b'P', b'N', b'G', 0, 0])
        .expect("binary");
    std::fs::write(dir.path().join("ok.rs"), "fn main() {}").expect("text");
    let root = WorkspaceRoot::try_from(dir.path()).expect("root");

    assert_eq!(
        root.read_file("big.log").expect("big"),
        WorkspaceFileContent::TooLarge {
            size: 1024 * 1024 + 1
        }
    );
    assert_eq!(
        root.read_file("pic.bin").expect("binary"),
        WorkspaceFileContent::Binary
    );
    assert_eq!(
        root.read_file("ok.rs").expect("text"),
        WorkspaceFileContent::Text {
            text: "fn main() {}".into()
        }
    );
}

#[test]
fn listing_stops_at_the_entry_limit() {
    let dir = tempfile::tempdir().expect("workspace");
    for n in 0..=WORKSPACE_ENTRY_LIMIT {
        std::fs::write(dir.path().join(format!("f{n}")), "").expect("file");
    }
    let root = WorkspaceRoot::try_from(dir.path()).expect("root");

    let listing = root.list_dir("", false).expect("list");
    assert_eq!(listing.entries.len(), WORKSPACE_ENTRY_LIMIT);
    assert!(listing.truncated);
}

#[test]
fn a_missing_root_is_rejected() {
    let dir = tempfile::tempdir().expect("workspace");
    let gone = dir.path().join("gone");
    assert!(matches!(
        WorkspaceRoot::try_from(gone.as_path()),
        Err(WorkspaceError::Root(_))
    ));
}

#[tokio::test]
async fn status_lists_changes_with_the_branch() {
    let repo = git_repo();
    std::fs::write(repo.path().join("a.rs"), "changed").expect("edit");
    std::fs::write(repo.path().join("new file.rs"), "new").expect("new");
    let root = WorkspaceRoot::try_from(repo.path()).expect("root");

    let WorkspaceGitStatus::Repository {
        branch, changes, ..
    } = root.git_status().await
    else {
        panic!("repository");
    };
    assert_eq!(branch.as_deref(), Some("main"));
    let rows: Vec<_> = changes
        .iter()
        .map(|change| (change.path.as_str(), change.kind.letter()))
        .collect();
    assert_eq!(rows, [("a.rs", 'M'), ("new file.rs", '?')]);
}

#[tokio::test]
async fn status_outside_a_repository() {
    let dir = tempfile::tempdir().expect("plain dir");
    let root = WorkspaceRoot::try_from(dir.path()).expect("root");
    assert_eq!(root.git_status().await, WorkspaceGitStatus::NotRepository);
}

#[tokio::test]
async fn diff_covers_tracked_untracked_and_binary_files() {
    let repo = git_repo();
    std::fs::write(repo.path().join("a.rs"), "changed\n").expect("edit");
    std::fs::write(repo.path().join("u.rs"), "untracked\n").expect("new");
    std::fs::write(repo.path().join("b.bin"), [0u8, 1, 2]).expect("binary");
    let root = WorkspaceRoot::try_from(repo.path()).expect("root");

    let WorkspaceDiff::Patch { patch, truncated } = root
        .git_diff(Some("a.rs"), WorkspaceDiffScope::Base)
        .await
        .expect("diff")
    else {
        panic!("patch");
    };
    assert!(patch.contains("+changed") && !truncated);
    let WorkspaceDiff::Patch { patch, .. } = root
        .git_diff(Some("u.rs"), WorkspaceDiffScope::Base)
        .await
        .expect("diff")
    else {
        panic!("patch");
    };
    assert!(patch.contains("+untracked"));
    assert_eq!(
        root.git_diff(Some("b.bin"), WorkspaceDiffScope::Base)
            .await
            .expect("diff"),
        WorkspaceDiff::Binary
    );
}

#[tokio::test]
async fn diff_of_a_deleted_file_and_an_unchanged_file() {
    let repo = git_repo();
    std::fs::remove_file(repo.path().join("a.rs")).expect("delete");
    let root = WorkspaceRoot::try_from(repo.path()).expect("root");

    let WorkspaceDiff::Patch { patch, .. } = root
        .git_diff(Some("a.rs"), WorkspaceDiffScope::Base)
        .await
        .expect("diff")
    else {
        panic!("patch");
    };
    assert!(patch.contains("-original"));
    git(repo.path(), &["checkout", "--", "a.rs"]);
    assert_eq!(
        root.git_diff(Some("a.rs"), WorkspaceDiffScope::Base)
            .await
            .expect("diff"),
        WorkspaceDiff::Unchanged
    );
    assert!(matches!(
        root.git_diff(Some("../a.rs"), WorkspaceDiffScope::Base)
            .await,
        Err(WorkspaceError::OutsideRoot)
    ));
}

#[tokio::test]
async fn diff_in_a_repository_without_commits() {
    let dir = tempfile::tempdir().expect("repository");
    git(dir.path(), &["init", "-q"]);
    std::fs::write(dir.path().join("first.rs"), "hello\n").expect("file");
    let root = WorkspaceRoot::try_from(dir.path()).expect("root");

    let WorkspaceDiff::Patch { patch, .. } = root
        .git_diff(Some("first.rs"), WorkspaceDiffScope::Base)
        .await
        .expect("diff")
    else {
        panic!("patch");
    };
    assert!(patch.contains("+hello"));
}

#[tokio::test]
async fn status_of_a_workspace_inside_a_larger_repository() {
    let repo = git_repo();
    std::fs::create_dir(repo.path().join("crate")).expect("dir");
    std::fs::write(repo.path().join("crate/lib.rs"), "new").expect("inside");
    std::fs::write(repo.path().join("outside.rs"), "new").expect("outside");
    let root = WorkspaceRoot::try_from(repo.path().join("crate").as_path())
        .expect("root");

    // Paths are relative to the workspace, and changes outside it are not listed.
    let WorkspaceGitStatus::Repository { changes, .. } =
        root.git_status().await
    else {
        panic!("repository");
    };
    let paths: Vec<_> =
        changes.iter().map(|change| change.path.as_str()).collect();
    assert_eq!(paths, ["lib.rs"]);
    let WorkspaceDiff::Patch { patch, .. } = root
        .git_diff(Some("lib.rs"), WorkspaceDiffScope::Base)
        .await
        .expect("diff")
    else {
        panic!("patch");
    };
    assert!(patch.contains("+new"));
}

#[tokio::test]
async fn diff_paths_are_literal_not_pathspecs() {
    let repo = git_repo();
    for path in ["app/[id]/page.tsx", "app/i/page.tsx", ":x.txt", "x.txt"] {
        let file = repo.path().join(path);
        std::fs::create_dir_all(file.parent().expect("parent")).expect("dir");
        std::fs::write(&file, "old\n").expect("file");
    }
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-q", "-m", "second"]);
    for path in ["app/[id]/page.tsx", "app/i/page.tsx", ":x.txt", "x.txt"] {
        std::fs::write(repo.path().join(path), format!("{path}\n"))
            .expect("edit");
    }
    let root = WorkspaceRoot::try_from(repo.path()).expect("root");

    // A glob or magic character names that one file, never its look-alikes.
    for (path, other) in
        [("app/[id]/page.tsx", "app/i/page.tsx"), (":x.txt", "x.txt")]
    {
        let WorkspaceDiff::Patch { patch, .. } = root
            .git_diff(Some(path), WorkspaceDiffScope::Base)
            .await
            .expect("diff")
        else {
            panic!("patch for {path}");
        };
        assert!(patch.contains(&format!("+{path}")), "{patch}");
        assert!(!patch.contains(&format!("+{other}")), "{patch}");
    }
}

/// One file's path with its `(insertions, deletions)`.
type FileCounts = (String, Option<u64>, Option<u64>);

/// Per-file counts of a repository status, by path.
fn counts(status: &WorkspaceGitStatus) -> Option<Vec<FileCounts>> {
    let WorkspaceGitStatus::Repository { changes, .. } = status else {
        return None;
    };
    Some(
        changes
            .iter()
            .map(|change| {
                (change.path.clone(), change.insertions, change.deletions)
            })
            .collect(),
    )
}

/// The `(base, insertions, deletions)` summary of a repository status.
fn summary(status: &WorkspaceGitStatus) -> Option<(String, u64, u64)> {
    let WorkspaceGitStatus::Repository {
        base,
        insertions,
        deletions,
        ..
    } = status
    else {
        return None;
    };
    Some((base.clone(), *insertions, *deletions))
}

/// The `(patch, truncated)` of a diff that has a patch.
fn patch(diff: WorkspaceDiff) -> Option<(String, bool)> {
    let WorkspaceDiff::Patch { patch, truncated } = diff else {
        return None;
    };
    Some((patch, truncated))
}

#[tokio::test]
async fn status_counts_lines_against_the_base() {
    let repo = git_repo();
    std::fs::write(repo.path().join("a.rs"), "a\nb\n").expect("edit");
    std::fs::write(repo.path().join("u.rs"), "1\n2\n3").expect("new");
    let root = WorkspaceRoot::try_from(repo.path()).expect("root");

    let status = root.git_status().await;
    assert_eq!(
        summary(&status).expect("repository"),
        ("HEAD".to_owned(), 5, 1)
    );
    assert_eq!(
        counts(&status).expect("repository"),
        [
            ("a.rs".to_owned(), Some(2), Some(1)),
            ("u.rs".to_owned(), Some(3), Some(0)),
        ]
    );
}

#[tokio::test]
async fn status_base_is_the_upstream_merge_base() {
    let origin = git_repo();
    let clone = tempfile::tempdir().expect("clone");
    git(
        clone.path(),
        &["clone", "-q", origin.path().to_str().expect("utf8"), "."],
    );
    std::fs::write(clone.path().join("a.rs"), "committed\n").expect("edit");
    git(clone.path(), &["commit", "-q", "-am", "local"]);
    std::fs::write(clone.path().join("b.rs"), "x\ny\n").expect("new");
    let root = WorkspaceRoot::try_from(clone.path()).expect("root");

    // The local commit is ahead of origin/main, so its line counts too.
    let status = root.git_status().await;
    assert_eq!(
        summary(&status).expect("repository"),
        ("origin/main".to_owned(), 3, 1)
    );
}

#[tokio::test]
async fn status_without_commits_counts_every_file_as_added() {
    let dir = tempfile::tempdir().expect("repository");
    git(dir.path(), &["init", "-q"]);
    std::fs::write(dir.path().join("staged.rs"), "a\nb\n").expect("file");
    git(dir.path(), &["add", "staged.rs"]);
    std::fs::write(dir.path().join("loose.rs"), "c\n").expect("file");
    let root = WorkspaceRoot::try_from(dir.path()).expect("root");

    let status = root.git_status().await;
    assert_eq!(
        summary(&status).expect("repository"),
        ("HEAD".to_owned(), 3, 0)
    );
}

#[tokio::test]
async fn diff_unstaged_scope_ignores_staged_changes() {
    let repo = git_repo();
    std::fs::write(repo.path().join("a.rs"), "staged\n").expect("edit");
    git(repo.path(), &["add", "a.rs"]);
    std::fs::write(repo.path().join("a.rs"), "staged\nloose\n").expect("edit");
    let root = WorkspaceRoot::try_from(repo.path()).expect("root");

    let (unstaged, _) = patch(
        root.git_diff(Some("a.rs"), WorkspaceDiffScope::Unstaged)
            .await
            .expect("diff"),
    )
    .expect("patch");
    assert!(unstaged.contains("+loose") && !unstaged.contains("+staged"));
    let (base, _) = patch(
        root.git_diff(Some("a.rs"), WorkspaceDiffScope::Base)
            .await
            .expect("diff"),
    )
    .expect("patch");
    assert!(base.contains("+loose") && base.contains("+staged"));
}

#[tokio::test]
async fn whole_workspace_patch_includes_untracked_files() {
    let repo = git_repo();
    std::fs::write(repo.path().join("a.rs"), "changed\n").expect("edit");
    std::fs::write(repo.path().join("u.rs"), "fresh\n").expect("new");
    let root = WorkspaceRoot::try_from(repo.path()).expect("root");

    let (patch, truncated) = patch(
        root.git_diff(None, WorkspaceDiffScope::Base)
            .await
            .expect("diff"),
    )
    .expect("patch");
    assert!(!truncated);
    assert!(
        patch.contains("+changed") && patch.contains("+fresh"),
        "{patch}"
    );
}

#[tokio::test]
async fn whole_workspace_patch_stops_at_the_limit() {
    let repo = git_repo();
    let line =
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde\n";
    std::fs::write(repo.path().join("big.log"), line.repeat(48 * 1024))
        .expect("large file");
    std::fs::write(repo.path().join("a.rs"), "changed\n").expect("edit");
    let root = WorkspaceRoot::try_from(repo.path()).expect("root");

    let (patch, truncated) = patch(
        root.git_diff(None, WorkspaceDiffScope::Base)
            .await
            .expect("diff"),
    )
    .expect("patch");
    assert!(truncated);
    assert!(patch.len() <= protocol::WORKSPACE_DIFF_LIMIT);
    assert!(patch.ends_with('\n'));
}

#[tokio::test]
async fn heavy_git_stderr_does_not_stall_the_whole_diff() {
    // With autocrlf on, `git diff` writes one warning per changed file to
    // stderr; well past a pipe's 64 KiB, it must still finish.
    let repo = git_repo();
    for n in 0..1500 {
        std::fs::write(repo.path().join(format!("f{n:04}.txt")), "a\n")
            .expect("file");
    }
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-q", "-m", "many"]);
    git(repo.path(), &["config", "core.autocrlf", "true"]);
    for n in 0..1500 {
        std::fs::write(repo.path().join(format!("f{n:04}.txt")), "a\nb\n")
            .expect("edit");
    }
    let root = WorkspaceRoot::try_from(repo.path()).expect("root");

    let started = std::time::Instant::now();
    let diff = root.git_diff(None, WorkspaceDiffScope::Base).await;
    assert!(diff.is_ok(), "{diff:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(8));
}

#[tokio::test]
async fn many_untracked_files_diff_quickly_and_odd_ones_are_skipped_or_kept() {
    use std::os::unix::ffi::OsStrExt as _;

    let repo = git_repo();
    for n in 0..2000 {
        std::fs::write(repo.path().join(format!("u{n:04}.txt")), "x\n")
            .expect("file");
    }
    // A name that is not UTF-8 is still diffed.
    let odd = std::ffi::OsStr::from_bytes(b"odd-\xff.txt");
    std::fs::write(repo.path().join(odd), "odd\n").expect("odd file");
    let root = WorkspaceRoot::try_from(repo.path()).expect("root");

    let started = std::time::Instant::now();
    let (patch, _) = patch(
        root.git_diff(None, WorkspaceDiffScope::Base)
            .await
            .expect("diff"),
    )
    .expect("patch");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "took {:?}",
        started.elapsed()
    );
    assert!(
        patch.contains("+++ b/u1999.txt"),
        "last untracked file listed"
    );
    assert!(patch.contains("+odd"), "the non-UTF-8 name is diffed");
}

#[tokio::test]
async fn untracked_line_counts_stop_at_the_file_limit() {
    let repo = git_repo();
    let big = "x\n".repeat((protocol::WORKSPACE_FILE_LIMIT as usize) / 2 + 10);
    std::fs::write(repo.path().join("huge.log"), big).expect("big file");
    std::fs::write(repo.path().join("small.txt"), "1\n2\n")
        .expect("small file");
    let root = WorkspaceRoot::try_from(repo.path()).expect("root");

    let counts = counts(&root.git_status().await).expect("repository");
    assert!(
        counts.contains(&("huge.log".to_owned(), None, None)),
        "{counts:?}"
    );
    assert!(counts.contains(&("small.txt".to_owned(), Some(2), Some(0))));
}
