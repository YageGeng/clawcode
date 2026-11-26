import { Text, View } from "nsh";

import type { PermissionOption, PermissionPrompt } from "../client/domain/model";
import { Button } from "../ui/button";
import type { ButtonVariant } from "../ui/button";
import { Appear } from "../ui/appear";
import { Surface } from "../ui/surface";
import { type } from "../ui/tokens";

/** How loudly an option asks: the first one-time allow leads, other allows are secondary, rejections are quiet. */
function optionVariant(option: PermissionOption, first: PermissionOption | undefined): ButtonVariant {
  if (option === first) return "primary";
  return option.kind.startsWith("reject") ? "ghost" : "secondary";
}

/**
 * The agent asks before acting: the request's title, its description and one button per option. The composer
 * around it answers Escape with no choice (the agent hears "cancelled").
 */
export function PermissionCard(props: { prompt: PermissionPrompt; onAnswer: (optionId: string | undefined) => void }) {
  const lead = () => props.prompt.options.find((option) => option.kind === "allow_once");
  return (
    <Appear entrance="rise" class="w-full">
      <Surface>
        <View class="gap-1" role="alert">
          <Text class={`${type.ui} font-semibold text-card-foreground`}>{props.prompt.title}</Text>
          <If when={props.prompt.description}>
            {(description) => <Text class={`${type.meta} text-muted-foreground`}>{description()}</Text>}
          </If>
        </View>
        <View class="flex-row flex-wrap justify-end gap-2">
          <Each of={[...props.prompt.options]} key={(option) => option.optionId}>
            {(option) => (
              <Button label={option().name} variant={optionVariant(option(), lead())} onPress={() => props.onAnswer(option().optionId)} />
            )}
          </Each>
        </View>
      </Surface>
    </Appear>
  );
}
