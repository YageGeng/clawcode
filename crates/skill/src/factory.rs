use std::collections::BTreeMap;
use std::path::PathBuf;

use protocol::{
    SkillDiagnostic, SkillDiagnosticCode, SkillDiagnosticSeverity,
    SkillResourceRequest, SkillSelectionRule,
};

use crate::SkillError;
use crate::catalog::SkillCatalog;
use crate::discovery::SkillDiscovery;
use crate::source::{SkillPathResolver, SkillSourcePlan};

/// Factory interface for Session-scoped immutable Skill catalogs.
pub trait SkillFactory: Send + Sync {
    /// Discovers effective Skills after project trust and Extension resources.
    fn create(
        &self,
        request: SkillResourceRequest,
    ) -> Result<SkillCatalog, SkillError>;
}

/// Filesystem Skill factory with immutable user roots and selection policy.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct FilesystemSkillFactory {
    /// Product-specific user configuration directory.
    global_root: PathBuf,
    /// User home used for portable `.agents/skills` discovery.
    user_home: PathBuf,
    /// Explicit Skill files or directories in configured order.
    #[builder(default)]
    configured_paths: Vec<PathBuf>,
    /// Ordered enable and disable rules applied after discovery.
    #[builder(default)]
    rules: Vec<SkillSelectionRule>,
}

impl SkillFactory for FilesystemSkillFactory {
    /// Builds one catalog from configured, project, user, and Extension sources.
    fn create(
        &self,
        request: SkillResourceRequest,
    ) -> Result<SkillCatalog, SkillError> {
        let cwd = std::fs::canonicalize(&request.cwd)?;
        let sources = SkillSourcePlan::builder()
            .cwd(cwd.clone())
            .global_root(self.global_root.clone())
            .user_home(self.user_home.clone())
            .configured_paths(self.configured_paths.clone())
            .project_resources_allowed(request.project_resources_allowed)
            .extension_paths(request.extension_skill_paths)
            .build()
            .into_sources()?;
        let mut discovery = SkillDiscovery::default();
        for source in &sources {
            discovery.visit(source);
        }
        let (mut skills, mut diagnostics) = discovery.into_parts();
        let mut enabled: BTreeMap<_, _> =
            skills.keys().map(|name| (name.clone(), true)).collect();
        for (index, rule) in self.rules.iter().enumerate() {
            match (&rule.path, &rule.name) {
                (Some(path), None) => {
                    let selected_path =
                        SkillPathResolver::new(&cwd, &self.user_home)
                            .resolve(path);
                    let selected_path =
                        std::fs::canonicalize(&selected_path).unwrap_or(selected_path);
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
                _ => diagnostics.push(
                    SkillDiagnostic::builder()
                        .severity(SkillDiagnosticSeverity::Warning)
                        .code(SkillDiagnosticCode::SelectionRuleInvalid)
                        .message(format!(
                            "Skill rule {index} must configure exactly one of path or name"
                        ))
                        .build(),
                ),
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
