# N3 桌面右侧面板实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**目标：** 补齐 N3 的最后一项「详情卡」。它是右侧面板的「详情」标签，按顺序展示工作区与 Git、在…中打开、关联会话、上下文与用量。

**架构：** 沿用 `WorkspacePanel` 的标签、分栏和窄窗口返回逻辑。数据全部来自主机已有的 ACP 接口：`_clawcode/workspace/git_status`、`_clawcode/session/runtime`、会话列表 meta 中的 `parentSessionId`（分叉来源）与 `ownership`（子代理的所属会话），以及 `usage_update` 和 idle `state_update` 中的 `usage`。本机和远程主机走同一套代码。只有「在…中打开」需要新增本机原生模块。

**技术栈：** NSH 0.7.1 TSX、`ext/backend`（Rust，`nsh_core` 的 `#[module]`）、ACP v2。

**规格：** [桌面设计规格](../specs/2026-11-25-desktop-nsh-design.md)第 8 节 N3；[N2 计划](./2026-11-26-desktop-nsh-n2.md)。

## 第一部分：已完成的文件、预览与变更

这部分已经实现并验收，交互细节记在[桌面 DESIGN.md](../../../apps/desktop/DESIGN.md)。

1. `2879fc1`：文件卡片点击「查看变更」后，在右侧打开变更面板。面板展示所选编辑的增删行数和统一 diff，可以用上一处、下一处逐个查看本轮的各次编辑。重复收到同一次编辑时保留阅读位置。宽窗口可以拖动分隔线，窄窗口下面板占满会话区，按 Escape 回到文件卡片。
2. `6076708`：会话顶栏可以打开工作区面板，在「文件」和「变更」之间切换。文件树通过 `_clawcode/workspace/list_dir` 按需读取所属主机的目录，用 `read_file` 显示只读预览，并区分空文件、二进制、大文件和读取失败。变更列表按轮次分组。每轮下方有通栏摘要，显示文件数和累计增删行，点「审查」直接打开本轮 diff。文件图标使用 Catppuccin 主题（浅色用 Latte，深色用 Mocha）。

验收：同机 Wayland 双实例，两端使用独立的设备身份和数据目录。控制端配对后从远程会话展开目录，读到了主机上的文件内容。1200 × 800 浅色窗口中会话与预览并排显示，800 × 560 深色窗口中返回按钮能回到会话。原生交互测试覆盖快速切换预览、返回同一次编辑和保留未发送草稿。

## 第二部分：详情标签

## 全局约束

- 新增的函数都要写英文函数级注释，非平凡逻辑也要写注释（AGENTS.md 第 1 条）。
- 测试只能放在 `apps/desktop/tests/` 下，或放在 Rust 的 `#[cfg(test)] mod tests` 中（第 10、17 条）。
- Rust 日志使用 `tracing::warn!("...")` 这种完整路径的写法，不用结构化字段（第 14 条）。
- 不新增 npm 或 crate 依赖。打开文件管理器只用系统命令：Linux 用 `xdg-open`，macOS 用 `open`，Windows 用 `explorer`。
- 「在…中打开」只在 `host.info.kind === "local"` 时显示。远程会话的路径在另一台机器上，不提供该入口。
- 「在…中打开」默认用文件管理器。之前选过的编辑器如果已经不在 PATH 上，就回落到文件管理器。
- 不做的内容：用户隐藏行、按高度降级密度、在父会话旁并排打开子代理。
- 文案和交互要同时适配中英文、亮暗主题，以及 1200 × 800 和 800 × 560 两种窗口尺寸。

## 审查重点

1. 主机上的工作区目录已被删除：`git_status` 会返回 JSON-RPC `-32602`。这时详情页显示「目录不存在」，其余各节照常显示，不能整页报错。由任务 2 负责测试。
2. 远程主机断线时，详情页保留最近一次读到的 Git 和用量数据，并标明离线，不能清空。重连后刷新一次。由任务 4 负责测试。
3. 旧版主机没有上报 `usage_update`、`parentSessionId` 或 `ownership`：对应的节显示「暂无」，或者不显示关联会话，都不能抛错。由任务 1 负责测试。
4. 编辑器名称来自设置存储，可能已被篡改：`open` 只接受 `editors()` 返回过的名称，其他值一律走文件管理器，绝不执行任意命令。由任务 3 负责测试。
5. 子代理没有在这台设备上打开过，`host.running()` 不知道它的状态：要通过 `_clawcode/session/runtime` 查询，查到在运行就算「运行中」；等待审批或等待回答的也算「运行中」。由任务 4 负责测试。
6. 主机读取 Git 失败（返回 `-32603`，或者回复格式不对）：Git 那一行显示「无法读取 Git 状态」，等下一次触发时再读，其他节照常显示。由任务 4 负责测试。

---

### 任务 1：解码关联会话与用量

**文件：**
- 修改：`apps/desktop/src/client/domain/model.ts`（`SessionSummary`，新增用量类型）
- 修改：`apps/desktop/src/client/acp/protocol.ts:181-189`（`sessionSummary`）
- 修改：`apps/desktop/src/client/workspace/updateDecoder.ts:120-122`（`state_update`）及 `sessionAction`（`usage_update`）
- 修改：`apps/desktop/src/client/workspace/sessionState.ts`（状态、动作、`SessionWorkspaceStager.apply`）
- 测试：`apps/desktop/tests/client/updateRouter.test.ts`、`apps/desktop/tests/stores/host.test.ts`

**接口：**
- 产出：`SessionSummary.parentSessionId?: SessionId`，表示分叉来源，由 `crates/kernel/src/runtime/session/fork.rs` 写入。
- 产出：`SessionSummary.owner?: Readonly<{ sessionId: SessionId; name: string; nickname: string }>`，表示子代理的所属会话，对应 meta 中 `ownership` 的 `{ sessionId, name, nickname }`，由 `crates/kernel/src/runtime/subagent/tools.rs` 写入。
- 产出：`type ContextUsage = Readonly<{ used: number; size: number }>`
- 产出：`type RunUsage = Readonly<{ totalTokens: number; inputTokens: number; outputTokens: number; thoughtTokens?: number; cachedReadTokens?: number }>`
- 产出：`SessionWorkspaceState.context?: ContextUsage`、`SessionWorkspaceState.lastRun?: RunUsage`
- 产出：新增两个动作 `{ type: "usage/context"; usage: ContextUsage }` 和 `{ type: "usage/run"; usage: RunUsage }`

- [ ] **步骤 1：写失败测试**
  - `decodes usage_update into the session's context usage`：输入 `{ sessionUpdate: "usage_update", used: 42000, size: 200000 }`，断言 `session(id).context` 为 `{ used: 42000, size: 200000 }`。
  - `records the settled run's usage from an idle state update`：输入 `{ sessionUpdate: "state_update", state: "idle", usage: { totalTokens: 1500, inputTokens: 1200, outputTokens: 300 } }`，断言 `lastRun.totalTokens === 1500`，并且 `running === false`。
  - `ignores usage without numeric used and size`：输入 `used: "x"`，断言 `context === undefined`。
  - `sessionSummary keeps parentSessionId from product meta`：meta 中 `parentSessionId` 为 `"s-parent"` 时保留，为 `null` 或缺失时不写入该字段。
  - `sessionSummary keeps a complete ownership edge`：`ownership: { sessionId: "s-root", name: "worker", nickname: "Ada" }` 时得到 `owner`；`ownership` 为 `null` 或缺少字段时不写入 `owner`。
- [ ] **步骤 2：运行测试，确认失败**

  命令：`cd apps/desktop && CLAW_CONFIG=$PWD/tests/fixture/config.toml nsh test`

  预期：上面 5 项失败。
- [ ] **步骤 3：实现**
  - `usage_update` 要求 `used` 和 `size` 都是有限非负数。
  - idle 的 `usage` 要求三个必需计数都是数字，否则不发出该动作。
  - `running/changed` 动作保持不变，`usage/run` 追加在它之后。
  - 主机回放历史时只会重发 `usage_update`，不会带上 idle 的 `usage`（见 `crates/acp/src/server.rs` 的回放分支）。所以重开会话后 `context` 能恢复，`lastRun` 要等这次连接里有新的运行结束才会出现。不需要为此修改主机。
- [ ] **步骤 4：运行测试，确认通过**

  命令同步骤 2，预期全部通过。

### 任务 2：读取工作区 Git 状态

**文件：**
- 修改：`apps/desktop/src/client/acp/workspaceFiles.ts`
- 测试：`apps/desktop/tests/client/acp/workspaceFiles.test.ts`（新建）

**接口：**
- 产出：

  ```ts
  type GitStatus =
    | { type: "repository"; branch?: string; base: string; insertions: number; deletions: number; changedFiles: number; truncated: boolean }
    | { type: "not_repository" } | { type: "git_unavailable" } | { type: "missing" };
  ```

- 产出：`WorkspaceFiles.gitStatus(root: string): Promise<GitStatus>`，请求 `_clawcode/workspace/git_status`，参数为 `{ root }`。
- 字段来自 `crates/protocol/src/workspace.rs` 的 `WorkspaceGitStatus`：`branch` 为 `null` 表示 detached；`changedFiles` 取 `changes.length`。

- [ ] **步骤 1：写失败测试**，使用 `tests/support/memoryTransport.ts` 回放主机回复：
  - `reads a repository status`：回复 `{ type: "repository", branch: "main", base: "origin/main", changes: [{…}, {…}], truncated: false, insertions: 120, deletions: 34 }`，断言 `changedFiles === 2`，`branch === "main"`。
  - `keeps a detached head without a branch`：`branch: null` 时，结果中没有 `branch` 字段。
  - `maps invalid params to a missing workspace`：主机回复错误 `{ code: -32602 }`，断言结果为 `{ type: "missing" }`。
  - `rejects other errors and malformed replies`：错误码 `-32603` 或缺少 `base` 时 reject。
- [ ] **步骤 2：运行测试，确认失败**（命令同任务 1）
- [ ] **步骤 3：实现 `gitStatus`**。用 `error instanceof AcpRequestError && error.rpc.code === -32602` 判断目录缺失。
- [ ] **步骤 4：运行测试，确认通过**

### 任务 3：本机「在…中打开」

**文件：**
- 新建：`apps/desktop/ext/backend/src/editors.rs`
- 修改：`apps/desktop/ext/backend/src/lib.rs`（加入 `mod editors;`，并在 `native()` 中注册 `.module::<editors::ExternalEditors>()`）
- 修改：`apps/desktop/src/stores/settings.ts`（`Settings.openIn`）
- 测试：`editors.rs` 内的 `#[cfg(test)] mod tests`、`apps/desktop/tests/stores/settings.test.ts`

**接口：**
- 产出（Rust）：常量 `EDITORS: [&str; 5] = ["zed", "code", "cursor", "codium", "subl"]`。
- 产出（Rust）：`struct EditorCatalog { editors: Vec<String> }`。按 AGENTS.md 第 15 条，把已安装列表作为上下文挂在结构体上，不在自由函数之间传来传去。旧客户端 `0d8b404:crates/desktop/src/ui/details/open_in.rs` 有同名结构，可以参考。
  - `fn detect(path_var: Option<&OsStr>) -> Self`：按 `EDITORS` 的顺序收集在 PATH 某个目录中存在且可执行的命令。
  - `fn resolve(&self, editor: &str) -> Option<&str>`：只在 `editor` 属于已安装列表时返回编辑器名，返回 `None` 表示使用文件管理器。
- 产出（Rust）：`#[module] pub struct ExternalEditors`，包含两个函数：
  - `pub fn editors() -> Vec<String>`：用当前进程的 `PATH` 调用 `EditorCatalog::detect`。
  - `pub fn open(path: String, editor: String) -> Result<(), String>`：`editor` 就是设置里的 `openIn` 值，`"file-manager"` 或任何未安装的名称都启动系统文件管理器。参数用 `String` 而不是 `Option<String>`，这样不依赖 NSH 是否支持可选参数。启动时直接把路径作为参数传给命令，不经过 shell，启动后另开一个线程 `wait()` 回收子进程。启动失败时先执行 `tracing::warn!("failed to open {} in {}: {}", path, target, error)`，再返回错误文本。
- 产出（TS）：`Settings.openIn: string`，默认值为 `"file-manager"`。`parse` 只保留长度在 1–32 之间、由 `[a-z]` 组成的值，其他值回到默认值。

- [ ] **步骤 1：写失败测试**
  - Rust `detect_lists_executables_in_editor_order`（加 `#[cfg(unix)]`，因为要设置可执行权限）：在 tempdir 中创建可执行的 `subl`、`zed`，以及不可执行的 `code`，断言结果为 `["zed", "subl"]`。
  - Rust `detect_is_empty_without_path`：传入 `None` 时返回空列表。
  - Rust `resolve_never_picks_an_unknown_editor`：已安装列表为 `["zed"]` 时，`resolve("rm")` 和 `resolve("file-manager")` 都返回 `None`，`resolve("zed")` 返回 `Some("zed")`。
  - TS `settings drop an invalid openIn`：存储中的值为 `"../x"` 或 `123` 时，读回 `"file-manager"`；为 `"zed"` 时保留。
- [ ] **步骤 2：运行测试，确认失败**：`cargo test -p clawcode-backend editors` 和 `nsh test`
- [ ] **步骤 3：实现**。文件管理器命令用 `#[cfg(target_os = …)]` 区分平台。
- [ ] **步骤 4：运行测试，确认通过**，再运行 `nsh check`

### 任务 4：详情标签界面

**文件：**
- 新建：`apps/desktop/src/client/relations.ts`
- 新建：`apps/desktop/src/session/DetailsPanel.tsx`
- 修改：`apps/desktop/src/stores/host.ts:64`（`SessionPanel.tab` 增加 `"details"`）、`host.ts:224`（新增 `HostStore.queryRunning`）
- 修改：`apps/desktop/src/session/WorkspacePanel.tsx`（新增第一个标签「详情」，并新增 `InspectorTab`）
- 修改：`apps/desktop/src/session/SessionView.tsx:93-97`（`togglePanel` 改为打开 `{ tab: "details" }`）
- 修改：`apps/desktop/src/shell/Shell.tsx:158`（向 `SessionView` 传入 `settings` 和 `onOpenSession`）
- 修改：`apps/desktop/locales/en.yml`、`apps/desktop/locales/zh-CN.yml`（新增 `details.*` 文案）
- 测试：`apps/desktop/tests/client/relations.test.ts`（新建）、`apps/desktop/tests/session/WorkspacePanel.test.tsx`、`apps/desktop/tests/session/SessionView.test.tsx`、`apps/desktop/tests/stores/host.test.ts`、`apps/desktop/tests/entries/session-details.tsx`（新建原生入口）

**接口：**
- 使用：任务 1 的 `parentSessionId`、`owner`、`context`、`lastRun`；任务 2 的 `gitStatus`；任务 3 的 `ExternalEditors` 和 `Settings.openIn`。
- 产出：

  ```ts
  type SessionRelations = Readonly<{ parent?: SessionSummary; running: readonly SessionSummary[]; previous: readonly SessionSummary[] }>;
  function sessionRelations(sessions: readonly SessionSummary[], sessionId: SessionId, active: (id: SessionId) => boolean): SessionRelations;
  ```

  - `parent`：`parentSessionId` 指向的会话，也就是分叉来源。
  - 子代理：`owner?.sessionId === sessionId` 的会话，按 `modifiedAtMs` 从新到旧排列。主机默认列出全部会话（`SessionScope::All`），所以子代理本来就在 `host.sessions()` 里。
  - `active` 由调用方传入，判断条件是：`host.running(id)` 为真，或 `queryRunning` 查到在运行，或 `!!host.permissionFor(id)`，或 `host.awaitsAnswer(id)`。
- 产出：`HostStore.queryRunning(sessionId: SessionId): Promise<boolean>`。它请求 `_clawcode/session/runtime`，返回 `running`，但不写入工作区状态，因为子代理往往没有在这台设备上打开过，`host.running()` 只知道已打开会话的状态。回复无效时 reject。
- 产出：`DetailsPanel(props: { host: HostStore; sessionId: SessionId; settings: SettingsStore; active: boolean; onOpenSession: (id: SessionId) => void; onShowChanges: () => void })`
- 产出：`SessionView` 新增 `settings: SettingsStore` 和 `onOpenSession: (id: SessionId) => void` 两个 prop，并透传给 `WorkspacePanel`。

**页面内容，从上到下：**

1. **工作区**：显示 `cwd` 的最后一段，悬停时显示完整路径。
   - `missing`：显示「目录不存在」。
   - `repository`：显示分支名，没有分支时显示「分离的 HEAD」。显示「对比 {base}」及 `LineCounts`，点击后调用 `onShowChanges`，切到「变更」标签；`truncated` 时在数字后加「+」。
   - `not_repository` 或 `git_unavailable`：显示一行对应说明。
   - `gitStatus` reject：显示「无法读取 Git 状态」，下次触发时重读。
2. **在…中打开**（仅本机主机）：一个按钮加一个菜单。菜单项为「文件管理器」和 `ExternalEditors.editors()` 返回的各编辑器，编辑器列表在模块加载时读一次。按钮使用 `settings.openIn`，所选编辑器不在列表中时显示为「文件管理器」。选择菜单项会写入 `openIn` 并立即打开。打开失败时用应用已有的提示显示错误文本。
3. **关联会话**：「分叉自」一行，显示父会话标题，图标为 `corner-left-up`。接着是「运行中」的子代理，显示 `owner.nickname`，每项带 `Spinner`。「之前的 N 个」默认折叠。点击任一行调用 `onOpenSession`。三者都没有时不显示这一节。
4. **上下文与用量**：
   - `context` 存在时显示 `used / size`、百分比（取整）和进度条；不存在时显示「暂无」。
   - `lastRun` 存在时显示「最近一次运行」的总 token 数，以及输入和输出数；不存在时不显示这一行（回放不会恢复它，见任务 1）。

**刷新时机：** 子代理状态在 `active` 变为 true、父会话 `running` 发生变化时，对每个子代理调用一次 `queryRunning`。Git 状态在 `active` 变为 true、`host.running(sessionId)` 由 true 变为 false、主机由离线变为 `ready` 这三种情况下重新读取，在途时不重复请求。离线时保留上次结果，并显示 `host` 的离线状态。

- [ ] **步骤 1：写失败测试**
  - `relations split running and previous subagents`：给定父会话、子会话 a（running）、b（awaitsAnswer）、c（空闲），断言 `running` 为 [a, b]，`previous` 为 [c]，`parent` 为父会话。
  - `relations are empty for a session without parent or children`：断言 `parent` 为 undefined，两个数组都为空。
  - `relations treat a subagent reported running by the host as running`：`host.running(c)` 为 false，但 `queryRunning(c)` 返回 true，c 归入 `running`。
  - `relations use ownership, not fork parents, for subagents`：只有 `parentSessionId === sessionId` 的会话不算子代理。
  - `host.test.ts` 的 `queryRunning reads runtime without attaching the session`：返回 `true`，并且 `host.session(id)` 仍为 undefined。
  - `WorkspacePanel.test.tsx` 的 `details tab is first`：标签顺序为 details、files、changes。
  - `SessionView.test.tsx` 的 `header opens the details tab`：点击顶栏按钮后 `tab === "details"`。原有断言打开 `files` 的测试改成 `details`。
  - `details hides Open in on a remote host`：传入 `kind: "remote"` 的主机时，找不到 Open in 按钮。
  - `details shows a missing workspace and keeps the other sections`：`gitStatus` 返回 `missing` 时，页面同时出现目录缺失文案和上下文节。
  - `details keeps the last Git status while offline`：先 `ready` 并读到 `main`，再切到离线，分支 `main` 仍然可见，并显示离线状态。
  - `details shows a Git read failure`：`gitStatus` reject 时显示「无法读取 Git 状态」，上下文节仍然显示。
- [ ] **步骤 2：运行测试，确认失败**（命令同任务 1）
- [ ] **步骤 3：实现 `relations.ts`、`DetailsPanel.tsx`，并完成上面列出的接线**
- [ ] **步骤 4：运行测试，确认通过**，然后运行 `nsh check` 和 `nsh build`，确保没有警告

### 任务 5：设计记录与实机验收

**文件：**
- 修改：`apps/desktop/DESIGN.md`（在「N3 文件浏览与预览」后新增「N3 详情」交互表）
- 修改：本计划（追加验收记录）

- [ ] **步骤 1：在 DESIGN.md 记录详情标签的交互**：标签切换、在…中打开菜单、关联会话折叠、离线状态；每项写明减少动态效果时的表现。
- [ ] **步骤 2：同机 Wayland 双实例实机验收**（启动方法见 [桌面 README](../../../apps/desktop/README.md)）：
  - 在本机和远程会话各看一遍详情。
  - 删除一个测试工作区目录，检查「目录不存在」的显示。
  - 用 `_clawcode/session/fork` 创建分叉会话，检查「分叉自」。另运行一个派生子代理的会话，在子代理运行中和结束后分别检查分组，点击后能打开对应会话。
  - 分别用文件管理器和 zed 打开工作区。
  - 检查 1200 × 800 浅色英文和 800 × 560 深色中文。
  - 截图保存到 `apps/desktop/target/n3-wayland/`。
- [ ] **步骤 3：全量检查**：`cargo test --workspace`、严格 Clippy、`cargo fmt --check`、`pre-commit run --all-files`、`nsh check`、`nsh build`、`nsh test`。把通过的项数写进验收记录。
- [ ] **步骤 4：提交**。等用户明确要求后再提交；提交前先读 `.gitmessage`，并运行 pre-commit 检查。

## 2026-11-27 实施记录

- NSH 命令行已升级到 0.7.2，`nsh-core` 随之从 `=0.7.1` 改为 `=0.7.2`（`Cargo.toml`、`Cargo.lock`、桌面 README）。
- 自动检查：Rust 工作区 1,493 项通过，2 项 MCP 互操作测试按仓库标记忽略；桌面 NSH 197 项通过；`nsh check`、`nsh build`、严格 Clippy、格式检查、仓库 pre-commit 钩子均通过。
- Wayland 原生目标（`GDK_BACKEND=wayland nsh test --target linux`）中详情标签的 4 项交互测试通过：本机的 Git、分叉来源、运行中与之前的子代理、上下文用量和跳转变更；远程主机隐藏「在…中打开」，目录不存在时显示提示；离线后保留最近一次的 Git 状态；Git 读取失败时其余各节照常显示。
- 与计划不同的地方：
  - 详情页的原生测试单独放在 `tests/session/DetailsPanel.test.tsx`。
  - 「在…中打开」启动失败时，错误显示在按钮下方，而不是弹出提示。
  - Git 状态在详情页可见、主机就绪且会话空闲时重新读取。
  - token 数以紧凑形式显示（如 `42k / 200k tokens · 21%`）。
- 待实机补做：同机双实例配对后的远程详情、真实分叉与子代理会话，以及用文件管理器和 zed 打开工作区的截图。

## 2026-11-27 改版：会话详情浮动卡

看过标签版之后，用户改为参照 T3 Code 的会话详情卡，并确定了范围：

- 位置：浮在会话区右上角，宽 280。对话列两侧都放得下卡片时常驻显示，否则从顶栏「会话详情」按钮以弹层打开。右侧面板恢复为「文件」「变更」两个标签。
- 内容：只保留「主机 + 在…中打开」和「会话关系」两组。Git、改动和上下文用量不做。
- 已删除的标签版代码：`gitStatus`、用量的解码和状态、详情标签及其测试与文案。保留的部分：`parentSessionId`、`owner`、`queryRunning`、`ExternalEditors`、设置项 `openIn`。
- 实现：`src/session/SessionDetails.tsx`；`src/client/relations.ts` 新增 `subagentActivity`；`SessionView` 负责测量对话列宽度并决定卡片位置。
- 顺带处理的审查意见：
  - 子代理状态查询只在子代理集合变化或父会话启停时触发。
  - 离线时不显示转圈。
  - 标题和昵称变化时，对应行即时更新。
  - 只有在下拉中选择编辑器时才保存。
  - 中文文案改为「在 %{target} 中打开」。
  - `queryRunning` 校验回复中的会话 id。
- 测试：
  - 单元测试：`relations.test.ts`，覆盖等待审批或回答的子代理算作运行中。
  - 原生测试：`tests/session/SessionDetails.test.tsx`，覆盖宽窗口常驻、窄窗口弹层、远程主机没有打开入口、离线后不显示转圈。
- 文本选择错位：用户在会话中拖选文字时，选中的行比指针低。用探针逐行扫描，确认偏移量正好等于可选视图到窗口顶部的距离：把视图下移 100 pt，偏移量从约 60 pt 变为约 160 pt。原因在 `nsh-platform-linux` 0.7.2 的 `selectable_view::to_area`，指针坐标没有换算到可选区域的坐标系；0.7.2 是目前最新版。绕过方法是把 `Shell` 根视图设为可选区域。在真实外壳中拖选 Line 07 后，选中的是 `m07` 这一行。
- 检查结果：
  - 桌面 NSH 无头测试 189 项全部通过；`nsh check`、`nsh build` 通过。
  - Linux 原生目标下全量 111 项中有 48 项失败。去掉绕过后对照，这些失败数量相同，属于原生目标下已有的问题。
  - `SessionDetails` 宽窗口用例在原生目标下失败，原因是屏幕只有 1600 pt 宽，窗口被限制后卡片按设计改为弹层。

## 2026-11-28 反馈修正

- 详情卡：会话区宽度能放下「对话 760 + 卡片 296」时，卡片默认显示。对话列、输入框和「回到底部」按钮一起向左让出卡片的宽度。窄窗口改为弹层，弹层离顶栏下边留出间距，不再盖住分隔线。
- 子代理：
  - 卡片默认列出全部子代理。
  - 统一使用机器人图标，颜色按昵称哈希，从 Catppuccin 的 8 种强调色中选：浅色主题用 Latte，深色主题用 Mocha。运行中的子代理在图标右下角加一个蓝点。
  - 子代理不再出现在左侧会话列表。
- 运行中的回合实时显示「已工作 X 分 Y 秒」，每秒刷新。读屏只读「正在工作」，不会每秒播报一次。
- 新会话草稿的设置按钮改为显示主机默认模型，数据来自 `_clawcode/session/default_config`。按钮的无障碍名称仍是「会话设置」。
- 会话列表：
  - 悬停时出现「…」按钮，右键也能打开同样的菜单，包含重命名、置顶 / 取消置顶、删除。
  - 重命名在行内编辑：Enter 保存，Esc 或焦点离开时取消，通过 `session/set_title` 保存。
  - 删除前会弹出确认，通过 `session/delete` 执行。
  - 置顶的会话按主机保存在本机，显示在侧栏顶部的「已置顶」组中，不再出现在「最近」里。
- 窗口按钮：用户自己的 `~/.config/gtk-4.0/gtk.css` 处于 USER 优先级，会给窗口按钮加上 M3 圆底。现在把标题栏样式的优先级设为 USER + 1，按钮恢复为扁平样式。
- 文本选择错位已在上游报告：[longbridge/nsh#3568](https://github.com/longbridge/nsh/issues/3568)。
- 检查：桌面 NSH 201 项全部通过；`nsh check` 无类型错误；`nsh build`、`cargo fmt --check`、Clippy 均通过。

## 2026-11-28 改用系统标题栏

- 窗口配置从 `titleBar.style: "hidden"` 改为 `"default"`。KDE Wayland 上由 KWin 绘制原生标题栏，窗口按钮、提示、双击和窗口菜单都由系统提供。
- 应用不再自绘标题栏，已删除 `TitleBar.tsx`、`ext/backend/src/title_bar.{rs,css}`（窗口按钮的样式、优先级覆盖和悬停提示），以及 `window.*` 文案。
- 新增 `NavBar`：参照 Antigravity，放在系统标题栏下方的工具行里，包含侧栏开关、后退/前进和远程设备连接数。侧栏展开时它在侧栏顶部，与会话标题同一行；侧栏收起时它移到会话顶栏的标题前，空白页则放在主区域第一行。
- 检查：桌面 NSH 208 项全部通过；`nsh check`、`nsh build`、Clippy 通过；在 KDE Wayland 上实机截图，确认使用的是系统标题栏。

### 系统标题栏审查修复

- 侧栏收起后，只要当前不显示会话视图，主区域第一行就放导航按钮。条件同时考虑了主机状态：以前主机启动中或失败、但已有打开的会话时，导航按钮会消失，侧栏再也打不开。新增测试 `a_collapsed_sidebar_can_reopen_while_the_open_sessions_host_has_failed`。
- `SessionView` 的 `leading` 插槽改为只读一次，不再多构造一个用不上的 `NavBar`。缩进从 `-ml-3` 改为 `-ml-5`，侧栏展开和收起时导航按钮都在 x = 8pt。新增测试 `the_navigation_buttons_stay_put_when_the_sidebar_collapses`；我把缩进临时改回 `-ml-3` 验证过，测试能报出 16 对 8 的偏差。
- 去掉主区域原来为自绘标题栏保留的左上圆角和顶边线，侧栏与主区域只用一条竖线分隔。远程连接数按钮加了 `min-w-0 shrink`，窄侧栏里可以收缩，不会撑出边界。
- `DESIGN.md` 和 `Shell.tsx` 的注释已改为系统标题栏下的布局说明。
- 检查：桌面 NSH 211 项全部通过，`nsh check` 和 `nsh build` 通过，在 KDE Wayland 上截图对比了侧栏展开和收起两种状态。

## 2026-11-28 NSH nightly 与运行中展示

- **NSH 依赖改用 nightly**：命令行切到 nightly（`0.7.2 54ca221`）。根 `Cargo.toml` 的 `nsh-core` 改为 GitHub 上的 `rev = "54ca221"`。NSH 生成宿主工作区时，会把 `nsh-platform-linux` 等 crate 都指向同一份 git 检出，所以开发宿主也用上了 `main` 的代码。
- **去掉选区绕过**：用真实外壳复验，去掉 `Shell` 根视图的 `selectable` 后，拖选第 07 行选中的就是第 07 行（#3568 已在上游修复）。
- **适配 nightly 的严格类型检查**：nightly 下 `t()` 只接受确定存在的翻译键。项目里 4 处动态拼接的键改为 `Extract<TranslationKey, ...>` 的写法。
- **运行中展开工作过程**：回合运行时，工作区块以及其中的思考和中间消息都默认展开。用户仍可手动折叠，折叠状态记在单独的 `live:` 键下。回合结束后统一回到折叠状态。对应测试是 `running_work_stays_open_and_folds_when_the_turn_ends`。
- **检查**：桌面 NSH 210 项全部通过；`nsh check`、`nsh build`、Clippy、`cargo fmt --check` 通过。

### 补丁位置调整、侧栏和过程图标

- **依赖写法**：根 `Cargo.toml` 恢复为 `nsh-core = "=0.7.2"`，nightly 补丁改放在仓库根的 `.cargo/config.toml`，删掉这个文件即可回到发布版。根 `Cargo.toml` 里的 `[patch]` 不会传给 NSH 生成的宿主和测试运行器工作区（已实测），而 cargo 配置文件里的补丁对两边都生效。所有 nsh crate 都要指向同一个提交：只补丁一部分时，crates.io 上的 `nsh-engine` 与 git 上的 `nsh-core` 混在一起无法编译。在仓库根运行 cargo 时会提示 24 条「patch 未使用」，可以忽略。
- **#3568 回复**：已在上游 issue 回复复验结果。
- **侧栏**：会话行之间留出 2pt 间隙，悬停色和选中色不再贴在一起。
- **过程图标**：「思考」用 brain 图标，中间消息用 message-square 图标，`think` 类工具也改用 brain。
- **检查**：桌面 NSH 210 项全部通过，锁文件在仓库根和 `apps/desktop` 两种运行环境下一致；在原生目标上截图确认了运行中的显示效果。

### 连接状态并入会话标题行

- 打开会话时，主机的连接状态不再单独占一行横幅，而是放在会话标题行右侧：
  - 重连中：黄色圆点闪烁，旁边写「Reconnecting…」。
  - 离线：红色圆点，旁边写「Offline」，完整原因放在悬停提示里；如果是远程主机，旁边还有一个重试图标按钮。
- 没有会话视图时（空白页、主机启动失败），仍使用原来的横幅。
- 测试：重连用例检查重试按钮与导航行垂直居中对齐，并且横幅里的原因文字不再出现。另有 3 个用例的离线判断改为等待「Offline」。桌面 NSH 210 项全部通过，并在原生目标上截图确认。

### 删除会话后菜单出现两份

- **原因**：侧栏是虚拟列表，被换下的单元格会留在复用池里，并保留它上一次绘制的行。菜单和重命名的状态只按行 key 判断，删除一行之后，残留单元格的 key 可能与新打开菜单的那一行相同，于是也弹出一份脱离原行的菜单，飘在左上角。
- **修复**：状态 key 加上单元格编号（`stateKey`），菜单和重命名只在实际被点击的那个单元格里打开。
- **测试**：新增 `after_a_delete_another_rows_menu_opens_once`。修复前它得到 2 份菜单，修复后为 1 份。桌面 NSH 211 项全部通过。

### UI 更新后的同类隐患审查

- **探针结论**：对 `slot?: JSX.Element` 这类插槽，`<If when={props.slot}>{(s) => s()}</If>` 会把元素构造两次，因为 `If` 回调里的 accessor 会重新读取 getter。只构造一次的写法有三种：直接渲染、用 `"slot" in props` 判断、用 `computed` 缓存。
- **修复**：`ui/dialog.tsx` 的 `actions`、`SessionView` 的 `leading` 和 `status`、`WorkspacePanel` 的 `status` 都改为用 `computed` 读取。新增测试 `dialog_actions_are_built_once`，修复前得到 `built 2`，修复后为 `built 1`。
- **已排查、无问题**：
  - 会话流（`TurnRow`、`ToolRow`、`ToolContent`、`ChangeCard`、`MessageActions`）的列表单元里没有弹层或自动聚焦输入框，复用单元不会弹出重复内容。
  - 斜杠命令列表的选中按位置判断，残留的复用单元不可见。
  - 文件树由 nsh 的 `Tree` 按路径管理状态。
  - 其余 `!== undefined` 判断针对的都是普通值，不是 JSX 插槽。
- **检查**：桌面 NSH 212 项全部通过，`nsh build` 通过。
