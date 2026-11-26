import { Image } from "nsh";
import { t } from "nsh/i18n";

/** A scannable invitation. The native encoder supplies the high contrast and four-module quiet zone. */
export function QrCode(props: { uri: string }) {
  return <Image src={props.uri} accessibilityLabel={t("remote.qr")} class="size-[232px] self-center" fadeDuration={0} />;
}
