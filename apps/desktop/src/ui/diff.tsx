import { DiffView } from "nsh/base";

import type { RecordedDiff } from "../client/domain/model";

/** Shows the host's patch or version pair in the native, read-only unified viewer. */
export function UnifiedDiff(props: { diff: RecordedDiff; path: string }) {
  return (
    <DiffView class="flex-1 min-h-0 bg-background" layout="unified" indicators="classic"
      contextLines={3} language={props.path} softWrap
      patch={"patch" in props.diff ? props.diff.patch : undefined}
      oldValue={"oldText" in props.diff ? props.diff.oldText : undefined}
      newValue={"newText" in props.diff ? props.diff.newText : undefined}
      classes={{ lineAdded: "bg-added/10", lineDeleted: "bg-removed/10", indicatorAdded: "text-added",
        indicatorDeleted: "text-removed", separator: "bg-muted", lineNumber: "text-muted-foreground" }} />
  );
}
