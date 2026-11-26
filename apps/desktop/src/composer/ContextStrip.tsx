import { View } from "nsh";

import { HostSwitcher } from "../shell/HostSwitcher";
import type { AppStore } from "../stores/app";

/** Where a new Session runs, above the composer on the empty state: the host (the heading picks the workspace). */
export function ContextStrip(props: { store: AppStore; onAddHost?: () => void }) {
  return (
    <View class="flex-row items-center gap-1">
      <HostSwitcher store={props.store} size="sm" onAddHost={props.onAddHost} />
    </View>
  );
}
