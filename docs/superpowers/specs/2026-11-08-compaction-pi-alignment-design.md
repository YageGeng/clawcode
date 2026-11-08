# 上下文压缩与 pi 当前实现二次对齐设计

## 目标

[上一轮对齐](./2026-08-17-compaction-alignment-design.md)以当时的 pi v4 为基线。pi 之后又修复了切点回退、摘要截断、run 内阈值检查等问题，并补齐了按模型覆盖、扩展钩子和分支摘要上下文。本设计以 pi `a44127d91` 为基线，使本项目的压缩行为与之重新一致。

完成后应满足：

- 带 Tool 调用的长 Run 在每个后续 Turn 之前检查阈值，不再只能依赖 provider 溢出后的一次重试。
- 尾部 Tool 结果单独超出 `keep_recent_tokens` 时仍能压缩更早的历史。
- 被输出上限截断的摘要永远不会成为 checkpoint；摘要输出上限按模型能力和剩余窗口计算，而不是按 `reserve_tokens` 比例计算。
- 摘要输入、split-turn 前缀提示词与 pi 当前文本一致。
- `[compaction.model_overrides."provider/model"]` 可按模型覆盖 `reserve_tokens` 与 `keep_recent_tokens`。
- `session_before_compact` 提供完整准备数据与自定义指令；新增 `session_compact_failed` 观察点。
- 分支摘要按 pi 规则生成，并作为独立消息进入模型上下文、ACP 回放和 WebUI；ACP 与 WebUI 导航时可请求生成摘要。

## 参照基线

| 差异 | pi 提交 |
|---|---|
| run 内后续请求前压缩 | `56700d42e` (#8782) |
| 尾部超大 Tool 结果的切点回退 | `8bdcd4498` |
| 拒绝截断摘要 | `97fa14e39` (#7048) |
| 摘要请求不再使用固定输出上限 | `6fcb48670` (#11036) |
| 摘要输入省略 thinking | `15e7bdc14` |
| split-turn 前缀提示词 | `d192bd6dc` (#9908) |

本设计取代上一轮设计中"摘要最大输出为 `min(reserve_tokens * 0.8, model.max_output_tokens)`"的规则。

## 已确认差异

1. 阈值只在新 prompt 之前（`run/turn.rs:215`）和 Run 无后续工作时（`run/turn.rs:1197`）检查；Tool 循环中途不检查。
2. 带排队后续消息时，`next_action` 在计算 `auto_compaction_reason` 前返回，`OverflowRecovery::CompactOnly` 被丢弃。
3. `CompactionPreparation::from_history` 找不到不早于预算越界点的切点时回退到第一个切点（`compaction/context.rs:226`），导致无可摘要内容。
4. 摘要接受 `StopReason::MaxTokens`；输出上限固定为 `0.8 × reserve` / `0.5 × reserve`。
5. 摘要序列化包含 `[Assistant thinking]`；Tool 调用参数格式与 pi 不同。
6. split-turn 前缀提示词为旧版本，且不接收自定义指令。
7. 摘要请求固定 `ThinkingLevel::Off`；pi 使用会话当前 thinking level。
8. `keep_recent_tokens = 0` 时不保留任何消息；pi 至少保留最后一个切点之后的内容。
9. 可见 Extension 消息是切点但不算 Turn 起点；pi 将其视为 Turn 起点。
10. 无按模型覆盖配置。
11. `SessionBeforeCompactEvent` 无准备数据，`custom_instructions` 从未赋值；无失败观察点。
12. 分支摘要不进入模型上下文（`session/replay.rs:351` 忽略 `EntryKind::BranchSummary`）；生成时无预算（pi 使用独立的 `branchSummary.reserveTokens`）、不序列化、提示词与文件追踪均不同。
13. ACP 与 WebUI 导航固定不生成分支摘要（`session/tree.rs:118`），只有扩展可以请求。

## 设计

### 1. Kernel 内部行为修正

#### 1.1 切点

`CompactionPreparation::from_history`：

- 从最新消息向前累加估算 token，跳过估算为 0 的消息。累计值达到 `keep_recent_tokens` 时，选择位置不早于当前消息的第一个合法切点；不存在时选择最后一个合法切点。
- 删除 `keep_recent_tokens == 0` 的特殊分支，统一走上述循环。
- Turn 起点集合为 `User`、`ExpandedUser`、`BashExecution`、`include_in_context = true` 的 `Extension` 和第 5 节新增的 `BranchSummary`。合法切点集合在此基础上加上 `Assistant`；隐藏 Extension 仍然不是切点。

行为后果（与 pi 一致）：压缩至少保留最后一个合法切点之后的内容，因此只有一个 user 消息的 history 没有可压缩内容。新会话第一个 prompt 就溢出时，overflow 恢复得到 `NoContextToCompact`，不重试并以 provider 错误结束；当前实现会把这条 user 消息压进摘要后带着摘要重试，此行为被移除。只有一轮对话的手动压缩会切到 turn 中间：前缀摘要覆盖 user 消息，保留最后的 Assistant。

#### 1.2 摘要输出上限与终止原因

`Kernel::generate_compaction_summary` 对 Compaction、TurnPrefix、Branch 三类请求统一计算 `max_tokens`，并显式传给 provider（`None` 会被 provider 填为 `request_max_tokens`）：

```text
input  = 请求中 system 与 user 消息的估算 token
margin = max(4096, ceil(input * 0.15))
room   = context_tokens - input - margin
if room < 32768:
    room = min(32768, context_tokens - input - 4096)
max_tokens = min(max_output_tokens, max(1, room))
```

全部使用饱和运算。终止原因为 `StopReason::MaxTokens` 时返回 `ModelError::Protocol`，文本按请求类型分别为 `Summarization failed: generation hit the token cap and the summary is incomplete`、`Turn prefix summarization failed: …`、`Branch summarization failed: …`，不记录 usage，也不持久化。`generate_compaction_summary` 的重试循环（`compaction.rs:818`）只重试 Preflight、Request、Stream 错误，因此截断不会被重试，与 pi 一致。

摘要请求使用 `session.model.snapshot()` 返回的 thinking level；provider 层照常降级不支持的等级。

#### 1.3 序列化

`SummaryPrompt::render`：

- 不输出 Reasoning 块。
- Tool 调用格式为 `name(k1=<json>, k2=<json>)`，多个调用以 `; ` 连接。不保证参数顺序。
- `BranchSummary` 序列化为 `[User]: <model_text>`。

#### 1.4 split-turn 前缀

- 提示词替换为附录 A.1 的 pi 文本。
- 请求布局为 `# Conversation\n{conversation}\n\n# Instructions\n{prompt}`，自定义指令非空时追加 `\n\nAdditional focus: {instructions}`。
- `SummaryProtocol::TurnPrefix` 增加 `custom_instructions: Option<&str>`，由 `compact_with_identity` 传入与历史摘要相同的指令。

### 2. Run 内阈值检查

`RunScope::next_action` 中 `continue_for_tools || next_queued_item.is_some()` 分支改为调用新方法 `RunScope::compact_before_next_turn(turn, turn_loop, &settled) -> Result<Option<RunCompletion>, KernelError>`：

1. 决定原因：
   - `settled.overflow_recovery == Some(OverflowRecovery::CompactOnly)` 时为 `Overflow`。
   - 否则以 `session.model.active()?.profile()` 解析生效策略，若 `ContextUsageEstimate::from_history(history).should_compact(context_tokens, policy)` 为真则为 `Threshold`。
   - 否则返回 `Ok(None)`。
2. 调用 `Kernel::perform_compaction`，不排除消息，`will_retry = false`。
3. 结果处理：
   - `Ok(_)`：返回 `Ok(None)`，下一 Turn 的 `model_request` 读取压缩后的 history。
   - `Err(error)` 且 `error.is_cancellation()` 或 Run 已取消：返回 `Ok(Some(turn_loop.finish(…, AgentOutcome::Cancelled, None)))`，避免被 `settle` 记为 Failed。
   - `Err(KernelError::ExtensionBlocked(_))`：扩展取消压缩时 `compaction.rs:294` 已记录 `warn`，且取消发生在 `CompactionStart` 之前、没有生命周期事件，这里只记 `tracing::debug!` 并返回 `Ok(None)`。
   - 其他错误：`tracing::warn!` 记录后返回 `Ok(None)`。这些错误发生在 `CompactionStart` 之后，已由压缩生命周期发出 `CompactionEnd::Failed`，并按 4.2 发出 `session_compact_failed`。

新 prompt 前检查、Run 结束检查、overflow CompactAndRetry 路径保持不变。排队消息在压缩期间保持已认领状态，Run 出错时照常退回队列。子 Agent 的子 Run 使用同一个 `RunScope`，同样获得 Run 内阈值检查，这是预期行为。

`compact_with_identity` 中"没有模型上下文可压缩"的日志：手动压缩保持 `warn`，自动压缩降为 `debug`，避免每个 Turn 重复。

### 3. 按模型覆盖

config crate 用真实结构体替换 `pub use protocol::CompactionPolicy as CompactionConfig`：

```rust
pub struct CompactionConfig {
    #[serde(flatten)]
    pub policy: CompactionPolicy,
    #[serde(default)]
    pub model_overrides: BTreeMap<String, CompactionModelOverride>,
}

pub struct CompactionModelOverride {
    #[serde(default)]
    pub reserve_tokens: Option<u64>,
    #[serde(default)]
    pub keep_recent_tokens: Option<u64>,
}

impl CompactionConfig {
    /// 以 "provider_id/model_id" 精确匹配覆盖项；enabled 始终取全局值。
    pub fn resolve(&self, provider_id: &str, model_id: &str) -> CompactionPolicy;
}

impl From<CompactionPolicy> for CompactionConfig;
```

键中的 `model_id` 是本地模型 `id`（与 `ModelProfile::model_id` 和 `active_model` 一致），不是 `upstream_id`。`resolve` 接收 id 而非 profile，因为 provider factory 在构建 profile 之前就需要 reserve。

生效策略替换以下读取点：

- provider factory 默认 `request_max_tokens = min(max_output_tokens, 生效 reserve)`（`provider/src/factory/mod.rs:193`、`:346`）。
- 配置校验 `RequestBudgetExceedsReserve`（`config/src/config.rs:328`）。
- Kernel 字段 `compaction_policy` 类型改为 `CompactionConfig`，builder 使用 `setter(into)`，现有测试传入 `CompactionPolicy` 不变；app 传入完整配置（`app/src/lib.rs:203`）。
- 新 prompt 前检查、`compact_with_identity`、`run/turn.rs:833` overflow 判定、`run/turn.rs:1201` Run 结束检查、第 2 节 Run 内检查。分支摘要预算使用第 5.3 节的独立配置，不读取 compaction 覆盖项。

校验：

- 加载配置时，每个键必须是非空 `provider/model` 格式（与 `active_model` 的格式检查相同），否则返回新错误 `ConfigValidationError::MalformedCompactionOverride { key }`。
- Kernel 构建时（`runtime.rs:244`，模型目录已包含扩展提供的模型）：用 `ModelCatalog::profiles()`（`kernel/src/model.rs:189`）检查每个键都对应一个 `provider_id/model_id`，否则返回新错误 `ConfigValidationError::UnknownCompactionOverride { key }`；对 active 模型和所有带覆盖项的模型，校验生效 reserve 小于 `context_tokens`、`request_max_tokens` 不超过生效 reserve。无覆盖的非 active 模型保持现状，不新增校验。

`README.md` 与 `README_zh.md` 的压缩配置说明补充 `model_overrides`。

### 4. 扩展钩子

#### 4.1 准备数据

protocol 新增 `CompactionPreparation`（typed-builder）：

| 字段 | 类型 |
|---|---|
| `messages_to_summarize` | `Vec<AgentMessage>` |
| `turn_prefix_messages` | `Vec<AgentMessage>` |
| `retained_tail` | `Vec<AgentMessage>` |
| `is_split_turn` | `bool` |
| `tokens_before` | `u64` |
| `previous_summary` | `Option<String>` |
| `read_files` | `Vec<String>` |
| `modified_files` | `Vec<String>` |
| `settings` | `CompactionPolicy` |

`SessionBeforeCompactEvent` 增加 `preparation` 字段。Kernel 内部 `CompactionPreparation` 通过 `impl From<&CompactionPreparation> for protocol::CompactionPreparation` 转换；消息复制以 `ponytail:` 注释标注（压缩频率低）。`retained_tail` 是本项目特有字段，扩展返回 `CompactionData` 时原样带回即可。

`compact_with_identity` 构造事件时设置 `custom_instructions(instruction.clone())`。

#### 4.2 失败观察点

protocol 新增 `SessionCompactFailedEvent { reason, error_message: Option<String>, aborted, will_retry, from_extension }`（typed-builder），按 `session_compact` 的方式接入：`extension/src/point.rs` 的 `define_point!` 与名称列表、`extension/src/registry.rs`、`extension/src/runtime/session.rs` 的 `emit_session_compact_failed`、示例 `SessionObserver`。

发出时机：

| 场景 | aborted | error_message |
|---|---|---|
| 摘要生成或持久化失败（`lifecycle.fail` 之后） | false | 错误文本 |
| 压缩被取消 | true | 无 |
| 扩展在 `session_before_compact` 中取消 | true | 无 |
| 手动压缩遇到 `NoContextToCompact` | false | 错误文本 |
| overflow 已重试一次后再次需要恢复 | false | 附录 A.4 文本 |
| 自动压缩遇到 `NoContextToCompact` | 不发出 | — |

手动 `NoContextToCompact` 当前在创建扩展上下文（`compaction.rs:277`）之前就被拒绝（`compaction.rs:247`），实现时需把扩展上下文的创建移到该检查之前。

overflow 二次失败的触发条件为 `settled.overflow_recovery == Some(OverflowRecovery::CompactAndRetry) && turn_loop.overflow_recovery_attempted`，在 `next_action` 跳过重试分支处发出，`reason = Overflow`、`will_retry = false`、`from_extension = false`。`CompactAndRetry` 同时覆盖 provider 溢出与截断回复，因此按 `settled.stop_reason` 选择文本：`StopReason::Error` 用附录 A.4 的上下文溢出文本，`StopReason::MaxTokens` 用截断回复文本。截断且无 Tool 调用的回复在现有流程中以 Succeeded 结束，pi 在该情况下同样只报告恢复失败、不改变回复。该场景只发扩展事件，不发 `CompactionEnd`，因为没有配对的 `CompactionStart`。

### 5. 分支摘要

#### 5.1 消息类型

```rust
MessageContent::BranchSummary {
    #[serde(flatten)]
    branch: BranchSummaryMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, typed_builder::TypedBuilder)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryMessage {
    pub entry_id: EntryId,
    pub from_id: String,
    pub summary: String,
    #[serde(default)]
    pub read_files: Vec<String>,
    #[serde(default)]
    pub modified_files: Vec<String>,
}
```

派生与序列化风格与 `CompactionSummaryMessage`（`protocol/src/message.rs:355`）一致；`read_files` 与 `modified_files` 同时标注 `#[builder(default)]`。`BranchSummaryMessage::model_text()` 返回附录 A.3 的前缀 + `summary` + 后缀。所有 `MessageContent` 穷举匹配补齐：`role()` 为 `None`；`text_content()` 为 `summary`；图片适配跳过；provider 边界映射为 User 文本；`plan/note.rs`、`session_stats.rs`、ACP 映射；token 估算为 `summary` 字符数 / 4；切点与 Turn 起点见 1.1。

#### 5.2 上下文与回放

- `session/replay.rs` 重建 history 时将 `EntryKind::BranchSummary` 转为该消息：message id 与 turn id 均为 `branch-summary-{entry_id}`（`TurnId` 只要求非空，见 `protocol/src/scalar.rs:63`），三个时间字段都取 entry 时间戳，经 `notes.push` 放入 history，与普通消息共享 ModeNote 排序。
- 客户端回放同样输出该消息。
- 已有会话中的旧分支摘要会开始进入上下文；旧 payload 无文件列表时为空。
- 分支摘要消息可能落入之后某次压缩的 `retained_tail`，从而随 `CompactionData` 持久化。重放遇到 Compaction entry 时会重置 history，不会与 BranchSummary entry 重复。

#### 5.3 生成

`session/tree.rs` 的导航摘要：

- 选取：Message entry（跳过 ToolResult，且只取 `estimated_tokens() > 0` 的模型可见消息，从而排除隐藏 Extension、SlashCommand 与 `exclude_from_context` 的 bash，与 retained tail 的过滤方式 `compaction/context.rs:264` 一致）、Compaction entry（转为 `CompactionSummary` 消息）、嵌套 BranchSummary entry。预算为 `context_tokens.saturating_sub(branch_summary.reserve_tokens)`，预算为 0 表示不限制（与 pi `tokenBudget > 0` 判断一致）；从新到旧选取，超出预算即停；越界项是摘要 entry 且已选总量小于预算 90% 时仍纳入。
- 配置：新增 `[branch_summary] reserve_tokens`，默认 16384，对应 pi `branchSummary.reserveTokens`。config crate 新增 `BranchSummaryConfig { reserve_tokens: u64 }`，`AppConfig` 以 `#[serde(default)]` 持有；Kernel builder 增加 `#[builder(default)] branch_summary: BranchSummaryConfig`。不引入 pi 的 `skipPrompt`（TUI 专用）。`README.md` 与 `README_zh.md` 补充该配置。
- 文件追踪：先累积嵌套分支摘要（非 `fromHook`）的 `details.readFiles/modifiedFiles`，再从遍历到的 Assistant Tool 调用提取。
- 提示词：`SummaryProtocol::Branch` 改为 `SummaryPrompt` 序列化 `<conversation>…</conversation>` 后接附录 A.2 的 `BRANCH_SUMMARY_PROMPT`；自定义指令默认以 `Additional focus:` 追加，`replace_instructions` 为真时替换整个提示词。删除 `BRANCH_SUMMARY_INSTRUCTION`。
- 输出：附录 A.2 的 preamble + 摘要 + `<read-files>`/`<modified-files>`。payload 写入 `details: {readFiles, modifiedFiles}` 与生成的 `usage`。
- 选取结果为空时不写 entry（保留现状；pi 写入 "No content to summarize"，此处为有意差异）。
- 适用 1.2 的输出上限、截断拒绝与 thinking level。

#### 5.4 ACP 与 WebUI

- ACP（`acp/src/mapping.rs` 两处）将 `BranchSummary` 映射为 `user_message`，product meta 带 `origin: "branch_summary"`。
- WebUI `updateDecoder.ts` 识别该 origin，新增可折叠的 `BranchSummaryCard`，在 `Conversation.tsx` 中渲染。WebUI 导航后会清空 transcript 并重新打开会话（`web/src/workspace/controller.ts:338-343`）。ACP 的 `session/load` 可能复用 Run 开始时捕获基线的保留投影 journal，而树导航不是 Operation，不会刷新它；因此 Kernel 在提交导航后、仍持有 Session 操作锁时向 Session sink 发出 `AgentEventPayload::BranchNavigated { old_leaf_id, new_leaf_id }`（ACP、扩展宿主等所有入口一致），ACP 投影收到后丢弃已结束的 journal（运行中的 journal 必然晚于导航开始、基线已是新分支，予以保留；实时订阅不受影响），使重新打开走 Store 的持久化回放，分支摘要随回放出现。WebUI 仅在导航会放弃条目时（当前 leaf 不是目标的祖先）显示摘要选择面板。

#### 5.5 客户端请求分支摘要

当前 ACP 的 Navigate 与 Branch 方法都调用 `Kernel::navigate_session`，固定 `summarize = false`（`session/tree.rs:118`），只有扩展能通过 `extension/host.rs:765` 请求摘要。pi 在 TUI 导航时提供"不摘要 / 摘要 / 带自定义指令摘要"三个选项，本节为 ACP 与 WebUI 提供同等能力。

- protocol `AcpNavigateParameters`（`protocol/src/acp.rs:24`）增加 `#[serde(default)] summarize: bool` 与 `#[serde(default)] custom_instructions: Option<String>`。旧客户端不传这两个字段时行为不变。
- `entry_id` 为空（导航到根节点）且 `summarize = true` 时返回 invalid params；根节点导航保持现有清空逻辑。
- Kernel 新增 `TreeNavigationRequest { target: EntryId, summarize: bool, custom_instructions: Option<String> }`，`navigate_session_with_summary` 改为接收该请求。`custom_instructions` 写入 `SessionBeforeTreeEvent.custom_instructions`（当前恒为 `None`），并作为生成时的默认自定义指令；扩展返回的 `custom_instructions`/`replace_instructions` 仍可覆盖。扩展宿主 `navigate_tree` 传 `None`。
- ACP Navigate 与 Branch 将新参数转发给 `navigate_session_with_summary`。
- WebUI `SessionTree.tsx`：Navigate 与 Branch 在目标不是当前 leaf 时先展开一个内联选择面板，提供"不生成摘要""生成摘要""带指令生成摘要"（附多行输入框）。`WorkspaceController.navigate/branch` 增加 `{ summarize, customInstructions }` 参数并随请求发送。摘要生成期间沿用现有 `busy` 状态提示。

## 错误处理

- 摘要失败、截断、取消均不移动 leaf、不替换 history，与现有行为一致。
- Run 内阈值压缩失败不终止 Run；取消终止为 Cancelled。
- 配置错误在启动时失败，不延迟到首次压缩。
- 分支摘要失败使导航失败，与现有行为一致。

## 测试策略

测试只放在 crate 的 `tests/` 目录或 `#[cfg(test)] mod tests` 中。

- `compaction/context.rs` 单元测试：尾部超大 Tool 结果回退到最后切点；`keep_recent_tokens = 0` 保留最后切点；可见 Extension 起始的切点不产生 split-turn；更新 `hidden_extension_is_not_a_compaction_cut_point` 的预期。
- `kernel/tests/compaction.rs`：
  - `MaxTokens` 摘要被拒绝且不持久化；`max_tokens` clamp 计算；序列化不含 thinking；Tool 调用格式；前缀提示词布局与自定义指令。
  - Run 内阈值压缩发生在第 N 与 N+1 个 Turn 之间且下一请求以摘要开头；排队消息前的 CompactOnly；Run 内摘要失败后 Run 成功；Run 内扩展取消后 Run 成功且不记 warn；Run 内取消得到 Cancelled；低于阈值不压缩；Run 内压缩后 Run 结束检查不重复压缩。
  - overflow 二次失败：provider 溢出与截断回复分别发出对应文本的 `session_compact_failed`。
  - fixture 改造：`CompactionFixture` 默认改为两轮历史，`keep_recent_tokens` 等于第二轮的估算值，使切点落在第二条 user 消息上而不切 turn；`OverflowFixture` 在触发溢出的 prompt 之前先完成一轮历史，使 overflow 恢复有可压缩内容。依赖请求下标与次数的断言随之调整。
  - 复核所有依赖 `keep_recent_tokens = 0` 的测试（含 `slash_commands.rs`、`lifecycle_order.rs`）的断言；保持每个测试的原意，只改断言不改生产代码。
  - 新会话第一个 prompt 溢出时不压缩、不重试，以 provider 错误结束。
- config 测试：`model_overrides` 解析、`resolve` 合并与精确匹配、键格式错误返回 `MalformedCompactionOverride`。
- kernel 构建测试：未知覆盖键失败；覆盖 reserve 不小于窗口失败；覆盖改变阈值与保留量。
- provider 测试：覆盖 reserve 传导到默认 `request_max_tokens`。
- 扩展测试（`kernel/tests/extensions_host.rs`、`crates/extension/tests/runtime.rs`）：准备数据与实际分区一致；自定义指令可见；五种失败场景各发出一次 `session_compact_failed`，自动 `NoContextToCompact` 不发出；扩展返回的 `CompactionData` 正确持久化。
- 分支摘要：导航后下一 Turn 请求包含分支摘要文本；resume 后 history 与导航后一致；隐藏 Extension 与 SlashCommand 不进入分支摘要输入；分支摘要进入 retained tail 后再次重放不重复；`branch_summary.reserve_tokens` 预算截断、预算为 0 不限制与 90% 例外；嵌套文件追踪；截断拒绝；`acp/tests/mapping.rs` 映射断言。
- 客户端请求摘要：`AcpNavigateParameters` 缺省字段反序列化为不摘要；ACP Navigate/Branch 带 `summarize` 与 `customInstructions` 时生成 BranchSummary entry，自定义指令出现在摘要请求与 `SessionBeforeTreeEvent` 中；根节点导航请求摘要返回 invalid params。
- WebUI：按项目规则不写前端单元测试，使用生产 `app` 后端验收导航摘要选择面板与分支摘要卡片。

## 实施阶段

1. Kernel 内部行为：第 1 节。
2. Run 内阈值检查：第 2 节，依赖 1.1。
3. 接口变化：第 3、4、5 节。

## 非目标

- 不在 provider 层引入通用的上下文 clamp；clamp 只用于摘要请求。
- 不把 system prompt 与 Tool 定义计入压缩后的首次阈值估算（与现有 overflow 路径一致，下一次真实 usage 会校正）。
- 不校验无覆盖项的非 active 模型的 reserve。
- 不实现 pi 的 virtual model 路由与 codemode 嵌套调用文件追踪（本项目无对应功能）。
- 客户端导航摘要不提供 pi 的 `replaceInstructions` 与 `label` 参数，也不实现 `branchSummary.skipPrompt`；扩展仍可通过 `session_before_tree` 结果使用前两者。
- 不保证旧版本程序读取新会话：新增的 `MessageContent::BranchSummary` 可能出现在持久化的 `CompactionData.retained_tail` 中，旧版本反序列化会失败。新版本读取旧会话不受影响。
## 附录 A：pi 文本

### A.1 split-turn 前缀提示词

```text
The messages above are earlier context from an ongoing conversation. Later messages are stored separately and do not need to be reconstructed.

Create a concise checkpoint of the user's request and the progress shown above. This checkpoint will be placed before the later messages so the conversation can continue with the necessary context.

## Original Request
[What did the user ask for?]

## Progress So Far
- [Key decisions and work completed in these messages]

## Context Needed to Continue
- [Information from these messages needed to understand the later work]

Only summarize information explicitly present above. Do not infer or recreate later messages.
```

### A.2 分支摘要

Preamble（以两个换行结尾）：

```text
The user explored a different conversation branch before returning here.
Summary of that exploration:

```

`BRANCH_SUMMARY_PROMPT`：

```text
Create a structured summary of this conversation branch for context when returning later.

Use this EXACT format:

## Goal
[What was the user trying to accomplish in this branch?]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Work that was started but not finished]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [What should happen next to continue this work]

Keep each section concise. Preserve exact file paths, function names, and error messages.
```

### A.3 分支摘要上下文包装

前缀为 `The following is a summary of a branch that this conversation came back from:\n\n<summary>\n`，后缀为 `</summary>`（无前导换行）。

### A.4 overflow 二次失败文本

- 上下文溢出：`Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model.`
- 截断回复：`Truncated response recovery failed after one compact-and-retry attempt.`
