//! Skill discovery, loading, and catalog rendering.
//!
//! ## Architecture
//!
//! ```text
//! lib.rs           SkillMetadata, SkillScope, SkillRegistry (public API)
//! loader.rs        Discovery: scan .agents/skills/**/SKILL.md
//! rules.rs         Per-skill enable/disable via name/path selectors
//! render.rs        Catalog rendering for system prompt injection
//! injection.rs     $skill-name mention detection + body loading
//! ```
//!
//! ## Lifecycle
//!
//! 1. `SkillRegistry::discover(cwd, config)` — scan `<cwd>/.agents/skills/`
//!    and `$HOME/.agents/skills/`, parse SKILL.md frontmatter, deduplicate
//!    by name (Repo > User).
//! 2. `SkillRegistry::render_catalog()` — produce the `skills_xml` block
//!    injected into every turn's system prompt.
//! 3. `SkillRegistry::resolve_mentions(text)` — at turn start, scan user
//!    input for `$skill-name` tokens and return matching skills.

mod injection;
mod loader;
mod render;
pub(crate) mod rules;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

pub use config::skills::SkillsConfig;

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// The source scope of a skill, determining priority and display name.
///
/// Lower discriminant value = higher priority during deduplication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkillScope {
    /// Repository-level — from `.agents/skills/` within the current project.
    Repo = 0,
    /// User-level — from `$HOME/.agents/skills/`.
    User = 1,
}

/// Metadata parsed from a `SKILL.md` file's YAML frontmatter.
#[derive(Debug, Clone)]
pub struct SkillMetadata {
    /// Skill name from frontmatter `name`, falling back to parent directory name.
    /// Used as the key for `$skill-name` mention matching.
    pub name: String,
    /// Skill description from frontmatter `description`.
    pub description: String,
    /// Canonical absolute path to the `SKILL.md` file.
    pub path: PathBuf,
    /// Source scope (Repo or User).
    pub scope: SkillScope,
}

/// Errors that can occur during skill loading.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("missing frontmatter in {path}")]
    MissingFrontmatter { path: PathBuf },
    #[error("invalid YAML frontmatter in {path}: {source}")]
    InvalidYaml {
        path: PathBuf,
        source: serde_yaml::Error,
    },
}

// ---------------------------------------------------------------------------
// SkillRegistry
// ---------------------------------------------------------------------------

/// Registry of all discovered skills in the current session.
///
/// Created once per turn via [`SkillRegistry::discover`] and treated as
/// immutable for the remainder of the turn.  Results are cached by the
/// canonical working-directory path so that repeated calls within the
/// same directory avoid filesystem re-scanning.
#[derive(Debug, Clone, Default)]
pub struct SkillRegistry {
    /// All discovered skills (enabled + disabled).
    skills: Vec<SkillMetadata>,
    /// Canonical paths of disabled skills (resolved from config rules).
    disabled_paths: HashSet<PathBuf>,
    /// Lookup index: skill name → position in `skills`.
    by_name: HashMap<String, usize>,
}

impl SkillRegistry {
    // -- public API --------------------------------------------------------

    /// Discover skills from `.agents/skills/` directories by walking upward
    /// from `cwd`, then apply the enable/disable rules from `config`.
    ///
    /// Results are cached by canonical cwd path via a static `OnceLock` map
    /// to avoid repeated filesystem scans within the same working directory.
    pub fn discover(cwd: &std::path::Path, config: &SkillsConfig) -> Arc<Self> {
        use std::sync::OnceLock;

        static CACHE: OnceLock<std::sync::Mutex<HashMap<PathBuf, Arc<SkillRegistry>>>> =
            OnceLock::new();
        let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));

        let key = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());

        {
            let c = cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cached) = c.get(&key) {
                return Arc::clone(cached);
            }
        }

        let registry = Arc::new(Self::discover_uncached(cwd, config));
        cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, Arc::clone(&registry));
        registry
    }

    /// Number of discovered skills (both enabled and disabled).
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// Whether no skills were discovered.
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Whether a skill is enabled (not blocked by config rules).
    pub fn is_enabled(&self, name: &str) -> bool {
        self.by_name
            .get(name)
            .and_then(|&idx| self.skills.get(idx))
            .is_some_and(|s| !self.disabled_paths.contains(&s.path))
    }

    /// Iterate over all enabled skills (sorted: Repo first, then by name).
    pub fn enabled_skills(&self) -> Vec<&SkillMetadata> {
        self.skills
            .iter()
            .filter(|s| !self.disabled_paths.contains(&s.path))
            .collect()
    }

    /// Render the skill catalog as system prompt text.
    /// Returns `None` when there are no enabled skills.
    pub fn render_catalog(&self) -> Option<String> {
        let enabled = self.enabled_skills();
        render::render_catalog(&enabled)
    }

    /// Scan `text` for `$skill-name` tokens and resolve them to enabled
    /// [`SkillMetadata`].  Matches are case-insensitive and require exact,
    /// unambiguous name resolution.
    pub fn resolve_mentions(&self, text: &str) -> Vec<&SkillMetadata> {
        injection::MentionMatcher::resolve(self, text)
    }

    /// Load the full body of `SKILL.md` for a skill by name.
    /// Returns `None` if the name is unknown or the file cannot be read.
    pub fn load_body(&self, name: &str) -> Option<String> {
        let idx = self.by_name.get(name)?;
        let skill = self.skills.get(*idx)?;
        std::fs::read_to_string(&skill.path).ok()
    }

    // -- internal ----------------------------------------------------------

    fn discover_uncached(cwd: &std::path::Path, config: &SkillsConfig) -> Self {
        let mut loader = loader::SkillLoader::new(cwd);
        let skills = loader.load();
        let disabled_paths = rules::ConfigRules::new(config).resolve(&skills);

        // Build name → index lookup.  Skills are already deduplicated and
        // sorted by `SkillLoader::load`.
        let by_name: HashMap<String, usize> = skills
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i))
            .collect();

        Self {
            skills,
            disabled_paths,
            by_name,
        }
    }
}
