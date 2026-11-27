import { Image, token } from "nsh";

import { effectiveIconTheme } from "../stores/iconTheme";
import type { IconThemeName } from "../stores/iconTheme";
import { catppuccinIconBodies, resolveCatppuccinIcon } from "./catppuccinIcons";
import type { CatppuccinColorKey, FileIconKind } from "./catppuccinIcons";
import { Icon } from "./icon";

const monochromeIcons = { file: "file-text", directory: "folder", symlink: "link" } as const;

type CatppuccinFlavor = "latte" | "frappe" | "macchiato" | "mocha";

/** Cached data URIs keyed by `${flavor}:${iconName}` so each SVG is serialized at most once per flavor. */
const uriCache = new Map<string, string>();

/** Builds an SVG data URI for `iconName` tinted with the requested Catppuccin flavor tokens from `theme.json`. */
function catppuccinDataUri(iconName: string, flavor: CatppuccinFlavor): string {
  const cacheKey = `${flavor}:${iconName}`;
  const hit = uriCache.get(cacheKey);
  if (hit !== undefined) return hit;

  const bodies = catppuccinIconBodies();
  const raw = bodies[iconName] ?? bodies._file!;
  let viewBox = "0 0 16 16";
  let body = raw;
  if (raw.startsWith("@")) {
    const sep = raw.indexOf("|");
    viewBox = raw.slice(1, sep);
    body = raw.slice(sep + 1);
  }
  const colored = body.replace(/\$([a-z0-9-]+)/g, (_, key: CatppuccinColorKey) => String(token(`ctp-${flavor}-${key}`)));
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="${viewBox}">${colored}</svg>`;
  const uri = `data:image/svg+xml,${encodeURIComponent(svg)}`;
  uriCache.set(cacheKey, uri);
  return uri;
}

/** Extracts the Catppuccin flavor suffix when `theme` is a Catppuccin pack, or `null` for minimal icons. */
function flavorOf(theme: IconThemeName): CatppuccinFlavor | null {
  switch (theme) {
    case "catppuccin-latte":
      return "latte";
    case "catppuccin-frappe":
      return "frappe";
    case "catppuccin-macchiato":
      return "macchiato";
    case "catppuccin-mocha":
      return "mocha";
    case "minimal":
      return null;
  }
}

/** Renders a file or folder icon according to the user's configured icon theme. */
export function FileIcon(props: {
  path: string;
  kind?: FileIconKind;
  open?: boolean;
  size?: "sm" | "md";
  class?: string;
}) {
  const dimension = () => (props.size === "sm" ? 14 : 16);
  const flavor = () => flavorOf(effectiveIconTheme());
  return (
    <If when={flavor()} else={
      <Icon name={monochromeIcons[props.kind ?? "file"]}
        size={props.size ?? "md"} color="muted-foreground" class={props.class} />
    }>
      {(activeFlavor) => (
        <Image
          src={catppuccinDataUri(
            resolveCatppuccinIcon(props.path, props),
            activeFlavor(),
          )}
          fadeDuration={0}
          accessibilityElementsHidden
          class={props.class}
          style={{ width: dimension(), height: dimension() }}
        />
      )}
    </If>
  );
}
