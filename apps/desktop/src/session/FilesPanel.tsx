import { cleanup, computed, signal, Text, View } from "nsh";
import type { TreeItem } from "nsh/base";
import { t } from "nsh/i18n";

import type { DirectoryListing, FilePreview, WorkspaceFiles } from "../client/acp/workspaceFiles";
import { messageOf } from "../client/errors";
import { Button, IconButton } from "../ui/button";
import { CodePreview } from "../ui/code";
import { CopyButton } from "../ui/copy";
import { Empty, Spinner } from "../ui/feedback";
import { FileIcon } from "../ui/fileIcon";
import { FilePath } from "../ui/filePath";
import { TreeList } from "../ui/list";
import { Tooltip } from "../ui/menu";
import { SplitGroup, SplitPane } from "../ui/split";
import { inlineLabel, type } from "../ui/tokens";

type Load<T> = { status: "loading" } | { status: "failed"; reason: string } | { status: "ready"; value: T };

/** Remembers the user's dragged file-preview height across file switches. */
const previewHeight = signal(220);

/** Formats a byte count into human-readable binary units for oversized file notices. */
function formatFileSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return t("toolDetail.bytes", { count: bytes });
}

/** A lazy workspace tree and a resizable read-only preview, scoped to one connection and workspace root. */
export function FilesPanel(props: { reader: WorkspaceFiles; root: string }) {
  const directories = signal<ReadonlyMap<string, Load<DirectoryListing>>>(new Map());
  const expanded = signal<string[]>([]);
  const selected = signal<string | null>(null);
  const previewPath = signal("");
  const preview = signal<FilePreview | { type: "loading" } | { type: "failed"; reason: string }>({ type: "loading" });
  let readVersion = 0;
  let mounted = true;
  cleanup(() => { mounted = false; });

  /** Shares pending and completed listings; expanding a failed folder tries it again. */
  async function loadDirectory(path: string) {
    const cached = directories().get(path);
    if (cached && cached.status !== "failed") return;
    directories.update((all) => new Map(all).set(path, { status: "loading" }));
    let result: Load<DirectoryListing>;
    try {
      result = { status: "ready", value: await props.reader.list(props.root, path) };
    } catch (error) {
      console.warn(`Workspace directory request failed: ${messageOf(error)}`);
      result = { status: "failed", reason: messageOf(error) };
    }
    if (mounted) directories.update((all) => new Map(all).set(path, result));
  }

  /** Only the most recent selection can populate the preview, regardless of response order. */
  async function readFile(path: string) {
    const version = ++readVersion;
    previewPath.set(path);
    preview.set({ type: "loading" });
    try {
      const result = await props.reader.read(props.root, path);
      if (mounted && version === readVersion) preview.set(result);
    } catch (error) {
      console.warn(`Workspace preview request failed: ${messageOf(error)}`);
      if (mounted && version === readVersion) preview.set({ type: "failed", reason: messageOf(error) });
    }
  }

  /** Closes the preview pane and invalidates any in-flight file read so it cannot reopen the pane. */
  function closePreview() {
    ++readVersion;
    previewPath.set("");
  }

  const tree = computed(() => {
    const cache = directories();
    const open = new Set(expanded());
    // NSH passes only the row ID, label and folder state through its native tree renderer.
    const symlinkPaths = new Set<string>();
    /** A trailing slash cannot identify a real entry. Its row also makes unloaded folders expandable. */
    function children(path: string): TreeItem[] {
      const listing = cache.get(path);
      /** Presents a directory status as a nonselectable child. */
      const statusRow = (label: string): TreeItem => ({ id: path + "/", label, disabled: true });
      if (listing?.status === "failed") return [statusRow(t("files.directoryFailed", { reason: listing.reason }))];
      if (listing?.status !== "ready") return [statusRow(t("files.loading"))];
      const rows = listing.value.entries.map((entry): TreeItem => {
        if (entry.kind === "symlink") symlinkPaths.add(entry.path);
        const row: TreeItem = { id: entry.path, label: entry.name, disabled: entry.kind === "symlink" };
        if (entry.kind === "directory") {
          row.children = open.has(entry.path) ? children(entry.path) : [{ id: entry.path + "/", label: "", disabled: true }];
        }
        return row;
      });
      if (listing.value.truncated) rows.push(statusRow(t("files.truncated")));
      else if (!rows.length) rows.push(statusRow(t("files.emptyDirectory")));
      return rows;
    }
    return { items: children(""), symlinkPaths };
  });
  const rootLoading = computed(() => {
    const root = directories().get("");
    return !root || root.status === "loading";
  });
  const rootError = computed(() => {
    const root = directories().get("");
    return root?.status === "failed" ? root : undefined;
  });
  const failure = computed(() => { const value = preview(); return value.type === "failed" ? value : undefined; });
  const tooLarge = computed(() => { const value = preview(); return value.type === "too_large" ? value : undefined; });
  const text = computed(() => { const value = preview(); return value.type === "text" ? value : undefined; });
  /** Directory selection stays in the tree; a file selection also opens its preview. */
  function select(path: string) {
    selected.set(path);
    const parent = directories().get(path.slice(0, Math.max(0, path.lastIndexOf("/"))));
    if (path !== previewPath() && parent?.status === "ready" && parent.value.entries.some((entry) => entry.path === path && entry.kind === "file")) {
      void readFile(path);
    }
  }
  void loadDirectory("");
  return (
    <View class="min-h-0 flex-1 bg-background">
      <SplitGroup axis="vertical" handleAccessibilityLabel={t("files.resizePreview")} class="min-h-0 flex-1">
        <SplitPane minSize={132} class="min-h-0">
          <If when={rootError()} else={
            <If when={rootLoading()} else={
              <TreeList class="min-h-0 flex-1" accessibilityLabel={t("files.tree")} items={tree().items}
                symlinkPaths={tree().symlinkPaths} expanded={expanded()}
                selected={selected()} onSelectedChange={select} onExpandedChange={(next) => {
                  const previous = new Set(expanded());
                  expanded.set(next);
                  for (const path of next) if (!previous.has(path)) void loadDirectory(path);
                }} />
            }>
              <View class="flex-1 items-center justify-center gap-2">
                <Spinner label={t("files.loading")} />
                <Text class={`${type.meta} text-muted-foreground`}>{t("files.loading")}</Text>
              </View>
            </If>
          }>
            {(error) => <Empty class="flex-1" icon="circle-alert" title={t("files.loadFailed")} description={error().reason}
              action={<Button size="sm" variant="outline" label={t("remote.retry")} onPress={() => void loadDirectory("")} />} />}
          </If>
        </SplitPane>
        <SplitPane size={previewHeight()} minSize={132} fixed visible={!!previewPath()}
          onResize={(size) => previewHeight.set(size)} class="min-h-0">
          <If when={previewPath()}>
            {(path) => <View class="min-h-0 flex-1 bg-background">
              <View class="min-h-9 flex-row items-center justify-between gap-2 border-b border-border bg-muted/45 px-3 py-1">
                <View class="min-w-0 flex-1 flex-row items-center gap-1.5">
                  <FileIcon path={path()} size="sm" />
                  <FilePath path={path()} class={`flex-1 ${inlineLabel} font-mono ${type.meta} text-foreground`} />
                </View>
                <View class="flex-row items-center gap-0.5">
                  <If when={text()}>
                    {(file) => <If when={file().text.length > 0}>
                      <CopyButton text={file().text} label={t("files.copyFile")} size="sm" />
                    </If>}
                  </If>
                  <IconButton size="sm" icon="x" label={t("files.closePreview")}
                    tooltip={<Tooltip text={t("files.closePreview")} />} onPress={closePreview} />
                </View>
              </View>
              <If when={text()}>
                {(file) => <CodePreview value={file().text} path={path()} placeholder={t("files.emptyFile")}
                  accessibilityLabel={t("files.preview", { path: path() })} />}
              </If>
              <If when={preview().type === "loading"}>
                <View class="flex-1 items-center justify-center gap-2">
                  <Spinner label={t("files.loading")} />
                  <Text class={`${type.meta} text-muted-foreground`}>{t("files.loading")}</Text>
                </View>
              </If>
              <If when={preview().type === "binary"}>
                <Empty class="flex-1" icon="file-text" title={t("files.binary")} />
              </If>
              <If when={tooLarge()}>
                {(large) => <Empty class="flex-1" icon="info" title={t("files.tooLarge")} description={formatFileSize(large().size)} />}
              </If>
              <If when={failure()}>
                {(error) => <Empty class="flex-1" icon="circle-alert" title={t("files.loadFailed")} description={error().reason}
                  action={<Button size="sm" variant="outline" label={t("remote.retry")} onPress={() => void readFile(path())} />} />}
              </If>
            </View>}
          </If>
        </SplitPane>
      </SplitGroup>
    </View>
  );
}
