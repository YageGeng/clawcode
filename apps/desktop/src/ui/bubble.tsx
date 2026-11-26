import { Text, View } from "nsh";

import { Appear } from "./appear";
import { type } from "./tokens";

/** The user's message in the conversation: a right-aligned quiet pill; it rises in when `arriving` (a new turn). */
export function MessageBubble(props: { text: string; arriving?: boolean }) {
  return (
    <Appear entrance="rise" play={props.arriving === true} class="max-w-[80%] self-end">
      <View class="rounded-[18px] bg-secondary px-4 py-2">
        <Text class={`${type.body} text-secondary-foreground`}>{props.text}</Text>
      </View>
    </Appear>
  );
}
