import { Text, View } from "nsh";

import { CopyButton } from "../ui/copy";
import { type } from "../ui/tokens";

/** What sits under an answer: copy it, and when it was written. */
export function MessageActions(props: { text: string; timestampMs: string }) {
  const time = () => new Date(Number(props.timestampMs)).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  return (
    <View class="flex-row items-center gap-2">
      <CopyButton text={props.text} />
      <Text class={`${type.meta} text-muted-foreground`}>{time()}</Text>
    </View>
  );
}
