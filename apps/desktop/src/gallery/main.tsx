import { RemoteHosting } from "clawcode-backend";
import { QrCode } from "../ui/qr";
import { mount, Scroll, setColorScheme, signal, Text, View } from "nsh";
import type { JSX } from "nsh/jsx-runtime";

import { Button, IconButton } from "../ui/button";
import type { ButtonVariant } from "../ui/button";
import { CopyButton } from "../ui/copy";
import { configureDesktopFonts } from "../ui/fonts";
import { confirm, Dialog, Sheet } from "../ui/dialog";
import { MessageBubble } from "../ui/bubble";
import { Disclosure } from "../ui/disclosure";
import { Empty, Skeleton, Spinner } from "../ui/feedback";
import { FileIcon } from "../ui/fileIcon";
import { Markdown } from "../ui/markdown";
import { Field, Input, Textarea } from "../ui/input";
import { ListItem, TreeList } from "../ui/list";
import { Menu, MenuItem, MenuSeparator, Popover, Submenu, Tooltip } from "../ui/menu";
import { Checkbox, Segmented, Select, Switch } from "../ui/selection";
import { Badge, LineCounts, Separator, StatusDot, Surface } from "../ui/surface";
import { Tabs } from "../ui/tabs";
import { Toasts, ToastStore } from "../ui/toast";
import { type } from "../ui/tokens";

/** One titled block of the gallery. */
function Section(props: { title: string; children: JSX.Element }) {
  return (
    <Surface>
      <Text class={`${type.ui} font-semibold text-card-foreground`}>{props.title}</Text>
      <View class="flex-row flex-wrap items-center gap-2">{props.children}</View>
    </Surface>
  );
}

const variants: ButtonVariant[] = ["primary", "secondary", "subtle", "ghost", "outline", "danger"];

/** Every `ui` component in its variants, sizes and states, for review in light and dark. */
function Gallery() {
  const presses = signal(0);
  const dark = signal(false);
  const draft = signal("");
  const prompt = signal("");
  const sent = signal("");
  const composerDraft = signal("");
  const checked = signal(true);
  const on = signal(false);
  const model = signal<string | null>("fast");
  const tab = signal("transcript");
  const dialogOpen = signal(false);
  const sheetOpen = signal(false);
  const confirmed = signal("No answer yet");
  const lastCommand = signal("No command yet");
  const approval = signal("auto");
  const workOpen = signal(false);
  const expanded = signal<string[]>(["src"]);
  const selectedNode = signal<string | null>("src/main.rs");
  const toasts = new ToastStore();

  return (
    <View class="flex-1 bg-background">
      <View class="h-12 flex-row items-center justify-between border-b border-border px-4">
        <Text class="text-base font-semibold text-foreground">Component gallery</Text>
        <IconButton
          icon={dark() ? "sun" : "moon"}
          label={dark() ? "Light appearance" : "Dark appearance"}
          onPress={() => {
            dark.update((value) => !value);
            setColorScheme(dark() ? "dark" : "light");
          }}
        />
      </View>
      <Scroll class="flex-1" contentClass="gap-3 p-4">
        <Section title="Button">
          <Each of={variants}>{(variant) => <Button variant={variant()} label={`${variant()[0]?.toUpperCase()}${variant().slice(1)}`} onPress={() => presses.update((n) => n + 1)} />}</Each>
          <Button variant="primary" label="Small" size="sm" icon="plus" />
          <Button variant="secondary" label="Selected" selected />
          <Button variant="primary" label="Disabled" disabled onPress={() => presses.update((n) => n + 1)} />
          <Button variant="primary" label="Loading" loading />
          <Text class={`${type.meta} text-muted-foreground`}>Pressed {presses()}</Text>
        </Section>
        <Section title="IconButton">
          <IconButton icon="settings" label="Settings" tooltip={<Tooltip text="Settings" />} />
          <IconButton icon="plus" label="New session" variant="outline" />
          <IconButton icon="ellipsis" label="More" size="sm" />
          <IconButton icon="x" label="Close" disabled />
          <IconButton icon="search" label="Search" loading />
          <CopyButton text="cargo test --workspace" />
        </Section>
        <Section title="Input">
          <View class="flex-row items-start gap-3">
          <Field label="Session title" description="Shown in the sidebar.">
            <Input value={draft()} placeholder="Untitled" onChange={(value) => draft.set(value)} class="w-64" />
          </Field>
          <Field label="Model id" error="Use provider/model.">
            <Input value="deepseek" invalid onChange={() => {}} class="w-64" />
          </Field>
          <Field label="Prompt" description="Enter sends, Shift+Enter adds a line.">
            <Textarea
              value={prompt()}
              placeholder="Ask anything"
              accessibilityLabel="Prompt"
              maxLines={8}
              onChange={(value) => prompt.set(value)}
              onSubmit={() => {
                sent.set(prompt());
                prompt.set("");
              }}
              class="w-96"
            />
          </Field>
          </View>
          <Text class={`${type.meta} text-muted-foreground`}>Sent: {sent() === "" ? "nothing" : sent()}</Text>
        </Section>
        <Section title="Selection">
          <Checkbox label="Auto-approve reads" checked={checked()} onChange={(value) => checked.set(value)} />
          <Checkbox label="Disabled" checked={false} disabled onChange={() => {}} />
          <Switch label="Thinking" checked={on()} onChange={(value) => on.set(value)} />
          <Segmented
            accessibilityLabel="View mode"
            items={[
              { value: "fast", label: "Conversation", icon: "message-square" },
              { value: "deep", label: "Trajectory", icon: "chart-gantt" },
            ]}
            value={model() === "deep" ? "deep" : "fast"}
            onChange={(value) => model.set(value)}
          />
          <Select
            items={[
              { value: "fast", title: "Fast model" },
              { value: "deep", title: "Deep model" },
              { value: "off", title: "Unavailable", disabled: true },
            ]}
            value={model()}
            accessibilityLabel="Model"
            onChange={(value) => model.set(value)}
            class="w-48"
          />
        </Section>
        <Section title="Tabs">
          <Tabs
            accessibilityLabel="Panel"
            items={[
              { value: "transcript", label: "Transcript" },
              { value: "changes", label: "Changes", badge: 3 },
              { value: "trajectory", label: "Trajectory" },
              { value: "context", label: "Context", disabled: true },
            ]}
            value={tab()}
            onChange={(value) => tab.set(value)}
          />
          <View class="h-12 flex-row items-center border-b border-border px-3">
            <Tabs
              variant="bar"
              accessibilityLabel="Workspace panel"
              items={[
                { value: "transcript", label: "Files" },
                { value: "changes", label: "Changes", badge: 2 },
              ]}
              value={tab() === "changes" ? "changes" : "transcript"}
              onChange={(value) => tab.set(value)}
            />
          </View>
          <Text class={`${type.meta} text-muted-foreground`}>Showing {tab()}</Text>
        </Section>
        <Section title="Menu">
          <Menu trigger={<Button label="Session actions" variant="outline" icon="ellipsis" />}>
            <MenuItem label="Rename" icon="settings" onSelect={() => lastCommand.set("Rename")} />
            <MenuItem label="Copy id" icon="copy" shortcut="Ctrl+C" onSelect={() => lastCommand.set("Copy id")} />
            <Submenu label="Move to">
              <MenuItem label="Archive" onSelect={() => lastCommand.set("Archive")} />
              <MenuItem label="Pinned" onSelect={() => lastCommand.set("Pinned")} />
            </Submenu>
            <MenuSeparator />
            <MenuItem label="Delete" icon="x" danger onSelect={() => lastCommand.set("Delete")} />
          </Menu>
          <Text class={`${type.meta} text-muted-foreground`}>Last command: {lastCommand()}</Text>
          <Menu title="How should actions be approved?" trigger={<Button label="Approval mode" variant="ghost" icon="shield-check" />}>
            <MenuItem label="Ask" description="Ask before editing files outside the workspace" icon="hand" checked={approval() === "ask"} onSelect={() => approval.set("ask")} />
            <MenuItem label="Auto-review" description="Ask only for risky actions" icon="shield-check" checked={approval() === "auto"} onSelect={() => approval.set("auto")} />
            <MenuItem label="Full access" description="Run anything without asking" icon="triangle-alert" warning checked={approval() === "full"} onSelect={() => approval.set("full")} />
          </Menu>
          <Popover trigger={<Button label="Popover" variant="ghost" />} class="w-64 gap-2">
            <Text class={`${type.ui} text-popover-foreground`}>Context window</Text>
            <Text class={`${type.meta} text-muted-foreground`}>32,768 tokens, 12% used.</Text>
          </Popover>
        </Section>
        <Section title="Dialog">
          <Button label="Open dialog" onPress={() => dialogOpen.set(true)} />
          <Button label="Open sheet" onPress={() => sheetOpen.set(true)} />
          <Button
            label="Delete session"
            variant="danger"
            onPress={() => {
              void confirm({ title: 'Delete "Roadmap"?', description: "Its history is removed.", action: "Delete", danger: true }).then((yes) =>
                confirmed.set(yes ? "Deleted" : "Kept"),
              );
            }}
          />
          <Text class={`${type.meta} text-muted-foreground`}>{confirmed()}</Text>
        </Section>
        <Section title="Toast">
          <Button label="Show success" onPress={() => toasts.show({ title: "Session saved", tone: "success" })} />
          <Button label="Show error" variant="outline" onPress={() => toasts.show({ title: "Backend stopped", description: "Retry from the status bar.", tone: "danger" })} />
        </Section>
        <Section title="ListItem">
          <View class="w-[280px] gap-0.5 rounded-(--radius-lg) bg-sidebar p-2">
            <ListItem label="Refactor the parser" description="clawcode · 2 min ago" icon="search" selected />
            <ListItem label="A very long session title that keeps going well past the width of the sidebar row" />
            <ListItem
              label="Hover shows row actions"
              actions={
                <>
                  <IconButton icon="pin" label="Pin" size="sm" />
                  <IconButton icon="archive" label="Archive" size="sm" />
                </>
              }
            />
            <ListItem label="Fix flaky test" description="clawcode · 1 h ago" trailing={<Badge label="Running" tone="info" />} />
            <ListItem label="Archived" disabled />
          </View>
        </Section>
        <Section title="TreeList">
          <TreeList
            accessibilityLabel="Files"
            class="h-40 w-72"
            items={[
              { id: "src", label: "src", children: [{ id: "src/main.rs", label: "main.rs" }, { id: "src/lib.rs", label: "lib.rs" }] },
              { id: "tests", label: "tests", children: [{ id: "tests/app.rs", label: "app.rs" }] },
              { id: "Cargo.toml", label: "Cargo.toml" },
            ]}
            expanded={expanded()}
            selected={selectedNode()}
            onExpandedChange={(value) => expanded.set(value)}
            onSelectedChange={(id) => selectedNode.set(id)}
          />
          <View class="flex-row flex-wrap items-center gap-2.5 rounded-(--radius) border border-border bg-background px-3 py-2">
            <FileIcon path="src" kind="directory" open />
            <FileIcon path="Cargo.toml" />
            <FileIcon path="main.rs" />
            <FileIcon path="App.tsx" />
            <FileIcon path="App.test.tsx" />
            <FileIcon path="theme.json" />
            <FileIcon path="en.yml" />
            <FileIcon path="Dockerfile" />
            <FileIcon path="README.md" />
          </View>
        </Section>
        <Section title="Surface">
          <Badge label="Draft" />
          <Badge label="Passed" tone="success" />
          <Badge label="Pending" tone="warning" />
          <Badge label="Failed" tone="danger" />
          <Separator vertical />
          <StatusDot tone="success" label="Connected" />
          <StatusDot tone="info" label="Running" live />
          <StatusDot tone="danger" label="Stopped" />
          <LineCounts added={42} removed={7} />
          <Surface variant="floating" class="w-[480px]">
            <View class="flex-row gap-1 self-start rounded-full bg-secondary p-0.5">
              <Button label="This computer" icon="laptop" variant="ghost" size="sm" />
              <Button label="clawcode" icon="folder" variant="ghost" size="sm" />
            </View>
            <Textarea
              variant="bare"
              value={composerDraft()}
              placeholder="Ask for follow-up changes"
              accessibilityLabel="Composer sample"
              onChange={(value) => composerDraft.set(value)}
            />
            <View class="flex-row items-center justify-between">
              <IconButton icon="plus" label="Attach files" disabled />
              <IconButton icon="arrow-up" label="Send" variant="primary" round disabled={composerDraft() === ""} />
            </View>
          </Surface>
        </Section>
        <Section title="Conversation">
          <View class="w-[560px] gap-3">
            <MessageBubble text="Fix the build, please." />
            <Disclosure label="Worked for 3m 10s" open={workOpen()} onToggle={() => workOpen.update((open) => !open)}>
              <Markdown text="Ran `cargo build`; the **clipboard** module failed." />
            </Disclosure>
            <Markdown text={"Fixed it. See [the change](https://example.com).\n\n```rust\nlet atom = Atom::from(x);\n```"} />
          </View>
        </Section>
        <Section title="QR code">
          <QrCode uri={RemoteHosting.qr("Clawcode gallery")} />
        </Section>
        <Section title="Feedback">
          <Spinner label="Loading" />
          <View class="w-48 gap-2">
            <Skeleton />
            <Skeleton class="h-4 w-2/3" />
          </View>
          <Empty title="No sessions yet" description="Start one from the composer." icon="plus" action={<Button label="New session" variant="primary" size="sm" />} />
        </Section>
      </Scroll>
      <Dialog
        open={dialogOpen()}
        title="Rename session"
        onClose={() => dialogOpen.set(false)}
        actions={
          <>
            <Button label="Cancel" variant="ghost" onPress={() => dialogOpen.set(false)} />
            <Button label="Rename" variant="primary" onPress={() => dialogOpen.set(false)} />
          </>
        }
      >
        <Input value={draft()} placeholder="Untitled" onChange={(value) => draft.set(value)} />
      </Dialog>
      <Sheet open={sheetOpen()} title="Session details" onClose={() => sheetOpen.set(false)}>
        <Text class="text-[13px] text-popover-foreground">Details go here.</Text>
      </Sheet>
      <Toasts store={toasts} />
    </View>
  );
}

configureDesktopFonts();
mount(Gallery);
