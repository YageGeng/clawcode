import { t } from "nsh/i18n";

import type { AppStore } from "../stores/app";
import type { HostInfo } from "../stores/host";
import { Button } from "../ui/button";
import { Menu, MenuItem } from "../ui/menu";

/** The name a host shows: this computer's in the app's language, a paired one's as it calls itself. */
function hostName(info: HostInfo): string {
  return info.kind === "local" ? t("shell.thisComputer") : info.name;
}

/** The current computer, opening a menu of every host the app knows (this one now; paired ones from R1). */
export function HostSwitcher(props: { store: AppStore; size?: "sm" | "md" }) {
  return (
    <Menu
      title={t("shell.hosts")}
      trigger={<Button variant="ghost" size={props.size ?? "md"} icon="laptop" label={hostName(props.store.currentHost().info)} />}
    >
      <Each of={props.store.hosts()} key={(host) => host.info.id}>
        {(host) => (
          <MenuItem
            label={hostName(host().info)}
            icon="laptop"
            checked={props.store.currentHost().info.id === host().info.id}
            onSelect={() => props.store.selectHost(host().info.id)}
          />
        )}
      </Each>
    </Menu>
  );
}
