import { AcpProtocol } from "./protocol";

/** A slash command and optional argument hint advertised by the current ACP Session. */
export type AvailableCommand = Readonly<{ name: string; description: string; hint?: string }>;

/** Decodes one full snapshot in server order. An omitted or malformed envelope leaves the previous snapshot intact. */
export function decodeCommands(raw: unknown): readonly AvailableCommand[] | undefined {
  if (!Array.isArray(raw)) return undefined;
  const commands: AvailableCommand[] = [];
  const seen = new Set<string>();
  for (const entry of raw) {
    const item = AcpProtocol.record(entry);
    // Future ACP command types can coexist with the text commands the composer understands.
    if (item?.type !== "command" || typeof item.name !== "string" || !item.name || /\s/.test(item.name) ||
      typeof item.description !== "string" || seen.has(item.name)) continue;
    const input = AcpProtocol.record(item.input);
    if (item.input != null && (input?.type !== "text" || typeof input.hint !== "string")) continue;
    seen.add(item.name);
    commands.push({ name: item.name, description: item.description, ...(typeof input?.hint === "string" ? { hint: input.hint } : {}) });
  }
  return commands;
}

/** A leading slash without arguments filters the Session's commands, as in the WebUI. */
export function commandQuery(text: string): string | undefined {
  return text.startsWith("/") && !/\s/.test(text) ? text.slice(1).toLowerCase() : undefined;
}
