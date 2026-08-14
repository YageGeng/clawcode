//! Pi-compatible skill discovery and explicit invocation.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use protocol::SkillInfo;

/// Skill discovery and invocation failures.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    /// Filesystem access failed.
    #[error("skill I/O failed: {0}")]
    Io(#[from] std::io::Error),

    /// SKILL.md frontmatter was missing or invalid.
    #[error("invalid skill {path}: {reason}")]
    InvalidFrontmatter {
        /// Skill file that failed validation.
        path: PathBuf,

        /// Validation diagnostic.
        reason: String,
    },

    /// Explicit invocation referenced an unknown skill name.
    #[error("skill not found: {0}")]
    NotFound(String),
}

/// Parses protocol skill metadata from one SKILL.md path.
trait SkillInfoFromPath {
    /// Reads and validates required frontmatter fields.
    fn skill_info(&self) -> Result<SkillInfo, SkillError>;
}

impl SkillInfoFromPath for Path {
    /// Parses required name and description fields from SKILL.md frontmatter.
    fn skill_info(&self) -> Result<SkillInfo, SkillError> {
        let content = fs::read_to_string(self)?;
        let mut lines = content.lines();
        if lines.next() != Some("---") {
            return Err(SkillError::InvalidFrontmatter {
                path: self.to_path_buf(),
                reason: "missing opening frontmatter delimiter".to_string(),
            });
        }

        let mut name = None;
        let mut description = None;
        let mut closed = false;
        for line in lines {
            if line == "---" {
                closed = true;
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                match key.trim() {
                    "name" => {
                        name = Some(value.trim().trim_matches('"').to_string())
                    }
                    "description" => {
                        description =
                            Some(value.trim().trim_matches('"').to_string());
                    }
                    _ => {}
                }
            }
        }
        if !closed {
            return Err(SkillError::InvalidFrontmatter {
                path: self.to_path_buf(),
                reason: "missing closing frontmatter delimiter".to_string(),
            });
        }
        let name = name.filter(|value| !value.is_empty()).ok_or_else(|| {
            SkillError::InvalidFrontmatter {
                path: self.to_path_buf(),
                reason: "missing name".to_string(),
            }
        })?;
        let description = description
            .filter(|value| !value.is_empty())
            .ok_or_else(|| SkillError::InvalidFrontmatter {
                path: self.to_path_buf(),
                reason: "missing description".to_string(),
            })?;

        Ok(SkillInfo {
            name,
            description,
            path: self.to_path_buf(),
        })
    }
}

/// Factory interface used by kernel construction to acquire a skill catalog.
pub trait SkillFactory: Send + Sync {
    /// Discovers configured skill roots into one deterministic catalog.
    fn create(&self) -> Result<SkillCatalog, SkillError>;
}

/// Filesystem skill factory with roots ordered from lowest to highest priority.
#[derive(Debug, Clone)]
pub struct FilesystemSkillFactory {
    roots: Vec<PathBuf>,
}

impl FilesystemSkillFactory {
    /// Creates a skill factory from ordered discovery roots.
    #[must_use]
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }
}

impl SkillFactory for FilesystemSkillFactory {
    /// Recursively discovers SKILL.md files, allowing later roots to override names.
    fn create(&self) -> Result<SkillCatalog, SkillError> {
        let mut discovery = SkillDiscovery::default();
        for root in &self.roots {
            if root.exists() {
                discovery.visit(root)?;
            }
        }
        Ok(SkillCatalog {
            skills: discovery.skills,
        })
    }
}

#[derive(Default)]
struct SkillDiscovery {
    skills: BTreeMap<String, SkillInfo>,
}

impl SkillDiscovery {
    /// Traverses one root, stopping recursion below each discovered skill directory.
    fn visit(&mut self, path: &Path) -> Result<(), SkillError> {
        if path.is_file() {
            if path.file_name().is_some_and(|name| name == "SKILL.md") {
                let descriptor = path.skill_info()?;
                self.skills.insert(descriptor.name.clone(), descriptor);
            }
            return Ok(());
        }

        let skill_file = path.join("SKILL.md");
        if skill_file.is_file() {
            let descriptor = skill_file.as_path().skill_info()?;
            self.skills.insert(descriptor.name.clone(), descriptor);
            return Ok(());
        }

        let mut children =
            fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            self.visit(&child.path())?;
        }
        Ok(())
    }
}

/// Deterministic catalog supporting metadata lookup and explicit full-file invocation.
pub struct SkillCatalog {
    skills: BTreeMap<String, SkillInfo>,
}

impl SkillCatalog {
    /// Returns discovered metadata for one skill name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&SkillInfo> {
        self.skills.get(name)
    }

    /// Reads and returns the complete selected SKILL.md file.
    pub fn invoke(&self, name: &str) -> Result<String, SkillError> {
        let descriptor = self
            .skills
            .get(name)
            .ok_or_else(|| SkillError::NotFound(name.to_string()))?;
        fs::read_to_string(&descriptor.path).map_err(SkillError::Io)
    }

    /// Clones descriptors in deterministic name order for read-only clients.
    #[must_use]
    pub fn descriptors(&self) -> Vec<SkillInfo> {
        self.skills.values().cloned().collect()
    }
}
