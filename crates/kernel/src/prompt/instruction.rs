//! Instruction file loading: AGENTS.md and .agents/*.md.
//!
//! P1: loads AGENTS.md only with caching by working directory.
//! P2: resolves `@path` lines to include referenced file contents (one level).
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

            static CACHE: std::sync::OnceLock<
                Mutex<HashMap<PathBuf, Option<Instructions>>>,
            > = std::sync::OnceLock::new();
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
        let raw_content = std::fs::read_to_string(&agents_md).ok()?;
        let content = resolve_at_references(&raw_content, cwd);
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

/// Resolve `@path` lines in instruction content by replacing them with
/// the included file's path and content.
///
/// Only the first level of references is resolved (no recursion).
/// If a referenced file cannot be found or read, the original `@` line
/// is preserved as-is.
fn resolve_at_references(content: &str, cwd: &Path) -> String {
    let mut result = String::with_capacity(content.len());
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(path_str) = trimmed.strip_prefix('@') {
            let path_str = path_str.trim();
            if path_str.is_empty() {
                // Bare `@` or `@  ` — keep as-is
                result.push_str(line);
                result.push('\n');
                continue;
            }
            let path = Path::new(path_str);
            let resolved = if path.is_absolute() {
                path.to_path_buf()
            } else {
                cwd.join(path)
            };
            match std::fs::read_to_string(&resolved) {
                Ok(included) => {
                    result.push_str(&format!(
                        "# Included from: {}\n",
                        resolved.display()
                    ));
                    result.push_str(included.trim_end());
                    result.push('\n');
                }
                Err(_) => {
                    // File not found or cannot be read, preserve original
                    result.push_str(line);
                    result.push('\n');
                }
            }
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
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
    #[test]
    fn resolve_at_reference_replaces_with_included_content() {
        let dir = tempfile::tempdir().unwrap();
        let included_path = dir.path().join("notes.md");
        std::fs::write(&included_path, "# Included notes\nnote content")
            .unwrap();

        let input = format!("before\n@{}\nafter", included_path.display());
        let result = resolve_at_references(&input, dir.path());

        assert!(result.contains("# Included from:"));
        assert!(result.contains(&included_path.display().to_string()));
        assert!(result.contains("# Included notes"));
        assert!(result.contains("note content"));
        assert!(result.contains("before"));
        assert!(result.contains("after"));
    }

    #[test]
    fn resolve_at_reference_uses_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("local.md"), "local content").unwrap();

        let input = "before\n@local.md\nafter";
        let result = resolve_at_references(input, dir.path());

        assert!(result.contains("# Included from:"));
        assert!(result.contains("local.md"));
        assert!(result.contains("local content"));
    }

    #[test]
    fn resolve_at_reference_preserves_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();

        let input = "before\n@nonexistent.md\nafter";
        let result = resolve_at_references(input, dir.path());

        assert!(result.contains("@nonexistent.md"));
        assert!(result.contains("before"));
        assert!(result.contains("after"));
        assert!(!result.contains("# Included from:"));
    }

    #[test]
    fn resolve_at_reference_leaves_non_at_lines_unchanged() {
        let dir = tempfile::tempdir().unwrap();

        let input = "# Heading\nsome text\n  indented";
        let result = resolve_at_references(input, dir.path());

        assert_eq!(result.trim(), input.trim());
    }

    #[test]
    fn resolve_at_reference_handles_bare_at() {
        let dir = tempfile::tempdir().unwrap();

        let input = "before\n@\nafter";
        let result = resolve_at_references(input, dir.path());

        assert!(result.contains("@\n"));
        assert!(result.contains("before"));
        assert!(result.contains("after"));
    }

    #[test]
    fn instructions_load_resolves_at_references() {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("AGENTS.md");
        std::fs::write(&md, "# Test\n\n@extras.md").unwrap();
        std::fs::write(dir.path().join("extras.md"), "## Extra rules\nrule 1")
            .unwrap();

        let result = Instructions::load(dir.path());
        assert!(result.is_some());
        let rendered = result.unwrap().render();

        assert!(rendered.contains("# Test"));
        assert!(rendered.contains("## Extra rules"));
        assert!(rendered.contains("rule 1"));
        assert!(rendered.contains("# Included from:"));
    }
}
