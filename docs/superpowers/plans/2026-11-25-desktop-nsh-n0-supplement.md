# Desktop NSH 迁移 N0 补充：多主机结构与界面方向 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把桌面应用的状态层改成多主机结构（为 R1 的 `RelayTransport` 留好位置），并按 spec 5.1 把设计系统调整为参考 ChatGPT 桌面应用的界面方向。

**Architecture:**
- 状态层拆成三块：`NavigationHistory`（纯逻辑的前进/后退）、`HostStore`（一台主机的传输、连接阶段与会话列表）、`AppStore`（主机列表、当前位置与导航）。`HostStore` 只依赖 `AcpTransport`，本机用 `NativeTransport`，R1 再加 `RelayTransport`。
- 设计系统：颜色、圆角、字号全部进主题 token；`ui` 组件改用新 token 与字号档位；DESIGN.md 按 5.1 重写；展示页与截图检查跟着更新。

**Tech Stack:** NSH 0.6.1（TSX，`nsh test`），现有 `src/client`、`src/ui`。

**Spec:** [Desktop 客户端迁移到 NSH 设计](../specs/2026-11-25-desktop-nsh-design.md)，重点 §4.4、§5、§6、§8（N0 补充）。

## Global Constraints

- 工作在 worktree `.claude/worktrees/desktop-nsh`，分支 `feat/desktop-nsh`；只在用户明确要求时提交，本计划不含 commit 步骤。
- 真实后端的测试一律 `CLAW_CONFIG=$PWD/tests/fixture/config.toml nsh test`（在 `apps/desktop`）。
- 字号：对话正文 15pt（行高 24）、界面文字 14pt（行高 20）、元信息 13pt（行高 18）、空状态大标题 28pt。
- 圆角：控件 `radius` 10、弹层与卡片 `radius-lg` 16、输入框卡片 `radius-xl` 22；胶囊与圆形用 `rounded-full`。
- 颜色只出现在 `theme.json`；页面与 `ui` 组件只用 token 类或 `token()`。
- 不用 ChatGPT 的 logo、图标与品牌元素；图标仍是 Lucide。
- 界面文案暂用英文源串；双语在 N1 接入 `nsh/i18n` 时做。
- 不在本计划：应用数据目录的环境变量（R1 随设备密钥一起做，现在没有使用者）、`desktop.json` 持久化（N1 设置页）、会话页面本身（N1）。
- AGENTS.md：每个新函数有英文文档注释；前端不强制 TDD，但本计划为 store 逻辑写测试。

## Review Focus

1. 一台主机连不上（远程离线、本机后端启动失败）时，其他主机照常可用。由 Task 3 的 `one_host_failing_leaves_others_ready` 固定。
2. 主机连上了但一直不回应时，界面不能永远停在 Starting。由 Task 2 的 `host_times_out_when_backend_never_answers` 固定。
3. 反复打开同一个位置、或后退后再打开新位置时，前进/后退的历史不出现重复和错乱。由 Task 1 的 `pushing_the_current_location_is_a_no_op` 与 `push_after_back_drops_forward_entries` 固定。
4. 暗色模式下，用透明度表示的次要文字与描边对比度不足。由 Task 6 的亮暗截图与完工检查固定。
5. 很长的会话标题在侧栏胶囊行里撑破布局。由 Task 5 展示页里的长标题行与 Task 6 截图固定。

---

### Task 1：`NavigationHistory`

**Files:**
- Create: `apps/desktop/src/stores/navigation.ts`
- Test: `apps/desktop/src/stores/navigation.test.ts`

**Interfaces:**
- Produces：

```ts
/** Where the app is: a host, and the open Session on it, if any. */
export type AppLocation = Readonly<{ hostId: string; sessionId: string | null }>;

/** Back/forward history of Locations, newest last; at most 100 entries. */
export class NavigationHistory {
  constructor(initial: AppLocation);
  get current(): AppLocation;
  get canGoBack(): boolean;
  get canGoForward(): boolean;
  /** Moves to `location`, dropping forward entries; a no-op when it equals `current`. */
  push(location: AppLocation): void;
  /** Steps back; the new current, or `undefined` at the start. */
  back(): AppLocation | undefined;
  /** Steps forward; the new current, or `undefined` at the end. */
  forward(): AppLocation | undefined;
}
```

- [ ] **Step 1：写测试**（`nsh/test` 的 `test`、`expect`）
  - `back_and_forward_walk_the_history`：`a → push(b) → push(c)`；`back()` 得 `b`、再 `back()` 得 `a`、`canGoBack` 为 false、`forward()` 得 `b`。
  - `pushing_the_current_location_is_a_no_op`：`push(a)` 两次后 `back()` 返回 `undefined`。
  - `push_after_back_drops_forward_entries`：`a,b,c` 后 `back()` 到 `b`，`push(d)`，`canGoForward` 为 false，`back()` 得 `b`。
  - `history_keeps_the_newest_100`：从 `s0` 推到 `s150`，连续 `back()` 能走 100 步，第 101 步返回 `undefined`。
- [ ] **Step 2：运行确认失败**：`nsh test src/stores/navigation.test.ts`（在 `apps/desktop`），模块不存在。
- [ ] **Step 3：实现**：数组加下标；比较按 `hostId` 与 `sessionId` 两个字段。
- [ ] **Step 4：运行确认通过**：同上命令，4 个测试通过。

---

### Task 2：`HostStore` 与共享的测试传输

**Files:**
- Create: `apps/desktop/src/stores/host.ts`
- Create: `apps/desktop/tests/support/memoryTransport.ts`（把 `src/client/connection.test.ts` 里的 `MemoryTransport` 移到这里并导出；该测试改为导入它）
- Test: `apps/desktop/src/stores/host.test.ts`

**Interfaces:**
- Consumes：`AcpTransport`（`src/client/transport.ts`）、`AcpConnection`、`AcpProtocol`。
- Produces：

```ts
export type HostKind = "local" | "remote";
/** A host's identity: `id` is stable ("local" for this computer, a device key later). */
export type HostInfo = Readonly<{ id: string; name: string; kind: HostKind }>;
export type HostPhase =
  | Readonly<{ kind: "starting" }>
  | Readonly<{ kind: "ready" }>
  | Readonly<{ kind: "failed"; reason: string }>;
/** One host's connection and the Sessions it lists; its methods are the only writers. */
export type HostStore = Readonly<{
  info: HostInfo;
  phase: Accessor<HostPhase>;
  sessions: Accessor<readonly SessionInfo[]>;
  /** Connects, initializes and lists the Sessions; failures land in `phase`, never throw. */
  start: () => Promise<void>;
  /** Closes the connection. */
  stop: () => void;
}>;
export function createHostStore(
  info: HostInfo,
  transport: AcpTransport,
  options?: Readonly<{ requestTimeoutMs?: number }>,
): HostStore;
export const LOCAL_HOST: HostInfo; // { id: "local", name: "This computer", kind: "local" }
```

- 现有 `createAppStore` 中的连接、`initialize`、`session/list` 与失败文案（`Backend failed to start: …`、`Backend did not answer: …`、`Backend stopped: …`）原样移到这里；`requestTimeoutMs` 默认 15 000。
- `memoryTransport.ts` 导出 `MemoryTransport`（与现在同样的构造参数与 `deliver`、`end`），另加 `acpPeer(sessions: SessionInfo[]): (frame, transport) => void`：对 `initialize` 回 `{ protocolVersion: 2 }`，对 `session/list` 回 `{ sessions }`，其他请求不回。

- [ ] **Step 1：写测试 `host.test.ts`**
  - `host_lists_sessions_after_start`：`acpPeer` 给两个会话；`start()` 后 `phase().kind === "ready"`，`sessions().length === 2`。
  - `host_reports_a_transport_that_cannot_open`：`open()` 拒绝 `"config file not found"` 的传输；`phase()` 为 `{ kind: "failed", reason: "Backend failed to start: config file not found" }`。
  - `host_times_out_when_backend_never_answers`：对任何请求都不回的对端，`requestTimeoutMs: 50`；`start()` 结束后 `phase().kind === "failed"`，`reason` 以 `"Backend did not answer:"` 开头。
- [ ] **Step 2：运行确认失败**：`nsh test src/stores/host.test.ts`。
- [ ] **Step 3：实现 `host.ts` 与 `memoryTransport.ts`；`connection.test.ts` 改为导入共享的 `MemoryTransport`。**
- [ ] **Step 4：运行确认通过**：`nsh test src/stores/ src/client/`，全部通过。

---

### Task 3：多主机的 `AppStore` 与应用接线

**Files:**
- Modify: `apps/desktop/src/stores/app.ts`（改为多主机）
- Modify: `apps/desktop/App.tsx`
- Test: `apps/desktop/src/stores/app.test.ts`、`apps/desktop/App.test.tsx`

**Interfaces:**
- Consumes：Task 1 的 `NavigationHistory`、`AppLocation`；Task 2 的 `HostStore`、`LOCAL_HOST`。
- Produces：

```ts
export type AppStore = Readonly<{
  hosts: Accessor<readonly HostStore[]>;
  currentHost: Accessor<HostStore>;
  location: Accessor<AppLocation>;
  canGoBack: Accessor<boolean>;
  canGoForward: Accessor<boolean>;
  /** Adds a host (R1: a paired remote host) and starts it. */
  addHost: (host: HostStore) => void;
  /** Shows a host with no Session open. */
  selectHost: (hostId: string) => void;
  /** Opens a Session; the sidebar follows to its host (spec §4.4). */
  openSession: (hostId: string, sessionId: string) => void;
  back: () => void;
  forward: () => void;
  /** Starts every host independently; one failing does not stop the others. */
  start: () => Promise<void>;
  /** Stops every host. */
  stop: () => void;
}>;
/** The first host is the initial location. */
export function createAppStore(hosts: readonly HostStore[]): AppStore;
```

- `AppStoreContext`、`useAppStore` 保留。未知 `hostId` 的 `selectHost`、`openSession` 不改位置并 `console.warn`。
- `App.tsx`：
  - 用 `createHostStore(LOCAL_HOST, nativeTransport)` 与 `createAppStore([local])`；退出时仍调用 `nativeTransport.closeNow()`，`cleanup` 调 `store.stop()`。
  - 根界面换成占位外壳（N1 再做正式界面）：左侧 280pt 的 `bg-sidebar` 栏，顶部显示 `currentHost().info.name`，下面是 `Sessions` 小标题与当前主机的会话行（Task 5 的 `ListItem` 胶囊样式，点击调用 `openSession`）；右侧白色主区显示该主机的 `Starting`、失败原因，或「No session open」。

- [ ] **Step 1：写测试 `app.test.ts`**（两台用 `MemoryTransport` 的主机 `a`、`b`）
  - `selecting_a_host_navigates_and_back_returns`：初始 `location()` 为 `{ hostId: "a", sessionId: null }`；`selectHost("b")` 后 `currentHost().info.id === "b"`；`back()` 后回到 `a`，`canGoForward()` 为 true。
  - `opening_a_session_on_another_host_switches_current_host`：当前在 `a`，`openSession("b", "s1")` 后 `currentHost().info.id === "b"`，`location().sessionId === "s1"`。
  - `one_host_failing_leaves_others_ready`：`b` 的传输打不开；`await start()` 后 `a` 为 ready、`b` 为 failed。
  - `unknown_host_keeps_the_location`：`selectHost("nope")` 后 `location()` 不变。
- [ ] **Step 2：运行确认失败**：`nsh test src/stores/app.test.ts`。
- [ ] **Step 3：实现 `app.ts`；改 `App.tsx` 接线与占位外壳。**
- [ ] **Step 4：`App.test.tsx` 保持两个现有测试（`connects_and_lists_sessions` 仍等 `text:Sessions`；`startup_failure_reaches_the_screen` 仍等原失败文案），新增 `shows_the_local_host_name`：等 `text:This computer`。**
- [ ] **Step 5：运行确认通过**：`CLAW_CONFIG=$PWD/tests/fixture/config.toml nsh test`，全部通过；`nsh check`、`nsh build` 无警告。

---

### Task 4：主题 token 与 DESIGN.md

**Files:**
- Modify: `apps/desktop/theme.json`
- Modify: `apps/desktop/src/ui/tokens.ts`（加字号类）
- Modify: `apps/desktop/DESIGN.md`

**Interfaces:**
- Produces：`theme.json` 的 token（下表），以及 `tokens.ts`：

```ts
/** The type scale as classes (spec §5.1); sizes come from theme tokens so settings can change them. */
export const type = {
  body: "text-(length:--text-body) leading-[24px]",
  ui: "text-(length:--text-ui) leading-[20px]",
  meta: "text-(length:--text-meta) leading-[18px]",
  heading: "text-(length:--text-heading) leading-[36px] font-medium",
} as const;
```

| token | 亮色 | 暗色 | 用途 |
| --- | --- | --- | --- |
| `background` | `#ffffff` | `#212121` | 主区 |
| `foreground` | `#1f2328` | `#ececec` | 正文 |
| `sidebar` / `sidebar-foreground` | `#f6f6f6` / `#1f2328` | `#181818` / `#ececec` | 应用底色、侧栏 |
| `sidebar-accent` / `sidebar-accent-foreground` | `#e9e9e9` / `#1f2328` | `#2f2f2f` / `#ececec` | 侧栏选中胶囊 |
| `muted` / `muted-foreground` | `#f4f4f4` / `#1f23287e` | `#2f2f2f` / `#ececec80` | 次要底色、次要文字 |
| `secondary` / `secondary-foreground` | `#f4f4f4` / `#1f2328` | `#303030` / `#ececec` | 用户消息胶囊、上下文条 |
| `accent` / `accent-foreground` | `#1f23280d` / `#1f2328` | `#ffffff12` / `#ececec` | 悬停洗色 |
| `primary` / `primary-foreground` | `#0d0d0d` / `#ffffff` | `#ececec` / `#0d0d0d` | 发送按钮等主操作 |
| `popover`、`card` 及其 foreground | `#ffffff` / `#1f2328` | `#2a2a2a` / `#ececec` | 弹层、卡片 |
| `border` / `input` / `ring` | `#1f232814` / `#1f23281f` / `#1f232866` | `#ffffff1a` / `#ffffff24` / `#ffffff66` | 描边、输入框、焦点 |
| `link` | `#0b66d0` | `#6cb6ff` | 正文链接（新增） |
| `destructive` / `destructive-foreground` | `#d1242f` / `#ffffff` | `#f85149` / `#450a0a` | 危险 |
| `success`、`warning`、`info` | `#1a7f37`、`#b35900`、`#0b66d0` | `#3fb950`、`#f0b72f`、`#6cb6ff` | 状态（foreground 沿用现值） |
| `added` / `removed` | `#1a7f37` / `#d1242f` | `#3fb950` / `#f85149` | 增删行数 |
| `radius` / `radius-lg` / `radius-xl` | `10px` / `16px` / `22px` | 同左 | 圆角档位 |
| `text-body` / `text-ui` / `text-meta` / `text-heading` | `15px` / `14px` / `13px` / `28px` | 同左 | 字号档位 |

- [ ] **Step 1：改 `theme.json` 与 `tokens.ts`。**
- [ ] **Step 2：重写 DESIGN.md**：视觉语言、字号/圆角/间距档位、颜色角色表（上表的用途列）、交互状态、外壳布局（spec §5.1 的侧栏、会话视图、输入框、右侧面板，写明这是 N1 的布局约定）、动效 token（不变）、展示页的动效清单（在现有清单上加：弹层标题行与说明文字无额外动效；`ListItem` 悬停时行尾操作 `instant` 淡入，减少动效时直接出现）。检查记录一节清空，留给 Task 6。
- [ ] **Step 3：验证**：`nsh check && nsh build`，`.nsh/types/nsh/tokens.d.ts` 里有 `link`、`radius-xl`、`text-body` 等新 token；无未知 class 警告。

---

### Task 5：`ui` 组件与展示页按 5.1 调整

**Files:**
- Modify: `apps/desktop/src/ui/{button,input,selection,tabs,menu,dialog,toast,list,surface,feedback,copy,motion}.tsx/ts`（按需）
- Modify: `apps/desktop/src/gallery/main.tsx`、`apps/desktop/src/gallery/Gallery.test.tsx`

**Interfaces:**
- Consumes：Task 4 的 token 与 `type`。
- Produces（新增或变化的 props，其余不变）：
  - `Menu`：新增 `title?: string`（弹层顶部的灰色标题行）。
  - `MenuItem`：新增 `description?: string`（标题下方的灰色说明）与 `warning?: boolean`（用 `warning` 色，给「完全访问」这类有风险但非破坏的选项，spec 5.1）；`danger` 仍用 `destructive` 色（删除等）。
  - `Surface`：新增 `variant?: "card" | "floating"`；`card`（默认）`rounded-(--radius-lg) border border-border bg-card`；`floating` 为输入框卡片：`rounded-(--radius-xl) bg-background`，一圈 `border` 描边加 `shadow-md`。
  - `ListItem`：选中与悬停为 `rounded-full` 胶囊（`selected:bg-sidebar-accent hover:bg-accent`）；新增 `actions?: JSX.Element`，只在悬停或键盘焦点时显示（`group-hover:opacity-100 group-focus-visible:opacity-100`，`instant`），标题单行截断。
- 字号：按钮、输入框、菜单、列表、Tabs 用 `type.ui`；说明与元信息用 `type.meta`；Toast 标题 `type.ui`、说明 `type.meta`。控件高度不变（`sm` 24、`md` 32）；圆角换成新档位（控件 `radius`、弹层 `radius-lg`）。弹层统一用 `menu.tsx` 的 `popupSurface`（`radius-lg`）。

- [ ] **Step 1：在 `Gallery.test.tsx` 加测试**
  - `menu_shows_title_and_descriptions`：按「Approval mode」触发按钮，等 `text:How should actions be approved?` 与 `text:Ask before editing files outside the workspace`。
  - `long_titles_stay_on_one_row`：等 `text:A very long session title that keeps going well past the width of the sidebar row` 出现（截断是外观，Task 6 截图核对）。
- [ ] **Step 2：运行确认失败**：`nsh test src/gallery/Gallery.test.tsx`。
- [ ] **Step 3：按 Interfaces 改组件；展示页**：
  - 「Menu」区加一个「Approval mode」菜单：`title` 为 `How should actions be approved?`，三项 `Ask`（图标 `hand`）/ `Auto-review`（`shield-check`）/ `Full access`（`triangle-alert`），说明分别为 `Ask before editing files outside the workspace`、`Ask only for risky actions`、`Run anything without asking`（最后一项 `warning`），`Auto-review` 为当前项（`checked`）。
  - 「ListItem」区放进一个 `bg-sidebar` 的 280pt 侧栏样框，加长标题行与带 `actions`（置顶、归档两个 `IconButton`）的行。
  - 新增「Surface」区的 `floating` 示例（一张输入框卡片样例：上方一条 `secondary` 胶囊上下文条，内含 `This computer` 与 `clawcode` 两个 ghost 小按钮）。
  - 需要的新 Lucide 图标（`pin`、`archive`、`laptop`、`folder`、`shield-check`、`hand`）用 N0 的生成脚本加入 `assets/icons` 并注册进 `Icon`。
- [ ] **Step 4：运行确认通过**：`nsh test src/gallery/`，全部通过；`nsh check`、`nsh build` 无警告。

---

### Task 6：外观检查与全量验证

- [ ] **Step 1：截图**：`nsh test src/gallery/main.tsx --target linux`（标准输入写步骤），亮色与暗色各截展示页上下两屏，外加「Approval mode」菜单展开、对话框与 Toast；再截 `App.tsx` 的占位外壳（带测试配置启动）。
- [ ] **Step 2：完工检查**：按 NSH 设计规范的完工检查清单（圆角、排版、间距、对齐、颜色、悬停、按下、状态、动效）逐组核对，重点看 Review Focus 第 4、5 条；发现的问题当场修，结果写进 DESIGN.md 的「检查记录」。
- [ ] **Step 3：全量验证**：
  - 在 `apps/desktop`：`nsh check && nsh build && CLAW_CONFIG=$PWD/tests/fixture/config.toml nsh test`
  - 在仓库根：`cargo fmt --all --check && cargo clippy --workspace --all-targets -- -Dwarnings`，`pre-commit run rig-boundary --all-files`
  - Expected：全部通过，`web/` 与 HEAD 一致（`git status --short -- web` 为空）。
- [ ] **Step 4：报告**：截图、检查记录、仍待 N1 处理的事项。
