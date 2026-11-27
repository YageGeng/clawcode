import { Editor } from "nsh/base";

/** Read-only syntax-highlighted viewer for workspace file previews. */
export function CodePreview(props: {
  value: string;
  path: string;
  accessibilityLabel: string;
  placeholder?: string;
}) {
  return (
    <Editor
      class="min-h-0 flex-1 bg-background"
      accessibilityLabel={props.accessibilityLabel}
      value={props.value}
      language={props.path}
      placeholder={props.placeholder}
      readOnly
      activeLine={false}
      autoClose={false}
      autoIndent={false}
      softWrap
      scrollBeyondLastLine={false}
      classes={{
        lineNumber: "text-muted-foreground",
        foldMarker: "text-muted-foreground",
        indentGuide: "bg-border/60",
        placeholder: "text-muted-foreground",
      }}
    />
  );
}
