import { mount, signal } from "nsh";
import { Button } from "../../src/ui/button";
import { Dialog } from "../../src/ui/dialog";

/** Opens two views inside one native dialog, as device settings do when showing an invitation. */
function Entry() {
  const page = signal<"devices" | "invitation" | null>(null);
  return <>
    <Button label="Open devices" onPress={() => page.set("devices")} />
    <Dialog controlledDismissal open={page() !== null} title={page() === "invitation" ? "Invitation" : "Devices"}
      onClose={() => page.set(page() === "invitation" ? "devices" : null)}>
      <If when={page() === "devices"}>
        <Button label="Open invitation" onPress={() => page.set("invitation")} />
      </If>
    </Dialog>
  </>;
}

mount(Entry);
