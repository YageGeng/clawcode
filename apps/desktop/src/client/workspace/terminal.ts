import { Buffer } from "buffer";
import { AcpProtocol } from "../acp/protocol";
import type { TerminalEntity } from "../domain/model";

/** Applies one terminal patch or byte chunk to an immutable, branch-local projection. */
export function decodeTerminal(previous: TerminalEntity | undefined, update: Readonly<Record<string, unknown>>): TerminalEntity | undefined {
  if (typeof update.terminalId !== "string") return undefined;
  const chunk = update.sessionUpdate === "terminal_output_chunk";
  if (chunk && typeof update.data !== "string") return undefined;
  const command = update.command === null ? undefined : typeof update.command === "string" ? update.command : previous?.command;
  const cwd = update.cwd === null ? undefined : typeof update.cwd === "string" ? update.cwd : previous?.cwd;
  const exit = AcpProtocol.record(update.exitStatus);
  const exitStatus = update.exitStatus === null ? undefined : exit ? {
    ...(typeof exit.exitCode === "number" ? { exitCode: exit.exitCode } : {}),
    ...(typeof exit.signal === "string" ? { signal: exit.signal } : {}),
  } : previous?.exitStatus;
  const data = chunk ? update.data : AcpProtocol.record(update.output)?.data;
  let output = previous?.output ?? "";
  let pendingUtf8 = previous?.pendingUtf8;
  // A snapshot starts a new byte stream. Keep only an unfinished character across chunks,
  // so abandoned replays and Sessions using the same terminal id cannot share decoder state.
  if (!chunk && (typeof data === "string" || update.output === null)) {
    output = "";
    pendingUtf8 = undefined;
  }
  if (typeof data === "string") {
    let bytes: Uint8Array;
    try {
      bytes = Buffer.from(data, "base64");
    } catch {
      console.warn(`Could not decode output bytes for terminal ${update.terminalId}`);
      return undefined;
    }
    if (pendingUtf8?.length) bytes = Buffer.concat([pendingUtf8, bytes]);
    const decoder = new TextDecoder("utf-8", { ignoreBOM: true });
    output += decoder.decode(bytes, { stream: !exitStatus });
    pendingUtf8 = undefined;
    if (!exitStatus && decoder.decode().length) {
      // Flushing yielded a replacement character, so the stream ended partway through a code point.
      let start = bytes.length - 1;
      while (start > 0 && (bytes[start]! & 0xc0) === 0x80) start--;
      pendingUtf8 = Uint8Array.from(bytes.subarray(start));
    }
  } else if (exitStatus && pendingUtf8) {
    output += new TextDecoder().decode(pendingUtf8);
    pendingUtf8 = undefined;
  }
  return { terminalId: update.terminalId, command, cwd, output, exitStatus, pendingUtf8 };
}
