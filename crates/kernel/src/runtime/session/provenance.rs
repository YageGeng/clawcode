//! Creation-time facts written into new Session headers (Codex `collect_git_info`).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use protocol::{ProductIdentity, SessionGitInfo, SessionProvenance};

/// Per-command limit, matching Codex `GIT_COMMAND_TIMEOUT`.
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Reads git state of one working directory for a new Session.
pub(in crate::runtime) struct ProvenanceProbe<'a> {
    pub(in crate::runtime) cwd: &'a Path,
}

impl ProvenanceProbe<'_> {
    /// Collects commit, branch, and origin; any failing command leaves its field empty.
    pub(in crate::runtime) async fn collect(&self) -> SessionProvenance {
        let (sha, branch, origin_url) = tokio::join!(
            self.git(&["rev-parse", "HEAD"]),
            self.git(&["rev-parse", "--abbrev-ref", "HEAD"]),
            self.git(&["remote", "get-url", "origin"]),
        );
        let git = SessionGitInfo {
            sha,
            // A detached checkout reports the literal `HEAD`, which names no branch.
            branch: branch.filter(|branch| branch != "HEAD"),
            origin_url: origin_url.as_deref().and_then(sanitized_remote),
        };
        SessionProvenance {
            git: (git != SessionGitInfo::default()).then_some(git),
            app_version: ProductIdentity::VERSION.to_owned(),
        }
    }

    /// Runs one read-only git command and returns its trimmed, non-empty stdout.
    async fn git(&self, args: &[&str]) -> Option<String> {
        let output = tokio::time::timeout(
            GIT_TIMEOUT,
            tokio::process::Command::new("git")
                .args(args)
                .current_dir(self.cwd)
                // Probing must never take the index lock from a concurrent git command.
                .env("GIT_OPTIONAL_LOCKS", "0")
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .ok()?
        .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_owned())
    }
}

/// Removes credentials from a git remote URL, following Codex `SanitizedGitUrl`.
///
/// Remote-helper prefixes (`transport::`) are kept, userinfo before the host is
/// dropped except SSH's conventional `git` user, and a URL containing
/// whitespace is discarded because a helper command line can embed secrets.
fn sanitized_remote(url: &str) -> Option<String> {
    if url.chars().any(char::is_whitespace) {
        return None;
    }
    let mut address = url;
    while let Some((transport, nested)) = address.split_once("::")
        && !transport.is_empty()
        && transport.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '+' | '-' | '.')
        })
    {
        address = nested;
    }
    // `address` is a suffix of `url`, so what precedes it is the helper chain.
    let helper = url.strip_suffix(address).unwrap_or_default();
    // SCP-style remotes (`user@host:path`) have no scheme and are SSH.
    let (scheme, rest) = match address.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, address),
    };
    // Git treats `?` and `#` as authority text on non-HTTP transports.
    let authority_end = if matches!(scheme, Some("http" | "https")) {
        rest.find(['/', '?', '#'])
    } else {
        rest.find('/')
    }
    .unwrap_or(rest.len());
    let (authority, path) = rest.split_at(authority_end);
    let Some((userinfo, host)) = authority.rsplit_once('@') else {
        return Some(url.to_owned());
    };
    let ssh = matches!(scheme, None | Some("ssh"));
    let user = if ssh && userinfo.split(':').next() == Some("git") {
        "git@"
    } else {
        ""
    };
    let scheme = scheme
        .map(|scheme| format!("{scheme}://"))
        .unwrap_or_default();
    Some(format!("{helper}{scheme}{user}{host}{path}"))
}

#[cfg(test)]
mod tests {
    use super::sanitized_remote;

    /// Credentials disappear while the rest of every remote form survives.
    #[test]
    fn sanitized_remote_strips_credentials() {
        let cases = [
            (
                "https://user:token@example.com/o/r.git",
                Some("https://example.com/o/r.git"),
            ),
            (
                "https://token@example.com/o/r.git",
                Some("https://example.com/o/r.git"),
            ),
            (
                "https://example.com/o/r.git?q=a@b",
                Some("https://example.com/o/r.git?q=a@b"),
            ),
            ("git@github.com:o/r.git", Some("git@github.com:o/r.git")),
            ("alice@host:o/r.git", Some("host:o/r.git")),
            (
                "ssh://git:secret@host/o/r.git",
                Some("ssh://git@host/o/r.git"),
            ),
            ("ssh://alice@host/o/r.git", Some("ssh://host/o/r.git")),
            ("file://user@host/srv/r.git", Some("file://host/srv/r.git")),
            (
                "persistent-https::https://u:p@host/r",
                Some("persistent-https::https://host/r"),
            ),
            ("ext::ssh -i key host %S", None),
            ("/srv/repos/r.git", Some("/srv/repos/r.git")),
        ];
        for (url, expected) in cases {
            assert_eq!(sanitized_remote(url).as_deref(), expected, "{url}");
        }
    }
}
