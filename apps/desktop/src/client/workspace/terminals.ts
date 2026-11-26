/** One ACP terminal: the command, where it ran, its raw output, and how it ended. */
export type TerminalEntity = Readonly<{
  terminalId: string;
  command?: string;
  cwd?: string;
  output: string;
  exitStatus?: Readonly<{ exitCode?: number; signal?: string }>;
}>;

type Json = Record<string, unknown>;

const isRecord = (value: unknown): value is Json => typeof value === "object" && value !== null && !Array.isArray(value);

/** Decodes base64 into bytes. */
function bytes(data: string): Uint8Array {
  const binary = atob(data);
  return Uint8Array.from(binary, (character) => character.charCodeAt(0));
}

// Matches CSI and OSC sequences and two-byte escapes; output stays raw in state
// and is stripped only for display, so a sequence split across chunks still
// disappears once the rest arrives.
// eslint-disable-next-line no-control-regex
const ANSI = /\u001b\[[0-?]*[ -/]*[@-~]|\u001b\][^\u0007\u001b]*(?:\u0007|\u001b\\)|\u001b[@-Z\\-_]/g;

/** Applies ACP terminal updates and output chunks for one Session. */
export class TerminalDecoder {
  // Chunks may split a UTF-8 character, so each terminal keeps a streaming decoder.
  private readonly decoders = new Map<string, TextDecoder>();

  /** Applies a `terminal_update` patch: null clears, a value replaces, output replaces whole. */
  update(previous: TerminalEntity | undefined, update: Json): TerminalEntity | undefined {
    if (typeof update.terminalId !== "string") return undefined;
    const terminalId = update.terminalId;
    let next: Record<string, unknown> = { ...(previous ?? { terminalId, output: "" }) };
    const field = <T>(key: string, value: unknown, narrow: (raw: unknown) => T | undefined) => {
      if (!(key in update)) return;
      const narrowed = value === null ? undefined : narrow(value);
      if (narrowed === undefined) {
        next = Object.fromEntries(Object.entries(next).filter(([name]) => name !== key));
      } else {
        next[key] = narrowed;
      }
    };
    field("command", update.command, (value) => typeof value === "string" ? value : undefined);
    field("cwd", update.cwd, (value) => typeof value === "string" ? value : undefined);
    field("exitStatus", update.exitStatus, (value) => isRecord(value)
      ? { ...(typeof value.exitCode === "number" ? { exitCode: value.exitCode } : {}), ...(typeof value.signal === "string" ? { signal: value.signal } : {}) }
      : undefined);
    if ("output" in update) {
      this.decoders.delete(terminalId);
      next.output = isRecord(update.output) && typeof update.output.data === "string"
        ? new TextDecoder().decode(bytes(update.output.data))
        : "";
    }
    return next as TerminalEntity;
  }

  /** Appends one `terminal_output_chunk` to the terminal's output. */
  chunk(previous: TerminalEntity | undefined, update: Json): TerminalEntity | undefined {
    if (typeof update.terminalId !== "string" || typeof update.data !== "string") return undefined;
    let decoder = this.decoders.get(update.terminalId);
    if (decoder === undefined) {
      decoder = new TextDecoder();
      this.decoders.set(update.terminalId, decoder);
    }
    const appended = decoder.decode(bytes(update.data), { stream: true });
    return { ...(previous ?? { terminalId: update.terminalId, output: "" }), output: `${previous?.output ?? ""}${appended}` };
  }
}

/** Removes ANSI escapes and carriage-return overwrites for plain display. */
export function displayText(output: string): string {
  return output
    .replace(ANSI, "")
    .split("\n")
    // A bare carriage return redraws the line; keep only the final redraw.
    .map((line) => line.split("\r").filter((part) => part.length > 0).at(-1) ?? "")
    .join("\n");
}
