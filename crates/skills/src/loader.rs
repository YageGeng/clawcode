use std::collections::{HashSet, VecDeque};
use std::path::Path;

use snafu::ResultExt;
use tokio::fs;

use crate::{
    Result,
    error::{IoSnafu, ParseSnafu},
    model::{SkillConfig, SkillLoadError, SkillLoadOutcome, SkillMetadata},
};

const SKILL_FILE_NAME: &str = "SKILL.md";
const REPO_SKILLS_DIR: &str = ".agents/skills";
const MAX_SCAN_DEPTH: usize = 6;

/// Loads skills from explicit roots plus optional repo-local skill roots.
#[derive(Debug, Clone)]
pub struct SkillsManager {
    config: SkillConfig,
}

impl SkillsManager {
    /// Builds a manager around one immutable skill discovery config.
    pub fn new(config: SkillConfig) -> Self {
        Self { config }
    }

    /// Scans all configured roots and returns both parsed skills and non-fatal errors.
    pub async fn load(&self) -> SkillLoadOutcome {
        if !self.config.enabled {
            return SkillLoadOutcome::default();
        }

        let mut outcome = SkillLoadOutcome::default();
        let mut roots = self.config.roots.clone();
        if let Some(cwd) = &self.config.cwd {
            roots.push(cwd.join(REPO_SKILLS_DIR));
        }

        for root in roots {
            scan_root(&root, &mut outcome).await;
        }

        outcome
    }
}

/// Iteratively scans one root to avoid recursive async futures.
async fn scan_root(root: &Path, outcome: &mut SkillLoadOutcome) {
    if fs::metadata(root)
        .await
        .map(|metadata| !metadata.is_dir())
        .unwrap_or(true)
    {
        return;
    }

    let mut visited = HashSet::new();
    let mut queue = VecDeque::from([(root.to_path_buf(), 0usize)]);

    while let Some((dir, depth)) = queue.pop_front() {
        if depth > MAX_SCAN_DEPTH || !visited.insert(dir.clone()) {
            continue;
        }

        let mut entries = match fs::read_dir(&dir).await {
            Ok(entries) => entries,
            Err(error) => {
                outcome.errors.push(SkillLoadError {
                    path: dir,
                    message: format!("failed to read skills directory: {error}"),
                });
                continue;
            }
        };

        while let Ok(Some(entry)) = entries.next_entry().await {
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            if file_name.starts_with('.') {
                continue;
            }

            let path = entry.path();
            let metadata = match entry.metadata().await {
                Ok(metadata) => metadata,
                Err(error) => {
                    outcome.errors.push(SkillLoadError {
                        path,
                        message: format!("failed to stat skills path: {error}"),
                    });
                    continue;
                }
            };

            if metadata.is_dir() {
                queue.push_back((path, depth + 1));
            } else if metadata.is_file() && file_name == SKILL_FILE_NAME {
                match parse_skill_file(&path).await {
                    Ok(skill) => outcome.skills.push(skill),
                    Err(error) => outcome.errors.push(SkillLoadError {
                        path,
                        message: error.to_string(),
                    }),
                }
            }
        }
    }
}

/// Reads and parses the required YAML frontmatter fields from one skill file.
async fn parse_skill_file(path: &Path) -> Result<SkillMetadata> {
    let path = path.to_path_buf();
    let contents = fs::read_to_string(&path).await.context(IoSnafu {
        stage: "skills-read-skill-file".to_string(),
        path: path.clone(),
    })?;
    let frontmatter = extract_frontmatter(&path, &contents)?;
    let name = extract_frontmatter_field(&path, &frontmatter, "name")?;
    let description = extract_frontmatter_field(&path, &frontmatter, "description")?;
    let disable_model_invocation =
        extract_optional_bool_field(&path, &frontmatter, "disableModelInvocation")?;

    Ok(SkillMetadata {
        name,
        description,
        path,
        disable_model_invocation,
    })
}

/// Extracts the first YAML frontmatter block delimited by `---` markers.
fn extract_frontmatter(path: &Path, contents: &str) -> Result<String> {
    let mut lines = contents.lines();
    if !matches!(lines.next(), Some(line) if line.trim() == "---") {
        return ParseSnafu {
            message: "missing YAML frontmatter delimited by ---".to_string(),
            stage: "skills-parse-frontmatter-open".to_string(),
            path: path.to_path_buf(),
        }
        .fail();
    }

    let mut frontmatter = Vec::new();
    for line in lines {
        if line.trim() == "---" {
            if frontmatter.is_empty() {
                return ParseSnafu {
                    message: "missing YAML frontmatter fields".to_string(),
                    stage: "skills-parse-frontmatter-fields".to_string(),
                    path: path.to_path_buf(),
                }
                .fail();
            }
            return Ok(frontmatter.join("\n"));
        }
        frontmatter.push(line);
    }

    ParseSnafu {
        message: "missing YAML frontmatter closing delimiter".to_string(),
        stage: "skills-parse-frontmatter-close".to_string(),
        path: path.to_path_buf(),
    }
    .fail()
}

/// Parses a simple `key: value` frontmatter field and normalizes whitespace.
fn extract_frontmatter_field(path: &Path, frontmatter: &str, field: &str) -> Result<String> {
    for line in frontmatter.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim() == field {
            let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
            if value.is_empty() {
                return ParseSnafu {
                    message: format!("missing field `{field}`"),
                    stage: "skills-parse-frontmatter-field".to_string(),
                    path: path.to_path_buf(),
                }
                .fail();
            }
            return Ok(value);
        }
    }

    ParseSnafu {
        message: format!("missing field `{field}`"),
        stage: "skills-parse-frontmatter-field".to_string(),
        path: path.to_path_buf(),
    }
    .fail()
}

/// Parses an optional boolean frontmatter field, defaulting to false when absent.
fn extract_optional_bool_field(path: &Path, frontmatter: &str, field: &str) -> Result<bool> {
    for line in frontmatter.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim() == field {
            return match value.trim() {
                "true" => Ok(true),
                "false" => Ok(false),
                other => ParseSnafu {
                    message: format!("invalid boolean for `{field}`: {other}"),
                    stage: "skills-parse-frontmatter-bool".to_string(),
                    path: path.to_path_buf(),
                }
                .fail(),
            };
        }
    }

    Ok(false)
}
