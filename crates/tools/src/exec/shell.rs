//! Which shell runs an exec command and how it is invoked.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Shell families whose state a snapshot can capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    /// GNU bash.
    Bash,
    /// Z shell.
    Zsh,
    /// POSIX sh or a compatible shell such as dash.
    Sh,
    /// Any other shell, including Windows shells.
    Other,
}

/// The shell binary used for one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserShell {
    /// Executable path.
    pub path: PathBuf,
    /// Family derived from the executable name.
    pub kind: ShellKind,
}

impl From<PathBuf> for UserShell {
    /// Classifies a shell by its file name.
    fn from(path: PathBuf) -> Self {
        let kind = match path.file_stem().and_then(|name| name.to_str()) {
            Some("bash") => ShellKind::Bash,
            Some("zsh") => ShellKind::Zsh,
            Some("sh" | "dash" | "ash") => ShellKind::Sh,
            _ => ShellKind::Other,
        };
        Self { path, kind }
    }
}

impl UserShell {
    /// Uses `$SHELL` when set, otherwise bash, then sh; `cmd.exe` on Windows.
    #[must_use]
    pub fn detect(env_shell: Option<&str>) -> Self {
        if let Some(shell) = env_shell.filter(|shell| !shell.is_empty()) {
            return Self::from(PathBuf::from(shell));
        }
        if cfg!(windows) {
            return Self::from(PathBuf::from("cmd.exe"));
        }
        let bash = Path::new("/bin/bash");
        Self::from(if bash.exists() {
            bash.to_path_buf()
        } else {
            PathBuf::from("/bin/sh")
        })
    }

    /// Whether a snapshot can capture and replay this shell's state.
    #[must_use]
    pub fn supports_snapshot(&self) -> bool {
        !cfg!(windows)
            && matches!(
                self.kind,
                ShellKind::Bash | ShellKind::Zsh | ShellKind::Sh
            )
    }

    /// Builds the argv that runs `cmd`. A login command with a `snapshot`
    /// sources it instead of running the login startup, re-exports `exports`
    /// so the snapshot cannot override them, and evaluates `cmd` in the same
    /// shell so restored functions and aliases apply.
    #[must_use]
    pub fn argv(
        &self,
        cmd: &str,
        login: bool,
        snapshot: Option<&Path>,
        exports: &BTreeMap<String, String>,
    ) -> Vec<String> {
        let program = self.path.to_string_lossy().into_owned();
        if cfg!(windows) && self.kind == ShellKind::Other {
            return vec![program, "/C".to_string(), cmd.to_string()];
        }
        if let Some(snapshot) =
            snapshot.filter(|_snapshot| login && self.supports_snapshot())
        {
            let exports: String = exports
                .iter()
                .filter(|(name, _value)| is_shell_name(name))
                .map(|(name, value)| {
                    format!("export {name}='{}'\n", single_quoted(value))
                })
                .collect();
            let script = format!(
                "if . '{}' >/dev/null 2>&1; then :; fi\n{exports}eval '{}'",
                single_quoted(&snapshot.to_string_lossy()),
                single_quoted(cmd)
            );
            return vec![program, "-c".to_string(), script];
        }
        let flag = if login { "-lc" } else { "-c" };
        vec![program, flag.to_string(), cmd.to_string()]
    }
}

/// Escapes text for the inside of a single-quoted shell word.
pub(super) fn single_quoted(text: &str) -> String {
    text.replace('\'', "'\\''")
}

/// Whether `name` can be assigned with `export name=...`.
fn is_shell_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|rest| rest == '_' || rest.is_ascii_alphanumeric())
}
