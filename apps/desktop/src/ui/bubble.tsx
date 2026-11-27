import { Text, View } from "nsh";
import { t } from "nsh/i18n";

import { Appear } from "./appear";
import { type } from "./tokens";

/** The user's message in the conversation: a right-aligned quiet pill; it rises in when `arriving` (a new turn). */
export function MessageBubble(props: { text: string; context?: string; imageCount?: number; arriving?: boolean }) {
  return (
    <Appear entrance="rise" play={props.arriving === true} class="max-w-[80%] self-end">
      <View class="rounded-[18px] border border-border/70 bg-secondary px-4 py-2.5">
        <If when={props.context}>
          {(context) => <Text class={`${type.meta} text-muted-foreground`}>{context()}</Text>}
        </If>
        <If when={props.imageCount}>
          {(count) => <Text class={`${type.meta} text-muted-foreground`}>{t(count() === 1 ? "attachments.oneImage" : "attachments.manyImages", { count: count() })}</Text>}
        </If>
        <If when={props.text || !props.imageCount}>
          <Text class={`${type.body} text-secondary-foreground`}>{props.text}</Text>
        </If>
      </View>
    </Appear>
  );
}
