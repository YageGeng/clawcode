import type { McpSessionSnapshot } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";
import { McpPromptEntry } from "./McpPromptEntry";
import { McpResourceEntry } from "./McpResourceEntry";
import { McpTemplateEntry } from "./McpTemplateEntry";

export type McpCatalogProps = Readonly<{
  serverId: string;
  snapshot: McpSessionSnapshot;
  controller: WorkspaceController;
}>;

/** Renders every catalog family from the authoritative Session snapshot. */
export function McpCatalog({ serverId, snapshot, controller }: McpCatalogProps) {
  const tools = snapshot.catalog.tools.filter((item) => item.reference.serverId === serverId);
  const prompts = snapshot.catalog.prompts.filter((item) => item.reference.serverId === serverId);
  const resources = snapshot.catalog.resources.filter((item) => item.reference.serverId === serverId);
  const templates = snapshot.catalog.resourceTemplates.filter((item) => item.serverId === serverId);
  if (tools.length + prompts.length + resources.length + templates.length === 0) return null;

  return (
    <div className="mcp-catalog">
      {tools.map((tool) => (
        <details key={`tool:${tool.publicName}`}>
          <summary><span>Tool</span>{tool.publicName}</summary>
          {tool.description === undefined ? null : <p>{tool.description}</p>}
          <pre>{JSON.stringify(tool.inputSchema, null, 2)}</pre>
        </details>
      ))}
      {prompts.map((prompt) => <McpPromptEntry controller={controller} key={`prompt:${prompt.reference.remoteName}`} prompt={prompt} />)}
      {resources.map((resource) => <McpResourceEntry controller={controller} key={`resource:${resource.reference.remoteUri}`} resource={resource} />)}
      {templates.map((template) => <McpTemplateEntry controller={controller} key={`template:${template.uriTemplate}`} template={template} />)}
    </div>
  );
}
