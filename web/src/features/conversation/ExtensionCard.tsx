import type { ExtensionEntity } from "../../domain/model";

export type ExtensionCardProps = Readonly<{
  extension: ExtensionEntity;
}>;

/** Renders ACP extension messages and sanitized handler failures without schema assumptions. */
export function ExtensionCard({ extension }: ExtensionCardProps) {
  const text = extension.blocks
    .filter((block): block is Record<string, unknown> => typeof block === "object" && block !== null && !Array.isArray(block))
    .map((block) => typeof block.text === "string" ? block.text : "")
    .filter((value) => value.length > 0)
    .join("\n");
  return (
    <article className={`extension-card extension-card--${extension.kind}`}>
      <header><span>{extension.extensionId}</span><small>{extension.customType}</small></header>
      {extension.message === undefined ? null : <p>{extension.message}</p>}
      {text.length === 0 ? null : <p className="extension-card__content">{text}</p>}
      {extension.details === undefined ? null : <details><summary>详细信息</summary><pre>{JSON.stringify(extension.details, null, 2)}</pre></details>}
    </article>
  );
}
