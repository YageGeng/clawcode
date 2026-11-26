import { Text, View } from "nsh";
import type { JSX } from "nsh/jsx-runtime";
import { t } from "nsh/i18n";

import type { HostStore } from "../stores/host";
import { Appear } from "../ui/appear";
import { Icon } from "../ui/icon";
import { type } from "../ui/tokens";
import { WorkspacePicker } from "./WorkspacePicker";
import { groupByWorkspace } from "./workspaces";

/** No Session open: what to build, in which workspace, and the composer that starts it. */
export function EmptyState(props: {
  host: HostStore;
  cwd: string | null;
  onCwdChange: (cwd: string) => void;
  composer?: JSX.Element;
}) {
  const workspaces = () => groupByWorkspace(props.host.sessions()).map((group) => group.cwd);
  return (
    <Appear entrance="fade" class="flex-1 items-center justify-center gap-6 px-6">
      <Icon name="terminal" size="md" color="muted-foreground" />
      {/* Full width so a narrow window wraps the heading instead of pushing it past the edges. */}
      <View class="w-full flex-row flex-wrap items-center justify-center gap-2">
        <Text class={`${type.heading} text-foreground`}>{t("empty.heading")}</Text>
        {/* The picker and the closing words stay together, so "?" never wraps onto a line alone. */}
        <View class="flex-row items-center gap-2">
          <WorkspacePicker remote={props.host.info.kind === "remote"} workspaces={workspaces()} value={props.cwd} onChange={props.onCwdChange} size="md" />
          <Text class={`${type.heading} text-foreground`}>{t("empty.headingAfter")}</Text>
        </View>
      </View>
      {props.composer}
    </Appear>
  );
}
