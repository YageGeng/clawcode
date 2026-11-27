# Clawcode desktop design standards

The app's own rules, applied with the NSH Design Guides and spec §5.1. Pages compose `src/ui/` only; a look `ui`
lacks becomes a variant there first. Colors, radii and type sizes come from tokens (`theme.json`), motion from
`src/ui/motion.ts`.

## Visual language

A coding-agent workbench modelled on the layout and hierarchy of the ChatGPT desktop app (Codex): quiet, light and
roomy, with content (the conversation, diffs, tool output) carrying the screen. We take its layout, hierarchy and
interaction patterns, never its logo, icons or brand elements.

- A near-white app canvas (`sidebar`) frames a pure white main area (`background`); dark mode mirrors it.
- Almost no borders: hierarchy comes from space, type size and grey levels. Secondary text is the foreground at about
  half opacity (`muted-foreground`).
- One strong action color (`primary`, near-black; near-white in dark) for the send button and primary buttons. Links
  use `link`. Status colors (`success`, `warning`, `destructive`, `info`) appear only for their meaning, always with
  an icon or a label.
- Soft geometry: large radii, pill-shaped selection, a single faint ring plus a light shadow on floating surfaces.

## Platforms

Desktop family: Linux (Wayland, GTK 4), macOS and Windows. Pointer and keyboard first: hover on everything pressable, visible
focus rings, Escape dismisses the topmost surface, tooltips on icon-only controls. Mobile (as a remote control later)
and TUI keep the components' semantics and change presentation only.

## Scales

| Scale | Values |
| --- | --- |
| Type | conversation body 15pt / 24 (`type.body`); UI text 14pt / 20 (`type.ui`); metadata 13pt / 18 (`type.meta`); empty-state heading 28pt / 36 (`type.heading`) |
| Spacing | 2, 4, 8, 12, 16, 24, 32 (Tailwind steps `0.5`, `1`, `2`, `3`, `4`, `6`, `8`) |
| Control height | `sm` 24, `md` 32 (default) |
| Rows | sidebar and menu rows 32; dense lists 28 |
| Icons | 14 in `sm` controls and dense rows, 16 elsewhere; Lucide for UI controls (`ui/icon`), Catppuccin file/folder icons (`ui/fileIcon`, Latte in light, Mocha in dark) for workspace paths |
| Radius | `radius` 10 for controls; `radius-lg` 16 for popovers, cards and dialogs; `radius-xl` 22 for the composer card; `rounded-full` for pills, dots and round buttons |

Related label/control pairs use 4–8; groups 12–16; sections 24–32.

## Color roles

| Token | Role |
| --- | --- |
| `background` | Main area |
| `sidebar`, `sidebar-foreground` | App canvas, sidebar |
| `sidebar-accent` | Selected sidebar pill |
| `foreground`, `muted-foreground` | Text, secondary text |
| `muted`, `secondary` | Quiet fills: user message pill, context strip, chips |
| `accent` | Hover wash |
| `primary`, `primary-foreground` | Send button and primary actions |
| `popover`, `card` | Floating surfaces |
| `border`, `input`, `ring` | Faint rings, field outlines, focus |
| `link` | Links in the conversation |
| `success`, `warning`, `destructive`, `info`, `added`, `removed` | Status and diff counts only |

## Interaction states

Every pressable control has, from tokens: hover (an `accent` wash, `instant`), pressed (scale 0.97, icon buttons
0.95, `fast`, soft settle), keyboard focus (a 2pt `ring` outside the border, `focus-visible:` only), selected (shares
the open-menu style; sidebar rows use the `sidebar-accent` pill), disabled (50% opacity, no hover or press) and
loading (a spinner in place of the icon, same size). Invalid fields show a `destructive` outline and the error text
under the field. Row actions (pin, archive) appear on hover or keyboard focus and are also reachable from the row's
menu, never hover-only.

## Shell layout (N1 builds it)

- **Window:** the title bar holds only the sidebar toggle and back/forward. No icon rail; settings sit at the bottom
  of the sidebar. No top-level session tabs: sessions are picked in the sidebar; tabs exist only in the right panel.
  If NSH cannot customize the Linux title bar, the system title bar stays and these buttons move to the top of the
  sidebar.
- **Sidebar** (about 280pt, `sidebar`): host switcher ("This computer ▾", listing paired remote hosts); New session;
  Workspaces grouped by directory, each expanding to its sessions; Recent. Rows are 32pt pills; trailing marks show
  running and needs-attention states.
- **Session view** (`background`): header with the workspace icon and title, more and panel toggles on the right. The
  transcript is one centered column at most about 760pt wide. User messages are right-aligned `secondary` pills;
  the latest assistant reply appears directly below the work and streams in place. Under "Working" or "Worked
  for X", thoughts, interim messages and named tool calls start folded in event order. Opening a tool reveals
  its input and result. Copy and timestamp sit under the finished reply. A separate disclosure lists changed
  files. A round button returns to the bottom.
- **Empty state:** a centered icon and a `type.heading` line "What should we build in *workspace*?", the workspace
  name opening a switcher.
- **Composer:** a `floating` surface card at the bottom center. For a new session a context strip above it picks
  host / workspace / more. Footer: `+` attachments and the approval-mode menu on the left; the model and thinking
  popover and a round send button (stop while running) on the right.
- **Right panel:** tabs (Changes now; Files and Terminal later) with `+` opening a tile page. Changes: turn scope
  ("Last turn +6 −1"), file header with path and counts, unified diff with unchanged runs folded. With the panel
  closed, a small floating card at the top right shows "Changes: repo · branch +N −N".
- **Popovers:** menus, selects, the approval-mode menu and the model popover share one surface, optionally with a
  title row and per-item descriptions.

## Motion tokens

| Token | Value | For |
| --- | --- | --- |
| `instant` | 120ms | Hover washes, color changes |
| `fast` | 180ms | Presses, a control's state |
| `base` | 240ms | A selection sliding, a glyph swapping, a popup leaving |
| `slow` | 360ms | A menu, popover or dialog opening |
| `slower` | 500ms | A list's entrance wave |
| Easings | ease-out entering, ease-in-out moving, ease-in leaving, settle `cubic-bezier(0.22, 1, 0.36, 1)` for presses | |
| Distances | 4 / 8 / 16pt | A label rising / a popup sliding / items entering |
| Scales | press 0.97 (icon 0.95), enter 0.96 | |

Reduced motion (`motion-reduce:`) makes every line instant: no scale, no slide, popups appear at once.

A plain view's entrance is a motion, not a class: NSH 0.6.1 has no `starting:` variant on views (only popups take
`*StartingStyle`). `ui`'s `Appear` plays one (`createMotion({ enter })`: rise 8pt `slow`, rise 4pt `fast`, grow from
0.96 `base`, fade `fast`), which lands at once under reduced motion. Rows of a virtualized list play it only when
the content is new (a running turn), since the list remounts rows as they scroll back into view.

## Motion inventory: component gallery

| Change | Motion | Reduced motion |
| --- | --- | --- |
| Button / IconButton hover | `accent` wash in, `instant`, ease-out | Instant |
| Button / IconButton press | Scale 0.97 (icon 0.95), `fast`, settle | No scale |
| CopyButton copied | Copy glyph swaps to check (old fades out, new fades in), `base`; reverts after 1.5s | Instant swap |
| Checkbox toggles | Check mark switches immediately without fade; box color wash `instant` | Instant |
| Switch toggles | Thumb slides across the track, `base`, ease-in-out; track color `instant` | Thumb jumps |
| Tabs selection | One indicator slides to the chosen tab (base `Tabs` indicator), `base`, ease-in-out | Jumps |
| Select / Menu / Popover open | Appears and leaves at once (no fade-in or fade-out). A title row and item descriptions add no motion of their own | Appears and leaves at once |
| Submenu open | Appears and leaves at once beside the parent row | At once |
| Tooltip | Fades in 4pt toward the trigger, `fast`; leaves `instant` | At once |
| Dialog open | Scrim fades `slow`; panel rises 8pt from 0.96, `slow`, ease-out; leaves `base`, ease-in | At once |
| Sheet open | Slides in from its side, `slow`, ease-out; leaves `base` | At once |
| Toast arrives / leaves | Slides 16pt in from the bottom edge and fades, `slow`; leaves faster, `base`; the stack's springs move the others | At once |
| ListItem hover and selection | Hover wash `instant`; the selected pill switches `instant` (rows are dense data, no slide) | Instant |
| ListItem row actions | Fade in on hover or keyboard focus, `instant` | Appear at once |
| TreeList expand | Chevron rotates 90°, `fast` | Instant |
| Skeleton | Slow opacity pulse; content replaces it | No pulse |
| Spinner | Continuous rotation while loading | Rests |
| StatusDot running | Soft opacity pulse while running | Rests |
| Layout changes, drags | None in the gallery | — |

## Motion inventory: shell (title bar and sidebar)

| Change | Motion | Reduced motion |
| --- | --- | --- |
| Sidebar collapses / expands | `LayoutAnimation` for that update, `base`, ease-in-out; the main area widens with it | Jumps |
| Title bar buttons hover / press | As IconButton (wash `instant`, press 0.95 `fast`) | No scale |
| Back / Forward become enabled or disabled | Opacity `instant` | Instant |
| Current session changes | The selected pill switches `instant` (dense list, no slide) | Instant |
| Workspace group expands / collapses | Chevron rotates 90° `fast`; rows appear with `LayoutAnimation` `base` | Instant |
| Host menu opens | Shared instant popup surface (appears and leaves at once, no fade) | At once |
| A session starts or stops running | Spinner appears / leaves at once; spins while running | Spinner rests |
| A session needs approval | Status dot appears `instant`, in place of the running spinner (waiting on the user wins) | Instant |
| The host goes offline (its backend stopped) | The reason appears at once above the Session, which stays; spinners stop | Same |
| Another session opens | The session view is built anew for it (no draft, scroll or jump state carries over); no transition | Same |
| Settings dialog opens | Dialog motion (scrim fade, rise 8pt, `slow`) | At once |
| Empty state appears (no session open) | Heading and composer fade in, `fast` | At once |

## Motion inventory: session view

| Change | Motion | Reduced motion |
| --- | --- | --- |
| A new turn's user pill arrives | `Appear` rise: 8pt and fades in, `slow`, ease-out (running turns only, not replays or remounted rows) | Appears |
| Reply streams in | Reply text grows in place. A caret marks the running turn. | Caret rests |
| Assistant media or a recorded question decision arrives | Media appears between the surrounding text. Answers and skips appear in user bubbles at their transcript position. | Same |
| Work, thought, interim message, tool or changed files toggled | The chevron rotates 90° and the next layout uses the shared disclosure transition. Opening a parent restores its children's selections. | Instant |
| A turn ends | The duration replaces the working label. Open sections keep their state and the reply gains copy and time. | Same |
| File-change card appears | Appears with its turn (no motion of its own: the row remounts on scroll) | Appears |
| Jump-to-bottom button appears / leaves | Fades and rises 4pt, `fast`; leaves `instant` | Appears and leaves at once |
| Copy pressed | CopyButton glyph swap (`base`) | Instant swap |
| Scrolling while a reply streams | The list keeps the reader's position (native); at the bottom it follows | Same |

## Motion inventory: composer and approval card

| Change | Motion | Reduced motion |
| --- | --- | --- |
| Send becomes stop (a turn starts) and back | The incoming round button grows from 0.96 and fades in (`Appear` grow, `base`); not on the composer's first show | Instant swap |
| Send / stop / `+` pressed | Icon-button press scale (0.95, settle, `fast`) | No scale |
| Send enables as text arrives | Opacity wash, `instant` | Instant |
| The field grows from 2 to 8 lines | Grows with its text, no animation; past 8 lines it scrolls to the caret | Same |
| Approval card arrives | `Appear` rise: 8pt above the composer and fades in, `slow`, ease-out | Appears |
| Approval card answered | Fades out while the composer settles with `LayoutAnimation` `base`; withdrawn by the agent, it leaves at once | Leaves at once |
| A slash prefix opens command suggestions | The matching rows appear immediately above the field without fade. Arrow keys move selection within the list, and choosing a command returns focus to its draft. | The rows appear immediately; selection and focus stay visible. |
| Context strip (new session) | Fades in with the empty state, `fast`; leaves with it when the Session opens | At once |
| A failed send's text comes back | The field's text is replaced in place; a toast slides in (toast motion) | Toast appears |

## Check record

Gallery and app shell checked 2026-11-25 on Linux (GTK 4.24, `nsh test … --target linux`), light and dark, after
the §5.1 tokens.

| Group | Result |
| --- | --- |
| Radius | Passed: controls at `radius` 10, popovers, cards and dialogs at `radius-lg` 16, the composer sample at `radius-xl` 22, badges and dots round |
| Typography | Passed: UI text 14, metadata 13, menu descriptions 13 under 14pt titles; section titles one weight step up |
| Spacing | Passed: menu rows with descriptions breathe (6pt vertical padding); sidebar rows 32pt |
| Alignment | Passed: menu icons, titles and the trailing check align on one grid; long sidebar titles truncate on one line |
| Color | Passed: near-white canvas (`sidebar`) beside the white main area; dark mirrors it; `muted-foreground` at half opacity stays readable on `popover` in dark; the full-access item reads as a warning in both |
| Hover / pressed | Classes in place; not visible in stills (hover-revealed row actions verified hidden at rest). To check by hand in `nsh run` |
| States | Passed: selected pill, checked menu item, disabled and loading buttons, invalid field distinct in both appearances |
| Overlays | Passed: the approval menu opens above its trigger when there is no room below, sharing the one popover surface |
| Motion | Not verified live (stills only); inventory lines are wired through `motionClass` |

### N1 check record

Shell, session view, composer and approval card checked 2026-11-25 on Linux (GTK 4, KDE Wayland, `nsh test … --target
linux` against the scripted host entries), light and dark, English and Chinese.

| Group | Result |
| --- | --- |
| Radius | Passed: composer card `radius-xl`, approval card and dialog `radius-lg`, user pill 18, send / stop and jump buttons round, sidebar pills `radius` |
| Typography | Passed: heading on the empty state, body 15 for replies and the pill, UI 14 for the field and rows, meta 13 for "Worked for", timestamps and section titles |
| Spacing | Fixed: the empty-state "?" floated 12pt from the picker; picker and closing words now form one group, 8pt on both sides, so "?" never wraps alone |
| Alignment | Fixed: the sidebar host switcher sat centered (its wrapper stretched the trigger); it now starts on the row grid. Work summary, steps and answer share one left edge |
| Color | Passed: light canvas beside the white main area, dark mirrors it; primary buttons invert in dark; disabled send at half opacity |
| Hover / pressed | Not visible in stills; not yet checked by hand (Task 7 Step 3, the real `nsh run` pass, is deferred) |
| States | Passed: running spinner on sidebar rows, selected pill, send disabled → enabled → stop while a turn runs, streaming caret after "Working", approval card above the composer |
| Overlays | Passed: settings dialog centered over the scrim; approval card rises above the composer without covering it |
| Motion | Not verified live (stills only); `Appear` entrances replaced the unsupported `starting:` classes |
| Language | Fixed: "This computer" was the host's data name; the local host now reads `shell.thisComputer` ("这台电脑"). Chinese copy fits every control |
| Composer | The field grows to 8 lines and stops. Caret-follow past 8 lines and Return-to-send are checked by hand: on Wayland the test agent inserts text and line breaks into the buffer (no XTest), bypassing the key path and the view's scroll-to-caret |

On Linux, NSH 0.7.1 shapes text and reloads its font for drawing on a second thread. A fontconfig substitution can
resolve that description to a different face. On the affected English-locale desktop, this left Chinese table headers
and prose blank while Latin text remained legible. At startup the app probes the system sans and monospace faces
independently. If a face fails the round trip, the app uses the first installed fallback that covers the sample Han
characters and punctuation and reloads as the same face. To check the result visually from this directory, run the
following under the affected locale and inspect the table headers, mixed-language body and inline Chinese code.

```sh
GDK_BACKEND=wayland LC_ALL=en_US.UTF-8 nsh test tests/entries/markdown-cjk.tsx --target linux <<'STEPS'
await app.setViewport(620, 340)
await app.screenshot("target/markdown-cjk-verified.png")
STEPS
```

With NSH 0.7.1 on Wayland, the title bar uses GTK's native window buttons with 32-point hit areas and line icons.
Light and dark window captures covered the resting state. A Wayland window using the same controls and stylesheet
confirmed maximize, restore, minimize and close. In a separate KWin Wayland session, fcitx5 Pinyin received key
events in the desktop composer. The first Enter committed the preedit to the draft and the next Enter sent it.

## Motion inventory: remote control

| Change | Motion | Reduced motion |
| --- | --- | --- |
| Open the computer menu or select a paired computer | Existing menu appears and leaves at once without fade. The selected computer's workspace and sessions appear together. | Instant |
| Open sharing settings, pairing form, invitation or confirmation | One desktop dialog fades in. An incoming request takes focus and names this computer as the target. Closing returns to the preceding view. | Instant |
| Enable sharing, approve, disconnect, revoke or try again | The pressed control uses its normal wash and press scale. Pending actions disable repeats. Connection counts, device rows and errors update in place. | Instant, status text remains visible |
| Show a pairing code | The QR appears at full contrast with a quiet zone. Its expiration is shown in text. Copy uses the shared clipboard feedback. | Same |
| Lose a remote connection | The selected computer shows its offline reason and retry action immediately. A cached conversation stays readable. | Same |

### R1 Linux check record

Checked the production remote-control dialog on GTK/Wayland at 800 × 560 in English light mode and Simplified Chinese dark mode. Both languages fit the form, use the existing control and dialog radii, and align the name, switch, and device list to the same left edge. The empty, connected, disconnected, and revoked states have text or an action that identifies them. A camera decoder read the invitation directly from the rendered QR at the minimum window size.

Escape now returns from the invitation to the device list, then to Settings. The real GTK input path and a focused headless regression test cover that sequence. The two-window check exercised pairing, permission prompts, and revocation. Still images cover layout, colors, text, and resting states. Screen-reader narration, reduced-motion playback, and pointer hover/press timing have not been observed.

## Motion inventory for N2 session configuration

| Interaction | Feedback | Reduced motion |
| --- | --- | --- |
| Open session settings | The popup uses the shared instant surface with no fade transition. Keyboard focus enters the choices and returns on dismissal. | Selection and focus remain visible immediately. |
| Change a model, thinking level or mode | The current choice follows the host response. Disable overlapping edits while saving and keep the previous selection visible on failure. | Same feedback using reduced-motion settings. |
| Switch hosts or sessions | Show the selected session's settings with its composer. A closed connection leaves the last confirmed values readable. | Same. |
| Adjust external file approvals | A checked menu row identifies the current policy. The policy can be changed during a running turn. The automatic-allow choice uses the warning role and describes the file access it grants. | Same selection and text. |
| Answer a question or review a plan | A question card enters above the composer without taking keyboard focus. If another client answers an earlier question, keep the selected question and its draft in view. A disclosure reveals plan details. Long content scrolls separately from the answer buttons. | Omit entrance movement. |
| Choose a slash command | Suggestions update as the user types. Selecting one inserts its text into the draft and returns focus to the input. | Same. |
| Add or remove a picture | Show selected pictures together above the draft with individual removal buttons. Reading or failure is indicated in the composer. | Same feedback without movement. |

### N2 Linux check record

Two production GTK windows on Wayland used independent host profiles. The controller changed a remote session's model, thinking level, mode and external-write policy. The model picker showed the full model name at 800 × 560. In Simplified Chinese dark mode, all three file policy descriptions wrapped inside the menu and the check followed the active option. Keyboard steps and pointer dragging both changed the thinking level.

The production question flow kept the message draft visible while awaiting an answer. A proposed plan showed its Markdown above the separate decision buttons. The accepted plan could be reopened after the turn. The native file picker produced a removable thumbnail and the paired host received the selected image. The [N2 acceptance record](../../docs/superpowers/plans/2026-11-26-desktop-nsh-n2.md) lists the session, command and attachment checks.

## N3 change-card inspector

| Interaction | Feedback | Reduced motion |
| --- | --- | --- |
| 展开本轮文件变更或开始审查 | 通栏摘要左侧显示文件数和累计增删行数，箭头随文件列表开合旋转。右侧「审查」直接打开本轮 diff。 | 保留摘要和两个操作，立即切换展开状态。 |
| Open View changes | The file card opens the Changes inspector at the right. The conversation and its draft keep their place. A narrow session gives the inspector the available width. | Change the layout immediately. |
| Inspect another file or edit | The card totals all edits to the file in the turn. The inspector shows the selected edit's counts and unified diff. Previous and next step through the edits in order. Receiving the same edit again preserves the reader's scroll position. | Update the content immediately. |
| Close the inspector | The close button restores the conversation width. Dragging the wide layout's divider adjusts the split directly. | Restore the layout immediately. |

The GTK session fixture on Wayland showed the Chinese inspector beside the conversation at 1200 × 800 in both themes. Dragging the divider resized it. At 800 × 720 the diff filled the session area, and Escape returned to the expanded file card.

### N3 文件浏览与预览

| 交互 | 界面反馈 | 减少动态效果时 |
| --- | --- | --- |
| 从会话顶栏打开工作区面板 | 宽窗口并排展示会话与面板（即时开合，无跨栏位移动画），窄窗口用返回按钮回到会话 | 直接切换布局 |
| 在「文件」和「变更」之间切换 | 单一底边线上的标签指示线平滑滑动，显示已变更文件数徽标，保留当前文件预览和已选 diff | 直接切换标签 |
| 展开目录或选择文件 | 目录展开采用原生弹簧推进与箭头旋转；按需读取所属主机内容，在下方可拖拽分栏中即时打开/收起只读预览（无向上淡入淡出，支持一键复制）；空文件、二进制、大文件（附大小）和读取失败显示对应状态 | 无位移动画，同样显示读取状态 |
| 在单文件 diff 与变更列表间往返 | Diff 子顶栏左侧返回按钮退回按轮次分组的变更卡片列表，便于在窄窗口或多文件会话中切换文件 | 直接切换视图 |

Wayland 同机双实例验收使用独立的设备身份和后端数据目录。控制端配对后从远程会话展开目录并读到了主机文件内容。1200 × 800 浅色窗口并排显示会话与预览，800 × 560 深色窗口中返回按钮恢复会话。原生交互测试覆盖快速切换预览、返回同一次编辑以及保留未发送草稿。
