use std::fs;
use std::path::PathBuf;

use protocol::{PromptPolicy, PromptResourceRequest, PromptSourceScope};

use crate::instruction::InstructionDiscovery;
use crate::source::PromptSourceDiscovery;
use crate::{
    PromptError, PromptSession, PromptTemplateCatalog, PromptTemplateRoot,
    PromptTemplateRootRequirement,
};

/// Factory boundary for creating immutable Session Prompt snapshots.
pub trait PromptFactory: Send + Sync {
    /// Creates resources for one Session after trust and Extension discovery.
    fn create(
        &self,
        request: PromptResourceRequest,
    ) -> Result<PromptSession, PromptError>;
}

/// Filesystem-backed PromptFactory with immutable process configuration.
pub struct FilesystemPromptFactory {
    global_root: PathBuf,
    policy: PromptPolicy,
}

impl FilesystemPromptFactory {
    /// Creates a factory rooted at the user's global configuration directory.
    #[must_use]
    pub fn new(global_root: PathBuf, policy: PromptPolicy) -> Self {
        Self {
            global_root,
            policy,
        }
    }
}

impl PromptFactory for FilesystemPromptFactory {
    /// Resolves all Prompt sources once in deterministic priority order.
    fn create(
        &self,
        request: PromptResourceRequest,
    ) -> Result<PromptSession, PromptError> {
        let cwd = fs::canonicalize(&request.cwd).map_err(|error| {
            PromptError::ConfiguredResource {
                path: request.cwd.clone(),
                reason: error.to_string(),
            }
        })?;
        let discovered_sources = PromptSourceDiscovery::new(
            self.global_root.clone(),
            cwd.clone(),
            request.project_resources_allowed,
        )
        .discover(&self.policy)?;
        let (instructions, instruction_diagnostics) =
            InstructionDiscovery::new(
                self.global_root.clone(),
                cwd.clone(),
                request.project_resources_allowed
                    && self.policy.load_project_instructions,
            )
            .discover()?;

        let mut template_roots = Vec::new();
        if self.policy.load_templates {
            template_roots.push(PromptTemplateRoot {
                path: self.global_root.join("prompts"),
                scope: PromptSourceScope::User,
                requirement: PromptTemplateRootRequirement::Discovered,
            });
            if request.project_resources_allowed {
                template_roots.push(PromptTemplateRoot {
                    path: cwd.join(".pi/prompts"),
                    scope: PromptSourceScope::Project,
                    requirement: PromptTemplateRootRequirement::Discovered,
                });
            }
        }
        template_roots.extend(self.policy.template_paths.iter().map(|path| {
            PromptTemplateRoot {
                path: if path.is_absolute() {
                    path.clone()
                } else {
                    cwd.join(path)
                },
                scope: PromptSourceScope::Config,
                requirement: PromptTemplateRootRequirement::Required,
            }
        }));
        template_roots.extend(request.extension_prompt_paths.into_iter().map(
            |path| PromptTemplateRoot {
                path: if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                },
                scope: PromptSourceScope::Extension,
                requirement: PromptTemplateRootRequirement::Discovered,
            },
        ));
        let templates = PromptTemplateCatalog::discover(&template_roots)?;

        let mut diagnostics = discovered_sources.diagnostics;
        diagnostics.extend(instruction_diagnostics);
        diagnostics.extend_from_slice(templates.diagnostics());

        Ok(PromptSession::builder()
            .cwd(cwd)
            .custom_prompt(discovered_sources.custom_prompt)
            .append_system_prompt(discovered_sources.append_system_prompt)
            .instructions(instructions)
            .templates(templates)
            .diagnostics(diagnostics)
            .build())
    }
}
