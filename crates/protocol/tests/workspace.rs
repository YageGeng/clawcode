//! Workspace tool extensions: method registration and Git porcelain parsing.

use protocol::{AcpExtensionMethod, WorkspaceChangeKind, WorkspaceGitStatus};

#[test]
fn workspace_methods_are_registered_queries() {
    for (method, name) in [
        (
            AcpExtensionMethod::WorkspaceListDir,
            "_clawcode/workspace/list_dir",
        ),
        (
            AcpExtensionMethod::WorkspaceReadFile,
            "_clawcode/workspace/read_file",
        ),
        (
            AcpExtensionMethod::WorkspaceGitStatus,
            "_clawcode/workspace/git_status",
        ),
        (
            AcpExtensionMethod::WorkspaceGitDiff,
            "_clawcode/workspace/git_diff",
        ),
    ] {
        assert_eq!(method.as_str(), name);
        assert!(method.is_query());
        assert_eq!(AcpExtensionMethod::parse(name), Some(method));
    }
}

#[test]
fn porcelain_parses_branch_and_change_kinds() {
    let output = b"# branch.oid 1234\0# branch.head main\0\
1 .M N... 100644 100644 100644 a a src/lib.rs\0\
1 A. N... 000000 100644 100644 0 a new.rs\0\
1 D. N... 100644 000000 000000 a 0 gone.rs\0\
u UU N... 1 2 3 4 a b c both.rs\0\
? notes.txt\0";
    let WorkspaceGitStatus::Repository {
        branch,
        changes,
        truncated,
        ..
    } = WorkspaceGitStatus::from_porcelain_v2(output, 10)
    else {
        panic!("repository");
    };
    assert_eq!(branch.as_deref(), Some("main"));
    assert!(!truncated);
    let kinds: Vec<(&str, char)> = changes
        .iter()
        .map(|change| (change.path.as_str(), change.kind.letter()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("src/lib.rs", 'M'),
            ("new.rs", 'A'),
            ("gone.rs", 'D'),
            ("both.rs", 'U'),
            ("notes.txt", '?')
        ]
    );
}

#[test]
fn porcelain_parses_renames_and_odd_names() {
    // A rename record carries the original path as the next NUL-separated field.
    let output = b"# branch.head (detached)\0\
2 R. N... 100644 100644 100644 a a R100 new name.rs\0old name.rs\0\
? \xe4\xb8\xad\xe6\x96\x87 dir/line\nbreak.txt\0";
    let WorkspaceGitStatus::Repository {
        branch, changes, ..
    } = WorkspaceGitStatus::from_porcelain_v2(output, 10)
    else {
        panic!("repository");
    };
    assert_eq!(branch, None);
    assert_eq!(changes[0].path, "new name.rs");
    assert_eq!(changes[0].original_path.as_deref(), Some("old name.rs"));
    assert_eq!(changes[0].kind, WorkspaceChangeKind::Renamed);
    assert_eq!(changes[1].path, "中文 dir/line\nbreak.txt");
}

#[test]
fn porcelain_stops_at_the_limit() {
    let output = b"? a\0? b\0? c\0";
    let WorkspaceGitStatus::Repository {
        changes, truncated, ..
    } = WorkspaceGitStatus::from_porcelain_v2(output, 2)
    else {
        panic!("repository");
    };
    assert_eq!(changes.len(), 2);
    assert!(truncated);
}

#[test]
fn numstat_fills_counts_and_totals() {
    let porcelain = b"# branch.head main\0\
1 .M N... 100644 100644 100644 a a a.rs\0\
1 .M N... 100644 100644 100644 a a b.png\0\
2 R. N... 100644 100644 100644 a a R100 new.rs\0old.rs\0";
    let mut status = WorkspaceGitStatus::from_porcelain_v2(porcelain, 10);
    status
        .apply_numstat(b"3\t1\ta.rs\0-\t-\tb.png\x002\t0\t\0old.rs\0new.rs\0");
    let WorkspaceGitStatus::Repository {
        changes,
        insertions,
        deletions,
        ..
    } = status
    else {
        panic!("repository");
    };
    let counts: Vec<(&str, Option<u64>, Option<u64>)> = changes
        .iter()
        .map(|change| {
            (change.path.as_str(), change.insertions, change.deletions)
        })
        .collect();
    assert_eq!(
        counts,
        [
            ("a.rs", Some(3), Some(1)),
            ("b.png", None, None),
            ("new.rs", Some(2), Some(0)),
        ]
    );
    assert_eq!((insertions, deletions), (5, 1));
}

#[test]
fn statuses_without_counts_still_decode() {
    let status: WorkspaceGitStatus =
        serde_json::from_value(serde_json::json!({
            "type": "repository",
            "branch": "main",
            "changes": [{ "path": "a.rs", "kind": "modified" }],
            "truncated": false
        }))
        .expect("an M3 answer decodes");
    let WorkspaceGitStatus::Repository { base, changes, .. } = status else {
        panic!("repository");
    };
    assert_eq!(base, "");
    assert_eq!(changes.first().and_then(|change| change.insertions), None);
}
