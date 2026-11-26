# Desktop NSH 迁移 N0（地基与可行性）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 搭好 NSH 桌面客户端的地基：共享 TS 客户端包、NSH 应用骨架、进程内后端扩展、设计系统，并逐项验证 spec 第 8 节的可行性清单；任何一项不通过就停在 N0。

**Architecture:**
- 从 `web/src` 抽出不依赖 React、zustand、浏览器 DOM 的客户端逻辑，放进 `packages/client`，WebUI 改为引用它。
- `apps/desktop` 是 NSH 的 TSX + Rust 扩展应用。`ext/backend` 在进程内启动 clawcode 后端，通过 native module 与 TSX 逐行交换 ACP 帧。
- 先完成设计系统和组件展示页，再写任何业务页面。

**Tech Stack:** NSH（TSX，QuickJS），nshm Rust 扩展，agent-client-protocol 5.5.1（`Lines` 连接器），Node 26 内置测试（`node --test`），Vite / React（WebUI 不变）。

**Spec:** [Desktop 客户端迁移到 NSH 设计](../specs/2026-11-25-desktop-nsh-design.md)

## 执行后修订（2026-11-25）

执行完成后，用户明确：产品是原生桌面客户端；WebUI 只是前期调试用，以后删除；gpui-kit 的 `crates/desktop` 是失败的尝试，直接删除；以后会做 relay 远程控制。spec 已同步修订。对本计划的影响：

- **Task 2 撤回**：web 恢复原样（不移动、不改 import），`packages/client` 去掉。客户端代码复制到 `apps/desktop/src/client`，归桌面应用所有；`WebSocketTransport` 去掉；客户端测试改为 `nsh test` 运行的 `src/client/*.test.ts`。`AcpTransport` 接口保留，作为以后 relay 传输的接入点。
- **Global Constraints 中「`crates/desktop` 到 N4 才删除」作废**：已在 N0 删除，连同根 `Cargo.toml` 中的 `gpui-kit`、`rust-i18n`、`sys-locale` 和 `scripts/vendor-catppuccin-icons.py`。旧实现从 git 历史 `0d8b404` 取。
- **Review Focus 第 4 条（WebUI 不回归）** 改为：web 与 HEAD 完全一致。
- 下文保留执行时的原始任务描述，作为记录。

## Global Constraints

- 工作在 worktree `.claude/worktrees/desktop-nsh`，分支 `feat/desktop-nsh`；不改主 checkout。
- 只在用户明确要求时提交；本计划的步骤不包含 commit。
- `crates/desktop` 在 N0 保持不动，到 N4 才删除。
- 安装工具链由用户完成：不运行下载的脚本，不改全局配置。
- 遵循 NSH 编码规范与设计规范：
  - 组件只运行一次；
  - 每个状态只有一个写入方；
  - 页面只用 `src/ui/` 的组件；
  - 颜色只来自 token；
  - 图标是 Lucide SVG，不用字符；
  - 每个页面先写动效清单。
- Rust 部分遵循 AGENTS.md：函数和测试都有英文文档注释，tracing 宏写全路径，结构体超过 3 个字段用 TypedBuilder，测试只放在 `tests/`。
- AGENTS.md 规则 12 要求依赖在根 `[workspace.dependencies]` 声明。`ext/backend` 按 NSH 模板自带 `[workspace]`，与此冲突：
  - 能作为根 workspace 成员时，作为成员；
  - 不能时保留独立 workspace，在 `apps/desktop/README.md` 写明原因。
  - 结果作为 Task 1 的结论记录。
- 文案：英文和简体中文双语。

## Review Focus

1. 后端帧里含换行或非 ASCII 文本（中文、emoji）。期望：逐行传输不截断、不乱码。由 Task 3 的 `lines_round_trip_unicode_and_escaped_newlines` 测试固定。
2. 后端在启动中失败（配置错误、目录不可写）。期望：TSX 收到明确的错误，界面显示错误而不是一直等待。由 Task 4 的 `startup_failure_reaches_the_screen` 测试固定。
3. 窗口关闭或热更新。期望：后端线程和 SQLite 租约被释放，没有残留线程。由 Task 3 的 `shutdown_releases_the_backend` 测试固定。
4. WebUI 抽离之后行为不变。期望：`npm run check`、`npm run build` 通过，现有的 elicitation 测试通过。由 Task 2 的验证步骤固定。
5. 后端高频通知（流式输出时每秒数百条）。期望：TSX 端按顺序收到全部帧，界面不冻结。由 Task 6 的吞吐量探针固定。

---

### Task 1：工具链与结构性可行性（第 8 节第 1、2 项）

**Files:**
- Create: `apps/desktop/`（由 `nsh init apps/desktop --template mixed` 生成）
- Create: `packages/client/src/probe.ts`（临时，验证完删除）
- Create: `apps/desktop/FEASIBILITY.md`

**Interfaces:**
- Produces：可构建的 `apps/desktop` 骨架；`FEASIBILITY.md` 中第 1、2 项的结论。

- [ ] **Step 1：请用户安装 `nsh` 命令行工具**

按 `https://nsh.dev/docs/installation.md` 的 Linux 安装方式由用户安装。安装后运行 `nsh --version` 和 `cargo --version`，把版本号写进 `FEASIBILITY.md`。

- [ ] **Step 2：生成应用**

Run: `nsh init apps/desktop --template mixed`，然后在 `apps/desktop` 中运行 `nsh check && nsh build && nsh test`。
Expected: 三条命令都成功。

- [ ] **Step 3：验证第 1 项，跨目录引用**

新建 `packages/client/src/probe.ts`，导出 `export const probe = "packages-client";`。在 `App.tsx` 中 `import { probe } from "../../packages/client/src/probe";`，并把它渲染出来。在 `App.test.tsx` 中写 `await app.waitFor("packages-client")`。
Run: `nsh check && nsh build && nsh test`
Expected: 通过。

如果不通过：先试 `tsconfig` 的 `paths` 映射；再试 nsh.json 的 `"dependencies"` 用 path 引入。都不行就在 `FEASIBILITY.md` 记为不通过，并停止 N0，向用户报告。

- [ ] **Step 4：验证第 2 项，扩展依赖本仓库的 crate**

在生成的 `ext/<name>-native/Cargo.toml` 中加入路径依赖 `protocol = { path = "../../../../crates/protocol" }`，在 `lib.rs` 里调用一次 `protocol::ProductIdentity::NAME`。
- 先试删掉扩展自带的 `[workspace]`，把它加入根 workspace 的 members。
- 不行再保留独立 workspace。
- 在 `FEASIBILITY.md` 记录最终采用哪种方式，以及它对规则 12 的影响。

Run: `nsh check && nsh test`
Expected: 通过。两种方式都不行时停止 N0。

- [ ] **Step 5：清理探针**

删除 `probe.ts` 及它在 `App.tsx` 和测试中的引用；把生成的示例组件改成空的根组件。
Run: `nsh check && nsh build && nsh test`
Expected: 通过。

---

### Task 2：抽出 `packages/client`，WebUI 改为引用

**Files:**
- Create: `packages/client/src/{acp,domain,workspace}/`，`packages/client/src/transport.ts`，`packages/client/src/websocket.ts`，`packages/client/package.json`，`packages/client/tsconfig.json`
- Move: `web/src/acp/*`、`web/src/domain/*`，`web/src/workspace/` 中除 `store.ts` 以外的文件，以及 `web/src/features/mcp/elicitationForm.ts`，全部移到 `packages/client/src/`
- Modify: `web/src` 中所有引用这些文件的 import；`web/src/workspace/store.ts` 留在 web
- Modify: `crates/app/tests/web/elicitation_form.test.mjs` 第 4 行的 import，改为指向 `packages/client/src/elicitationForm.ts`
- Test: `packages/client/test/transport.test.ts`、`packages/client/test/controller.test.ts`

**Interfaces:**
- Produces（`packages/client/src/transport.ts`）：

```ts
/** One bidirectional stream of ACP JSON-RPC frames, one JSON text per frame. */
export interface AcpTransport {
  /** Opens the stream; rejects when it cannot. */
  open(): Promise<void>;
  /** Sends one frame. */
  send(frame: string): void;
  /** Calls `listener` with every frame received, in order; returns the unsubscribe. */
  onFrame(listener: (frame: string) => void): () => void;
  /** Calls `listener` once when the stream ends, with the reason if it failed. */
  onClose(listener: (reason?: string) => void): () => void;
  /** Ends the stream. */
  close(): void;
}
```

- `websocket.ts`：`export class WebSocketTransport implements AcpTransport`，构造参数为 `url: string`。代码取自现在的 `acp/connection.ts` 中操作 WebSocket 的部分，包括超时与 `readyState` 判断。
- `acp/connection.ts`：`AcpConnection` 的构造参数从 URL 改为 `transport: AcpTransport`。JSON-RPC 的请求、应答、通知逻辑不变。
- `workspace/controller.ts`：构造时接收 `store: WorkspaceStoreAccess`（`updateRouter.ts` 中已有的类型），去掉对 `useWorkspaceStore` 的直接引用。WebUI 传入 zustand store 的 `getState` 和 `dispatch`。

- [ ] **Step 1：写测试 `packages/client/test/transport.test.ts`**

- `connection_answers_a_request_through_the_transport`：用内存假传输（测试文件里实现 `AcpTransport`）收到 `initialize` 请求帧后回写应答帧。断言 `AcpConnection` 的 `request("initialize", …)` 得到应答的 `result`。
- `connection_rejects_pending_requests_when_the_transport_closes`：假传输在应答前调用 close 监听。断言挂起的 `request` 被拒绝，错误信息包含关闭原因。

- [ ] **Step 2：写测试 `packages/client/test/controller.test.ts`**

`controller_dispatches_through_the_injected_store`：用一个记录 action 的 `WorkspaceStoreAccess` 和假传输构造 controller；送入一条 `session/update` 的 `agent_message_chunk` 通知帧。断言记录到的 action 序列包含该会话的消息更新。

- [ ] **Step 3：运行，确认失败**

Run: `node --experimental-strip-types --test packages/client/test/`
Expected: 失败，原因是模块不存在。

- [ ] **Step 4：移动文件并改造接口**

用 `git mv` 移动文件以保留历史，然后按 Interfaces 改造 `connection.ts` 和 `controller.ts`，新增 `transport.ts` 和 `websocket.ts`。`packages/client/package.json` 不声明运行时依赖。

- [ ] **Step 5：运行测试，确认通过**

Run: `node --experimental-strip-types --test packages/client/test/`
Expected: 通过。

- [ ] **Step 6：WebUI 不回归**

修改 `web/src` 的 import，让 WebUI 通过 `WebSocketTransport` 和注入的 store 使用 `packages/client`。
Run（在 `web/`）: `npm run check && npm run build && npm run test:elicitation`
Expected: 全部通过。

---

### Task 3：`ext/backend` 进程内后端（第 8 节第 3 项的基础）

**Files:**
- Modify: `crates/acp/src/transport.rs`，新增 `serve_lines`
- Create: `apps/desktop/ext/backend/`（nshm crate），替换 Task 1 生成的示例扩展；同步修改 `nsh.json` 的 `"dependencies"`
- Test: `crates/acp/tests/lines.rs`、`apps/desktop/ext/backend/tests/backend.rs`

**Interfaces:**
- Produces（`acp`）：

```rust
impl AcpServerFactory {
    /// Runs one ACP connection over string frames: `outgoing` receives each
    /// JSON text the server writes, `incoming` yields each one it reads.
    pub async fn serve_lines<O, I>(&self, outgoing: O, incoming: I) -> Result<(), AcpTransportError>
    where
        O: futures::Sink<String, Error = std::io::Error> + Send + 'static,
        I: futures::Stream<Item = std::io::Result<String>> + Send + 'static;
}
```

实现为 `self.component(AcpTransportKind::InProcess).connect_to(Lines::new(outgoing, incoming))`。

- Produces（`ext/backend`，native module 名为 `clawcode-backend`）：一个 `#[class] Backend`：

| 成员 | 签名 | 说明 |
| --- | --- | --- |
| 构造 | `async fn start() -> Result<Instance<Backend>, String>` | 在独立线程上建 tokio 运行时、构建 `app` 的 Application，再调用 `serve_lines` |
| `send` | `fn send(&self, frame: String) -> Result<(), String>` | 帧写入后端的输入通道 |
| `on_frame` | `fn on_frame(&mut self, listener: Callback<String>) -> Stop` | 引擎线程上的任务读取输出通道，用 `post` 逐帧交给 TSX |
| `on_close` | `fn on_close(&mut self, listener: Callback<Option<String>>) -> Stop` | 连接结束或后端停止时通知一次 |
| `shutdown` | `async fn shutdown(this: Instance<Self>)` | 结束连接、关闭 Application、等待线程退出 |

后端线程的结构照抄 `crates/desktop/src/backend/mod.rs` 中的 `Backend::start` 和 `BackendTask::run`，只把 `AcpClient::connect` 换成 `serve_lines`。

- [ ] **Step 1：写测试 `crates/acp/tests/lines.rs`**

- `serve_lines_answers_initialize`：用 `futures::channel::mpsc` 构造输出 sink 和输入 stream，送入一条 `initialize` 请求的 JSON 文本。断言输出收到一条 `id` 相同、带 `result.protocolVersion` 的 JSON 文本。
- `lines_round_trip_unicode_and_escaped_newlines`：送入的提示文本含中文、emoji 和 `\n`。断言回放或应答中的文本与原文逐字节一致，且每条输出都能单独解析成 JSON。

kernel 的构造照抄 `crates/acp/tests/extension.rs` 中的 `integration_kernel`。

- [ ] **Step 2：运行，确认失败**

Run: `cargo test -p acp --test lines`
Expected: 编译失败，`serve_lines` 不存在。

- [ ] **Step 3：实现 `serve_lines`**

- [ ] **Step 4：运行，确认通过**

Run: `cargo test -p acp --test lines`
Expected: 通过。

- [ ] **Step 5：写测试 `apps/desktop/ext/backend/tests/backend.rs`**

- `start_answers_initialize_through_frames`：在临时的 XDG 目录下 `start`，发送 `initialize`，断言 `on_frame` 收到应答。
- `shutdown_releases_the_backend`：`start` 之后调用 `shutdown`，断言 5 秒内结束，后端线程已退出（`JoinHandle` 结束），之后再 `send` 返回 Err。

如果 nshm 的 class 测试需要引擎，就按 NSH 的 `rust-testing.md` 用 `nsh_testing::Headless`；否则对后端线程这一层直接写单元测试，类只做薄包装。

- [ ] **Step 6：运行，确认失败；实现；再运行，确认通过**

Run: `cargo test --manifest-path apps/desktop/ext/backend/Cargo.toml`（作为根 workspace 成员时用 `cargo test -p clawcode-backend`）
Expected: 先失败，实现后通过。

---

### Task 4：原生传输与应用启动

**Files:**
- Create: `apps/desktop/src/transport/native.ts`、`apps/desktop/src/stores/app.ts`
- Modify: `apps/desktop/App.tsx`
- Test: `apps/desktop/App.test.tsx`

**Interfaces:**
- Consumes：Task 2 的 `AcpTransport`、`AcpConnection`，Task 3 的 `Backend`。
- Produces：
  - `export class NativeTransport implements AcpTransport`：`open` 调用 `Backend.start()`，其余方法对应 `send`、`onFrame`、`onClose`、`shutdown`。
  - `export type AppStore`：由 signal 组成的 `Copy` 结构。字段：
    - `phase: Signal<"starting" | "ready" | { failed: string }>`
    - `sessions: Signal<SessionSummary[]>`
  - `export function createAppStore(transport: AcpTransport): AppStore`：连接、`initialize`、`session/list`，结果写入 signal。
  - 根组件 `provideContext` 下发 store，组件里通过 `useAppStore()` 读取。

- [ ] **Step 1：写测试 `App.test.tsx`**

- `connects_and_lists_sessions`：在临时 XDG 下打开应用。断言先出现「Starting」，之后出现会话侧栏标题「Sessions」，并且 `phase` 为 ready。
- `startup_failure_reaches_the_screen`：用指向不可读配置的环境变量启动（变量名按 `config` crate 现有的配置路径变量）。断言界面出现以「Backend failed to start」开头的错误文本。

- [ ] **Step 2：运行，确认失败**

Run: `nsh test`
Expected: 失败。

- [ ] **Step 3：实现**

`App.tsx` 根据 `phase` 显示启动中、错误或会话列表。会话列表暂时用 `Text` 和 `List` 占位，正式界面在 N1 用 `ui` 组件重做。

- [ ] **Step 4：运行，确认通过**

Run: `nsh check && nsh build && nsh test`
Expected: 通过。

---

### Task 5：设计系统、`ui` 核心组件与组件展示页

**Files:**
- Create: `apps/desktop/DESIGN.md`
- Create: `apps/desktop/theme.json`（nsh.json 的 `"theme"` 指向它；shadcn `cssVars` 格式，含 light 和 dark）
- Create: `apps/desktop/src/ui/tokens.ts`、`motion.ts`、`icon.tsx`、`button.tsx`、`input.tsx`（`Input`、`Textarea`、`Field`）、`selection.tsx`（`Checkbox`、`Switch`、`Select`）、`tabs.tsx`、`menu.tsx`（`Menu`、`Popover`、`Tooltip`）、`dialog.tsx`（`Dialog`、`AlertDialog`、`Sheet`）、`toast.tsx`、`list.tsx`（`ListItem`、`TreeList`）、`surface.tsx`（`Surface`、`Badge`、`Separator`、`StatusDot`、`LineCounts`）、`feedback.tsx`（`Empty`、`Skeleton`、`Spinner`）、`copy.tsx`（`CopyButton`）
- Create: `apps/desktop/assets/icons/*.svg`（用到的 Lucide 图标）
- Create: `apps/desktop/src/gallery/Gallery.tsx`
- Test: `apps/desktop/src/gallery/Gallery.test.tsx`

**Interfaces:**
- Produces：
  - 每个 `ui` 组件只暴露 `variant`、`size`、状态（`selected`、`disabled`、`loading`、`invalid`）、内容插槽和事件，不接受颜色、圆角、字号。
  - `Button` 的 variant：`primary | secondary | ghost | outline | danger`；size：`sm | md`，md 为 32pt。
  - `tokens.ts` 导出语义 token 名。颜色值只写在 `theme.json` 里。
  - `motion.ts` 导出：

```ts
export const motion = {
  instant: 120, fast: 180, base: 240, slow: 360,
  enter: "ease-out", move: "ease-in-out", leave: "ease-in",
  settle: "cubic-bezier(0.22, 1, 0.36, 1)",
  distance: { sm: 4, md: 8, lg: 16 },
  scale: { press: 0.97, pressIcon: 0.95, enter: 0.96 },
} as const;
```

- `theme.json` 的颜色取自 `crates/desktop/src/theme.rs` 现有主题的亮色和暗色，按语义名映射：background、foreground、border、primary、accent、selection、focus、success、warning、danger、info。

- [ ] **Step 1：写 `DESIGN.md`**

内容：
- 视觉语言；
- 字号与间距的档位；
- 密度：控件 32pt，行 28–36pt，图标 14–16pt；
- 圆角、图标族（Lucide）；
- 交互状态的统一做法；
- 平台：本期为 Linux / macOS / Windows 桌面；
- 动效 token 表；
- 组件展示页本身的动效清单（按设计规范「Motion inventory」的格式写，覆盖按下、悬停、Tab 选中滑动、菜单和对话框开合、Toast 进出、减少动效）。

- [ ] **Step 2：写测试 `Gallery.test.tsx`**

- `gallery_shows_every_component`：打开展示页，逐个 `waitFor` 每个组件区块的标题文本，例如「Button」「IconButton」「Select」「Tabs」「Menu」「Dialog」「Toast」「TreeList」。
- `buttons_report_presses_and_disabled_ignores`：按下 primary 按钮，计数文本变为「Pressed 1」；按下 disabled 按钮，计数不变。
- `menu_opens_and_closes_with_escape`：按下菜单触发按钮，菜单项文本出现；按 Escape 后消失，焦点回到触发按钮。

- [ ] **Step 3：运行，确认失败；实现组件和展示页；再运行，确认通过**

Run: `nsh check && nsh build && nsh test`
Expected: 先失败，实现后通过。展示页从开发菜单或启动参数打开。具体入口方式先查 `.nsh/types` 中 `nsh/desktop` 提供的菜单接口，再定。

- [ ] **Step 4：外观检查**

`nsh run` 打开展示页，分别在亮色和暗色下截图，按设计规范的完工检查清单（圆角、排版、间距、对齐、颜色、悬停、按下、状态、动效）逐组核对，并逐条核对 DESIGN.md 里展示页的动效清单。结论写进 `DESIGN.md` 末尾的「检查记录」。

---

### Task 6：可行性第 3–7 项与停止门槛

**Files:**
- Create: `apps/desktop/src/probes/`（探针页面，N1 之前删除）
- Modify: `apps/desktop/FEASIBILITY.md`

- [ ] **Step 1：第 3 项，帧吞吐量**

新建临时扩展 `ext/probe`（不放进 `ext/backend`，避免生产代码里出现只为测试存在的代码，见 AGENTS.md 规则 10）。它的 `#[class] FrameProbe` 用和 `ext/backend` 相同的通道加 `post` 方式，每秒推送 500 条 `agent_message_chunk` 形状的帧，共 5 秒。探针页面统计收到的条数和顺序。
Run: `nsh test --trace`，配合探针测试 `frames_arrive_in_order_at_streaming_rate`：断言 2500 条全部按序到达，界面的输入仍可响应（期间在输入框输入文字后能读回）。
探针结束后删除 `ext/probe` 及它在 nsh.json 中的条目。

- [ ] **Step 2：第 4 项，输入框**

用展示页的 `Textarea` 实机验证：
- 中文输入法组字时按 Enter 只确认候选，不发送；
- 自动增高到 8 行后开始滚动；
- Enter 发送，Shift+Enter 换行。

- [ ] **Step 3：第 5 项，长 transcript**

探针页用 `List` 放 3000 行混合内容（Markdown 段落、代码块、短行）。其中最后一行以 20ms 一段的速度流式追加 Markdown。核对：
- 滚动流畅；
- 往回滚时被回收的行内容正确；
- 跳到第 1 行再跳回底部时位置正确；
- 流式输出时只有最后一行在重绘（`nsh test --trace` 的绘制计数）。

- [ ] **Step 4：第 6 项，`DiffView`**

用 1 MiB 的 patch（可从 acp 测试仓库生成）打开 `DiffView`，记录打开耗时和滚动表现。

- [ ] **Step 5：第 7 项，组件缺口**

在 `.nsh/types` 和 `https://nsh.dev/elements` 中查找树、可拖动分栏、剪贴板接口。找到的记下用法；找不到的，写清 `ui` 中的替代方案（例如 `TreeList` 基于 `List`、分栏基于拖动手势），并用一个最小探针证明可行。

- [ ] **Step 6：停止门槛**

把每一项的结论（通过 / 不通过 / 有条件通过，附数据或截图）写进 `FEASIBILITY.md`。
- 有任何一项不通过：停止，向用户报告，等待决定，不进入 N1。
- 全部通过：删除探针页面，运行 Task 7。

---

### Task 7：收尾验证

- [ ] **Step 1：全量检查**

Run：
- `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -Dwarnings && cargo test --workspace`
- `node --experimental-strip-types --test packages/client/test/`
- 在 `web/` 中：`npm run check && npm run build`
- 在 `apps/desktop/` 中：`nsh check && nsh build && nsh test`
- `pre-commit run --all-files`

Expected: 全部通过。gpui 界面测试已知的偶发崩溃（SIGSEGV、taffy panic）如果出现，单独重跑确认并在报告中写明。

- [ ] **Step 2：报告**

向用户报告：
- `FEASIBILITY.md` 的结论；
- 展示页的截图；
- Global Constraints 中规则 12 的处理方式；
- 进入 N1 前需要用户确认的事项。
