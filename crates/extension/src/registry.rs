use std::collections::BTreeMap;
use std::sync::Arc;

use protocol::{
    ExtensionCommandDefinition, ExtensionFlagDefinition, ExtensionId,
};

use crate::{
    AfterProviderResponsePoint, AgentEndPoint, AgentSettledPoint,
    AgentStartPoint, BeforeAgentStartPoint, BeforeProviderHeadersPoint,
    BeforeProviderRequestPoint, ContextPoint, ExtensionCommandHandler,
    ExtensionHandler, ExtensionPoint, InputPoint, MessageEndPoint,
    MessageStartPoint, MessageUpdatePoint, ModelSelectPoint, ProjectTrustPoint,
    ResourcesDiscoverPoint, SessionBeforeCompactPoint, SessionBeforeForkPoint,
    SessionBeforeSwitchPoint, SessionBeforeTreePoint, SessionCompactPoint,
    SessionInfoChangedPoint, SessionShutdownPoint, SessionStartPoint,
    SessionTreePoint, ThinkingLevelSelectPoint, ToolCallPoint,
    ToolExecutionEndPoint, ToolExecutionStartPoint, ToolExecutionUpdatePoint,
    ToolResultPoint, TurnEndPoint, TurnStartPoint, UserBashPoint,
};

/// One typed handler paired with the extension that registered it.
pub struct RegisteredHandler<P: ExtensionPoint> {
    source: ExtensionId,
    pub(crate) handler: Arc<dyn ExtensionHandler<P>>,
}

impl<P: ExtensionPoint> Clone for RegisteredHandler<P> {
    /// Clones only the shared handler handle and its immutable source identity.
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            handler: Arc::clone(&self.handler),
        }
    }
}

impl<P: ExtensionPoint> RegisteredHandler<P> {
    /// Returns the extension that registered this handler.
    #[must_use]
    pub fn source(&self) -> &ExtensionId {
        &self.source
    }
}

/// Associates a point marker with its strongly typed registry vector.
pub trait RegisterPoint: ExtensionPoint + Sized {
    /// Returns immutable handlers registered for this point.
    fn handlers(registry: &HandlerRegistry) -> &[RegisteredHandler<Self>];

    /// Returns mutable handlers registered for this point during construction.
    fn handlers_mut(
        registry: &mut HandlerRegistry,
    ) -> &mut Vec<RegisteredHandler<Self>>;
}

macro_rules! define_point_handlers {
    ($( $field:ident : $point:ty ),+ $(,)?) => {
        #[derive(Default, Clone, typed_builder::TypedBuilder)]
        pub struct PointHandlers {
            $( #[builder(default)] $field: Vec<RegisteredHandler<$point>>, )+
        }

        $(
            impl RegisterPoint for $point {
                fn handlers(
                    registry: &HandlerRegistry,
                ) -> &[RegisteredHandler<Self>] {
                    &registry.points.$field
                }

                fn handlers_mut(
                    registry: &mut HandlerRegistry,
                ) -> &mut Vec<RegisteredHandler<Self>> {
                    &mut registry.points.$field
                }
            }
        )+
    };
}

define_point_handlers!(
    project_trust: ProjectTrustPoint,
    resources_discover: ResourcesDiscoverPoint,
    session_start: SessionStartPoint,
    session_info_changed: SessionInfoChangedPoint,
    session_before_switch: SessionBeforeSwitchPoint,
    session_before_fork: SessionBeforeForkPoint,
    session_before_compact: SessionBeforeCompactPoint,
    session_compact: SessionCompactPoint,
    session_before_tree: SessionBeforeTreePoint,
    session_tree: SessionTreePoint,
    session_shutdown: SessionShutdownPoint,
    input: InputPoint,
    before_agent_start: BeforeAgentStartPoint,
    agent_start: AgentStartPoint,
    agent_end: AgentEndPoint,
    agent_settled: AgentSettledPoint,
    turn_start: TurnStartPoint,
    turn_end: TurnEndPoint,
    message_start: MessageStartPoint,
    message_update: MessageUpdatePoint,
    message_end: MessageEndPoint,
    context: ContextPoint,
    before_provider_request: BeforeProviderRequestPoint,
    before_provider_headers: BeforeProviderHeadersPoint,
    after_provider_response: AfterProviderResponsePoint,
    model_select: ModelSelectPoint,
    thinking_level_select: ThinkingLevelSelectPoint,
    tool_execution_start: ToolExecutionStartPoint,
    tool_execution_update: ToolExecutionUpdatePoint,
    tool_execution_end: ToolExecutionEndPoint,
    tool_call: ToolCallPoint,
    tool_result: ToolResultPoint,
    user_bash: UserBashPoint,
);

/// One command handler paired with its source and public definition.
pub struct RegisteredCommand {
    /// Extension that owns the command.
    pub extension_id: ExtensionId,
    /// Public command metadata.
    pub definition: ExtensionCommandDefinition,
    /// Runtime command implementation.
    pub handler: Arc<dyn ExtensionCommandHandler>,
}

impl Clone for RegisteredCommand {
    /// Clones immutable command metadata and its shared handler handle.
    fn clone(&self) -> Self {
        Self {
            extension_id: self.extension_id.clone(),
            definition: self.definition.clone(),
            handler: Arc::clone(&self.handler),
        }
    }
}

impl RegisteredCommand {
    /// Rebinds dynamic command ownership to the extension invoking the host action.
    #[must_use]
    pub fn bind_owner(mut self, extension_id: &ExtensionId) -> Self {
        self.extension_id = extension_id.clone();
        self
    }
}

/// Immutable per-session extension registrations.
#[derive(Default, Clone, typed_builder::TypedBuilder)]
#[builder(builder_method(vis = ""), builder_type(vis = ""))]
pub struct HandlerRegistry {
    #[builder(default)]
    points: PointHandlers,
    #[builder(default)]
    pub(crate) descriptors:
        BTreeMap<ExtensionId, protocol::ExtensionDescriptor>,
    #[builder(default)]
    pub(crate) commands: BTreeMap<String, RegisteredCommand>,
    #[builder(default)]
    pub(crate) flags: BTreeMap<String, ExtensionFlagDefinition>,
    #[builder(default)]
    pub(crate) tools: Vec<(ExtensionId, Arc<dyn tools::AgentTool>)>,
}

impl HandlerRegistry {
    /// Returns the number of handlers registered for one point.
    #[must_use]
    pub fn handler_count<P: RegisterPoint>(&self) -> usize {
        P::handlers(self).len()
    }

    /// Returns handler source identifiers in deterministic registration order.
    #[must_use]
    pub fn handler_sources<P: RegisterPoint>(&self) -> Vec<&str> {
        P::handlers(self)
            .iter()
            .map(|registered| registered.source().as_str())
            .collect()
    }

    /// Returns typed handlers for runtime composition within this crate.
    pub(crate) fn handlers<P: RegisterPoint>(&self) -> &[RegisteredHandler<P>] {
        P::handlers(self)
    }

    /// Adds one typed handler while the registry is still mutable.
    pub(crate) fn push<P: RegisterPoint>(
        &mut self,
        source: ExtensionId,
        handler: Arc<dyn ExtensionHandler<P>>,
    ) {
        P::handlers_mut(self).push(RegisteredHandler { source, handler });
    }

    /// Clones registered commands for a session-local dynamic registry.
    #[must_use]
    pub fn commands(&self) -> Vec<RegisteredCommand> {
        self.commands.values().cloned().collect()
    }

    /// Clones registered flags for immutable host discovery.
    #[must_use]
    pub fn flags(&self) -> Vec<ExtensionFlagDefinition> {
        self.flags.values().cloned().collect()
    }

    /// Clones extension tool handles with deterministic source order.
    #[must_use]
    pub fn tools(&self) -> Vec<(ExtensionId, Arc<dyn tools::AgentTool>)> {
        self.tools
            .iter()
            .map(|(source, tool)| (source.clone(), Arc::clone(tool)))
            .collect()
    }
}
