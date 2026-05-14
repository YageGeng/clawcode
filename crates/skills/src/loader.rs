//! Skill discovery: scan `.agents/skills/` directories for `SKILL.md` files,
//! parse YAML frontmatter, and return deduplicated [`SkillMetadata`] sorted
//! by scope priority (Repo before User).

use std::path::{Path, PathBuf};

use crate::{SkillError, SkillMetadata, SkillScope};

/// Maximum subdirectory depth to search under a skill root.
const MAX_SCAN_DEPTH: usize = 4;

// ---------------------------------------------------------------------------
// SkillRoot
// ---------------------------------------------------------------------------

/// A discovered skill root directory (`<parent>/.agents/skills`).
#[derive(Debug, Clone)]
pub(crate) struct SkillRoot {
    pub path: PathBuf,
    pub scope: SkillScope,
}

// ---------------------------------------------------------------------------
// SkillFrontmatter
// ---------------------------------------------------------------------------

/// YAML frontmatter structure of a `SKILL.md` file.
#[derive(Debug, serde::Deserialize)]
struct SkillFrontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

// ---------------------------------------------------------------------------
// SkillLoader
// ---------------------------------------------------------------------------

/// Discovers and parses skills from `.agents/skills/` directories.
///
/// ## Lifecycle
///
/// ```text
/// SkillLoader::new(cwd)
///   → detects root directories (.agents/skills/) walking up from cwd
///   → assigns scope: inside $HOME → User, otherwise → Repo
///
/// loader.load()
///   → for each root, recursively scans subdirectories for SKILL.md
///   → parses YAML frontmatter into SkillMetadata
///   → deduplicates by name (Repo > User), sorts by (scope, name)
/// ```
///
/// Individual parse failures are collected in [`errors`](SkillLoader::errors)
/// and do not block the remaining skills from loading.
pub(crate) struct SkillLoader {
    roots: Vec<SkillRoot>,
    errors: Vec<SkillError>,
}

impl SkillLoader {
    /// Collect skill roots from two fixed locations:
    ///
    /// 1. `<cwd>/.agents/skills/` — scoped `Repo`
    /// 2. `$HOME/.agents/skills/` — scoped `User`
    ///
    /// When both resolve to the same canonical directory, only the
    /// `Repo` entry is kept (first-seen wins).
    pub fn new(cwd: &Path) -> Self {
        let home = dirs::home_dir();
        let mut roots: Vec<SkillRoot> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        let base = if cwd.is_absolute() {
            cwd.to_path_buf()
        } else {
            match std::env::current_dir() {
                Ok(d) => d.join(cwd),
                Err(_) => {
                    return Self {
                        roots,
                        errors: Vec::new(),
                    };
                }
            }
        };

        // Repo root: <cwd>/.agents/skills/
        Self::push_root(&base, SkillScope::Repo, &mut roots, &mut seen);

        // User root: $HOME/.agents/skills/ (only if distinct)
        if let Some(h) = home
            && h != base
        {
            Self::push_root(&h, SkillScope::User, &mut roots, &mut seen);
        }

        Self {
            roots,
            errors: Vec::new(),
        }
    }

    fn push_root(
        base: &Path,
        scope: SkillScope,
        roots: &mut Vec<SkillRoot>,
        seen: &mut std::collections::HashSet<PathBuf>,
    ) {
        let candidate = base.join(".agents").join("skills");
        if candidate.is_dir() {
            let canonical = candidate
                .canonicalize()
                .unwrap_or_else(|_| candidate.clone());
            if seen.insert(canonical.clone()) {
                roots.push(SkillRoot {
                    path: canonical,
                    scope,
                });
            }
        }
    }

    /// Execute the full discovery pipeline: scan roots → parse → deduplicate → sort.
    pub fn load(&mut self) -> Vec<SkillMetadata> {
        let mut skills: Vec<SkillMetadata> = Vec::new();

        // Take roots out of `self` temporarily so we can still borrow
        // `self.errors` mutably inside the scan loop.
        let roots = std::mem::take(&mut self.roots);

        for root in &roots {
            let found = self.scan_root(root);
            for mut skill in found {
                // Repo-scoped skills from earlier roots (closer to cwd) beat
                // later roots.  Keep the first occurrence.
                if !skills
                    .iter()
                    .any(|s| s.name.eq_ignore_ascii_case(&skill.name))
                {
                    skill.scope = root.scope;
                    skills.push(skill);
                }
            }
        }

        skills.sort_by(|a, b| a.scope.cmp(&b.scope).then_with(|| a.name.cmp(&b.name)));
        skills
    }

    /// Soft errors collected during loading (individual parse failures).
    #[allow(dead_code)]
    pub fn errors(&self) -> &[SkillError] {
        &self.errors
    }

    // -- private helpers ----------------------------------------------------

    /// Recursively scan a single root directory for `SKILL.md` files.
    fn scan_root(&mut self, root: &SkillRoot) -> Vec<SkillMetadata> {
        let mut results = Vec::new();
        let mut queue: Vec<(PathBuf, usize)> = Vec::new();

        // Seed queue with immediate subdirectories of the root.
        if let Ok(entries) = std::fs::read_dir(&root.path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                if Self::is_hidden(&path) {
                    continue;
                }
                queue.push((path, 1));
            }
        }

        while let Some((dir, depth)) = queue.pop() {
            let skill_md = dir.join("SKILL.md");
            if skill_md.is_file() {
                match Self::parse_skill_file(&skill_md) {
                    Ok(meta) => results.push(meta),
                    Err(err) => self.errors.push(err),
                }
            }

            if depth < MAX_SCAN_DEPTH
                && let Ok(entries) = std::fs::read_dir(&dir)
            {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.is_dir() || Self::is_hidden(&path) {
                        continue;
                    }
                    queue.push((path, depth + 1));
                }
            }
        }

        results
    }

    /// Parse a single `SKILL.md` file into [`SkillMetadata`].
    ///
    /// Scope is set to a placeholder (`Repo`); the caller (`load`)
    /// overwrites it with the correct scope from the root walker.
    fn parse_skill_file(path: &Path) -> Result<SkillMetadata, SkillError> {
        let contents = std::fs::read_to_string(path).map_err(|e| SkillError::Read {
            path: path.to_path_buf(),
            source: e,
        })?;

        let frontmatter =
            Self::extract_frontmatter(&contents).ok_or_else(|| SkillError::MissingFrontmatter {
                path: path.to_path_buf(),
            })?;

        let parsed: SkillFrontmatter =
            serde_yaml::from_str(&frontmatter).map_err(|e| SkillError::InvalidYaml {
                path: path.to_path_buf(),
                source: e,
            })?;

        let name = parsed
            .name
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| Self::default_skill_name(path));

        let description = parsed
            .description
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_string();

        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

        Ok(SkillMetadata {
            name: name.to_string(),
            description,
            path: canonical,
            scope: SkillScope::Repo, // placeholder, overwritten by caller
        })
    }

    /// Extract YAML frontmatter between the first pair of `---` lines.
    fn extract_frontmatter(contents: &str) -> Option<String> {
        let mut lines = contents.lines();
        if lines.next()?.trim() != "---" {
            return None;
        }
        let mut fm_lines: Vec<&str> = Vec::new();
        for line in lines {
            if line.trim() == "---" {
                return Some(fm_lines.join("\n"));
            }
            fm_lines.push(line);
        }
        // No closing `---` → malformed frontmatter.
        None
    }

    /// Derive a skill name from the parent directory of the SKILL.md.
    fn default_skill_name(path: &Path) -> &str {
        path.parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed")
    }

    fn is_hidden(path: &Path) -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name.starts_with('.'))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_valid_frontmatter() {
        let contents = "---\nname: test\ndescription: desc\n---\n\n# Body\n";
        let fm = SkillLoader::extract_frontmatter(contents).unwrap();
        let parsed: SkillFrontmatter = serde_yaml::from_str(&fm).unwrap();
        assert_eq!(parsed.name.as_deref(), Some("test"));
        assert_eq!(parsed.description.as_deref(), Some("desc"));
    }

    #[test]
    fn extract_frontmatter_no_closing() {
        let contents = "---\nname: test\n";
        assert!(SkillLoader::extract_frontmatter(contents).is_none());
    }

    #[test]
    fn extract_frontmatter_no_opening() {
        let contents = "name: test\n---\n";
        assert!(SkillLoader::extract_frontmatter(contents).is_none());
    }

    #[test]
    fn default_name_from_parent_dir() {
        let name = SkillLoader::default_skill_name(Path::new("/some/path/my-skill/SKILL.md"));
        assert_eq!(name, "my-skill");
    }

    #[test]
    fn discovers_skills_from_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".agents").join("skills").join("demo");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(
            skills_dir.join("SKILL.md"),
            "---\nname: demo\ndescription: A demo skill\n---\n\n# Demo\n",
        )
        .unwrap();

        let mut loader = SkillLoader {
            roots: vec![SkillRoot {
                path: dir.path().join(".agents").join("skills"),
                scope: SkillScope::Repo,
            }],
            errors: Vec::new(),
        };
        let skills = loader.load();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "demo");
        assert_eq!(skills[0].description, "A demo skill");
        assert!(loader.errors().is_empty());
    }

    #[test]
    fn skips_dot_directories() {
        let dir = tempfile::tempdir().unwrap();
        let hidden_dir = dir
            .path()
            .join(".agents")
            .join("skills")
            .join(".hidden-skill");
        std::fs::create_dir_all(&hidden_dir).unwrap();
        std::fs::write(
            hidden_dir.join("SKILL.md"),
            "---\nname: hidden\ndescription: Should be skipped\n---\n\n# Hidden\n",
        )
        .unwrap();

        let mut loader = SkillLoader {
            roots: vec![SkillRoot {
                path: dir.path().join(".agents").join("skills"),
                scope: SkillScope::Repo,
            }],
            errors: Vec::new(),
        };
        let skills = loader.load();
        assert!(skills.is_empty());
    }

    #[test]
    fn parse_error_collected_in_errors() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".agents").join("skills").join("broken");
        std::fs::create_dir_all(&skills_dir).unwrap();
        // Write a SKILL.md with missing frontmatter.
        std::fs::write(skills_dir.join("SKILL.md"), "Just body, no frontmatter\n").unwrap();

        let mut loader = SkillLoader {
            roots: vec![SkillRoot {
                path: dir.path().join(".agents").join("skills"),
                scope: SkillScope::Repo,
            }],
            errors: Vec::new(),
        };
        let skills = loader.load();
        assert!(skills.is_empty());
        assert_eq!(loader.errors().len(), 1);
    }
}
