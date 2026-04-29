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
#[derive(Debug, Clone, PartialEq)]
pub struct ConfiguredToolSpec {
    pub spec: ToolSpec,
    pub supports_parallel_tool_calls: bool,
}

impl ConfiguredToolSpec {
    /// Builds a configured tool spec from a plain tool spec.
    pub fn new(spec: ToolSpec, supports_parallel_tool_calls: bool) -> Self {
        Self {
            spec,
            supports_parallel_tool_calls,
        }
    }

    /// Returns the stable visible tool name.
    pub fn name(&self) -> &str {
        self.spec.name()
    }
}
