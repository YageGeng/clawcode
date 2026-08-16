use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use protocol::{SkillResourceRequest, SkillSelectionRule};

use crate::SkillError;
use crate::catalog::SkillCatalog;
use crate::discovery::{SkillDiscovery, resolve_skill_path};

/// Factory interface for Session-scoped immutable Skill catalogs.
pub trait SkillFactory: Send + Sync {
    /// Discovers effective Skills after project trust and Extension resources.
    fn create(
        &self,
        request: SkillResourceRequest,
    ) -> Result<SkillCatalog, SkillError>;
}

/// Filesystem Skill factory with immutable global root and ordered rules.
#[derive(Debug, Clone)]
pub struct FilesystemSkillFactory {
    global_root: PathBuf,
    rules: Vec<SkillSelectionRule>,
}

impl FilesystemSkillFactory {
    /// Creates a factory using the global config root and ordered selection rules.
    #[must_use]
    pub fn new(global_root: PathBuf, rules: Vec<SkillSelectionRule>) -> Self {
        Self { global_root, rules }
    }
}

impl SkillFactory for FilesystemSkillFactory {
    /// Builds one catalog from global, trusted project, and Extension roots.
    fn create(
        &self,
        request: SkillResourceRequest,
    ) -> Result<SkillCatalog, SkillError> {
        let cwd = fs::canonicalize(&request.cwd)?;
        let mut roots = vec![self.global_root.join("skills")];
        if request.project_resources_allowed {
            roots.push(cwd.join(".pi/skills"));
            roots.push(cwd.join(".agents/skills"));
        }
        roots.extend(request.extension_skill_paths.into_iter().map(|path| {
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        }));

        let mut discovery = SkillDiscovery::default();
        for root in roots {
            if root.exists() {
                discovery.visit(&root, true);
            }
        }
        let (mut skills, mut diagnostics) = discovery.into_parts();
        let mut enabled: BTreeMap<_, _> =
            skills.keys().map(|name| (name.clone(), true)).collect();
        for (index, rule) in self.rules.iter().enumerate() {
            match (&rule.path, &rule.name) {
                (Some(path), None) => {
                    let selected_path = resolve_skill_path(path, &cwd);
                    for (name, skill) in &skills {
                        if skill.path == selected_path {
                            enabled.insert(name.clone(), rule.enabled);
                        }
                    }
                }
                (None, Some(selected_name)) => {
                    if enabled.contains_key(selected_name) {
                        enabled.insert(selected_name.clone(), rule.enabled);
                    }
                }
                _ => diagnostics.push(format!(
                    "Skill rule {index} must configure exactly one of path or name"
                )),
            }
        }
        skills
            .retain(|name, _skill| enabled.get(name).copied().unwrap_or(true));

        Ok(SkillCatalog {
            skills,
            diagnostics,
        })
    }
}
