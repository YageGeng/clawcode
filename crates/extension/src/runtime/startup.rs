use protocol::{
    ProjectTrustDecision, ProjectTrustEvent, ProjectTrustResult,
    ResourcesDiscoverEvent, ResourcesDiscoverResult,
};

use crate::{
    ExtensionContext, ExtensionRuntime, ProjectTrustPoint,
    ResourcesDiscoverPoint,
};

impl ExtensionRuntime {
    /// Returns the first explicit project-trust decision in registration order.
    pub async fn emit_project_trust(
        &self,
        event: &ProjectTrustEvent,
        context: &ExtensionContext,
    ) -> ProjectTrustResult {
        for registered in self.handlers::<ProjectTrustPoint>() {
            let handler_context = context.for_extension(registered.source());
            match registered.handler.handle(event, &handler_context).await {
                Ok(result)
                    if result.trusted != ProjectTrustDecision::Undecided =>
                {
                    return result;
                }
                Ok(_undecided) => {}
                Err(error) => {
                    self.report::<ProjectTrustPoint>(registered, &error).await;
                }
            }
        }
        ProjectTrustResult {
            trusted: ProjectTrustDecision::Undecided,
            remember: false,
        }
    }

    /// Accumulates server-side skill and prompt roots in registration order.
    pub async fn emit_resources_discover(
        &self,
        event: &ResourcesDiscoverEvent,
        context: &ExtensionContext,
    ) -> ResourcesDiscoverResult {
        let mut resources = ResourcesDiscoverResult::default();
        for registered in self.handlers::<ResourcesDiscoverPoint>() {
            let handler_context = context.for_extension(registered.source());
            match registered.handler.handle(event, &handler_context).await {
                Ok(result) => {
                    resources.skill_paths.extend(result.skill_paths);
                    resources.prompt_paths.extend(result.prompt_paths);
                }
                Err(error) => {
                    self.report::<ResourcesDiscoverPoint>(registered, &error)
                        .await;
                }
            }
        }
        resources
    }
}
