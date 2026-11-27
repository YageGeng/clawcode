import { ResizablePanel, ResizablePanelGroup } from "nsh/base";
import type { JSX } from "nsh/jsx-runtime";

/** Resizable split container sharing one hairline and hover grip treatment across horizontal and vertical axes. */
export function SplitGroup(props: {
  axis?: "horizontal" | "vertical";
  handleAccessibilityLabel: string;
  class?: string;
  children: JSX.Element;
}) {
  return (
    <ResizablePanelGroup
      axis={props.axis ?? "horizontal"}
      class={props.class ?? "flex-1"}
      handleClass="bg-border group-hover:bg-ring group-active:bg-ring transition-colors duration-120 motion-reduce:transition-none"
      handleThumbClass="h-6 w-1 rounded-full bg-muted-foreground opacity-0 group-hover:opacity-60 group-active:opacity-100 transition-opacity duration-120 motion-reduce:transition-none"
      handleAccessibilityLabel={props.handleAccessibilityLabel}
    >
      {props.children}
    </ResizablePanelGroup>
  );
}

/** One pane inside a `SplitGroup`. */
export function SplitPane(props: {
  size?: number;
  minSize?: number;
  maxSize?: number;
  fixed?: boolean;
  visible?: boolean;
  onResize?: (size: number) => void;
  class?: string;
  children: JSX.Element;
}) {
  return (
    <ResizablePanel
      size={props.size}
      minSize={props.minSize}
      maxSize={props.maxSize}
      fixed={props.fixed}
      visible={props.visible}
      onResize={props.onResize ? ({ size }) => props.onResize?.(size) : undefined}
      class={props.class}
    >
      {props.children}
    </ResizablePanel>
  );
}
