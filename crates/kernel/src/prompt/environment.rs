//! Environment information captured once per turn and injected
//! into the system prompt for every LLM request.

use std::path::PathBuf;

/// Snapshot of the runtime environment injected into each LLM request.
#[derive(Clone, Debug, typed_builder::TypedBuilder)]
pub(crate) struct EnvironmentInfo {
    /// Model identifier (e.g. "deepseek-v4-pro").
    pub model_id: String,
    /// Absolute working directory path.
    pub cwd: PathBuf,
    /// Whether the working directory is inside a git repository.
    pub is_git_repo: bool,
    /// Operating system platform: "darwin" | "linux" | "win32".
    #[builder(default = {
        if cfg!(target_os = "macos") { "darwin" } else { std::env::consts::OS }
    }.to_string())]
    pub platform: String,
    /// Current date in "YYYY-MM-DD" format.
    #[builder(default = chrono::Local::now().format("%Y-%m-%d").to_string())]
    pub date: String,
}

impl EnvironmentInfo {
    /// Capture environment info from the live system.
    ///
    /// Never fails — even if git detection fails, `is_git_repo` defaults
    /// to `false` rather than propagating an error.
    pub fn capture(model_id: String, cwd: PathBuf) -> Self {
        let is_git_repo = detect_git(&cwd);
        Self::builder()
            .model_id(model_id)
            .cwd(cwd)
            .is_git_repo(is_git_repo)
            .build()
    }

    /// Format the environment info as a text block for the system prompt.
    pub(crate) fn render_block(&self) -> String {
        format!(
            "You are powered by the model named {}.\n\
             Here is some useful information about the environment you are running in:\n\
             <env>\n  Working directory: {}\n  Is directory a git repo: {}\n  \
             Platform: {}\n  Today's date: {}\n</env>",
            self.model_id,
            self.cwd.display(),
            if self.is_git_repo { "yes" } else { "no" },
            self.platform,
            self.date,
        )
    }
}

/// Check whether `dir` is inside a git repository.
///
/// Results are cached by absolute path so the `git rev-parse` command
/// runs at most once per working directory during the process lifetime.
fn detect_git(cwd: &std::path::Path) -> bool {
    use std::collections::HashMap;
    use std::sync::Mutex;

    // Only cache absolute paths — relative paths are ambiguous.
    if let Some(key) = cwd.is_absolute().then(|| cwd.to_path_buf()) {
        // Cache git repo status by absolute path to avoid redundant `git rev-parse` calls.
        static CACHE: std::sync::OnceLock<Mutex<HashMap<PathBuf, bool>>> =
            std::sync::OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));

        {
            let c = cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(&is_git) = c.get(&key) {
                return is_git;
            }
        }

        let is_git = run_git_rev_parse(&key);
        cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, is_git);
        return is_git;
    }

    run_git_rev_parse(cwd)
}

fn run_git_rev_parse(cwd: &std::path::Path) -> bool {
    std::process::Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(cwd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_block_contains_all_fields() {
        let info = EnvironmentInfo {
            model_id: "deepseek-v4".to_string(),
            cwd: PathBuf::from("/home/user/project"),
            is_git_repo: true,
            platform: "linux".to_string(),
            date: "2026-05-14".to_string(),
        };
        let block = info.render_block();
        assert!(block.contains("deepseek-v4"));
        assert!(block.contains("/home/user/project"));
        assert!(block.contains("yes"));
        assert!(block.contains("linux"));
        assert!(block.contains("2026-05-14"));
    }

    #[test]
    fn capture_does_not_panic() {
        let info =
            EnvironmentInfo::capture("test-model".to_string(), std::env::current_dir().unwrap());
        assert!(!info.model_id.is_empty());
        assert!(!info.platform.is_empty());
        assert!(!info.date.is_empty());
    }
}
