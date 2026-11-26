import { t } from "nsh/i18n";
import { Folders } from "clawcode-backend";

import { messageOf } from "../client/errors";
import type { ControlSize } from "../ui/button";
import { Select } from "../ui/selection";
import { workspaceName } from "./workspaces";

/** The Select value that opens the system's folder chooser instead of choosing a workspace. */
const CHOOSE_FOLDER = "\u0000choose-folder";

/** Picks the working directory of a new Session: a known workspace, or any folder through the system chooser. */
export function WorkspacePicker(props: {
  workspaces: readonly string[];
  value: string | null;
  onChange: (cwd: string) => void;
  size?: ControlSize;
  class?: string;
}) {
  // A folder picked from the chooser joins the list so the trigger can show it.
  const known = () => (props.value === null || props.workspaces.includes(props.value) ? props.workspaces : [props.value, ...props.workspaces]);
  return (
    <Select
      accessibilityLabel={t("shell.workspace")}
      size={props.size ?? "sm"}
      class={props.class}
      items={[...known().map((cwd) => ({ value: cwd, title: workspaceName(cwd) })), { value: CHOOSE_FOLDER, title: t("empty.chooseFolder") }]}
      value={props.value}
      onChange={(value) => {
        if (value === null) return;
        if (value !== CHOOSE_FOLDER) {
          props.onChange(value);
          return;
        }
        void Folders.pick().then(
          (path) => path !== null && props.onChange(path),
          (error: unknown) => console.warn(`folder chooser: ${messageOf(error)}`),
        );
      }}
    />
  );
}
