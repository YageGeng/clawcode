import { Text } from "nsh";

import { Tooltip } from "./menu";

/** Keeps the filename visible when space is tight and reveals the full path on hover. */
export function FilePath(props: { path: string; label?: string; class: string }) {
  return (
    <Text class={`min-w-0 whitespace-nowrap overflow-hidden ${props.class}`}
      style={{ numberOfLines: 1, textOverflow: "ellipsis-start" }}
      tooltip={<Tooltip text={props.path} />}>
      {props.label ?? props.path}
    </Text>
  );
}
