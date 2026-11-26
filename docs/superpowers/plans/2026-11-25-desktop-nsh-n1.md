# Desktop NSH N1：外壳、会话与对话 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在本机主机上完成一次需要审批工具调用的完整对话：自绘标题栏与侧栏、会话列表与新建、transcript（流式 Markdown、工作过程折叠、工具摘要、文件改动卡片）、输入框、权限请求、设置对话框，界面中英双语。

**Architecture:**
- **控制层在 `HostStore`**：每台主机持有一个 `AcpConnection`，复用 `src/client` 的 `WorkspaceState` 归并器与 `SessionUpdateRouter`；`WorkspaceStoreAccess` 由一个 signal 实现。会话打开、新建、发送、取消、权限回答都是 `HostStore` 的方法。
- **渲染层按回合分组**：纯函数 `groupTurns` 把一个会话的 transcript 分成回合（用户消息、工作过程、最终回复），工具调用按连续段归组；界面只渲染回合，行内容用按 id 选取的 `computed`，只有变化的行重绘。
- **测试用脚本化 ACP 对端**：后端没有离线模型，自动化测试用 `MemoryTransport` 上的脚本化对端推送真实形状的 `session/update` 与 `session/request_permission`；完整对话的验收用你自己的 provider 配置手工跑。

**Tech Stack:** NSH 0.6.1（TSX、`nsh/base`、`nsh/desktop`、`nsh/i18n`），`ext/backend`（Rust，选目录对话框），现有 `src/client`、`src/ui`、`src/stores`。

**Spec:** [Desktop 客户端迁移到 NSH 设计](../specs/2026-11-25-desktop-nsh-design.md)，重点 §4.4、§5.1、§6、§8（N1）。

## Global Constraints

- worktree `.claude/worktrees/desktop-nsh`，分支 `feat/desktop-nsh`；只在用户明确要求时提交，本计划不含 commit 步骤。
- 连接真实后端的测试一律 `CLAW_CONFIG=$PWD/tests/fixture/config.toml nsh test`（在 `apps/desktop`）。
- 客户端只经 ACP 访问主机数据（spec 决策 14）；本地文件系统只用于附件（N2）与应用自己的设置。
- 页面只用 `src/ui` 组件与 `type`、token 类；颜色、圆角、字号不写在页面里；缺的外观先加到 `ui`。
- 每个页面编码前在 DESIGN.md 写它的动效清单（NSH 设计规范「Motion inventory」）。
- 所有界面文案经 `t()`，`locales/en.yml` 与 `locales/zh-CN.yml` 同时给出；测试里断言英文源串。
- 产品命名空间（ACP `_meta` 键与扩展方法前缀）为 `clawcode`。
- 不在本计划：审批模式与模型菜单、提问、计划、斜杠命令、附件（N2）；右侧面板、「查看变更」打开 diff（N3）；轨迹、消息编辑与分支、Provider 登录（N4）；远程主机（R1）。
- AGENTS.md：每个新函数有英文文档注释；Rust 部分 tracing 宏写全路径、测试只在 `tests/`。

## Review Focus

1. 回复流式输出时用户往上翻看历史，界面不能把人拽回底部；回到底部后才继续跟随。由 Task 5 的 `follows_the_bottom_only_when_at_bottom` 固定。
2. 后端在一个回合中途断开或 `session/prompt` 失败，输入框和「运行中」状态要复原，错误以通知出现，已输入的文字不丢。由 Task 2 的 `prompt_failure_returns_the_text_and_notifies` 固定。
3. 同一个权限请求在别处（另一个客户端）被回答后，本机的审批卡要自动消失，不能残留可点的按钮。由 Task 2 的 `permission_withdrawn_when_answered_elsewhere` 固定。
4. 很长的工作过程（几十个工具调用）折叠后只占一行，展开不卡顿，工具摘要按连续段合并。由 Task 1 的 `consecutive_tools_merge_into_one_group` 与 Task 5 的展开测试固定。
5. 切换会话再切回来，正在运行的会话不重复回放、不丢流式内容。由 Task 2 的 `reopening_a_running_session_does_not_replay` 固定。

---

### Task 1：回合分组（纯逻辑）

**Files:**
- Create: `apps/desktop/src/client/turns.ts`
- Test: `apps/desktop/src/client/turns.test.ts`

**Interfaces:**
- Consumes：`SessionWorkspaceState`（`src/client/workspace/sessionState.ts`）、`MessageEntity`、`ToolCallEntity`、`ToolKind`。
- Produces：

```ts
/** What a group of consecutive tool calls did, for its one-line summary. */
export type ToolActivity = "ran" | "edited" | "read" | "searched" | "fetched" | "other";
/** One item inside a turn's folded work. */
export type WorkStep =
  | Readonly<{ type: "message"; messageId: MessageId }>
  | Readonly<{ type: "tools"; toolCallIds: readonly ToolCallId[]; activities: readonly ToolActivity[] }>;
/** One file a tool changed, from its diff content. */
export type FileChange = Readonly<{ path: string; added: number; removed: number }>;
/** A user message and everything the agent did for it. */
export type TurnView = Readonly<{
  key: string;                       // the user message id, or the first entry's key for a turn without one
  userMessageId?: MessageId;
  steps: readonly WorkStep[];        // commentary and tool groups before the answer
  answerMessageId?: MessageId;       // the last assistant message once the turn ended
  changes: readonly FileChange[];    // merged per path, in first-seen order
  running: boolean;
  durationMs?: number;               // last entry's end minus the user message's start, once ended
}>;
export function groupTurns(session: SessionWorkspaceState): readonly TurnView[];
export function toolActivity(kind: ToolKind | undefined): ToolActivity;
export function fileChanges(tool: ToolCallEntity): readonly FileChange[];
```

- 规则：`execute`→`ran`；`edit`、`delete`、`move`→`edited`；`read`→`read`；`search`→`searched`；`fetch`→`fetched`；其余→`other`。`activities` 去重、按首次出现顺序。只有最后一个回合可能在运行（`session.running` 为 true 时）；运行中的回合没有 `answerMessageId`，全部消息都在 `steps` 里。`fileChanges` 读 `content` 中 `type: "diff"` 的项（`path`、`oldText`、`newText`），按行计算增删（`oldText` 为空视为新增）。

- [ ] **Step 1：写测试 `turns.test.ts`**（用手写的 `SessionWorkspaceState` 片段）
  - `answer_is_the_last_assistant_message_of_an_ended_turn`：用户 u1、助手 a1、工具 t1、助手 a2，回合已结束 → `steps` 为 `[a1, tools(t1)]`，`answerMessageId` 为 a2。
  - `running_turn_keeps_every_message_in_steps`：同上但 `running: true` → `answerMessageId` 为 `undefined`，`steps` 含 a2。
  - `consecutive_tools_merge_into_one_group`：t1(execute)、t2(read)、t3(execute) 连续 → 一个 `tools` 组，`activities` 为 `["ran", "read"]`；中间夹一条助手消息时拆成两组。
  - `file_changes_count_lines_per_path`：两个工具改同一路径（+3 −1、+2 −0）与另一路径新增 4 行 → `[{ path: "src/a.rs", added: 5, removed: 1 }, { path: "src/b.rs", added: 4, removed: 0 }]`。
  - `turns_split_at_user_messages`：两条用户消息 → 两个回合，`key` 为各自消息 id。
- [ ] **Step 2：运行确认失败**：`nsh test src/client/turns.test.ts`（在 `apps/desktop`）。
- [ ] **Step 3：实现 `turns.ts`。**
- [ ] **Step 4：运行确认通过。**

---

### Task 2：`HostStore` 成为会话控制器

**Files:**
- Modify: `apps/desktop/src/stores/host.ts`、`apps/desktop/src/stores/app.ts`（`openSession` 同时调用主机的 `openSession`）
- Create: `apps/desktop/tests/support/scriptedAgent.ts`
- Test: `apps/desktop/src/stores/host.test.ts`（扩充）

**Interfaces:**
- Consumes：`WorkspaceState`、`reduceWorkspace`、`initialWorkspaceState`、`SessionUpdateRouter`、`WorkspaceStoreAccess`、`AcpProtocol`、`AcpMethods.forNamespace`。
- Produces（`HostStore` 新增与变化的成员；`info`、`phase`、`start`、`stop` 不变）：

```ts
workspace: Accessor<WorkspaceState>;
/** The listed Sessions as summaries (was SessionInfo[]); titles fall back to the Session id. */
sessions: Accessor<readonly SessionSummary[]>;
/** Shows a Session: replays its transcript unless it is running and already attached. */
openSession: (sessionId: SessionId) => Promise<void>;
/** Creates a Session in `cwd`, refreshes the list and opens it; its id. */
newSession: (cwd: string) => Promise<SessionId>;
/** Sends `text`; resolves when the turn ends. On failure the text comes back in `returnedText` and a notice is added. */
prompt: (sessionId: SessionId, text: string) => Promise<void>;
/** Text a failed prompt handed back, by Session, until `takeReturnedText` reads it. */
returnedText: Accessor<ReadonlyMap<SessionId, string>>;
takeReturnedText: (sessionId: SessionId) => string | undefined;
/** Asks the agent to stop the running turn. */
cancel: (sessionId: SessionId) => void;
/** Answers a queued permission request; `undefined` dismisses it. */
answerPermission: (key: string, optionId: string | undefined) => void;
```

- `initialize` 参数：`protocolVersion: 2`、`info: { name: "clawcode-desktop", version: "0.1.0" }`、`capabilities: {}`、`_meta: { clawcode: { sessionUpdateBatch: { version: 1 } } }`。
- 会话列表映射成 `SessionSummary`：照搬 WebUI 的 `sessionSummary`（`web/src/workspace/controller.ts:916`），写成 `host.ts` 内的函数。
- 通知：`session/update` 经 `AcpProtocol.expandSessionUpdateBatch(…, "clawcode")` 展开后交给 `SessionUpdateRouter.applyMany`；路由器的三个回调：`refreshSessionRuntime` 调扩展方法 `sessionRuntime` 并派发 `running/changed`，`refreshSessionTree` 在 N1 为空操作，`listSession` 重新拉会话列表。
- 打开会话：未附加过或不在运行时，`requireFullReplay` → 派发 `transcript/cleared` → `session/resume`（`replayFrom: { type: "start" }`）→ 查 `sessionRuntime`；已附加且在运行时只查运行状态。照搬 WebUI 的 `loadAttachedSession` 的这一部分。
- 发送：`session/prompt` 带 `_meta.clawcode.submissionId`（`crypto.randomUUID()`）；失败时把文字放回 `returnedText`，并派发 `notice/added`（标题 `t("notice.promptFailed")`，说明为错误信息）。
- 权限：注册 `session/request_permission` 处理器，照搬 WebUI 的 `requestPermission`（`controller.ts:578`）：入队 `permission/asked`，回答或 `$/cancel_request` 撤回时 `permission/settled`。
- `scriptedAgent.ts` 导出 `scriptedAgent(script: AgentScript): (frame, transport) => void` 与帧构造函数 `agentText`、`userText`、`toolCall`、`toolUpdate`、`turnEnd`（带 `_meta.clawcode` 的 `turnId`、`timestampMs`、`sequence`），`AgentScript` 定义 `sessions`、每个会话 `resume` 时回放的帧、`prompt` 时依次推送的帧、是否在中途发 `session/request_permission` 并等待回答、`prompt` 的结果（`stopReason` 或错误）。帧的字段以 `src/client/workspace/updateDecoder.ts` 的解析为准。

- [ ] **Step 1：写测试**（扩充 `host.test.ts`）
  - `opening_a_session_replays_its_transcript`：脚本回放一条用户消息与一条助手消息 → `openSession` 后该会话 `transcript.length === 2`。
  - `prompt_streams_a_reply`：`prompt` 期间脚本推三段 `agent_message_chunk`，结束后该消息文本为三段拼接，`running` 为 false。
  - `permission_request_waits_for_the_answer`：脚本发出权限请求 → `workspace().permissions.length === 1`；`answerPermission(key, "allow-once")` 后脚本收到 `{ outcome: { outcome: "selected", optionId: "allow-once" } }`，队列清空。
  - `permission_withdrawn_when_answered_elsewhere`：脚本发出权限请求后再发 `$/cancel_request` → 队列清空，`answerPermission` 不再发送任何帧。
  - `prompt_failure_returns_the_text_and_notifies`：脚本对 `prompt` 回错误 → `takeReturnedText(id) === "hello"`，`workspace().notices.length === 1`，会话不在运行。
  - `cancel_sends_session_cancel`：`cancel(id)` → 脚本收到 `session/cancel` 通知，参数含该 `sessionId`。
  - `reopening_a_running_session_does_not_replay`：会话在运行且已打开过，再次 `openSession` → 脚本收到的 `session/resume` 仍只有一次。
- [ ] **Step 2：运行确认失败**：`nsh test src/stores/host.test.ts`。
- [ ] **Step 3：实现 `scriptedAgent.ts` 与 `host.ts` 的新成员；`app.ts` 的 `openSession` 先导航再 `void host.openSession(id)`。**
- [ ] **Step 4：运行确认通过**：`nsh test src/stores/`（三个文件分别运行），全部通过。

---

### Task 3：i18n 与设置

**Files:**
- Create: `apps/desktop/locales/en.yml`、`apps/desktop/locales/zh-CN.yml`
- Create: `apps/desktop/src/stores/settings.ts`
- Create: `apps/desktop/src/settings/SettingsDialog.tsx`
- Modify: `src/ui/*` 中写死的英文（`Close`、`Dismiss`、`Copy`、`Copied`、`Cancel`）改为 `t()`
- Test: `apps/desktop/src/stores/settings.test.ts`、`apps/desktop/src/settings/SettingsDialog.test.tsx`

**Interfaces:**
- Produces：

```ts
export type Appearance = "system" | "light" | "dark";
export type Language = "system" | "en" | "zh-CN";
export type Settings = Readonly<{ appearance: Appearance; language: Language }>;
export const DEFAULT_SETTINGS: Settings; // { appearance: "system", language: "system" }
/** The app's settings: loaded from `Storage` key "settings" (JSON), applied (`setColorScheme`, `setLocale`) and saved on change. */
export type SettingsStore = Readonly<{
  settings: Accessor<Settings>;
  load: () => Promise<void>;                 // unreadable or missing JSON keeps the defaults
  update: (change: Partial<Settings>) => void; // applies at once, saves (debounced 300 ms)
}>;
export function createSettingsStore(): SettingsStore;
```

- `SettingsDialog`：`ui` 的 `Dialog`，标题 `t("settings.title")`，两组 `Select`：外观（跟随系统 / 浅色 / 深色）与语言（跟随系统 / English / 简体中文）。
- 设置存在 NSH 的 `Storage`（应用私有键值存储），不另写 `desktop.json` 文件。

- [ ] **Step 1：写测试**
  - `settings.test.ts`：`missing_settings_keep_defaults`（`Storage` 为空 → 默认值）；`corrupt_settings_keep_defaults`（`"settings"` 为 `"{"` → 默认值）；`update_saves_after_debounce`（`update({ appearance: "dark" })` 后 400 ms 内 `Storage.getItem("settings")` 为 `{"appearance":"dark","language":"system"}`）。
  - `SettingsDialog.test.tsx`：打开对话框选「简体中文」后标题变为「设置」；`open(…, { locale: "zh-CN" })` 时关闭按钮的无障碍名是「关闭」。
- [ ] **Step 2：运行确认失败。**
- [ ] **Step 3：写 locales（至少覆盖本计划所有界面串）、`settings.ts`、`SettingsDialog.tsx`；`ui` 的写死文案改 `t()`。**
- [ ] **Step 4：运行确认通过**：`nsh test src/stores/settings.test.ts`、`nsh test src/settings/`。

---

### Task 4：外壳——标题栏与侧栏

**Files:**
- Create: `apps/desktop/src/shell/{Shell,TitleBar,Sidebar,HostSwitcher,SessionRows}.tsx`
- Modify: `apps/desktop/App.tsx`（`<Window>` 隐藏系统标题栏，根组件换成 `Shell`）、`apps/desktop/ext/backend/src/lib.rs`（新增 `Folders.pick`）
- Modify: `apps/desktop/DESIGN.md`（外壳的动效清单）
- Test: `apps/desktop/App.test.tsx`、`apps/desktop/src/shell/Shell.test.tsx`

**Interfaces:**
- Consumes：Task 2 的 `HostStore`；Task 3 的 `SettingsStore`、`SettingsDialog`；`AppStore`。
- Produces：
  - `ext/backend`：`#[module] Folders { pub async fn pick() -> Result<Option<String>, String> }`，内部 `pick_paths(OpenDialog::folders())`，取第一个路径；取消为 `None`。
  - `Shell`：标题栏（`TitleBar`）+ 侧栏（`Sidebar`）+ 主区（Task 5 的 `SessionView` 或空状态，Task 5 前先放空状态）。
  - `TitleBar`：高 44pt，`windowDrag`；左侧侧栏开关、后退、前进（`IconButton`，`canGoBack`/`canGoForward` 控制 `disabled`）；右侧留出 `titleBar().right` 的宽度给系统窗口按钮。`<Window titleBar={{ style: "hidden", height: 44 }}>`。
  - `Sidebar`（280pt，`bg-sidebar`，可由标题栏开关收起）：`HostSwitcher`（主机名 + chevron 的 `Menu`，列出 `hosts()`，当前项勾选）；「新会话」行；「工作区」：按 `cwd` 分组，组名为目录名（完整路径做 tooltip），可展开，组内会话按 `modifiedAtMs` 降序；「最近」：全部会话按 `modifiedAtMs` 降序取前 10；底部「设置」行打开 `SettingsDialog`。
  - 会话行（`ListItem`）：行尾标记——运行中（`runningSessionIds` 含它）显示 `Spinner` sm，有待审批权限显示 `info` 色 `StatusDot`（无文字时用无障碍名 `t("session.needsAttention")`）。
  - 空状态（无会话打开）：居中图标与 `type.heading` 标题 `t("empty.heading", { workspace })`，工作区名是一个 `Select`（已有工作区 + 「选择文件夹…」，后者调用 `Folders.pick()`），下方是 Task 6 的输入框（Task 6 前放占位）。
- `App.test.tsx` 的 `connects_and_lists_sessions` 改为等 `text:New session`（只在主机就绪后出现）。

- [ ] **Step 1：在 DESIGN.md 写外壳动效清单**：侧栏收起/展开（`LayoutAnimation`，`base`）、选中会话胶囊（`instant`）、工作区组展开（chevron 旋转 `fast`，行用 `LayoutAnimation` `base`）、主机菜单开合（沿用弹层）、运行中标记（`Spinner` 持续旋转）、减少动效时全部瞬时。
- [ ] **Step 2：写测试 `Shell.test.tsx`**（测试入口 `tests/entries/shell.tsx` 用 `scriptedAgent` 的主机挂载 `Shell`，三个会话分属两个 `cwd`）
  - `sidebar_groups_sessions_by_workspace`：等两个工作区名与三个会话标题。
  - `opening_a_session_selects_it_and_back_returns`：按会话 A、再按 B，按「Back」后 A 的行为选中。
  - `running_session_shows_a_spinner`：脚本把 B 标为运行中 → B 行内有 `label:Running`。
  - `new_session_in_a_picked_folder`：`modules` 替身让 `Folders.pick` 返回 `/work/new`，空状态里选「Choose folder…」并发送「hi」→ 脚本收到 `session/new`，`cwd` 为 `/work/new`。
- [ ] **Step 3：运行确认失败。**
- [ ] **Step 4：实现**；`cargo test -p clawcode-backend` 不受影响。
- [ ] **Step 5：运行确认通过**：`nsh test src/shell/`、`CLAW_CONFIG=$PWD/tests/fixture/config.toml nsh test App.test.tsx`。

---

### Task 5：会话视图与 transcript

**Files:**
- Create: `apps/desktop/src/session/{SessionView,TurnRow,WorkDisclosure,ToolGroup,ChangeCard,MessageActions}.tsx`
- Modify: `apps/desktop/src/ui/`（如需新增 `Disclosure` 折叠行，放进 `ui` 并加入展示页）
- Modify: `apps/desktop/DESIGN.md`（会话视图动效清单）
- Test: `apps/desktop/src/session/SessionView.test.tsx`（入口 `tests/entries/session.tsx`）

**Interfaces:**
- Consumes：Task 1 的 `groupTurns`、`fileChanges`、`ToolActivity`；Task 2 的 `HostStore`。
- Produces：
  - `SessionView`：顶栏（工作区图标 + 会话标题；「更多」菜单与面板开关在 N3 随右侧面板一起加，N1 不放没有作用的按钮）；`List` 渲染 `groupTurns(...)`，`keyExtractor` 为 `TurnView.key`，正文一列最宽 760pt 居中；底部留给 Task 6 的输入框。
  - 每个回合行（`TurnRow`）：用户消息为靠右的 `secondary` 圆角胶囊（`type.body`）；`WorkDisclosure`：结束的回合显示 `t("turn.workedFor", { duration })` 加 chevron，默认收起，运行中的回合默认展开并显示 `t("turn.working")` 与呼吸的光标；展开后依次是说明消息（`TextView` Markdown，`type.body`）与 `ToolGroup`；时长格式：不足 1 分钟为 `12s`，否则 `3m 10s`；`ToolGroup` 一行灰色摘要（`activities` 映射为 `t("tools.ran")` 等并用「、」/", " 连接，图标按首个活动：`ran`→`terminal`，`edited`→`pencil`，`read`→`file-text`，`searched`→`search`），点开是每个工具一行（标题，左侧竖线）；回答消息为 `TextView` Markdown（`options={{ streaming: true }}`）；`ChangeCard`：每个改动文件一张卡片「已编辑 文件名 +N −N」（「查看变更」按钮 N3 再接，N1 不显示）；`MessageActions`：复制回答（`CopyButton`）与时间。
  - 行内容通过 `computed` 按 id 选取实体（`messages.get(id)`、`tools.get(id)`），实体对象不变时下游不重算——满足 spec §6「只重绘变化的行」。
  - 跟随底部：在底部时新内容到来调用 `scrollToEnd`；用户上翻离开底部后不再跟随，显示圆形「回到底部」按钮，按下回到底部并恢复跟随。
- 需要的新 Lucide 图标（`terminal`、`pencil`、`file-text`、`arrow-down`）用 N0 的脚本生成并注册。

- [ ] **Step 1：在 DESIGN.md 写会话视图动效清单**：新回合用户胶囊上浮 8pt 淡入（`slow`）、工作过程展开（chevron `fast`，内容 `LayoutAnimation` `base`）、流式光标呼吸（运行时，静止时停）、回答结束后操作行淡入（`fast`）、回到底部按钮出现与消失（`fast`）、减少动效时全部瞬时、光标静止。
- [ ] **Step 2：写测试 `SessionView.test.tsx`**
  - `ended_turn_folds_work_under_worked_for`：回放一个结束的回合（说明 + 两个工具 + 回答）→ 可见 `Worked for` 与回答文本，看不到工具标题；按 `Worked for` 后工具摘要 `Ran commands` 出现，再按它出现两个工具标题。
  - `running_turn_shows_work_expanded`：脚本推一个未结束回合 → 可见 `Working`。
  - `file_changes_show_a_card`：回合内有 diff 工具 → 可见 `Edited a.rs` 与 `+3`。
  - `follows_the_bottom_only_when_at_bottom`：回放 60 个回合并在底部 → 推新回答后最后一行可见；`app.scroll` 上翻后再推内容 → 出现 `label:Jump to bottom`，最后一行不在屏幕上；按它后最后一行可见。
- [ ] **Step 3：运行确认失败。**
- [ ] **Step 4：实现**，`Shell` 的主区在有会话打开时显示 `SessionView`。
- [ ] **Step 5：运行确认通过**：`nsh test src/session/`。

---

### Task 6：输入框与权限审批

**Files:**
- Create: `apps/desktop/src/composer/{Composer,ContextStrip,PermissionCard}.tsx`
- Modify: `apps/desktop/src/ui/input.tsx`（`Textarea` 超过最大高度后跟随光标）
- Modify: `apps/desktop/DESIGN.md`（输入框与审批卡动效清单）
- Test: `apps/desktop/src/composer/Composer.test.tsx`（入口 `tests/entries/session.tsx`）

**Interfaces:**
- Consumes：Task 2 的 `prompt`、`cancel`、`takeReturnedText`、`answerPermission`、`workspace().permissions`。
- Produces：
  - `Composer`（`ui` 的 `Surface variant="floating"`，宽同 transcript 列）：`Textarea`（Enter 发送、Shift+Enter 换行、2–8 行）；右下圆形按钮：空闲时为发送（`primary`，`arrow-up` 图标，输入为空时 `disabled`），运行中为停止（`square` 图标，调用 `cancel`）；左下 `+` 按钮在 N1 为 `disabled`（附件是 N2），带 tooltip `t("composer.attachSoon")`。
  - 新会话时（空状态）卡片上方显示 `ContextStrip`：主机（`laptop` 图标 + 主机名）与工作区（`folder` 图标 + 目录名）两个 ghost 小按钮，点开分别是主机菜单与工作区 `Select`（复用 Task 4 的组件）。
  - 发送失败时，`takeReturnedText` 的文字放回输入框。
  - `PermissionCard`：当前会话有待审批请求时，浮在输入框上方的卡片（`Surface variant="card"`）：标题、说明（如有），每个选项一个按钮——`kind` 为 `allow_once`/`allow_always` 用 `secondary`，`reject_*` 用 `ghost`，第一个 `allow_once` 用 `primary`；按 Escape 等于拒绝（`answerPermission(key, undefined)`）。
  - `Textarea` 跟随光标：用 `onCaretMove` 的 `bounds` 判断光标是否在可视区之外，必要时滚动输入框使光标可见；若 NSH 0.6.1 的多行 `Input` 没有可调用的滚动方法，把 `Textarea` 包进一个高度受限的 `Scroll`，由 `Scroll` 负责滚动。这一项的验证在真实窗口截图里做（Task 7）。
- 需要的新 Lucide 图标（`arrow-up`、`square`）同上生成。

- [ ] **Step 1：在 DESIGN.md 写输入框与审批卡动效清单**：发送/停止字形交换（`base`）、按下缩放（沿用按钮）、审批卡从输入框上沿升起 8pt 淡入（`slow`），回答后下沉淡出（`base`），上下文条随新会话出现、发出后淡出（`fast`），减少动效时瞬时。
- [ ] **Step 2：写测试 `Composer.test.tsx`**
  - `enter_sends_and_the_button_becomes_stop`：输入「hello」按 Enter → 脚本收到 `session/prompt`，文本为 `hello`；回合进行中 `label:Stop` 可见；回合结束后 `label:Send` 可见且输入框为空。
  - `stop_cancels_the_turn`：运行中按 `Stop` → 脚本收到 `session/cancel`。
  - `failed_prompt_puts_the_text_back`：脚本对 `prompt` 回错误 → 输入框的值回到 `hello`，出现通知。
  - `permission_card_answers_and_disappears`：脚本在回合中请求权限（选项 `allow-once` / `reject-once`）→ 卡片显示标题；按 `Allow once` → 脚本收到所选选项，卡片消失。
  - `escape_rejects_the_permission`：卡片出现后按 Escape → 脚本收到 `cancelled` 结果。
- [ ] **Step 3：运行确认失败。**
- [ ] **Step 4：实现**，`SessionView` 与空状态挂上 `Composer`。
- [ ] **Step 5：运行确认通过**：`nsh test src/composer/`。

---

### Task 7：验收与收尾

- [ ] **Step 1：全部自动化检查**
  - 在 `apps/desktop`：`nsh check && nsh build && CLAW_CONFIG=$PWD/tests/fixture/config.toml nsh test`
  - 在仓库根：`cargo fmt --all --check && cargo clippy --workspace --all-targets -- -Dwarnings && cargo test -p clawcode-backend`，`pre-commit run rig-boundary --all-files`
  - `git status --short -- web` 为空。
- [ ] **Step 2：外观检查**：用脚本化主机的测试入口（`tests/entries/shell.tsx`、`tests/entries/session.tsx`）在 `--target linux` 截图：侧栏、空状态、结束的回合（收起与展开）、运行中的回合、审批卡、设置对话框，亮暗各一套，中英文各一张；跑完工检查清单，结果写进 DESIGN.md 的检查记录；`Textarea` 超过 8 行时光标仍可见。
- [ ] **Step 3：真实对话验收（需要你配合）**：请你确认后，用你自己的 provider 配置运行 `nsh run apps/desktop`，新建一个会话，让 agent 执行一个需要审批的工具调用（例如在临时目录里写一个文件），在界面上批准，看到流式回复、工作过程折叠、文件改动卡片与「Worked for」；同时用你的输入法（fcitx 或 ibus）在输入框里输入中文并在组字时按 Enter，确认不会误发送。截图记录。
- [ ] **Step 4：报告**：截图、检查记录、遗留到 N2/N3 的事项。
