use crate::collaboration::AgentRuntimeContext;

/// Model-visible prompt metadata associated with one tool specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ToolPromptMetadata {
    pub prompt_snippet: Option<&'static str>,
    pub prompt_guidelines: &'static [&'static str],
}

/// Describes one model-visible tool specification.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub definition: llm::completion::ToolDefinition,
    pub prompt_metadata: ToolPromptMetadata,
}

impl ToolSpec {
    /// Builds a function-style tool spec from an existing tool definition.
    pub fn function(definition: llm::completion::ToolDefinition) -> Self {
        Self {
            definition,
            prompt_metadata: ToolPromptMetadata::default(),
        }
    }

    /// Builds a function-style tool spec from an existing definition plus prompt metadata.
    pub fn function_with_prompt(
        definition: llm::completion::ToolDefinition,
        prompt_metadata: ToolPromptMetadata,
    ) -> Self {
        Self {
            definition,
            prompt_metadata,
        }
    }

    /// Returns the stable visible tool name.
    pub fn name(&self) -> &str {
        self.definition.name.as_str()
    }
}

/// Attaches runtime execution metadata to a visible tool spec.
#[derive(Clone)]
pub struct ConfiguredToolSpec {
    pub spec: ToolSpec,
    pub supports_parallel_tool_calls: bool,
    /// Optional visibility predicate evaluated against the current agent context.
    /// Returns `true` when the tool should be visible. `None` means always visible.
    pub visible_when: Option<fn(&AgentRuntimeContext) -> bool>,
}

impl ConfiguredToolSpec {
    /// Builds a configured tool spec from a plain tool spec with an optional visibility predicate.
    pub fn new(
        spec: ToolSpec,
        supports_parallel_tool_calls: bool,
        visible_when: Option<fn(&AgentRuntimeContext) -> bool>,
    ) -> Self {
        Self {
            spec,
            supports_parallel_tool_calls,
            visible_when,
        }
    }

    /// Returns the stable visible tool name.
    pub fn name(&self) -> &str {
        self.spec.name()
    }

    /// Evaluates the per-tool visibility predicate against the given agent context.
    pub fn is_visible_to(&self, agent: &AgentRuntimeContext) -> bool {
        self.visible_when.is_none_or(|predicate| predicate(agent))
    }
}

impl std::fmt::Debug for ConfiguredToolSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfiguredToolSpec")
            .field("spec", &self.spec)
            .field(
                "supports_parallel_tool_calls",
                &self.supports_parallel_tool_calls,
            )
            .field("visible_when", &self.visible_when.map(|_| "<fn>"))
            .finish()
    }
}

impl PartialEq for ConfiguredToolSpec {
    fn eq(&self, other: &Self) -> bool {
        self.spec == other.spec
            && self.supports_parallel_tool_calls == other.supports_parallel_tool_calls
    }
}
