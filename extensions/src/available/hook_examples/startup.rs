//! Project trust and resource discovery Hook examples.

use async_trait::async_trait;
use extension::{
    ExtensionContext, ExtensionError, ExtensionHandler, ExtensionRegistrar,
    ProjectTrustPoint, ResourcesDiscoverPoint,
};
use protocol::{
    ProductIdentity, ProjectTrustDecision, ProjectTrustEvent,
    ProjectTrustResult, ResourcesDiscoverEvent, ResourcesDiscoverResult,
};

struct TrustLocalProject;

#[async_trait]
impl ExtensionHandler<ProjectTrustPoint> for TrustLocalProject {
    /// Trusts an absolute server-side project path without persisting the decision.
    async fn handle(
        &self,
        event: &ProjectTrustEvent,
        _context: &ExtensionContext,
    ) -> Result<ProjectTrustResult, ExtensionError> {
        Ok(ProjectTrustResult {
            trusted: if event.cwd.is_absolute() {
                ProjectTrustDecision::Yes
            } else {
                ProjectTrustDecision::No
            },
            remember: false,
        })
    }
}

struct DiscoverResources;

#[async_trait]
impl ExtensionHandler<ResourcesDiscoverPoint> for DiscoverResources {
    /// Adds product-scoped skill and prompt directories under the current project.
    async fn handle(
        &self,
        event: &ResourcesDiscoverEvent,
        _context: &ExtensionContext,
    ) -> Result<ResourcesDiscoverResult, ExtensionError> {
        let root = event.cwd.join(ProductIdentity::CONFIG_DIR_NAME);
        Ok(ResourcesDiscoverResult {
            skill_paths: vec![root.join("skills")],
            prompt_paths: vec![root.join("prompts")],
        })
    }
}

/// Registers startup trust and resource-discovery examples.
pub(super) fn register(
    registrar: &mut ExtensionRegistrar,
) -> Result<(), ExtensionError> {
    registrar.on::<ProjectTrustPoint, _>(TrustLocalProject)?;
    registrar.on::<ResourcesDiscoverPoint, _>(DiscoverResources)
}
