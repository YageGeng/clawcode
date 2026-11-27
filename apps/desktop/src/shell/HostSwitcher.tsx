import { t } from "nsh/i18n";

import type { AppStore } from "../stores/app";
import type { HostInfo } from "../stores/host";
import { Button } from "../ui/button";
import { Menu, MenuItem, MenuSeparator } from "../ui/menu";

/** The name a host shows: this computer's in the app's language, a paired one's as it calls itself. */
function hostName(info: HostInfo): string {
  return info.kind === "local" ? t("shell.thisComputer") : info.name;
}

/** The current computer and a menu of paired computers with their individual connection states. */
export function HostSwitcher(props: { store: AppStore; size?: "sm" | "md"; onAddHost?: () => void }) {
  return (
    <Menu
      title={t("shell.hosts")}
      trigger={<Button variant="ghost" size={props.size ?? "md"} icon="laptop" trailingIcon="chevron-down" label={hostName(props.store.currentHost().info)} />}
    >
      <Each of={props.store.hosts()} key={(host) => host.info.id}>
        {(host) => (
          <MenuItem
            label={hostName(host().info)}
            description={host().info.kind === "remote" ? t(`remote.status.${host().phase().kind}`) : undefined}
            icon="laptop"
            checked={props.store.currentHost().info.id === host().info.id}
            onSelect={() => props.store.selectHost(host().info.id)}
          />
        )}
      </Each>
      <If when={props.onAddHost !== undefined}>
        <MenuSeparator />
        <MenuItem label={t("remote.addHost")} icon="plus" onSelect={() => props.onAddHost?.()} />
      </If>
    </Menu>
  );
}
