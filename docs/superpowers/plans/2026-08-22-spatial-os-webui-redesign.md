# Spatial OS WebUI 现代化重设计实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在不改变 ACP 方法和后端协议身份的前提下，将现有 WebUI 升级为已确认的 Spatial OS、Docked Lens 和 Narrative Stream，并完成键盘、响应式、重连恢复与真实后端验收。

**Architecture:** 保留现有 React 19、Zustand、领域 reducer 和原生 CSS 架构，不引入 Tailwind、shadcn 或新的运行时 UI 依赖。视觉层由语义 token 和现有组件类名统一驱动；仅为 sequence range、恢复分界线和 Context Lens 选择补充强类型前端状态，原始 ACP event、Tool start/end 和 Session URL 语义保持不变。

**Tech Stack:** React 19.2、TypeScript 6、Zustand 5、Vite 8、原生 CSS、Lucide React、react-markdown、highlight.js、ACP v2 WebSocket、Rust production `app`。

**Spec:** `docs/superpowers/specs/2026-08-22-spatial-os-webui-redesign-design.md`

## Global Constraints

- spec 和 plan 使用中文；新增函数和非平凡代码注释使用英文。
- 禁止 SubAgent 和 worktree，因此只能在当前会话中使用 `superpowers:executing-plans` 顺序执行。
- 未经用户再次明确允许，不运行 `git commit`；本计划不包含自动 commit 步骤。
- 前端按项目规则免于 TDD 和单元测试，但每个任务结束后运行 TypeScript/ESLint 检查。
- WebUI E2E 必须使用 production `app` 和仓库当前配置 Provider，不使用 fixture backend。
- 不新增 npm 依赖；保留当前精确版本依赖和第三方 notice 变更。
- 保留当前工作区未提交的 Session URL、Markdown、语法高亮和通知日志相关改动，不覆盖用户已有变更。
- 所有 shell 命令及命令链中的每一段均以 `rtk` 开头。
- 不修改 ACP 方法、Kernel Session 创建职责、后端持久化和 transport。

---

## 文件结构与职责

### 新增文件

- `web/src/features/inspector/ContextLens.tsx`：渲染当前选中 Message 或 Tool 的非模态悬浮详情。

### 修改文件

- `web/src/domain/model.ts`：增加 `EventSequenceRange`、`RecoveryNotice` 和 recovery transcript entry。
- `web/src/workspace/sessionState.ts`：在消息和工具 upsert 时维护 sequence range，并归约恢复分界线。
- `web/src/workspace/updateRouter.ts`：在 replay/resume 期间累计恢复事件范围并生成完成摘要。
- `web/src/workspace/controller.ts`：在每个 Session resume 前后开始和完成恢复跟踪。
- `web/src/theme/tokens.css`：定义 Spatial OS 语义色、玻璃材质、阴影、字体和动效 token。
- `web/src/theme/base.css`：定义 Canvas 背景、焦点、选区、滚动条、reduced-motion 和 reduced-transparency 基线。
- `web/src/theme/workbench.css`：落地 Docked Lens、Narrative Stream、Composer、Inspector、能力页面和四档响应式布局。
- `web/src/shell/AppShell.tsx`：编排自适应辅助面板、Context Lens 选择、状态标签和焦点恢复。
- `web/src/features/sessions/SessionSidebar.tsx`：升级 Session 状态和可访问文本，不改变现有业务操作。
- `web/src/features/conversation/Conversation.tsx`：渲染 recovery 分界线，并将消息或工具映射到可定位的 Tree entry。
- `web/src/features/conversation/MessageView.tsx`：改为 Narrative Stream 结构并暴露检查动作。
- `web/src/features/conversation/MessageActions.tsx`：增加可访问的 Context Lens 操作。
- `web/src/features/conversation/ReasoningBlock.tsx`：实现流式时展开、完成后默认折叠的 Reasoning Trace。
- `web/src/features/conversation/ToolCallCard.tsx`：强化 start/end 生命周期、Markdown 输出、JSON 输入和 Inspector 关联。
- `web/src/features/composer/Composer.tsx`：实现 Enter 发送、Shift+Enter 换行和 composition 保护。
- `web/src/features/inspector/Inspector.tsx`：实现可访问 tab 键盘行为，并向 Tree 传递外部定位目标。
- `web/src/features/sessions/NewSessionDialog.tsx`：补齐新建 Session Dialog 的可访问关系。
- `web/src/features/mcp/McpElicitationDialog.tsx`：补齐 Elicitation Dialog 的可访问关系。

Skills 和 MCP 普通列表沿用现有语义 JSX，只通过共享 CSS 获得新视觉，不复制或改写业务组件。

---

### Task 1：建立基线并补充 sequence range 与恢复分界线模型

**Files:**
- Modify: `web/src/domain/model.ts`
- Modify: `web/src/workspace/sessionState.ts`
- Modify: `web/src/workspace/updateRouter.ts`
- Modify: `web/src/workspace/controller.ts`
- Modify: `web/src/features/conversation/Conversation.tsx`

**Interfaces:**
- Produces: `EventSequenceRange`、`RecoveryNotice`、`RecoveryCommit`、`SessionUpdateRouter.beginRecovery()`、`SessionUpdateRouter.finishRecovery()`、`SessionUpdateRouter.cancelRecovery()`。
- Consumes: 现有 `EventOrder`、`RecoveryBatch.cursor`、`SessionWorkspaceAction` 和 `TranscriptEntry`。

- [ ] **Step 1：运行当前前端基线检查**

Run:

```bash
rtk npm --prefix web run check
rtk npm --prefix web run build
```

Expected: 两条命令均成功；如失败，只记录现有失败并先判断是否来自当前未提交改动。

- [ ] **Step 2：增加 sequence range 和恢复实体**

在 `web/src/domain/model.ts` 中增加以下强类型接口，并为 Message、Tool 和 Transcript 补字段：

```ts
export type EventSequenceRange = Readonly<{
  start: number;
  end: number;
}>;

export type RecoveryNotice = Readonly<{
  recoveryId: string;
  eventCount: number;
  sequenceRange: EventSequenceRange;
}>;

export type RecoveryCommit = Readonly<{
  notice: RecoveryNotice;
  order: EventOrder;
}>;
```

`MessageEntity` 与 `ToolCallEntity` 增加可选 `sequenceRange`；`TranscriptEntry` 增加 `{ type: "recovery"; notice: RecoveryNotice; order: EventOrder }`。这些字段只保存 UI 派生范围，不替代原始 ACP event sequence。

- [ ] **Step 3：在 reducer 中维护范围**

在 `sessionState.ts` 定义带英文函数注释的 `EventSequenceRanges.include()`，使用 `Math.min`/`Math.max` 扩展范围。`message/upserted`、两个 delta action 和 `tool/upserted` 都使用 `action.order.sequence` 更新实体范围；新增 `recovery/completed` action，将 notice 作为 transcript entry 按现有 `EventOrdering.compare` 排序。

- [ ] **Step 4：让 Router 跟踪一次 resume 的恢复摘要**

在 `SessionUpdateRouter` 内增加按 Session 隔离的 tracker。公开接口固定为：

```ts
/** Starts a fresh replay summary for one Session resume request. */
beginRecovery(sessionId: SessionId): void;

/** Returns the completed replay summary without retaining stale tracker state. */
finishRecovery(sessionId: SessionId): RecoveryCommit | undefined;

/** Drops an incomplete replay summary after a failed resume request. */
cancelRecovery(sessionId: SessionId): void;
```

`applyMany()` 仅在 tracker 存在且 `RecoveryBatch.cursor` 存在时累计 `sequence..lastSequence`，同一 projection group 只计一次；`finishRecovery()` 生成稳定 recoveryId，并使用最后一个已提交 batch 的 `EventOrder` 作为分界线顺序。

- [ ] **Step 5：在 Controller 的 resume 生命周期中开始和结束跟踪**

`recoverSessions()` 在发送每个 Session 的 `session/resume` 前调用 `beginRecovery(sessionId)`；resume、runtime 刷新完成后调用 `finishRecovery(sessionId)`，有结果时 dispatch `{ type: "recovery/completed", notice: commit.notice, order: commit.order }`。catch 路径调用 `cancelRecovery(sessionId)`，不留下半成品分界线。

- [ ] **Step 6：在 Conversation 中渲染恢复分界线**

处理 `entry.type === "recovery"`，渲染：

```tsx
<div className="recovery-divider" role="status">
  <span>
    已恢复 {entry.notice.eventCount} 条事件 · seq {entry.notice.sequenceRange.start}–{entry.notice.sequenceRange.end}
  </span>
</div>
```

分界线只出现一次，后续 live transcript entry 按顺序显示在其下方。

- [ ] **Step 7：运行前端检查**

Run: `rtk npm --prefix web run check`

Expected: TypeScript 和 ESLint 全部通过。

---

### Task 2：建立 Spatial OS token 与可访问性基础

**Files:**
- Modify: `web/src/theme/tokens.css`
- Modify: `web/src/theme/base.css`

**Interfaces:**
- Produces: 所有后续组件使用的稳定 CSS custom properties。
- Consumes: 无组件接口变化。

- [ ] **Step 1：替换为语义化 Spatial OS token**

`tokens.css` 至少提供以下变量，并保留旧选择器仍引用的兼容变量：

```css
--surface-canvas: #eaf1ff;
--surface-content: #fbfdff;
--surface-glass: rgb(255 255 255 / 72%);
--surface-panel: rgb(242 247 255 / 72%);
--surface-raised: #ffffff;
--surface-muted: #f3f7fc;
--border-subtle: rgb(84 112 163 / 18%);
--border-strong: rgb(84 112 163 / 32%);
--text-primary: #17253b;
--text-secondary: #687893;
--text-tertiary: #7a879b;
--accent: #536ff1;
--accent-hover: #455fd8;
--accent-soft: rgb(83 111 241 / 10%);
--spatial-cyan: #53d8cf;
--success: #13805d;
--warning: #946719;
--danger: #c53f57;
--focus-ring: 0 0 0 3px rgb(83 111 241 / 18%);
--shadow-soft: 0 16px 42px rgb(43 62 101 / 12%);
--shadow-floating: 0 22px 60px rgb(43 62 101 / 17%);
--motion-fast: 140ms;
--motion-standard: 180ms;
```

- [ ] **Step 2：升级基础页面和焦点规则**

`base.css` 使用静态低对比度径向光建立 Canvas，不让正文落在透明背景上；为 `button`、`input`、`textarea`、`summary` 和链接统一 `:focus-visible`。增加 `.visually-hidden` 或保留现有实现，确保仅图标按钮可以包含屏幕阅读器文字。

- [ ] **Step 3：增加材质和动态降级**

在 `prefers-reduced-motion` 中关闭循环动画和非必要过渡；在 `prefers-reduced-transparency` 以及 `@supports not (backdrop-filter: blur(1px))` 下，将 Glass 和 Panel token 覆盖为不透明浅色。

- [ ] **Step 4：运行前端检查**

Run: `rtk npm --prefix web run check`

Expected: PASS。

---

### Task 3：实现 Docked Lens Shell 与四档响应式面板

**Files:**
- Modify: `web/src/shell/AppShell.tsx`
- Modify: `web/src/theme/workbench.css`

**Interfaces:**
- Produces: 1280px 四栏、1024px Inspector 抽屉、768px 双抽屉、手机页面级会话切换。
- Consumes: `SessionSidebar.onCollapse`、`Inspector` 和现有 `PrimarySection`。

- [ ] **Step 1：调整断点状态初始化**

将 AppShell 媒体查询统一到 spec：Session 常驻阈值 `768px`，Inspector 常驻阈值 `1280px`。窗口跨越断点时只调整面板可见性，不重置当前 Session、section 或 Inspector tab。

- [ ] **Step 2：增加抽屉背景和焦点恢复**

为 Session 与 Inspector 切换按钮创建 `useRef<HTMLButtonElement>`；关闭覆盖面板后将焦点恢复到对应触发按钮。`768–1279px` 打开辅助面板时渲染可点击 backdrop，Escape 关闭当前最上层辅助面板；不使用不可聚焦的 `div` 代替按钮。

- [ ] **Step 3：重构 Shell CSS**

宽屏网格使用 `44px 252px minmax(0, 1fr) 336px`；外层留出 10–14px Canvas 间距并给工作台圆角。主导航、Session 和 Inspector 使用 Glass/Panel，主对话使用 Content。折叠只过渡 opacity/transform，不连续动画 grid width。

- [ ] **Step 4：实现四档响应式规则**

- `>=1280px`：四区常驻。
- `1024–1279px`：Inspector 固定右侧抽屉，Session 常驻。
- `768–1023px`：Session 和 Inspector 都是覆盖抽屉，同一时间只打开一个。
- `<768px`：主导航压缩到顶部或保留窄轨道；Session 作为会话列表页面，进入 Session 后主对话全宽，并提供返回会话入口。

触摸布局中的主要按钮不小于 44×44px。

- [ ] **Step 5：运行前端检查**

Run: `rtk npm --prefix web run check`

Expected: PASS。

---

### Task 4：升级 Session、Header、能力页和 Dialog 视觉层级

**Files:**
- Modify: `web/src/features/sessions/SessionSidebar.tsx`
- Modify: `web/src/features/sessions/NewSessionDialog.tsx`
- Modify: `web/src/shell/AppShell.tsx`
- Modify: `web/src/features/mcp/McpElicitationDialog.tsx`
- Modify: `web/src/theme/workbench.css`

**Interfaces:**
- Produces: 统一的 Session 状态、运行头部、能力卡片、空状态和 Raised Dialog。
- Consumes: 现有 Session、MCP、Skill props 和 controller 方法，不新增协议调用。

- [ ] **Step 1：升级主导航与 Session 列表**

主导航当前项增加左侧轨道指示；品牌标志增加连接状态的冗余文本。Session 当前项使用 Accent 左轨和浅背景；Running 同时保留图标、文字和状态点。搜索、新建、重命名和删除流程不改业务语义。

- [ ] **Step 2：升级 Header 状态表达**

连接成功不再占用持续 30px 彩色横条，改为 Header 状态 chip；Connecting、Reconnecting 和 Failed 仍显示持续可见状态条。Context 使用可访问文字和小型状态环，Retry 保留次数和倒计时。

- [ ] **Step 3：统一 Skills、MCP、About 和空状态**

能力页使用相同 Content/Raised 层级、边框、卡片间距和状态色。MCP server 的 ready、degraded、failed 不只依赖颜色。About 使用低信息密度单列，不复制对话布局。

- [ ] **Step 4：统一 Dialog 与 Elicitation**

Dialog 使用不透明 Raised 表面、可见标题、描述、错误和 footer；Backdrop 使用低透明冷色。补齐缺失的 `aria-labelledby`、`aria-describedby` 和关闭按钮名称，不改变现有提交逻辑。

- [ ] **Step 5：运行前端检查**

Run: `rtk npm --prefix web run check`

Expected: PASS。

---

### Task 5：落地 Narrative Stream、Reasoning Trace 与 Tool 生命周期

**Files:**
- Modify: `web/src/features/conversation/Conversation.tsx`
- Modify: `web/src/features/conversation/MessageView.tsx`
- Modify: `web/src/features/conversation/ReasoningBlock.tsx`
- Modify: `web/src/features/conversation/ToolCallCard.tsx`
- Modify: `web/src/features/conversation/CommandMessageCard.tsx`
- Modify: `web/src/features/conversation/CompactionCard.tsx`
- Modify: `web/src/theme/workbench.css`

**Interfaces:**
- Produces: Narrative Stream 视觉结构和 sequence range 诊断展示。
- Consumes: Task 1 的 `sequenceRange` 和 recovery transcript entry。

- [ ] **Step 1：移除 Agent 大气泡外观**

Assistant 消息直接参与文档流：`.message[data-role="assistant"] .message__body` 不使用 Raised 卡片、边框和阴影；用户消息保持右对齐浅 Accent 气泡。Markdown 标题、列表、引用、表格、行内代码和代码块使用文档式间距。

- [ ] **Step 2：实现 Reasoning Trace 默认状态**

`ReasoningBlock` 初始状态使用 `message.streaming`：流式时展开，已完成历史消息默认折叠。用户手动切换后保留当前组件生命周期内的选择；完成后停止运行脉冲。Summary 包含 `aria-expanded` 的原生 details 语义。

- [ ] **Step 3：升级 Tool 生命周期**

Tool summary 同时显示名称、Running/Completed/Failed/Blocked、耗时和图标。输入继续使用 `JsonBlock` 自动换行，输出继续使用 `MarkdownContent`/图片/diff/JSON 分流。详情诊断增加 `Sequence` 或 `SequenceRange`，start/end 原始时间和 `toolCallId` 保持不变。

- [ ] **Step 4：统一 Command、Compaction、Bash 和 Extension**

这些卡片使用同一状态边框、标题区、内容排版和错误语义。Compaction 使用居中的系统分界块，不伪装成 Agent 正文；未知 Extension 保留中性诊断外观。

- [ ] **Step 5：限制流式视觉成本**

流式 cursor 使用静态或低成本 opacity 动画；reduced-motion 下完全静止。消息、Reasoning 和 Tool 内容表面禁止使用 `backdrop-filter`。保留现有 `useTranscriptScroll` 锚点语义。

- [ ] **Step 6：运行前端检查**

Run: `rtk npm --prefix web run check`

Expected: PASS。

---

### Task 6：实现 Context Lens 选择与可访问 Inspector

**Files:**
- Create: `web/src/features/inspector/ContextLens.tsx`
- Modify: `web/src/features/inspector/Inspector.tsx`
- Modify: `web/src/features/inspector/SessionTree.tsx`
- Modify: `web/src/shell/AppShell.tsx`
- Modify: `web/src/features/conversation/Conversation.tsx`
- Modify: `web/src/features/conversation/MessageView.tsx`
- Modify: `web/src/features/conversation/MessageActions.tsx`
- Modify: `web/src/features/conversation/ToolCallCard.tsx`
- Modify: `web/src/theme/workbench.css`

**Interfaces:**
- Produces:

```ts
export type ContextSelection =
  | Readonly<{ type: "message"; messageId: MessageId; treeEntryId?: EntryId }>
  | Readonly<{ type: "tool"; toolCallId: ToolCallId; treeEntryId?: EntryId }>;
```

- Consumes: 当前 Session 的 `messages`、`tools`、Tree、Events 和现有 ToolCallCard。

- [ ] **Step 1：建立 Context Lens 选择状态**

AppShell 持有按 Session 隔离的 `ContextSelection | undefined` 和 Tree 定位 revision。切换 Session 时清空选择；用户点击“在 Context Lens 中检查”时打开悬浮窗和 Inspector，并强制 Inspector 回到 Tree。选择状态不得进入持久化 Workspace，也不得影响 ACP。

- [ ] **Step 2：增加消息检查动作**

`MessageActionsProps` 增加 `onInspect: () => void` 和 `inspecting: boolean`。使用 Lucide 检查图标渲染可访问按钮，提供 `aria-pressed`、`aria-label` 和 title；不把整个消息 article 变成点击区域。

- [ ] **Step 3：增加 Tool 检查入口**

`ToolCallCardProps` 增加可选 `onInspect` 与 `inspecting`。检查按钮放在 details 内容的诊断操作区，避免在 `<summary>` 内嵌套 button。Inspector 自身复用 ToolCallCard 时不再次渲染检查按钮。

- [ ] **Step 4：创建 ContextLens**

新增函数必须带英文函数级注释。悬浮窗使用非模态 dialog 语义，Message 上下文展示角色、MessageId、TurnId、时间、sequence range、模型、usage 和 Markdown 正文；Tool 上下文复用 ToolCallCard。关闭按钮和 Escape 均需恢复触发控件焦点，不复制 Markdown/JSON 解析逻辑。

- [ ] **Step 5：升级 Inspector tab 语义**

Tab button 增加 `aria-selected`、`aria-controls` 和稳定 id；ArrowLeft/ArrowRight 切换 tab，Home/End 跳转首尾。每次检查都重置到 Tree，并让 SessionTree 选中、居中滚动和高亮目标 entry；关闭悬浮窗后不清除 Tree 选中。Tool Call 优先映射同一 Turn 最近的 assistant message，无法可靠映射时不传入 entry。

- [ ] **Step 6：运行前端检查**

Run: `rtk npm --prefix web run check`

Expected: PASS。

---

### Task 7：实现 Composer Enter 语义与 Spatial 输入区

**Files:**
- Modify: `web/src/features/composer/Composer.tsx`
- Modify: `web/src/features/composer/CommandPalette.tsx`
- Modify: `web/src/theme/workbench.css`

**Interfaces:**
- Produces: Enter 发送、Shift+Enter 换行、composition 保护。
- Consumes: 现有 `submit()`、Command Palette、pending queue、图片和资源逻辑。

- [ ] **Step 1：重排 textarea 键盘处理顺序**

`onKeyDown` 固定按以下顺序处理：

```ts
if (event.nativeEvent.isComposing) return;
if (paletteOpen) {
  // Escape、ArrowUp、ArrowDown、Enter 和 Tab 保持命令选择语义。
  return;
}
if (event.key === "Enter" && !event.shiftKey) {
  event.preventDefault();
  if (!submitting && !readingImages && canSubmit) void submit({ text, resources, images });
}
```

不再以 Ctrl/Command + Enter 作为主要发送方式；Shift+Enter 不 `preventDefault()`，由 textarea 插入换行。

- [ ] **Step 2：集中 canSubmit 条件**

定义局部布尔值 `canSubmit`，统一 textarea Enter 和发送按钮的条件：非 submitting、非 readingImages，且文本、资源或图片至少一项有效。空白文本不发送。

- [ ] **Step 3：更新可见提示和队列层级**

提示文字改为“Enter 发送 · Shift+Enter 换行”。Running 时发送按钮文案为“加入队列”，停止按钮保持独立；follow-up 队列位于 Composer 上方并使用中性 Accent 表面，不使用整块警告色。

- [ ] **Step 4：优化附件、资源和 Command Palette**

附件 chip、图片预览、资源表单和 Palette 使用 Raised 表面与可见焦点。Palette 继续支持 ArrowUp/Down、Enter、Tab 和 Escape；长命令签名自动截断但保留 title 或可访问名称。

- [ ] **Step 5：运行前端检查**

Run: `rtk npm --prefix web run check`

Expected: PASS。

---

### Task 8：完成响应式、无障碍和性能收口

**Files:**
- Modify: `web/src/theme/base.css`
- Modify: `web/src/theme/workbench.css`
- Modify: `web/src/shell/AppShell.tsx`
- Modify: `web/src/features/sessions/SessionSidebar.tsx`
- Modify: `web/src/features/sessions/NewSessionDialog.tsx`
- Modify: `web/src/features/conversation/MessageActions.tsx`
- Modify: `web/src/features/composer/Composer.tsx`
- Modify: `web/src/features/inspector/Inspector.tsx`
- Modify: `web/src/features/mcp/McpElicitationDialog.tsx`

**Interfaces:**
- Produces: spec 中 375/768/1024/1280/1440px 的最终行为。
- Consumes: Tasks 2–7 的组件和 class names。

- [ ] **Step 1：检查所有图标按钮和状态**

每个仅图标按钮具有 `aria-label` 或可见/隐藏文字；Running、Success、Warning、Failed 同时使用图标和文本。删除不必要的 `title`-only 语义。

- [ ] **Step 2：检查焦点和 overlay**

键盘可完成导航、Session 操作、Message/Tool 检查、Inspector tab、Command Palette、Dialog 和抽屉关闭。关闭 overlay 后焦点返回触发器；Tab 顺序与视觉顺序一致。

- [ ] **Step 3：检查 overflow 和长内容**

Markdown 表格局部横向滚动；JSON、URL、cwd、SessionId、Tool 输出和错误使用 `overflow-wrap:anywhere` 或局部滚动，不撑破主对话、抽屉或手机视口。

- [ ] **Step 4：检查材质性能**

只允许工作台外框、导航、Session、Header 和 Inspector 使用 blur；消息、代码、Tool、Composer 正文不使用 blur。所有动画只使用 opacity/transform，时长 140–200ms；流式列表不做 stagger 或逐 token 动画。

- [ ] **Step 5：运行前端检查和生产构建**

Run:

```bash
rtk npm --prefix web run check
rtk npm --prefix web run build
rtk git diff --check
```

Expected: 全部成功。

---

### Task 9：代码审查、真实 production app E2E 与性能验收

**Files:**
- Review: 所有本计划修改文件
- Runtime output only: `web/dist`（不提交）

**Interfaces:**
- Produces: 可复现的静态检查、真实 Provider 会话、断线重连和响应式证据。
- Consumes: production `app`、仓库根目录真实配置、构建后的 `web/dist`。

- [ ] **Step 1：审查最终 diff**

Run:

```bash
rtk git status --short
rtk git diff -- web/src docs/superpowers/specs/2026-08-22-spatial-os-webui-redesign-design.md docs/superpowers/plans/2026-08-22-spatial-os-webui-redesign.md
```

检查未覆盖用户现有 URL/Markdown 改动，没有新增依赖，没有全局逐 token 动画，没有敏感日志。

- [ ] **Step 2：运行最终前端门禁**

Run:

```bash
rtk npm --prefix web run check
rtk npm --prefix web run build
```

Expected: PASS，并生成 production `web/dist`。

- [ ] **Step 3：运行相关 Rust 门禁**

本计划不预期修改 Rust。运行：

```bash
rtk cargo check -p app
rtk cargo test -p app
```

Expected: PASS；证明 production app 静态资源和 WebSocket 入口没有回归。

- [ ] **Step 4：启动真实 production app**

Run:

```bash
rtk cargo build -p app --bin clawcode
rtk target/debug/clawcode serve --bind 127.0.0.1:3000 --web-root web/dist
```

使用仓库当前真实配置与 Provider，不设置 fixture、mock 或替代 backend。服务保持运行供浏览器验收。

- [ ] **Step 5：使用 in-app Browser 验收真实会话**

打开 `http://127.0.0.1:3000`，依次验证：

1. 创建 cwd 为本项目的真实 Session。
2. 输入可产生 Thinking、Markdown、代码和 Tool Call 的真实 prompt。
3. 验证 Enter 发送、Shift+Enter 换行和中文输入法 composing 不误发送。
4. Running 时发送 follow-up，确认队列、移除和停止语义。
5. 展开/折叠 Reasoning、Tool 输入输出和 Context Lens。
6. 刷新 `/sessions/:sessionId`，确认自动选中同一 Session。
7. 主动断开 WebSocket 或暂停网络后恢复，验证 initialize、resume、cursor replay、恢复分界线、去重和继续 live stream。
8. 检查 Skills、MCP、About、Dialog 和 Elicitation 的统一视觉。

- [ ] **Step 6：验收响应式和可访问性**

在 375、768、1024、1280 和 1440px 检查布局；仅用键盘完成主导航、Session、Composer、Command Palette、Inspector 和 Dialog；打开 reduced-motion 与 reduced-transparency 模拟，确认材质和动态正确降级。

- [ ] **Step 7：记录性能证据**

使用浏览器 Performance 或 React Profiler 对包含长 replay 的 Session 记录：恢复事件数量、React commit 次数、主要 commit 耗时、主线程长任务和恢复后可交互时间。与修改前可获得的基线或同一 Session 冷刷新对比，不以主观感受代替数据。

- [ ] **Step 8：执行完成前验证并报告**

使用 `superpowers:verification-before-completion`，重新检查最近一次命令输出和真实 E2E 结果。报告实际通过项、性能数据、剩余风险和未创建 commit 的状态。
