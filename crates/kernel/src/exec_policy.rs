//! Minimal enhanced execpolicy support.

use std::path::{Path, PathBuf};

use protocol::ExecPolicyAmendment;
use tokio::sync::Semaphore;

/// Manages project-level execpolicy rules.
pub struct ExecPolicyManager {
    claw_home: PathBuf,
    update_lock: Semaphore,
}

impl ExecPolicyManager {
    /// Create an execpolicy manager rooted at the given claw home.
    #[must_use]
    pub fn new(claw_home: PathBuf) -> Self {
        Self {
            claw_home,
            update_lock: Semaphore::new(1),
        }
    }

    /// Append an allow-prefix amendment and refresh in-memory policy.
    pub async fn append_amendment_and_update(
        &self,
        amendment: &ExecPolicyAmendment,
    ) -> anyhow::Result<()> {
        let _guard = self.update_lock.acquire().await?;
        if amendment.command.is_empty() {
            anyhow::bail!("prefix rule requires at least one token");
        }

        let policy_path = self.claw_home.join("rules").join("default.rules");
        let line = Self::allow_prefix_rule_line(&amendment.command)?;
        tokio::task::spawn_blocking(move || {
            Self::append_unique_line(policy_path, line)
        })
        .await??;

        Ok(())
    }

    /// Return whether a command is allowed by project-level prefix rules.
    pub async fn allows_command(
        &self,
        command: &[String],
    ) -> anyhow::Result<bool> {
        let policy_path = self.claw_home.join("rules").join("default.rules");
        let contents = match tokio::fs::read_to_string(policy_path).await {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(false);
            }
            Err(error) => return Err(error.into()),
        };

        for line in contents.lines() {
            if let Some(prefix) = Self::parse_allow_prefix_rule(line)?
                && Self::command_starts_with(command, &prefix)
            {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Format a prefix_rule line with JSON-escaped command tokens.
    fn allow_prefix_rule_line(command: &[String]) -> anyhow::Result<String> {
        let tokens = command
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(format!(
            "prefix_rule(pattern=[{}], decision=\"allow\")",
            tokens.join(", ")
        ))
    }

    /// Append a unique line to a rules file, creating parent directories.
    fn append_unique_line(path: PathBuf, line: String) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                String::new()
            }
            Err(error) => return Err(error.into()),
        };

        if contents.lines().any(|existing| existing == line) {
            return Ok(());
        }

        if !contents.is_empty() && !contents.ends_with('\n') {
            contents.push('\n');
        }
        contents.push_str(&line);
        contents.push('\n');
        std::fs::write(path, contents)?;
        Ok(())
    }

    /// Parse a persisted allow-prefix rule line.
    fn parse_allow_prefix_rule(
        line: &str,
    ) -> anyhow::Result<Option<Vec<String>>> {
        let Some(pattern) = line
            .strip_prefix("prefix_rule(pattern=")
            .and_then(|rest| rest.strip_suffix(", decision=\"allow\")"))
        else {
            return Ok(None);
        };

        Ok(Some(serde_json::from_str(pattern)?))
    }

    /// Return whether `command` begins with every token in `prefix`.
    fn command_starts_with(command: &[String], prefix: &[String]) -> bool {
        !prefix.is_empty()
            && command.len() >= prefix.len()
            && command
                .iter()
                .zip(prefix.iter())
                .all(|(actual, expected)| actual == expected)
    }
}

/// Return the project-level execpolicy root for the current working directory.
#[must_use]
pub fn default_exec_policy_home(cwd: &Path) -> PathBuf {
    cwd.join(".clawcode")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Verifies prefix amendments create a default rules file.
    #[tokio::test]
    async fn append_amendment_creates_default_rules_file() {
        let dir = tempdir().expect("tempdir");
        let manager = ExecPolicyManager::new(dir.path().to_path_buf());
        let amendment = ExecPolicyAmendment::new(vec![
            "cargo".to_string(),
            "test".to_string(),
        ]);

        manager
            .append_amendment_and_update(&amendment)
            .await
            .expect("append amendment");

        let contents = std::fs::read_to_string(
            dir.path().join("rules").join("default.rules"),
        )
        .expect("rules file");
        assert_eq!(
            contents,
            "prefix_rule(pattern=[\"cargo\", \"test\"], decision=\"allow\")\n"
        );
    }

    /// Verifies duplicate amendments are not written twice.
    #[tokio::test]
    async fn append_amendment_dedupes_existing_rule() {
        let dir = tempdir().expect("tempdir");
        let manager = ExecPolicyManager::new(dir.path().to_path_buf());
        let amendment = ExecPolicyAmendment::new(vec!["cargo".to_string()]);

        manager
            .append_amendment_and_update(&amendment)
            .await
            .expect("first append");
        manager
            .append_amendment_and_update(&amendment)
            .await
            .expect("second append");

        let contents = std::fs::read_to_string(
            dir.path().join("rules").join("default.rules"),
        )
        .expect("rules file");
        assert_eq!(
            contents
                .lines()
                .filter(|line| line.contains("prefix_rule"))
                .count(),
            1
        );
    }

    /// Verifies prefix rules allow longer commands with the same leading tokens.
    #[tokio::test]
    async fn allowed_prefix_matches_longer_command() {
        let dir = tempdir().expect("tempdir");
        let manager = ExecPolicyManager::new(dir.path().to_path_buf());
        manager
            .append_amendment_and_update(&ExecPolicyAmendment::new(vec![
                "cargo".to_string(),
                "test".to_string(),
            ]))
            .await
            .expect("append amendment");

        assert!(
            manager
                .allows_command(&[
                    "cargo".to_string(),
                    "test".to_string(),
                    "-p".to_string(),
                    "kernel".to_string(),
                ])
                .await
                .expect("evaluate policy")
        );
    }

    /// Verifies the default execpolicy home is scoped to the current project.
    #[test]
    fn default_exec_policy_home_uses_project_clawcode_directory() {
        let dir = tempdir().expect("tempdir");

        assert_eq!(
            default_exec_policy_home(dir.path()),
            dir.path().join(".clawcode")
        );
    }
}
