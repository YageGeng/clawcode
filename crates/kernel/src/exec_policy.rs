//! Minimal enhanced execpolicy support.

use std::path::{Path, PathBuf};

use protocol::ExecPolicyAmendment;
use tokio::sync::Semaphore;

/// Cached set of parsed prefix rules, invalidated after every amendment write.
struct CachedRules {
    /// Parsed command prefixes (each inner Vec is one prefix_rule).
    prefixes: Vec<Vec<String>>,
}

/// Manages project-level execpolicy rules with in-memory rule caching.
/// Rules are read once and cached until an amendment write invalidates the cache.
pub struct ExecPolicyManager {
    claw_home: PathBuf,
    update_lock: Semaphore,
    cached_rules: tokio::sync::Mutex<Option<CachedRules>>,
}

impl ExecPolicyManager {
    /// Create an execpolicy manager rooted at the given claw home.
    #[must_use]
    pub fn new(claw_home: PathBuf) -> Self {
        Self {
            claw_home,
            update_lock: Semaphore::new(1),
            cached_rules: tokio::sync::Mutex::new(None),
        }
    }

    /// Append an allow-prefix amendment and invalidate the rule cache.
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

        // Invalidate after the file write so future policy checks observe the
        // persisted rule set without depending on watch receiver state.
        *self.cached_rules.lock().await = None;

        Ok(())
    }

    /// Return whether a command is allowed by project-level prefix rules.
    ///
    /// Parsed rules are cached in memory and re-read only when the cache is
    /// invalidated by an amendment write (see [`append_amendment_and_update`]).
    pub async fn allows_command(
        &self,
        command: &[String],
    ) -> anyhow::Result<bool> {
        let mut cache = self.cached_rules.lock().await;

        // Load lazily so a fresh manager observes rules that already exist on
        // disk before the first amendment is written in this process.
        if cache.is_none() {
            let policy_path =
                self.claw_home.join("rules").join("default.rules");
            let contents = tokio::task::spawn_blocking(move || {
                let path = policy_path;
                match std::fs::read_to_string(&path) {
                    Ok(contents) => Ok(contents),
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound =>
                    {
                        Ok(String::new())
                    }
                    Err(error) => Err(error),
                }
            })
            .await
            .map_err(|e| anyhow::anyhow!("join error: {e}"))??;

            let mut prefixes = Vec::new();
            if !contents.is_empty() {
                for line in contents.lines() {
                    if let Some(prefix) = Self::parse_allow_prefix_rule(line)? {
                        prefixes.push(prefix);
                    }
                }
            }
            *cache = Some(CachedRules { prefixes });
        }

        let Some(ref rules) = *cache else {
            return Ok(false);
        };

        Ok(rules
            .prefixes
            .iter()
            .any(|prefix| Self::command_starts_with(command, prefix)))
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

    /// Verifies persisted rules are loaded on the first policy evaluation.
    #[tokio::test]
    async fn existing_rules_file_is_loaded_on_first_allows_command() {
        let dir = tempdir().expect("tempdir");
        let rules_dir = dir.path().join("rules");
        std::fs::create_dir_all(&rules_dir).expect("rules dir");
        std::fs::write(
            rules_dir.join("default.rules"),
            "prefix_rule(pattern=[\"cargo\", \"fmt\"], decision=\"allow\")\n",
        )
        .expect("write rules");
        let manager = ExecPolicyManager::new(dir.path().to_path_buf());

        assert!(
            manager
                .allows_command(&["cargo".to_string(), "fmt".to_string()])
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
