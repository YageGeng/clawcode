import { AppDetails, ExternalEditors } from "clawcode-backend";
import { appInfo, signal, Text, View } from "nsh";
import { t } from "nsh/i18n";

import { messageOf } from "../client/errors";
import { FILE_MANAGER } from "../stores/settings";
import { IconButton } from "../ui/button";
import { FilePath } from "../ui/filePath";
import { Icon } from "../ui/icon";
import { Tooltip } from "../ui/menu";
import { type } from "../ui/tokens";

/** One labelled path with a button that shows it in the file manager; a missing path reads as not available. */
function PathRow(props: { label: string; path?: string | null; file?: boolean; first?: boolean }) {
  const failure = signal<string | undefined>(undefined);
  /** Opens the path, or a file's folder since the file manager opens directories; a launch failure shows under the row. */
  const reveal = (path: string) => {
    // Windows paths separate with backslashes; a bare file name has no folder part to open.
    const cut = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
    const target = props.file ? (cut > 0 ? path.slice(0, cut) : cut === 0 ? "/" : ".") : path;
    try {
      ExternalEditors.open(target, FILE_MANAGER);
      failure.set(undefined);
    } catch (error) {
      failure.set(t("settings.about.revealFailed", { detail: messageOf(error) }));
    }
  };
  return (
    <View class={`flex-row items-center gap-3 px-4 py-3 ${props.first ? "" : "border-t border-border"}`}>
      <View class="min-w-0 flex-1 gap-0.5">
        <Text class={`${type.ui} font-medium text-foreground`}>{props.label}</Text>
        {/* A long path keeps its end, the file or folder name, and shows in full on hover. */}
        <If when={props.path} else={<Text class={`${type.meta} text-muted-foreground`}>{t("settings.about.notAvailable")}</Text>}>
          {(path) => <FilePath path={path()} class={`font-mono ${type.meta} text-muted-foreground`} />}
        </If>
        <If when={failure()}>{(text) => <Text class={`${type.meta} text-destructive`}>{text()}</Text>}</If>
      </View>
      <If when={props.path}>
        {(path) => (
          <IconButton icon="folder" size="sm" label={t("settings.about.reveal")} tooltip={<Tooltip text={t("settings.about.reveal")} />}
            onPress={() => reveal(path())} />
        )}
      </If>
    </View>
  );
}

/** The About page: this build's versions and where it keeps its configuration, data and log. */
export function AboutPage() {
  const app = appInfo();
  const about = AppDetails.about();
  return (
    <View class="gap-4">
      <View class="flex-row items-center gap-3.5 rounded-(--radius-lg) border border-border bg-card p-4">
        <View class="size-10 shrink-0 items-center justify-center rounded-(--radius) bg-muted">
          <Icon name="bot" size="md" color="foreground" />
        </View>
        <View class="min-w-0 flex-1 gap-0.5">
          {/* The window already titles the page "About"; the app's name is a section title under it, not a second heading. */}
          <Text class={`${type.ui} font-semibold text-foreground`}>{app.name ?? "Clawcode"}</Text>
          <Text class={`tabular-nums ${type.meta} text-muted-foreground`}>
            {`${t("settings.about.version", { version: app.version ?? "—" })} · ${t("settings.about.nsh", { version: about.nshVersion })}`}
          </Text>
        </View>
      </View>
      <View class="overflow-hidden rounded-(--radius-lg) border border-border bg-card">
        <PathRow first file label={t("settings.about.configFile")} path={about.configFile} />
        <PathRow label={t("settings.about.dataDir")} path={about.dataDir} />
        <PathRow file label={t("settings.about.logFile")} path={process.logFile} />
      </View>
    </View>
  );
}
