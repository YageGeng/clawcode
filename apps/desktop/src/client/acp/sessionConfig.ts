import { AcpProtocol } from "./protocol";

/** A selectable Session setting and the values advertised by its host. */
export type SessionConfigSelect = Readonly<{
  id: string;
  name: string;
  currentValue: string;
  options: readonly Readonly<{ value: string; name: string }>[];
}>;

/** Decodes supported ACP selects, flattening groups in server order and rejecting ambiguous snapshots. */
export function decodeSessionConfig(raw: unknown): readonly SessionConfigSelect[] | undefined {
  if (!Array.isArray(raw)) return undefined;
  const selects: SessionConfigSelect[] = [];
  for (const item of raw) {
    const select = AcpProtocol.record(item);
    if (!select || typeof select.configId !== "string" || typeof select.type !== "string") return undefined;
    const id = select.configId;
    if (select.type !== "select" || !["model", "thinking_level", "mode", "file_approval"].includes(id)) continue;
    if (typeof select.name !== "string" || typeof select.currentValue !== "string" || !Array.isArray(select.options)
      || selects.some((previous) => previous.id === id)) return undefined;
    const options: { value: string; name: string }[] = [];
    const seen = new Set<string>();
    for (const entry of select.options) {
      // ACP allows either ungrouped values or named groups of values.
      const group = AcpProtocol.record(entry);
      for (const choice of Array.isArray(group?.options) ? group.options : [entry]) {
        const option = AcpProtocol.record(choice);
        if (typeof option?.value !== "string" || typeof option.name !== "string" || seen.has(option.value)) return undefined;
        options.push({ value: option.value, name: option.name });
        seen.add(option.value);
      }
    }
    if (!seen.has(select.currentValue)) return undefined;
    selects.push({ id, name: select.name, currentValue: select.currentValue, options });
  }
  return selects;
}
