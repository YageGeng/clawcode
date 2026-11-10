//! Child environment and shell selection of unified exec.

use std::collections::BTreeMap;
use std::path::PathBuf;

use config::{EnvInherit, ExecEnvPolicy};
use protocol::{ProductIdentity, SessionId, TurnId};
use tools::exec::{ExecEnvironment, ShellKind, UserShell};

/// Builds the child environment from a literal parent.
fn build(
    policy: &ExecEnvPolicy,
    parent: &[(&str, &str)],
) -> BTreeMap<String, String> {
    ExecEnvironment::new(policy).build(
        parent
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string())),
        &SessionId::try_from("session-env").expect("session id"),
        &TurnId::try_from("turn-env").expect("turn id"),
    )
}

/// `core` keeps only the platform's core variables plus the forced ones.
#[test]
fn env_inherit_core_keeps_only_core_vars() {
    let policy = ExecEnvPolicy::builder().inherit(EnvInherit::Core).build();

    let env = build(&policy, &[("PATH", "/bin"), ("FOO", "bar")]);

    assert_eq!(env.get("PATH").map(String::as_str), Some("/bin"));
    assert!(!env.contains_key("FOO"));
}

/// Secret-looking names are removed only after opting into the filter.
#[test]
fn env_default_excludes_apply_only_when_not_ignored() {
    let parent = [
        ("PATH", "/bin"),
        ("GITHUB_TOKEN", "t"),
        ("AWS_SECRET_ACCESS_KEY", "s"),
    ];

    let kept = build(&ExecEnvPolicy::default(), &parent);
    let filtered = build(
        &ExecEnvPolicy::builder()
            .ignore_default_excludes(false)
            .build(),
        &parent,
    );

    assert!(kept.contains_key("GITHUB_TOKEN"));
    assert!(!filtered.contains_key("GITHUB_TOKEN"));
    assert!(!filtered.contains_key("AWS_SECRET_ACCESS_KEY"));
    assert!(filtered.contains_key("PATH"));
}

/// `include_only` runs after `set`, so a set variable it does not match is gone.
#[test]
fn env_include_only_runs_after_set() {
    let policy = ExecEnvPolicy::builder()
        .set(BTreeMap::from([("FOO".to_string(), "1".to_string())]))
        .include_only(vec!["PATH".to_string()])
        .build();

    let env = build(&policy, &[("PATH", "/bin"), ("HOME", "/home/u")]);

    assert!(!env.contains_key("FOO"));
    assert!(!env.contains_key("HOME"));
    assert!(env.contains_key("PATH"));
}

/// Patterns are `*` globs compared without case.
#[test]
fn env_patterns_are_case_insensitive_globs() {
    let policy = ExecEnvPolicy::builder()
        .exclude(vec!["*proxy*".to_string()])
        .build();

    let env = build(&policy, &[("HTTP_PROXY", "x"), ("PATH", "/bin")]);

    assert!(!env.contains_key("HTTP_PROXY"));
    assert!(env.contains_key("PATH"));
}

/// Forced values replace inherited ones and the Session identity is exported.
#[test]
fn forced_env_overrides_parent() {
    let env = build(
        &ExecEnvPolicy::default(),
        &[("PAGER", "less"), ("TERM", "xterm-256color")],
    );

    assert_eq!(env.get("PAGER").map(String::as_str), Some("cat"));
    assert_eq!(env.get("GIT_PAGER").map(String::as_str), Some("cat"));
    assert_eq!(env.get("TERM").map(String::as_str), Some("dumb"));
    assert_eq!(env.get("NO_COLOR").map(String::as_str), Some("1"));
    assert_eq!(env.get("CLAWCODE_CI").map(String::as_str), Some("1"));
    assert_eq!(
        env.get(ProductIdentity::SESSION_ID_ENV).map(String::as_str),
        Some("session-env")
    );
    assert_eq!(
        env.get(ProductIdentity::TURN_ID_ENV).map(String::as_str),
        Some("turn-env")
    );
}

/// Login shells use `-lc`; non-login shells use `-c`.
#[test]
fn shell_argv_uses_login_flag() {
    let shell = UserShell::from(PathBuf::from("/bin/zsh"));

    assert_eq!(shell.kind, ShellKind::Zsh);
    let no_exports = BTreeMap::new();
    assert_eq!(
        shell.argv("echo hi", true, None, &no_exports),
        ["/bin/zsh", "-lc", "echo hi"]
    );
    assert_eq!(
        shell.argv("echo hi", false, None, &no_exports),
        ["/bin/zsh", "-c", "echo hi"]
    );
}

/// `$SHELL` wins; without it bash is preferred, then sh.
#[cfg(unix)]
#[test]
fn shell_detect_prefers_env_shell() {
    let from_env = UserShell::detect(Some("/bin/zsh"));
    let fallback = UserShell::detect(None);

    assert_eq!(from_env.path, PathBuf::from("/bin/zsh"));
    let expected = if std::path::Path::new("/bin/bash").exists() {
        "/bin/bash"
    } else {
        "/bin/sh"
    };
    assert_eq!(fallback.path, PathBuf::from(expected));
}

/// Snapshot overrides keep only the `set` values `include_only` admits.
#[test]
fn overrides_respect_include_only() {
    let policy = ExecEnvPolicy::builder()
        .include_only(vec!["PATH".to_string()])
        .set(BTreeMap::from([
            ("PATH".to_string(), "/opt/bin".to_string()),
            ("EXTRA".to_string(), "x".to_string()),
        ]))
        .build();

    let overrides = ExecEnvironment::new(&policy).overrides(
        &SessionId::try_from("session-env").expect("session id"),
        &TurnId::try_from("turn-env").expect("turn id"),
    );

    assert_eq!(overrides.get("PATH").map(String::as_str), Some("/opt/bin"));
    assert!(!overrides.contains_key("EXTRA"), "{overrides:?}");
    assert_eq!(overrides.get("NO_COLOR").map(String::as_str), Some("1"));
}
