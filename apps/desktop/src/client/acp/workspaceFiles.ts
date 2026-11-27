import type { AcpConnection } from "./connection";
import { AcpProtocol } from "./protocol";

export type DirectoryEntry = { path: string; name: string; kind: "file" | "directory" | "symlink" };
export type DirectoryListing = { entries: DirectoryEntry[]; truncated: boolean };
export type FilePreview = { type: "text"; text: string } | { type: "binary" } | { type: "too_large"; size: number };

/** Reads root-relative paths through the session host's authenticated ACP connection. */
export class WorkspaceFiles {
  /** Shares the host connection and its request deadline with both workspace operations. */
  constructor(private readonly connection: AcpConnection, private readonly timeoutMs: number) {}

  /** Lists one level and admits only direct children, whose paths are also the native tree's row IDs. */
  async list(root: string, path: string): Promise<DirectoryListing> {
    const reply = await this.connection.request<unknown>("_clawcode/workspace/list_dir", { root, path }, { timeoutMs: this.timeoutMs });
    const data = AcpProtocol.decodeRecord(reply, "workspace directory");
    if (!Array.isArray(data.entries) || typeof data.truncated !== "boolean") throw new Error("Invalid workspace listing");
    const entries = data.entries.map((value): DirectoryEntry => {
      const { path: child, name, kind } = AcpProtocol.decodeRecord(value, "workspace entry");
      if (typeof name !== "string" || !name || name === "." || name === ".." || name.includes("/")
        || child !== (path ? `${path}/${name}` : name) || (kind !== "directory" && kind !== "file" && kind !== "symlink")) {
        throw new Error("Invalid workspace entry");
      }
      return { path: child, name, kind };
    });
    return { entries, truncated: data.truncated };
  }

  /** Reads one preview, retaining binary and size-limit responses as displayable outcomes. */
  async read(root: string, path: string): Promise<FilePreview> {
    const reply = await this.connection.request<unknown>("_clawcode/workspace/read_file", { root, path }, { timeoutMs: this.timeoutMs });
    const data = AcpProtocol.decodeRecord(reply, "workspace preview");
    if (data.type === "text" && typeof data.text === "string") return { type: "text", text: data.text };
    if (data.type === "binary") return { type: "binary" };
    if (data.type === "too_large" && typeof data.size === "number") return { type: "too_large", size: data.size };
    throw new Error("Invalid workspace preview");
  }
}
