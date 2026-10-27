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
          <summary><span>Tool · {tool.metadata?.exposure ?? "direct"} · {tool.disabled === true ? "已禁用" : tool.loaded === true ? "已加载" : tool.loaded === false ? "未加载" : "状态未知"}</span>{tool.title ?? tool.publicName}</summary>
          {tool.title === undefined ? null : <p><code>{tool.publicName}</code></p>}
          {tool.description === undefined ? null : <p>{tool.description}</p>}
          {tool.metadata === undefined ? null : <p>服务器声明（提示）：{Object.entries(tool.metadata.annotations).map(([name, value]) => `${name}=${String(value)}`).join(" · ") || "未提供注解"}</p>}
          <p>输入 Schema</p>
          <pre>{JSON.stringify(tool.inputSchema, null, 2)}</pre>
          {tool.metadata?.outputSchema === undefined ? null : <><p>输出 Schema</p><pre>{JSON.stringify(tool.metadata.outputSchema, null, 2)}</pre></>}
        </details>
      ))}
      {prompts.map((prompt) => <McpPromptEntry controller={controller} key={`prompt:${prompt.reference.remoteName}`} prompt={prompt} />)}
      {resources.map((resource) => <McpResourceEntry controller={controller} key={`resource:${resource.reference.remoteUri}`} resource={resource} />)}
      {templates.map((template) => <McpTemplateEntry controller={controller} key={`template:${template.uriTemplate}`} template={template} />)}
    </div>
  );
}
