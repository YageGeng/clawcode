pub(crate) mod command_runner;
pub(crate) mod discovery;
pub(crate) mod dispatcher;
pub(crate) mod output_parser;

use protocol::{HookEventName, HookRunSummary, HookSource};

use crate::events::{
    PostToolUseOutcome, PostToolUseRequest, PreToolUseOutcome,
    PreToolUseRequest,
};

pub use discovery::DiscoveryConfig;

/// Shell command selected from hook configuration.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct ConfiguredHandler {
    /// Lifecycle event that selects this handler.
    pub event_name: HookEventName,
    /// Matcher pattern normalized for the event.
    #[builder(default)]
    pub matcher: Option<String>,
    /// Shell command to execute.
    pub command: String,
    /// Timeout in seconds.
    pub timeout_sec: u64,
    /// Optional status message rendered while running.
    #[builder(default)]
    pub status_message: Option<String>,
    /// Source hook file path.
    pub source_path: std::path::PathBuf,
    /// Source layer.
    pub source: HookSource,
    /// Stable display order.
    pub display_order: i64,
}

impl ConfiguredHandler {
    /// Build the stable run id shown for hook lifecycle events.
    #[must_use]
    pub(crate) fn run_id(&self) -> String {
        format!(
            "{:?}:{}:{}",
            self.event_name,
            self.display_order,
            self.source_path.display()
        )
    }
}

/// Runtime engine for selecting and executing configured hooks.
#[derive(Debug, Clone, Default)]
pub struct HookEngine {
    handlers: Vec<ConfiguredHandler>,
    warnings: Vec<String>,
}

impl HookEngine {
    /// Discover hooks for the configured roots and create a hook engine.
    #[must_use]
    pub fn discover(config: DiscoveryConfig) -> Self {
        let (handlers, warnings) = discovery::discover_handlers(config);

        Self { handlers, warnings }
    }

    /// Build an engine from preconfigured handlers for focused tests.
    #[cfg(test)]
    #[must_use]
    pub fn from_handlers_for_test(handlers: Vec<ConfiguredHandler>) -> Self {
        Self {
            handlers,
            warnings: Vec::new(),
        }
    }

    /// Return discovery warnings.
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Return preview summaries for matching `PreToolUse` hooks.
    #[must_use]
    pub fn preview_pre_tool_use(
        &self,
        request: &PreToolUseRequest,
    ) -> Vec<HookRunSummary> {
        crate::events::pre_tool_use::preview(&self.handlers, request)
    }

    /// Run matching `PreToolUse` hooks and fold their outcomes.
    pub async fn run_pre_tool_use(
        &self,
        request: PreToolUseRequest,
    ) -> PreToolUseOutcome {
        crate::events::pre_tool_use::run(&self.handlers, request).await
    }

    /// Return preview summaries for matching `PostToolUse` hooks.
    #[must_use]
    pub fn preview_post_tool_use(
        &self,
        request: &PostToolUseRequest,
    ) -> Vec<HookRunSummary> {
        crate::events::post_tool_use::preview(&self.handlers, request)
    }

    /// Run matching `PostToolUse` hooks and fold their outcomes.
    pub async fn run_post_tool_use(
        &self,
        request: PostToolUseRequest,
    ) -> PostToolUseOutcome {
        crate::events::post_tool_use::run(&self.handlers, request).await
    }
}
