use protocol::{
    WORKSPACE_DIFF_LIMIT, WorkspaceChangeKind, WorkspaceChanges, WorkspaceDiff,
};

/// Joins NUL-terminated records the way `git diff -z` writes them.
fn records(parts: &[&str]) -> Vec<u8> {
    parts
        .iter()
        .flat_map(|part| part.bytes().chain(std::iter::once(0)))
        .collect()
}

/// Raw records give each file's status; numstat records give its counts.
#[test]
fn raw_numstat_lists_modified_added_and_deleted() {
    let output = records(&[
        ":100644 100644 a b M",
        "src/a.rs",
        ":000000 100644 0 c A",
        "new.rs",
        ":100644 000000 d 0 D",
        "old.rs",
        "3\t1\tsrc/a.rs",
        "5\t0\tnew.rs",
        "0\t7\told.rs",
    ]);
    let changes = WorkspaceChanges::from_raw_numstat(&output, 10);
    let summary: Vec<_> = changes
        .changes
        .iter()
        .map(|change| {
            (
                change.path.as_str(),
                change.kind,
                change.insertions,
                change.deletions,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("src/a.rs", WorkspaceChangeKind::Modified, Some(3), Some(1)),
            ("new.rs", WorkspaceChangeKind::Added, Some(5), Some(0)),
            ("old.rs", WorkspaceChangeKind::Deleted, Some(0), Some(7)),
        ]
    );
    assert_eq!((changes.insertions, changes.deletions), (8, 8));
    assert!(!changes.truncated);
}

/// A rename names two paths in both sections but is one change.
#[test]
fn raw_numstat_keeps_a_rename_as_one_change() {
    let output = records(&[
        ":100644 100644 a b R087",
        "old/x.rs",
        "new/x.rs",
        "2\t2\t",
        "old/x.rs",
        "new/x.rs",
    ]);
    let changes = WorkspaceChanges::from_raw_numstat(&output, 10);
    assert_eq!(changes.changes.len(), 1);
    let change = &changes.changes[0];
    assert_eq!(change.path, "new/x.rs");
    assert_eq!(change.original_path.as_deref(), Some("old/x.rs"));
    assert_eq!(change.kind, WorkspaceChangeKind::Renamed);
    assert_eq!((change.insertions, change.deletions), (Some(2), Some(2)));
}

/// Binary files have no line counts and add nothing to the totals.
#[test]
fn raw_numstat_marks_binary_files_without_counts() {
    let output = records(&[
        ":100644 100644 a b M",
        "img.png",
        ":100644 100644 c d M",
        "a.rs",
        "-\t-\timg.png",
        "1\t1\ta.rs",
    ]);
    let changes = WorkspaceChanges::from_raw_numstat(&output, 10);
    let image = &changes.changes[0];
    assert_eq!((image.insertions, image.deletions), (None, None));
    assert_eq!((changes.insertions, changes.deletions), (1, 1));
}

/// Files past the limit are left out and the list says so.
#[test]
fn raw_numstat_stops_at_the_limit() {
    let output = records(&[
        ":100644 100644 a b M",
        "a",
        ":100644 100644 a b M",
        "b",
        ":100644 100644 a b M",
        "c",
        "1\t0\ta",
        "1\t0\tb",
        "1\t0\tc",
    ]);
    let changes = WorkspaceChanges::from_raw_numstat(&output, 2);
    assert_eq!(changes.changes.len(), 2);
    assert!(changes.truncated);
    assert_eq!(changes.insertions, 2);
}

/// One file's raw `git diff` output becomes no change, a binary notice, or a
/// patch cut at the last whole line within the limit.
#[test]
fn from_git_output_classifies_binary_and_cuts_at_the_limit() {
    assert_eq!(
        WorkspaceDiff::from_git_output(Vec::new(), false),
        WorkspaceDiff::Unchanged
    );
    let binary =
        b"diff --git a/x b/x\nBinary files a/x and b/x differ\n".to_vec();
    assert_eq!(
        WorkspaceDiff::from_git_output(binary, false),
        WorkspaceDiff::Binary
    );
    let line = "+0123456789abcdef\n";
    let long = line.repeat(WORKSPACE_DIFF_LIMIT / line.len() + 1);
    assert!(long.len() >= WORKSPACE_DIFF_LIMIT + 10);
    let WorkspaceDiff::Patch { patch, truncated } =
        WorkspaceDiff::from_git_output(long.into_bytes(), false)
    else {
        panic!("a long text diff is a patch");
    };
    assert!(truncated);
    assert!(patch.ends_with('\n'));
    assert!(patch.len() <= WORKSPACE_DIFF_LIMIT);
}

/// An unmerged file comes as a `U` record and an `M` record with a numstat
/// line each; it is one change, still a conflict, with both counts summed.
#[test]
fn raw_numstat_merges_an_unmerged_path() {
    let output = records(&[
        ":000000 100644 0000000 0000000 U",
        "f",
        ":100644 100644 f2ad6c7 0000000 M",
        "f",
        "0\t0\tf",
        "4\t0\tf",
    ]);
    let changes = WorkspaceChanges::from_raw_numstat(&output, 10);
    assert_eq!(changes.changes.len(), 1);
    let change = &changes.changes[0];
    assert_eq!(change.kind, WorkspaceChangeKind::Conflicted);
    assert_eq!((change.insertions, change.deletions), (Some(4), Some(0)));
    assert_eq!((changes.insertions, changes.deletions), (4, 0));
}
