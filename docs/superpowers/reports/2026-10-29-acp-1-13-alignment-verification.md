# ACP 3.6 / 1.13 对齐实现与验证记录

## 当前结论

设计 `docs/superpowers/specs/2026-10-29-acp-1-13-alignment-design.md` 与计划 `docs/superpowers/plans/2026-10-29-acp-1-13-alignment-implementation.md` 的阶段 A–E 全部完成。代码保留在 `main` 工作区，按用户选择**没有创建 worktree，也没有任何 commit**。

每个阶段的 WebUI 验收都使用生产 `clawcode serve`、DeepSeek 官方 API（密钥只经环境变量 `DEEPSEEK_API_KEY` 传入）和 headless Chrome（`playwright-core`）完成，没有使用 fixture backend。

2026-10-29 的 review 后修复了两项问题：消息 hook / Handled input 期间的插入前取消，以及 elicitation 字符串长度的 Unicode 码点计算。本轮独立复验结果见末节；仍未创建 commit。2026-10-30 又修复了第二轮 review 的前四项问题，见「Review 修复（2026-10-30）」；仍未创建 commit。

## 自动化验证

| 命令 | 结果 |
| --- | --- |
| `rtk cargo fmt --all -- --check` | 通过 |
| `rtk cargo clippy --workspace --tests --examples -- -Dwarnings` | 通过，无问题 |
| `rtk cargo test --workspace --no-fail-fast` | **928 passed，9 ignored，107 suites**（2026-10-30 复验） |
| `web/` 下 `npm run test:elicitation` | 3 个 Unicode 边界回归通过，无新增依赖 |
| `web/` 下 `rtk npm run check` | TypeScript 与 ESLint 通过 |
| `web/` 下 `rtk npm run build` | 生产构建通过 |

已知的负载敏感用例：

- `kernel::provider_runtime::provider_stream_lifecycle_is_logged_without_message_content`：实现前已存在。
- `mcp::oauth::oauth_authorizes_and_reuses_persisted_credentials`：启动超时只有 3 s。

这两个用例只在整个工作区并行运行时偶发失败，单独运行均多次通过，最终一轮全量测试全部通过。本次没有修改它们。

## 阶段与验收（首次实现记录）

### 阶段 A：prompt 接受语义、user-bash、取消、投影与恢复

**内容：**
- prompt 在插入时应答并返回 `messageId`。
- 插入前取消返回 `-32800`，会话不活跃返回 `-32002`，版本协商不符返回 `-32022`（由 SDK 处理）。
- `!bash` 成为带 run id 的操作，有开始、结束和恢复。
- close 带 10 s 宽限期。
- 发送确认（`SessionDelivery`）。
- resume 依次发送标题和状态快照；回放不含历史 `state_update` / `notice`。

**WebUI 验收 9/9：**
- prompt 已插入并得到回答，输入框清空。
- `!echo` 只显示一张 bash 卡片。
- `/session` 命令。
- 长 `!bash` 刷新后仍显示运行中，结束后为空闲。
- 停止后保留新输入的内容。
- 无控制台错误。

### 阶段 B：状态与实体

**内容：**
- `requires_action`。
- 工具 `name` / `kind` / `locations`，以及贯穿持久化的 `cancelled` 标记。
- 标准 `compaction_update`；卡片细节放在 `_meta.clawcode.compaction`。
- 只发实时的 warning notice。
- idle 携带本轮 usage。
- setup 响应携带 `availableCommands`。

**WebUI 验收 9/9：**
- prompt 诊断以 toast 显示，同一次附着只显示一次，6 秒后自动消失。
- 工具被停止后，在实时阶段、收到结果消息后、刷新后都保持「已取消」。
- `/compact` 显示压缩卡片，刷新后仍在。
- 无控制台错误。

### 阶段 C：标准 elicitation

**内容：**
- Kernel 把 `tool_call_id` 关联进 elicitation，并新增 `ElicitationResolved`。
- 每个连接一个 `ElicitationBridge`：
  - 按 initialize 声明的模式发送 `elicitation/create`；
  - 在 watcher 之外等待回复；
  - 别处已回答时发 `$/cancel_request`；
  - resume 时重建仍挂起的请求；
  - schema 无法编译或回答不合法时按 cancel 处理。
- 删除 `_clawcode/mcp/elicitation/list|respond`。

**WebUI 验收 25/25**，使用参考 MCP stdio server 新增的 `ask` / `visit` 工具：
- 表单按字段类型渲染，并预填默认值；缺少必填项时拦截提交；回答到达工具。
- 拒绝、Escape 取消。
- URL 模式显示 host，确认后打开页面。
- 两个标签页时先回答者生效，另一页的对话框被撤回。
- 刷新后重新提问，回答到达工具。
- 无控制台错误。

### 阶段 D：session config options

**内容：**
- 共享的 `SessionSettingsUpdate`：持久化、发出扩展事件和 `SettingsChanged`；按会话串行。
- `configOptions` 包含 model 和 thinking_level。
- `session/set_config_option` 先等待本连接发送完更新，再在发送许可内读取快照并响应。

**WebUI 验收 11/11：**
- 页头显示模型与 thinking 下拉框。
- 切换后下一条回答来自新 provider。
- 刷新后两项选择保持。
- 无控制台错误。

### 阶段 E：terminal 实体

**内容：**
- bash 结果保留 exit code 与 signal：执行器在丢弃 `ExitStatus` 前读取 signal，由我们终止时记为 SIGKILL。
- 失败保留 details。
- `!bash` 实时输出事件 `UserBashOutput`；消息保存 cwd。
- bash 工具与 `!bash` 映射为 ACP terminal：开始、只发 terminal 的进度、带 exitStatus 的终态，回放可重建。
- journal 中每个 terminal 只保留最新输出快照。
- WebUI `TerminalView`；`!bash` 以「用户执行」工具卡片显示。

**WebUI 验收 14/14：**
- `!bash` 实时输出与退出码。
- SIGTERM 显示信号并标为失败；停止后显示 SIGKILL 并标为已取消。
- 长命令运行中刷新恢复最新输出，并能运行到结束。
- 刷新后已结束的命令仍显示保存的 SIGTERM 与 cwd。
- 模型 bash 工具的 terminal 显示输出与退出码 3。
- 无控制台错误。

## 验收环境差异

各阶段使用 `scratchpad/e2e/claw.toml` 的副本，只做以下改动：

| 阶段 | 改动 | 原因 |
| --- | --- | --- |
| B | `keep_recent_tokens = 50` | 默认 20000 时短会话全部算作「最近内容」，`/compact` 会以 NoContextToCompact 拒绝 |
| C | 启用参考 MCP server（deferred），隐藏 `hidden_probe` | DeepSeek 拒绝该探针的空 input schema；早先的 MCP 验收也这样配置 |
| D | 新增指向同一端点与模型的 `deepseek-alt` provider | 不猜测其他 DeepSeek 模型 id，通过 Provider 元信息验证切换 |

## 主要实施调整

以下是对计划的调整；完整的 Ruling 清单见最终汇报。

- `SettingsChanged` 携带完整的 `SessionSettings`（已移入 protocol crate），因为 `ConfigOptionUpdate` 必须给出完整的选项列表。
- 设置修改按会话串行，扩展观察者在锁外运行，避免并发修改造成快照乱序或重入死锁。
- bash 被信号终止不再算成功，改为错误结果「Command terminated by SIGxxx」。
- 非零退出、超时、中止都返回带 details 的错误结果，文本与原错误一致。
- 同一连接上重复 resume 不再重复发送启动诊断：WebUI 的 new→resume 原先会让 toast 翻倍。
- `elicitation/create` 在 `_meta.clawcode.mcpServerId` 中携带 MCP server，供对话框显示。
- 参考 MCP 示例新增 `ask` / `visit` 工具；rmcp 的 `elicitation` feature 只在 mcp crate 的 dev-dependency 中开启。

## 未覆盖或延后

- 首轮未覆盖的等待 gate 的 `!bash` / `!!bash` 取消、`-32800` 输入框恢复，已在本轮通过真实 WebSocket 与受控通知延迟下的真实 WebUI 补验，详见末节。
- 扩展失败与 Skill 诊断的 toast 和 prompt 诊断共用同一路径，只有映射测试覆盖。
- MCP server 连接失败的 notice 没有自动化测试，因为 ACP 测试缺少会失败的 MCP fixture。
- 旧版（升级前）bash 工具结果没有 details，回放时只显示文本，不显示 terminal。
- 有两处 `shortcut:` 延后：不发送 `terminal_output_chunk`；出站队列不合并快照。另有一处：不发送 `elicitation/complete`。

## Review 修复与独立复验（2026-10-29）

### 修复与回归

- `crates/kernel/src/runtime/run.rs`：普通用户消息在异步 message-start/message-end hook 返回后、实际持久化前再次检查取消；Handled 输入在实时 UserMessage 插入前也检查取消。内部输入与已插入消息沿用原有语义，只在拒绝边界记录 session/run 标识，不记录输入正文。
- `crates/kernel/tests/protocol_alignment/insertion.rs`：新增 message-start、message-end、Handled input 三个确定性回归。修复前均错误返回成功 messageId；修复后均返回 `PromptCancelled`，不产生完整用户消息或用户消息持久化记录，普通 Run 正确结算为 cancelled。协议对齐测试共 37 个通过。
- `web/src/features/mcp/elicitationForm.ts`：minLength/maxLength 按 Unicode 码点计数，替代 UTF-16 code unit 数量。
- `crates/app/tests/web/elicitation_form.test.mjs`：使用 Node 内置测试，覆盖 21/40/41 个 emoji 的上限、单/双 emoji 的下限，以及组合字符和 ASCII 边界。修复前两项失败，修复后三项全部通过；可在 `web/` 执行 `npm run test:elicitation`。

### 真实端到端结果

使用当前工作区重新构建的生产 `app`、已配置的 DeepSeek 官方端点与模型、项目 reference MCP stdio server，以及独立 headless Chrome。会话、文件和证据均使用新的临时目录。测试配置仅隔离数据、启用 reference MCP、增加同端点的 provider 别名，并将 `keep_recent_tokens` 调整为 50 以便短会话测试压缩。

| 测试组 | 结果 | 覆盖 |
| --- | --- | --- |
| 真实 WebUI | **13/13 通过**，无浏览器 JavaScript 错误 | 模型回答与 messageId/usage、Unicode/ANSI terminal、非零退出、SIGTERM/SIGKILL、运行中刷新与取消回放、Slash Command、模型/thinking 切换持久化、MCP 表单/URL/重连/双标签页、压缩回放 |
| 真实 WebSocket | **7/7 通过** | 版本/活跃会话错误码、等待 gate 的 `!`/`!!` 不执行、close 在 idle(cancelled) 后响应、断线续跑、多连接配置收敛、压缩取消 |
| 受控通知延迟下的真实 WebUI | **1/1 通过** | 短暂缓冲真实状态通知使 prompt 等待 gate；真实 `-32800` 返回后，原输入恢复到新草稿前，shell 未执行；不伪造响应、不替换后端 |

Unicode 场景不仅验证弹窗关闭，也检查了真实 MCP 工具返回的 structured result：`action` 为 `accept`，`content.note` 完整保留 21 个 emoji。

本轮证据目录：`/private/tmp/clawcode-acp-fix-v01_60tf/`。其中 `kernel-red.log`、`unicode-red.log` 记录修复前失败；`kernel-green.log`、`cargo-test.log`、`clippy.log`、`web-build.log` 记录修复后检查；`evidence/` 保存逐场景 JSON、真实 WebSocket 帧和截图。未覆盖其他操作系统、其他 provider 或长时间负载测试。

## Review 修复（2026-10-30）

第二轮 review 提出 6 项问题，核实后均存在。本轮修复前 5 项；第 6 项（流式解码状态未按会话隔离）暂缓，因为后端目前只发快照，不会触发。

### 修复与回归

1. **[P1] 关闭任务随连接取消**
   - 问题：`session/close` 原先由 `connection.spawn` 执行 Kernel 关闭。连接中断会丢弃尚未完成的关闭，留下已请求关闭、却未清理的 Session。
   - 修复：Kernel 关闭改由 `tokio::spawn` 独立执行，连接任务只等待结果并应答。关闭成功后，在本连接已应答或已离开时再移除投影。
   - 同一问题也存在于 `session/set_config_option` 的 Kernel 修改，已按同样方式改为独立执行。
   - 回归：`close_completes_after_requesting_connection_drops`。修复前 5 s 内 Session 仍未关闭；修复后通过。宽限期等其余 close 用例全部通过。
2. **[P2] ACP 重复实现路径解析**
   - 问题：ACP 用 `cwd.join(path)` 计算位置，与工具实际使用的 `ResolvedPath` 规则不一致。
   - 修复：新增 `AgentTool::locations` 与 `ToolRegistry::locations`，由 read、write、edit 按执行时相同的规则（`@`、`~`、`file://`、Unicode 空格；read 还包括截图文件名回退）返回位置。Kernel 写入 `ToolExecutionStart.locations`，ACP 只做投影。
   - 回归：工具层 `file_tools_report_paths_resolved_like_execution`；Kernel 层 `tool_start_reports_resolved_location`，验证 `read("@page.txt")` 报告 `<cwd>/page.txt`。
3. **[P2] 事件日志保留过期 terminal 快照**
   - 问题：事件日志只按 2,000 条限制，同一 terminal 的整份输出快照被重复保留。
   - 修复：携带完整输出的 `terminal_update` 事件标记 `snapshotOf`，reducer 与 stager 共用 `retainEvents`，每个 terminal 只保留最新快照，再按条数截断。
   - 验证：用真实 decoder 与 stager 输入 2,000 个 50 KiB 快照，事件日志保留 1 条，约 0.07 MiB（按 review 测量，修复前约 130 MiB）；terminal 当前输出仍为 50 KiB。前端按仓库规则未新增单元测试，该检查脚本保留在会话临时目录。
4. **[P2] 压缩摘要重复携带**
   - 问题：`_meta.clawcode.compaction.result` 含完整摘要，一份摘要在两条通知里出现 6 次。
   - 修复：metadata 只保留 entryId、turnId、tokensBefore、usage 与时间，摘要只出现在标准 `summary` 字段（另有通用 passthrough 事件本身）。
   - 回归：映射测试断言 metadata 不含 summary，且 `compaction_update` 序列化结果中摘要只出现一次。

5. **[P3] 回放过滤重复深拷贝**
   - 问题：`EventGroup::replayed()` 先克隆过滤后的 updates，又用 `..self.clone()` 复制整个事件组，再丢弃被覆盖的字段。
   - 修复：逐个写出字段，只克隆需要保留的部分，完整的 updates 不再多复制一次。
   - 回归：沿用现有回放用例，`cargo test -p acp` 154 个通过。

### 复验

| 检查 | 结果 |
| --- | --- |
| `rtk cargo fmt --all -- --check` | 通过 |
| `rtk cargo clippy --workspace --tests --examples -- -Dwarnings` | 通过，无问题 |
| `rtk cargo test --workspace --no-fail-fast` | **928 passed，9 ignored，107 suites**，无失败 |
| `web/` 下 `rtk npm run check`、`rtk npm run build` | 通过 |
| `web/` 下 `npm run test:elicitation` | 3/3 通过 |
| 真实 WebUI（生产 app + DeepSeek + headless Chrome） | 阶段 B 9/9、阶段 D 11/11、阶段 E 14/14 重新通过 |

