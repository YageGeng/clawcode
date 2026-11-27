import { Image, View } from "nsh";
import { t } from "nsh/i18n";

import type { PromptImage } from "../client/acp/prompt";
import { IconButton } from "../ui/button";
import { Tooltip } from "../ui/menu";

/** A bounded row of draft thumbnails with a keyboard-accessible removal action for each selected file. */
export function AttachmentStrip(props: { images: readonly PromptImage[]; onRemove: (id: string) => void }) {
  return (
    <View class="flex-row flex-wrap gap-2">
      <Each of={props.images} key={(image) => image.id}>
        {(image) => (
          <View class="flex-row items-center gap-1 rounded-(--radius) bg-muted p-1">
            <Image src={`data:${image().mimeType};base64,${image().data}`} accessibilityLabel={image().name}
              fadeDuration={0} class="size-10 rounded-(--radius) object-cover" />
            <IconButton icon="x" size="sm" label={t("attachments.remove", { name: image().name })}
              tooltip={<Tooltip text={t("attachments.remove", { name: image().name })} />} onPress={() => props.onRemove(image().id)} />
          </View>
        )}
      </Each>
    </View>
  );
}
