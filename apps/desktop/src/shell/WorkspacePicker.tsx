import { signal } from "nsh";
import { t } from "nsh/i18n";
import { Folders } from "clawcode-backend";

import { messageOf } from "../client/errors";
import { Dialog } from "../ui/dialog";
import { Field, Input } from "../ui/input";
import { Button } from "../ui/button";
import type { ControlSize } from "../ui/button";
import { Select } from "../ui/selection";
import { workspaceName } from "./workspaces";

/** The Select value that opens the system's folder chooser instead of choosing a workspace. */
const CHOOSE_FOLDER = "\u0000choose-folder";

/** Picks the working directory of a new Session: a known workspace, or any folder through the system chooser. */
export function WorkspacePicker(props: {
  workspaces: readonly string[];
  remote?: boolean;
  value: string | null;
  onChange: (cwd: string) => void;
  size?: ControlSize;
  class?: string;
}) {
  // A folder picked from the chooser joins the list so the trigger can show it.
  const known = () => (props.value === null || props.workspaces.includes(props.value) ? props.workspaces : [props.value, ...props.workspaces]);
  const editing = signal(false);
  const draft = signal("");
  /** Accepts an absolute path in the remote OS's syntax without probing the controller's filesystem. */
  const valid = () => {
    const path = draft().trim();
    return path.startsWith("/") || /^[A-Za-z]:[\\/]/.test(path) || path.startsWith("\\\\");
  };
  /** Selects the entered path for this host only. */
  const usePath = () => {
    if (!valid()) return;
    props.onChange(draft().trim());
    editing.set(false);
  };
  return (
    <>
      <Select
        accessibilityLabel={t("shell.workspace")}
        placeholder={t("shell.workspace")}
        size={props.size ?? "sm"}
        class={props.class}
        items={[...known().map((cwd) => ({ value: cwd, title: workspaceName(cwd) })), { value: CHOOSE_FOLDER, title: t(props.remote ? "remote.enterPath" : "empty.chooseFolder") }]}
        value={props.value}
        onChange={(value) => {
          if (value === null) return;
          if (value !== CHOOSE_FOLDER) {
            props.onChange(value);
            return;
          }
          if (props.remote) {
            draft.set(props.value ?? "");
            editing.set(true);
            return;
          }
          void Folders.pick().then(
            (path) => path !== null && props.onChange(path),
            (error: unknown) => console.warn(`folder chooser: ${messageOf(error)}`),
          );
        }}
      />
      <If when={editing()}>
        <Dialog open title={t("remote.workspacePath")} onClose={() => editing.set(false)}
          actions={<Button label={t("remote.useWorkspace")} variant="primary" disabled={!valid()} onPress={usePath} />}>
          <Field label={t("remote.workspacePath")} description={t("remote.pathHelp")}>
            <Input value={draft()} accessibilityLabel={t("remote.workspacePath")} onChange={(value) => draft.set(value)} onSubmit={usePath} />
          </Field>
        </Dialog>
      </If>
    </>
  );
}
