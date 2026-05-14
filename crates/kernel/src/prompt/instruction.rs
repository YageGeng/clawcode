//! Instruction file loading: AGENTS.md and .agents/*.md.
//!
//! P1: loads AGENTS.md only with caching by working directory.
//! Future: .agents/ directory scanning with content-hash deduplication.

use std::path::{Path, PathBuf};

/// Collected instruction files ready for rendering.
///
/// Holds structured source+content pairs rather than a pre-formatted
/// string, so rendering decisions stay in one place.
#[derive(Clone, Debug)]
pub(crate) struct Instructions {
    /// AGENTS.md found by walking up from the working directory.
    pub agents_md: Option<InstructionFile>,
}

/// A single instruction file with its source path and content.
#[derive(Clone, Debug)]
pub(crate) struct InstructionFile {
    pub source: PathBuf,
    pub content: String,
}

impl Instructions {
    /// Load instruction files for the given working directory.
    ///
    /// Returns `None` when no instruction files are found.
    /// Results are cached by absolute path.
    pub fn load(cwd: &Path) -> Option<Self> {
        if let Some(key) = cwd.is_absolute().then(|| cwd.to_path_buf()) {
            use std::collections::HashMap;
            use std::sync::Mutex;

            static CACHE: std::sync::OnceLock<Mutex<HashMap<PathBuf, Option<Instructions>>>> =
                std::sync::OnceLock::new();
            let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));

            {
                let c = cache.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(cached) = c.get(&key) {
                    return cached.clone();
                }
            }

            let result = Self::load_uncached(cwd);
            cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, result.clone());
            return result;
        }

        Self::load_uncached(cwd)
    }

    /// Render the instruction block for the system prompt.
    pub(crate) fn render(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(ref f) = self.agents_md {
            parts.push(format!(
                "Instructions from: {}\n{}",
                f.source.display(),
                f.content.trim(),
            ));
        }
        parts.join("\n")
    }

    fn load_uncached(cwd: &Path) -> Option<Self> {
        let agents_md = find_agents_md(cwd)?;
        let content = std::fs::read_to_string(&agents_md).ok()?;
        if content.trim().is_empty() {
            return None;
        }
        Some(Self {
            agents_md: Some(InstructionFile {
                source: agents_md,
                content,
            }),
        })
    }
}

/// Walk up from `start` to find the first `AGENTS.md`.
fn find_agents_md(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start)
    };

    loop {
        let candidate = current.join("AGENTS.md");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !current.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_agents_md_in_working_dir() {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("AGENTS.md");
        std::fs::write(&md, "# Test instructions").unwrap();

        let result = Instructions::load(dir.path());
        assert!(result.is_some());
        let ins = result.unwrap();
        assert!(ins.agents_md.is_some());
        assert!(ins.render().contains("# Test instructions"));
    }

    #[test]
    fn finds_agents_md_in_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("AGENTS.md");
        std::fs::write(&md, "# Parent instructions").unwrap();

        let child = dir.path().join("sub").join("deep");
        std::fs::create_dir_all(&child).unwrap();

        let result = Instructions::load(&child);
        assert!(result.is_some());
        assert!(result.unwrap().render().contains("# Parent instructions"));
    }

    #[test]
    fn returns_none_when_no_agents_md() {
        let dir = tempfile::tempdir().unwrap();
        let result = Instructions::load(dir.path());
        assert!(result.is_none());
    }

    #[test]
    fn render_is_empty_when_no_files() {
        let ins = Instructions { agents_md: None };
        assert!(ins.render().is_empty());
    }
}
