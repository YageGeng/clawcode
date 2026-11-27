import { Image, token } from "nsh";

import archive from "../../assets/icons/archive.svg";
import arrowDown from "../../assets/icons/arrow-down.svg";
import arrowLeft from "../../assets/icons/arrow-left.svg";
import arrowRight from "../../assets/icons/arrow-right.svg";
import arrowUp from "../../assets/icons/arrow-up.svg";
import check from "../../assets/icons/check.svg";
import fileText from "../../assets/icons/file-text.svg";
import fileDiff from "../../assets/icons/file-diff.svg";
import panelRight from "../../assets/icons/panel-right.svg";
import panelLeft from "../../assets/icons/panel-left.svg";
import pencil from "../../assets/icons/pencil.svg";
import square from "../../assets/icons/square.svg";
import squarePen from "../../assets/icons/square-pen.svg";
import terminal from "../../assets/icons/terminal.svg";
import chevronDown from "../../assets/icons/chevron-down.svg";
import chevronLeft from "../../assets/icons/chevron-left.svg";
import chevronRight from "../../assets/icons/chevron-right.svg";
import circleAlert from "../../assets/icons/circle-alert.svg";
import circleCheck from "../../assets/icons/circle-check.svg";
import copy from "../../assets/icons/copy.svg";
import ellipsis from "../../assets/icons/ellipsis.svg";
import folder from "../../assets/icons/folder.svg";
import hand from "../../assets/icons/hand.svg";
import info from "../../assets/icons/info.svg";
import laptop from "../../assets/icons/laptop.svg";
import link from "../../assets/icons/link.svg";
import loaderCircle from "../../assets/icons/loader-circle.svg";
import moon from "../../assets/icons/moon.svg";
import pin from "../../assets/icons/pin.svg";
import plus from "../../assets/icons/plus.svg";
import search from "../../assets/icons/search.svg";
import settings from "../../assets/icons/settings.svg";
import shieldCheck from "../../assets/icons/shield-check.svg";
import sun from "../../assets/icons/sun.svg";
import triangleAlert from "../../assets/icons/triangle-alert.svg";
import x from "../../assets/icons/x.svg";

/** The Lucide icons the app ships, by name. */
const icons = {
  archive,
  "arrow-down": arrowDown,
  "arrow-left": arrowLeft,
  "arrow-right": arrowRight,
  "arrow-up": arrowUp,
  check,
  "file-text": fileText,
  "file-diff": fileDiff,
  "panel-left": panelLeft,
  "panel-right": panelRight,
  pencil,
  square,
  "square-pen": squarePen,
  terminal,
  "chevron-down": chevronDown,
  "chevron-left": chevronLeft,
  "chevron-right": chevronRight,
  "circle-alert": circleAlert,
  "circle-check": circleCheck,
  copy,
  ellipsis,
  folder,
  hand,
  info,
  laptop,
  link,
  "loader-circle": loaderCircle,
  moon,
  pin,
  plus,
  search,
  settings,
  "shield-check": shieldCheck,
  sun,
  "triangle-alert": triangleAlert,
  x,
} as const;

/** The name of an icon `Icon` draws. */
export type IconName = keyof typeof icons;

/** The theme color roles an icon may be tinted with. */
export type IconColor =
  | "foreground"
  | "muted-foreground"
  | "primary-foreground"
  | "destructive"
  | "destructive-foreground"
  | "accent-foreground"
  | "secondary-foreground"
  | "success"
  | "warning"
  | "info";

/** One Lucide icon at the app's sizes (`sm` 14pt, `md` 16pt), tinted with a theme color. */
export function Icon(props: { name: IconName; size?: "sm" | "md"; color?: IconColor; class?: string }) {
  return (
    <Image
      src={icons[props.name]}
      fadeDuration={0}
      accessibilityElementsHidden
      class={props.class}
      style={{
        width: props.size === "sm" ? 14 : 16,
        height: props.size === "sm" ? 14 : 16,
        // Colors are strings; `token` also types the theme's lengths.
        tintColor: String(token(props.color ?? "foreground")),
      }}
    />
  );
}
