# 内置 Plan 模式设计规格

日期：2026-11-05。状态：六段设计已在对话中逐段确认，待审阅本文。参考：pi 的示例扩展 `packages/coding-agent/examples/extensions/plan-mode/`（只读工具集、bash 白名单、`Plan:` 提取）；Claude Code 的 ACP 适配器 `agentclientprotocol/claude-agent-acp`（`ExitPlanMode`、`TodoWrite` 与计划更新的映射）。本设计改为 Kernel 原生实现，并按 Claude Code 的体验交付。前置：[ACP 登录设计](./2026-11-05-acp-provider-login-design.md) 已为 WebUI 引入请求级弹窗；[子代理设计](./2026-10-28-subagent-design.md) 定义了子会话的创建路径。

## 1. 目标与已确认决策

目标：在会话中切换到 plan 模式后，Agent 只能调研、不能修改；计划写好后交给用户审批，批准后自动回到普通模式并在同一次运行中开始执行。

成功标准：WebUI 头部切到 Plan，让模型规划一个改动；模型调研期间文件不变；审批弹窗显示完整计划；点“批准并执行”后模式变回 Default，模型完成修改；点“继续规划”则模式不变、运行结束，用户可以写反馈。批准后输入框上方的计划面板显示已批准的计划；模型用 `todo_write` 记录待办时，面板显示待办与进度。

| 决策 | 结论 |
|---|---|
| 体验 | Claude Code 式：模式下拉框 + `exit_plan_mode` 审批 + 批准后自动执行 |
| 实现位置 | Kernel 原生会话模式，ACP 以 `mode` 类别配置项暴露；不做成扩展 |
| 持久化 | `custom` 条目（`extensionId:"clawcode"`、`customType:"session_mode"`），跟随分支 |
| 限制方式 | 工具定义与系统提示词在两种模式下完全相同，plan 模式在执行时拦截 |
| 模式说明 | 在切换位置向历史追加一条说明消息，不修改系统提示词 |
| bash | 只读白名单；定位为防止误操作，不是安全边界 |
| 审批协议 | 标准 `session/request_permission`，两个选项：`approve`、`keep_planning` |
| 子代理 | plan 模式下创建的子会话以 plan 模式启动，且不能发起审批 |
| 计划发布 | 仿照 Claude Code：批准后发出 `plan_update`（`markdown`），为此开启 ACP 特性 `unstable_plan_operations`；不使用 `plan_removed` |
| 待办清单 | 新增 Kernel 工具 `todo_write`，仿照 `TodoWrite`，映射为 `plan_update`（`items`） |
| 保留名 | `exit_plan_mode`、`todo_write` 只能由 Kernel 提供，扩展注册同名工具会被拒绝 |
| 发布数据来源 | 只读持久化的工具结果详情（`PlanApproval`、`TodoList`），实时与重放一致 |
| planId | 固定常量：`plan-mode`（已批准的计划）、`todos`（待办清单） |
| 配置 | 不新增 `claw.toml` 配置项；新会话默认 Default |

## 2. 现状

- 会话设置（模型、Thinking）由 `SessionSettingsUpdate`（`crates/kernel/src/runtime/session/settings.rs`）写入存储并发出 `SettingsChanged`；打开会话时 `session/assembly.rs` 按分支重放 `model_change`、`thinking_level_change` 条目。
- ACP 的 `crates/acp/src/config.rs` 把设置映射为配置项；WebUI 的 `SessionConfigSelects` 按配置项通用渲染下拉框。ACP v2 提供 `SessionConfigOptionCategory::Mode`。
- 工具可见性由 `SessionToolSnapshot::declares`（`crates/kernel/src/runtime/tools/state.rs`）统一计算；每轮开始取一次工具快照并重新生成系统提示词。系统提示词不含每轮变化的内容，同一会话的请求前缀稳定，prompt cache 可以持续命中。
- 模型发起的工具调用在 `crates/kernel/src/runtime/tools/batch.rs` 依次经过扩展 `tool_call` 钩子、参数校验，被拦截时作为错误工具结果返回，本轮继续。结束运行的唯一途径是扩展在执行前拦截时给出的 `ToolResultDetails::Blocked { terminate }`；工具执行后的结果不能要求结束运行。
- 内置工具只有 `read`、`grep`、`edit`、`write`、`bash`；Kernel 另外注册 `tool_search`、`subagent`、`background_subagent`。Kernel 注册的工具与扩展工具同在 `extensions` 分区（`tools/state.rs`），扩展若先提供同名 `subagent`，Kernel 会跳过自己的实现（`subagent/mod.rs`）。`ToolAnnotations.read_only_hint` 目前只有 MCP 工具会带。
- 上下文压缩后，`history_from_store` 把历史重置为“摘要 + 保留的尾部”，压缩点之前的消息不再进入上下文。
- `command-guard` 扩展（`extensions/src/available/command_guard.rs`）已有识别引号的命令切分与 `shlex` 解析。
- MCP elicitation 已实现“Kernel 发事件 → ACP 转客户端请求 → 答复交回 Kernel”，支持多客户端先答先得、撤回、`requires_action` 状态和恢复后重发（`pending_elicitation_is_reestablished_after_resume`）。ACP 与 WebUI 都还没有 `session/request_permission`。
- 会话文件是 clawcode 自己的 v4 格式（代码注释称 “Pi v4”，但 pi 本身最高是 v3，且读不了该格式）。条目 `type` 按固定枚举解析，新增类型会让旧版 clawcode 无法打开会话；`custom` 类型被重放时跳过。
- `Kernel::history_from_store`（`session/replay.rs`）从分支条目重建模型历史，目前跳过 `custom` 条目；运行中由 transcript 实时追加历史。
- 扩展 `tool_call` 钩子改写参数只修改工具批次内的调用（`tools/batch.rs`），持久化的助手消息保留模型原始参数；工具结果的 `details` 随工具结果一起持久化（`protocol/src/message.rs`）。
- WebUI 的恢复有两种模式：`resume` 完整重放会话；`watch` 在运行中按游标继续接收增量更新，不重放之前的内容。`navigate`、`branch` 先派发 `transcript/cleared` 再完整重放（`web/src/workspace/controller.ts`）。
- ACP v2 的计划更新是 `session/update` 中的 `plan_update`，同一 `planId` 的新内容整体替换旧内容。内容类型 `items`（`PlanEntry` 列表）是稳定规范；`markdown`、`file` 与独立的 `plan_removed` 更新需要 `unstable_plan_operations` 特性。v2 的 `ClientCapabilities` 没有计划能力字段，所有 v2 客户端都接受计划更新。clawcode 目前没有开启该特性，也不发送或渲染任何计划更新。
- Claude Code 的 ACP 适配器不解析计划文本：`ExitPlanMode` 的 Markdown 在批准后以 `markdown` 计划发布（`planId = "plan-mode"`，`exit-plan.ts`）；结构化清单来自 `TodoWrite` 的参数，逐项映射为 `PlanEntry`，`priority` 固定为 `medium`，状态为 `in_progress` 时显示 `activeForm`，且不显示工具卡片（`tools.ts`、`acp-agent.ts`）。

## 3. 模式状态与持久化

### 3.1 protocol

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    #[default]
    Default,
    Plan,
}

pub struct SessionSettings {
    // existing fields ...
    pub mode: SessionMode,
}
```

### 3.2 存储格式

模式切换写入一条 `custom` 条目，复用 `append_extension_entry` 的现有形状：

```json
{"kind":"entry","lane":"main","id":"entry-…","seq":42,"parentId":"entry-…","timestamp":"…","type":"custom","extensionId":"clawcode","customType":"session_mode","data":{"mode":"plan","turnId":"turn-…"}}
```

- `data.turnId` 是写入时所在 Turn 的 ID，用于让重建出的说明消息（§4.4）与运行中的身份一致。
- 重放时只认 `extensionId == "clawcode"` 且 `customType == "session_mode"` 的条目。`data.mode` 缺失或是不认识的值、`data.turnId` 缺失或非法时，记 `warn`、跳过该条并沿用前一个模式。这与模型、Thinking 条目“格式错误即报错”的做法不同，是有意为之：以后新增模式值（例如 `review`）时，旧版 clawcode 必须仍能打开会话，这正是选用 `custom` 条目的原因。
- 扩展只能用自己的 ID 写条目，不能冒充 `clawcode`。
- 旧版 clawcode 读到该条目会跳过，会话照常打开，模式为 Default。
- 恢复时与 Thinking 一起在 `session/assembly.rs` 按当前分支重放；切换分支后模式随分支变化。

### 3.3 Kernel

- `SettingChange` 增加 `Mode(SessionMode)`，编码为上述 `custom` 条目；`SessionSettingsUpdate::mode` 与 `thinking_level` 同构：加锁、值相同则直接返回、先持久化再改内存、发出 `SettingsChanged`，记 `tracing::info!`。
- 新增 `Kernel::set_session_mode(session_id, mode)`。
- 生效时机：每轮开始时读取。正在执行的这批工具调用按切换前的模式完成。
- 不新增扩展事件，扩展也不能直接设置模式。

### 3.4 ACP 配置项

- 第三个配置项：`configId = "mode"`，名称 `Mode`，类别 `Mode`，选项 `default`（Default）与 `plan`（Plan），排在 `model`、`thinking_level` 之后。
- `ConfigChange` 增加 `Mode`；非法值返回 -32602。
- 模式变化经现有 `SettingsChanged` → `config_option_update` 推送给客户端。

### 3.5 子代理

- `create_owned_session` 接收父会话当前模式；父会话为 plan 时，子会话创建后立即写入 `session_mode = plan` 条目。
- 子会话的说明消息使用子代理版本（§4.4），不提 `exit_plan_mode`，只要求调研并把结果交回父会话。
- 父会话之后退出 plan 模式不影响已经存在的子会话。

## 4. Plan 模式下的限制

### 4.1 前缀稳定原则

两种模式下，请求中的系统提示词与工具定义逐字节相同；模式差异只体现在执行时拦截和历史里的说明消息。这样切换模式只多出一条说明消息的 token，不会让此前的 prompt cache 失效。

### 4.2 工具拦截

`exit_plan_mode` 一直注册；所有工具定义保持可见。plan 模式下，`tools/batch.rs` 在扩展 `tool_call` 钩子之后、执行之前检查，按顺序判定：

1. **Kernel 自己实现的工具**：`exit_plan_mode`（§5）、`todo_write`（§6.4）、`tool_search`、`subagent`、`background_subagent` 放行，子代理的子会话以 plan 模式启动（§3.5）。“是 Kernel 的实现”在注册时记录，不能只看名称：扩展提供的同名 `subagent`、`background_subagent` 不享受这条放行，按第 3 条判定。`exit_plan_mode`、`todo_write` 是保留名：Kernel 总是注册自己的实现，因此这两个名字在任何会话中都指向 Kernel 的实现。扩展在运行中注册同名工具时，注册调用返回错误（`ToolError::ReservedName`，经扩展宿主错误交给扩展），会话不受影响；会话组装时的静态注册遇到同名工具则跳过并记 `tracing::warn!`，以免会话无法创建。
2. **`bash`**：按 §4.3 白名单检查每条命令；参数里没有字符串 `command` 时拒绝（覆盖了 `bash` 的扩展工具同样适用）。
3. **其他工具**：仅当元数据 `annotations.read_only_hint == Some(true)` 时放行。内置 `read`、`grep` 补充这个标注；MCP 工具使用服务器声明的标注；扩展工具（包括覆盖了 `read`、`grep` 的）必须自己标注。`edit`、`write` 等没有标注，一律拒绝。

拒绝时生成错误工具结果，例如：`Plan mode is active: edit is not allowed. Finish the plan and call exit_plan_mode.`，并沿用现有的 blocked 处置（`ToolResultDetails::Blocked`，`terminate = false`），让客户端能区分“被策略拦截”和“工具执行失败”。放行规则由 Kernel 中的 `PlanModeGate` 判定：它在每轮开始时与工具快照一起捕获当时的模式和“Kernel 实现”名单，对每个调用给出放行或拒绝原因。不放在 `SessionMode` 上，因为 `SessionMode` 位于 protocol crate，而判定需要 tools crate 的 bash 白名单和每轮的工具快照。

### 4.3 bash 只读白名单

把 `command-guard` 中的命令切分与解析（`ShellCommandLine`、`CommandInvocation`）移到 `tools` crate 的新模块 `command_line.rs`，`command-guard` 与 plan 模式共用；`extensions` crate 因此新增对 `tools` 的依赖（经 workspace 声明）。

全部满足才放行：

1. 按引号外的 `;`、`&&`、`||`、`|`、`&`、换行切成若干段；任一段无法用 `shlex` 解析（如引号未闭合）即拒绝。
2. 每段可执行文件（取 basename）必须在白名单中，并遵守该命令的参数限制。参数限制同时检查独立写法与合并写法（如 `-o FILE`、`-oFILE`、`--output=FILE`）：

   | 类别 | 命令 | 禁止的参数或用法 |
   |---|---|---|
   | 查看 | `cat head tail wc nl stat` | 无 |
   | 查看 | `file` | `-C`、`--compile` |
   | 搜索 | `grep` | 无 |
   | 搜索 | `rg` | `--pre`、`--pre-glob` |
   | 搜索 | `fd` | `-x`、`-X`、`--exec`、`--exec-batch` |
   | 搜索 | `find` | `-delete`、`-exec`、`-execdir`、`-ok`、`-okdir`、`-fprint`、`-fprint0`、`-fprintf`、`-fls` |
   | 目录 | `cd ls pwd du df realpath` | 无 |
   | 目录 | `tree` | `-o` |
   | 文本 | `cut tr diff jq echo printf` | 无 |
   | 文本 | `sort` | `-o`、`--output` |
   | 文本 | `uniq` | 多于一个文件参数（第二个是输出文件） |
   | 系统 | `which uname whoami date ps` | 无 |
   | git | 见下 | 见下 |

   git 规则：第一个参数必须是子命令，不接受任何全局选项（如 `-c`、`-C`、`--config-env`、`--exec-path`、`--git-dir`）；子命令为 `status log diff show blame grep ls-files rev-parse describe shortlog branch remote` 之一；`branch` 只允许不带参数或列出类参数（`-a`、`-r`、`-v`、`-vv`、`--list`、`--show-current`）；`remote` 只允许 `-v`；任何以 `--output` 开头的参数、`--ext-diff`、`-O`、`--open-files-in-pager` 都拒绝。
3. 整条命令中不得出现 `$(`、反引号、`<(`、`>(`（无论是否在引号内）。这会误拒 `grep '$(' f` 这类写法，模型可以改用 `grep` 工具。
4. 引号外不得出现输出重定向 `>`、`>>`、`&>`，例外仅 `2>&1`、`1>&2` 与目标为 `/dev/null` 的重定向。

刻意排除：能间接执行命令的 `env xargs sh bash zsh timeout nice nohup sudo`；能写文件的 `sed awk tee`；会产生构建产物的 `cargo npm pnpm yarn make`；会把密钥带进记录的 `printenv`。用户在 WebUI 中执行的 `!bash` 不受 plan 模式限制。

拒绝时的工具结果说明原因类别（例如 `output redirection is not allowed in plan mode`），不复述整条命令；涉及参数时只写参数名（如 `--output`），不带值。

### 4.4 模式说明消息

- `session_mode` 条目在模型历史中渲染为一条不显示给用户的说明消息，位置就是条目在分支中的位置。消息复用现有的 `MessageContent::Extension`（`include_in_context = true`，不显示），与扩展自定义消息进入上下文的方式相同，不新增消息类型。
- 说明文本是代码常量，共三种：
  - 进入 plan（普通会话）：当前为只读；`edit`、`write` 等会被拒绝，bash 仅限只读命令；先调研，有疑问就提问；计划完成后调用 `exit_plan_mode`，`plan` 参数放完整 Markdown 计划，然后等待审批；不要声称已开始执行。
  - 进入 plan（子代理会话）：当前为只读，限制同上；只调研，把结果作为最终回复交回父会话；不要调用 `exit_plan_mode`。
  - 退出 plan：`Plan mode has ended; you may edit files now.`
- 顺序规则：条目若落在一组工具调用与其工具结果之间，说明消息放在这组工具结果之后。
- 压缩规则：压缩后历史从“摘要 + 保留的尾部”开始；若压缩点上生效的模式是 plan，在摘要之后紧跟一条对应版本的“进入 plan”说明，避免进入 plan 时的那条说明被压缩掉后，模型不再知道自己处于 plan 模式。压缩本身已使缓存失效，这条消息不增加缓存代价。
- 一致性：`Kernel::history_from_store` 重建与运行中的实时追加（含压缩）必须产出相同的消息与位置。
- 只有 Kernel 写入的 `session_mode` 条目进入模型上下文，扩展写的 `custom` 条目仍不进入。

## 5. `exit_plan_mode` 与审批

### 5.1 工具

- Kernel 内置，保留名（§4.2），ACP 工具类型 `switch_mode`（ACP 的工具类型按名称映射，`crates/acp/src/tool_call.rs`，因此依赖保留名），参数 `{ "plan": string }`，必填且去掉首尾空白后非空。
- 模型可见的描述说明：仅在 plan 模式下、计划完成后调用；`plan` 是完整计划。
- 执行顺序：
  1. 不在 plan 模式：错误 `Plan mode is not active.`
  2. 会话由工具创建（子代理）：错误，提示把调研结果交回父会话。
  3. 本会话已有待审批：错误。
  4. 否则把计划作为工具内容（Markdown 文本）发布，然后发起审批并等待。

### 5.2 Kernel 与 ACP

- Kernel 每个会话最多保存一个待审批项，发出 `PlanApprovalRequested { request_id, tool_call_id, plan }`；新增 `Kernel::respond_plan_approval(session_id, request_id, decision)`；结束时发出 `PlanApprovalResolved { request_id, decision }`。结构与 MCP elicitation 的待处理表一致。
- ACP 投影把请求转成 `session/request_permission`，复用 elicitation 的投递方式：通知之后再发请求；多客户端先答先得，其余用 `$/cancel_request` 撤回；恢复会话后重新发出；等待期间会话状态为 `requires_action`。
- 请求内容：
  - `title`：`Ready to execute the plan?`
  - `subject`：`ToolCall`，`toolCallId` 为本次调用，`kind = switch_mode`，内容为计划全文
  - `options`：`approve`（`allow_once`，`Approve and execute`）、`keep_planning`（`reject_once`，`Keep planning`）
- 答复中的未知 `optionId` 按 `cancelled` 处理。

### 5.3 结果

| 结果 | 模式 | 工具结果 | 之后 |
|---|---|---|---|
| `approve` | 经 `SessionSettingsUpdate::mode` 切回 Default | `The user approved the plan. Plan mode has ended; you can now edit files. Carry out the plan.` | 发布计划（§6.3）；运行继续，下一轮开始执行 |
| `keep_planning` | 不变（沿用当前值） | `The user chose to keep planning. Stop and wait for their feedback.` | 本批工具结束后终止运行（见下） |
| 客户端回复 `cancelled` | 不变（沿用当前值） | 同 `keep_planning` | 同 `keep_planning` |
| 运行被取消（`session/cancel`） | 不变（沿用当前值） | 按现有取消语义 | 撤回客户端请求 |

终止运行：现有的 terminate 只来自执行前的 `Blocked` 处置，不能复用。新增 `ToolResultDetails::PlanApproval { approved: bool, plan: Option<String> }` 作为 `exit_plan_mode` 的结果详情，批准时 `plan` 为本次实际审批的计划（去掉首尾空白），其余情况为 `None`；`tools/batch.rs` 汇总结果时，遇到 `approved == false` 就把本批的 `terminate` 置为真，之后沿用现有的“本批结束后停止运行”流程。不借用 `Blocked`，因为它表示执行前被策略拦截，客户端会把卡片显示为已拦截。

“不变”指不主动修改模式：通常仍是 Plan；若用户在等待期间已经手动切到 Default，则保持 Default。

等待没有超时。

进程重启：待审批只保存在内存中。进程退出后，这次运行按现有的中断语义结束，会话保持 plan 模式；客户端的审批请求随连接断开而失效。用户重新发消息后，模型需要再次提交计划。

### 5.4 日志与扩展

- 发起审批、批准、继续规划、取消各记一条 `tracing::info!`，只写会话 ID 和工具调用 ID，不写计划内容。
- `exit_plan_mode` 同样经过扩展 `tool_call` 钩子；不新增扩展事件。

## 6. 计划发布与 `todo_write`

### 6.1 开启特性

`crates/acp/Cargo.toml` 中 `agent-client-protocol` 的 features 增加 `unstable_plan_operations`（子 crate 本地添加特性，符合规则 12），只为发送 `markdown` 类型的计划；`file` 与 `plan_removed` 不使用。所有 v2 客户端都接受计划更新，不按客户端能力区分。

### 6.2 固定的 planId

| planId | 内容 | 类型 | 来源 |
|---|---|---|---|
| `plan-mode` | 最近一次批准的计划（Markdown 原文） | `markdown` | `exit_plan_mode` 被批准 |
| `todos` | 待办清单 | `items` | `todo_write` |

- 两个 ID 都是代码常量，整个会话期间（每次批准、恢复、重放）不变，新内容整体替换旧内容。`plan-mode` 沿用 Claude Code 的 `PLAN_MODE_PLAN_ID`；不像 Claude Code 为其 AIR 客户端追加审批 ID 后缀。
- 每个会话（含子代理会话）各自维护这两个计划，子会话的计划只出现在子会话的更新流中。

### 6.3 批准后发布计划

- ACP 在映射工具结果消息（`MessageEnd` 中的 `ToolResult`）时推导计划更新：实时运行与重放都会经过这条路径，而工具结束事件不会被重放。`exit_plan_mode` 的结果详情为 `PlanApproval { approved: true, plan: Some(..) }` 时，在该消息的工具结果更新之后紧接着发出：

  ```json
  {"sessionUpdate":"plan_update","plan":{"type":"markdown","planId":"plan-mode","content":"<计划原文>"}}
  ```

- 内容只取自结果详情中的 `plan`，不读工具调用参数：扩展可能改写过参数，而持久化的助手消息保留的是原始参数。这与 Claude Code 优先读取工具输出中的 `plan` 一致。`keep_planning`、`cancelled`、运行取消都不发布。
- 不新增存储：结果详情随工具结果持久化，实时投影与重放读取同一份数据。

### 6.4 `todo_write` 工具

- Kernel 内置，保留名（§4.2），按 Claude Code 的 `TodoWrite` 实现，名称沿用 clawcode 工具的小写风格。所有会话都注册，包括子代理会话。
- 参数：每次传入完整清单，替换上一份。

  ```json
  {"todos":[{"content":"Add notes line","status":"in_progress","activeForm":"Adding notes line"}]}
  ```

  `status` 为 `pending`、`in_progress`、`completed` 之一；`content`、`activeForm` 去掉首尾空白后非空；空数组表示清空。“同一时间只有一项 `in_progress`”只写在工具描述中，不强制校验，与 Claude Code 一致。
- 工具描述（含义沿用 Claude Code，文字自行撰写并尽量简短，因为它随每次请求发送）：三步以上的复杂任务、用户一次给出多项任务、收到新指令时使用；开始某项前标为 `in_progress`，完成后立即标为 `completed`；简单任务不要使用。
- 工具结果：固定确认语，提醒模型继续按清单推进；结果详情为新增的 `ToolResultDetails::TodoList { todos }`，内容是校验后的完整清单。
- plan 模式：不改动文件，作为 Kernel 实现放行（§4.2），与 Claude Code 在 plan 模式下可用 `TodoWrite` 一致。
- ACP 映射：映射 `todo_write` 成功的工具结果消息（详情为 `TodoList`）时发出 `plan_update`，`type = items`，`planId = todos`；每项映射为 `{content, status, priority: "medium"}`，`status == in_progress` 时 `content` 取 `activeForm`。被拦截或校验失败的调用不发布。因为是保留名，ACP 按名称识别该工具：工具开始、结束事件和助手消息里的调用块都不发 `tool_call_update`，结果消息只发计划更新。调用与结果仍保留在模型历史中。
- 日志：执行时记 `tracing::debug!`，只写会话 ID 与条目数量，不写条目文本。

### 6.5 恢复、重放与分支

- 完整重放（`resume`）按对话记录顺序重新发出这两类计划更新，后者覆盖前者；数据只来自结果详情（§6.3、§6.4）。
- `watch` 增量恢复只继续发送游标之后的更新，不重发、也不清除计划。
- 不发送 `plan_removed`。切换分支时由客户端清空状态后完整重放：WebUI 已在 `navigate`、`branch` 中先派发 `transcript/cleared` 再重放（§7）；第三方客户端按 ACP 语义，在加载会话时从空状态重建。

### 6.6 与 Claude Code 的差异

- 不提供“批准并清空上下文”（在新上下文中执行计划）。
- 不提供“自动接受编辑 / 手动确认编辑”两种批准选项；clawcode 尚无编辑审批，只保留 `approve`。
- 不支持在审批弹窗中修改计划。
- 不定期在对话中提醒模型更新待办。
- 待办在 `todo_write` 执行成功后才更新；Claude Code 在模型发出调用时就更新，因此被拦截的调用在 Claude Code 中也会改变清单。

## 7. WebUI

- **模式下拉框**：由通用配置项渲染自动出现，无需改动。plan 模式时在输入框旁显示 `Plan · 只读` 标记。
- **权限请求**：controller 注册 `session/request_permission` 处理函数，校验后放入现有弹窗队列，与 elicitation 按到达顺序逐个显示；Agent 撤回时经 `signal` 关闭并回复 `cancelled`。
- **`PermissionDialog`**：
  - 计划审批（`subject` 为 `switch_mode` 工具调用）：标题“执行这个计划？”；正文用 `MarkdownRenderer` 渲染计划，可滚动；按钮“继续规划”（`keep_planning`，默认焦点）与“批准并执行”（`approve`）。
  - 其他权限请求：显示 Agent 提供的标题、说明与选项，拒绝类按钮在前。
  - Esc 与关闭回复 `cancelled`；焦点沿用 `useModalFocus`。
- **工具卡片**：`switch_mode` 使用 `ClipboardList` 图标，标题“计划”；计划正文沿用现有 Markdown 渲染；等待审批时默认展开。
- 等待审批期间不额外显示状态，弹窗即提示。
- **计划状态**：解码 `plan_update`（`items`、`markdown`；其他类型忽略），每个会话按 `planId` 保存。在 `transcript/cleared` 与完整重放（`resume`）开始时清空；`watch` 增量恢复时保留。
- **计划面板**：输入框上方的可折叠面板，两类计划都不存在时不显示。
  - 待办（`todos`）：每项带状态图标（`pending` 空心圆、`in_progress` 转圈、`completed` 打勾加删除线；其他状态按 `pending` 显示），标题显示进度，例如 3/5。`in_progress` 项的文字已由 Agent 换成 `activeForm`。
  - 已批准的计划（`plan-mode`）：默认折叠，展开后用 `MarkdownRenderer` 显示全文。
- `todo_write` 的调用不显示为工具卡片。

## 8. 文档

`README.md` 与 `README_zh.md` 增加 plan 模式小节：如何切换、允许的工具与 bash 白名单（防止误操作，不是沙箱）、审批流程与“继续规划”后如何反馈、计划面板与 `todo_write`。

## 9. 测试与验收

Rust 按 TDD 进行：先写失败测试。WebUI 按项目规则不写单元测试，端到端测试使用生产 `app` 后端与配置的 provider。

| 编号 | 场景 | 必须证明的结果 |
|---|---|---|
| P01 | 模式持久化 | 切换写入 `session_mode` 条目，相同值不写；重新打开后恢复；切换分支后模式随分支变化 |
| P02 | 防伪造与兼容 | `extensionId` 不是 `clawcode` 的 `session_mode` 条目被忽略；未知 `customType` 不影响加载；`data.mode` 缺失或为未知值（如 `review`）时跳过该条、沿用前一个模式并继续加载 |
| P03 | ACP 配置项 | `mode` 配置项类别为 `Mode`；`set_config_option` 切换并推送 `config_option_update`；非法值 -32602 |
| P04 | 工具拦截 | plan 模式下 `edit`、`write` 返回 blocked 错误且文件不变；`read`、`grep` 照常；MCP 工具仅 `readOnlyHint: true` 可执行；带只读标注的扩展工具可执行；扩展提供的同名 `subagent` 或未标注的 `read` 被拒绝 |
| P05 | bash 白名单 | 表驱动：拒绝 `ls; rm x`、`cat a > b`、`echo "$(rm x)"`、`find . -delete`、`env rm x`、`git branch -D x`、`git diff --output=x`、`git -c core.pager=x log`、`git diff --ext-diff`、`git grep -O x`、`rg --pre=sh x`、`fd -x rm`、`sort -o f a`、`sort -of a`、`tree -o f`、`uniq a b`、`file -C`、未闭合引号；放行 `cd src && ls`、`git log \| head`、`grep ">" f`、`ls 2>&1`、`cmd >/dev/null`、`uniq a`。`command-guard` 现有测试在解析器移动后通过 |
| P06 | 前缀稳定 | 切换前后模型请求的工具定义与系统提示词相同；历史只在切换位置多一条说明，之前的消息不变 |
| P07 | 说明消息顺序 | 工具执行期间切换模式时，说明消息位于这组工具结果之后；重新打开会话后重建出的历史与运行中相同 |
| P08 | `exit_plan_mode` 拒绝情形 | 非 plan 模式、子代理会话、已有待审批时分别报错 |
| P09 | 批准 | 模式切回 Default 并发出 `SettingsChanged`；工具结果为批准文本；运行继续且下一轮可以编辑文件 |
| P10 | 继续规划与取消 | 模式不被修改（等待期间手动切到 Default 的保持 Default）；结果详情为 `PlanApproval { approved: false, plan: None }`，运行在本批工具后结束；`cancelled` 与未知 `optionId` 同效；`session/cancel` 撤回请求 |
| P11 | 子代理 | plan 模式下创建的子会话为 plan 模式，其中 `edit` 被拒绝；子会话的说明消息是子代理版本，不提 `exit_plan_mode` |
| P12 | ACP 权限请求 | `subject` 为 `switch_mode` 工具调用，选项为 `approve`、`keep_planning`；多客户端先答先得、其余被撤回；恢复后重发；等待期间 `requires_action` |
| P13 | 日志 | 全流程日志不含计划内容、待办文本与完整 bash 命令 |
| P14 | WebUI 静态检查 | `npm run check`、`npm run build` 通过 |
| P15 | 端到端：继续规划 | 真实后端与模型、临时 git 仓库：切到 Plan，要求规划“在 `notes.txt` 中加一行”；弹窗出现并显示计划；点“继续规划”后模式仍为 Plan、文件不变、输入框可用 |
| P16 | 端到端：批准 | 同上，点“批准并执行”：下拉框变为 Default，文件被修改 |
| P17 | 工程检查 | `cargo fmt`、clippy（`-D warnings`）、workspace 测试、`pre-commit run --all-files` 通过 |
| P18 | 压缩后的说明 | plan 模式下压缩，且进入 plan 的说明位于压缩点之前：压缩后的历史在摘要之后有一条“进入 plan”说明；Default 模式下压缩不插入说明；重新打开会话后重建的历史与运行中相同 |
| P19 | 批准后发布计划 | 批准后，在工具结果更新之后发出 `plan-mode` 的 `markdown` 更新，内容等于结果详情中的 `plan`；扩展钩子改写过 `plan` 参数时，实时发布与重放都使用改写后的计划；`keep_planning`、`cancelled`、运行取消时不发 |
| P20 | `todo_write` 映射 | 成功执行后发出 `todos` 的 `items` 更新，来源是 `TodoList` 详情；`in_progress` 项使用 `activeForm`；`priority` 为 `medium`；空数组产生空清单；没有 `tool_call` 与 `tool_call_update`；被扩展钩子拦截的调用不发布 |
| P21 | `todo_write` 参数校验与保留名 | 非法 `status`、空 `content` 或空 `activeForm` 返回错误，不发计划更新；扩展注册名为 `todo_write` 或 `exit_plan_mode` 的工具被拒绝并记 `warn` |
| P22 | plan 模式与子代理 | plan 模式下 `todo_write` 不被拦截；子代理会话的 `todo_write` 只出现在子会话的更新流中 |
| P23 | 重放与恢复 | 完整重放按顺序重发两类计划，数据来自结果详情；`watch` 增量恢复不重发计划；Agent 从不发送 `plan_removed` |
| P24 | 端到端：计划面板 | P16 批准后计划面板出现“已批准的计划”；切换到批准之前的分支后，面板不再显示该计划；若执行中模型调用了 `todo_write`，面板显示待办与进度（依赖模型行为，只断言可观察结果） |

P15、P16 依赖真实模型：提示词明确要求先调研再调用 `exit_plan_mode`，超时放宽，只断言弹窗、模式与文件等可观察结果。“模型在 plan 模式下尝试编辑”由 P04 覆盖，`todo_write` 的映射与校验由 P20、P21 覆盖。

## 10. 风险与已知限制

- bash 白名单是规则过滤，不是沙箱；模型刻意利用白名单命令的冷门用法仍可能改动文件。
- MCP 的 `readOnlyHint` 由服务器自行声明，不可信。
- plan 模式下工具定义保持可见，模型偶尔会尝试 `edit` 并收到错误，浪费一次工具调用。
- 审批弹窗不能填写反馈；用户在“继续规划”后通过下一条消息反馈。等待审批时直接在输入框发消息，消息会排在被阻塞的 `exit_plan_mode` 之后，弹窗不会因此关闭，需要先点按钮。
- 进程重启会丢失待审批状态，模型需要重新提交计划（§5.3）。
- 每次切换多出一条说明消息的 token；切换模型、`tool_search` 加载工具、`set_active_tools`、上下文压缩仍按现状影响缓存。
- 第三方 ACP 客户端对 `switch_mode` 审批的显示未实测。
- 子会话创建后不随父会话退出 plan 模式。
- `markdown` 计划属于 ACP 的 UNSTABLE 能力，升级 `agent-client-protocol` 时可能变化，需要随之调整。
- `todo_write` 与 `exit_plan_mode` 的工具描述随每次请求发送，长期占用输入 token（可被缓存）；描述需保持简短。
- 第三方客户端若在切换分支后不重建会话状态，可能残留旧分支的计划；clawcode 的分支切换是自有扩展方法，目前只有 WebUI 使用。
- `todo_write` 不强制“只有一项 `in_progress`”，清单的准确性取决于模型。

## 11. 不在范围

基于文本标记的进度跟踪（pi 的 `[DONE:n]`，进度改由 `todo_write` 表达）、计划落盘为文件（含 `file` 类型的计划更新）、`plan_removed` 更新、§6.6 列出的 Claude Code 功能、审批弹窗中的反馈输入、`approval = OnRequest` 的实现（本次只为其铺好 `request_permission` 通道）、默认模式配置项、扩展设置模式的接口、修正代码中 “Pi v4 compatible” 的命名。
