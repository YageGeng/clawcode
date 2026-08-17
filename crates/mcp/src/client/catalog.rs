use protocol::{
    McpArgumentInfo, McpCatalog, McpPromptInfo, McpPromptRef, McpResourceInfo,
    McpResourceRef, McpResourceTemplateInfo, McpServerId, McpToolInfo,
    McpToolRef,
};

/// Converts fully paginated rmcp entities into one shared immutable catalog.
pub(super) struct CatalogMapper {
    server_id: McpServerId,
}

impl CatalogMapper {
    /// Creates a mapper that stamps structured references with one owning Server.
    pub(super) fn new(server_id: McpServerId) -> Self {
        Self { server_id }
    }

    /// Converts all four discovered entity families without retaining rmcp types.
    pub(super) fn map(
        &self,
        tools: Vec<rmcp::model::Tool>,
        prompts: Vec<rmcp::model::Prompt>,
        resources: Vec<rmcp::model::Resource>,
        resource_templates: Vec<rmcp::model::ResourceTemplate>,
    ) -> McpCatalog {
        McpCatalog::builder()
            .tools(
                tools
                    .into_iter()
                    .map(|tool| {
                        let reference = McpToolRef {
                            server_id: self.server_id.clone(),
                            remote_name: tool.name.into_owned(),
                        };
                        McpToolInfo::builder()
                            .public_name(reference.public_name())
                            .reference(reference)
                            .description(
                                tool.description
                                    .map(|value| value.into_owned()),
                            )
                            .input_schema(serde_json::Value::Object(
                                (*tool.input_schema).clone(),
                            ))
                            .build()
                    })
                    .collect(),
            )
            .prompts(
                prompts
                    .into_iter()
                    .map(|prompt| {
                        McpPromptInfo::builder()
                            .reference(McpPromptRef {
                                server_id: self.server_id.clone(),
                                remote_name: prompt.name,
                            })
                            .title(prompt.title)
                            .description(prompt.description)
                            .arguments(
                                prompt
                                    .arguments
                                    .unwrap_or_default()
                                    .into_iter()
                                    .map(|argument| McpArgumentInfo {
                                        name: argument.name,
                                        description: argument.description,
                                        required: argument
                                            .required
                                            .unwrap_or(false),
                                    })
                                    .collect(),
                            )
                            .build()
                    })
                    .collect(),
            )
            .resources(
                resources
                    .into_iter()
                    .map(|resource| {
                        McpResourceInfo::builder()
                            .reference(McpResourceRef {
                                server_id: self.server_id.clone(),
                                remote_uri: resource.uri,
                            })
                            .name(resource.name)
                            .title(resource.title)
                            .description(resource.description)
                            .mime_type(resource.mime_type)
                            .size(resource.size)
                            .build()
                    })
                    .collect(),
            )
            .resource_templates(
                resource_templates
                    .into_iter()
                    .map(|template| {
                        McpResourceTemplateInfo::builder()
                            .server_id(self.server_id.clone())
                            .uri_template(template.uri_template)
                            .name(template.name)
                            .title(template.title)
                            .description(template.description)
                            .mime_type(template.mime_type)
                            .build()
                    })
                    .collect(),
            )
            .build()
    }
}
