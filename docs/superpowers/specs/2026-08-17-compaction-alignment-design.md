# 上下文压缩与 pi v4 对齐设计

## 目标

修复当前上下文压缩与 pi v4 不一致的问题，使摘要生成、持久化、恢复后的模型上下文、ACP 回放和 WebUI 提示形成一条可验证的完整链路。

完成后应满足：

- 摘要模型收到 pi 风格的专用 system prompt、结构化 checkpoint 模板和明确的会话边界。
- 二次压缩使用上一份摘要做增量更新，不把上一份摘要当成普通系统消息重新总结。
- compaction entry 保留摘要、retained tail、压缩前 token 数、摘要调用 usage 和扩展详情。
- 原始分支记录不删除；模型上下文从最新 compaction boundary 恢复。
- 压缩开始、成功、失败和取消都有明确事件；成功卡片可以通过 ACP 回放。
- WebUI 压缩过程中显示临时状态，完成后显示持久化的折叠卡片。
- 没有模型上下文可压缩时拒绝操作，不调用 provider，也不写 compaction entry。

## 已确认问题

当前实现存在以下差异：

1. `COMPACTION_INSTRUCTION` 只有一句通用要求，没有 pi 的结构化摘要格式。
2. 摘要请求把指令作为 `System` 消息直接放在历史前面，没有使用独立 system prompt，也没有 `<conversation>` 和 `<previous-summary>` 边界。
3. 自定义压缩要求会替换默认摘要指令，而不是作为 `Additional focus` 追加。
4. 恢复时把摘要构造成普通 `System` 消息；pi 使用独立的 `compactionSummary`，在 provider 边界映射为带 `<summary>` 标签的用户消息。
5. 二次压缩没有显式的上一摘要增量更新语义，也没有 split-turn 摘要。
6. `CompactionData` 没有持久化摘要调用 usage，operation records 也没有完整表达 pi v4 的 compaction intent 和 attempt。
7. WebUI 只显示运行状态 chip 和 `/compact` 的普通成功输出；compaction entry 不能作为独立卡片回放。
8. 真实测试生成了 `tokensBefore: 0` 的 compaction entry，模型把自定义要求解释成普通任务，证明当前提示和前置校验均不可靠。

## 领域模型

### 压缩准备

Kernel 从当前 lane 的完整 root-to-leaf branch 构建 `CompactionPreparation`，而不是只根据已投影的内存 history 猜测边界。准备结果包含：

- `messages_to_summarize`：需要并入历史摘要的模型可见消息。
- `turn_prefix_messages`：切分超大 Turn 时单独摘要的前缀。
- `retained_tail`：压缩后继续原样提供给模型的最近消息。
- `previous_summary`：最近 compaction entry 的摘要。
- `tokens_before`：压缩前有效模型上下文的 token 估算。
- `read_files` 与 `modified_files`：被摘要历史中涉及的文件操作。
- `is_split_turn`：是否需要补充 Turn 前缀摘要。

Slash Command 调用和输出不进入模型上下文，也不计入可压缩 token。若没有上一摘要，且没有任何需要摘要的模型可见消息，则返回“没有可压缩的上下文”，不访问 provider、不移动 lane leaf。

### 摘要消息

在 `protocol` 中定义独立的压缩摘要内容类型，避免把摘要伪装成普通 System 或 User 消息。该类型至少包含：

- `summary`
- `tokens_before`
- compaction entry identity 和 timing 所需关联信息

Provider 适配层只在构建模型请求时将其转换为用户消息：

```text
The conversation history before this point was compacted into the following summary:

<summary>
...
</summary>
```

ACP 和 WebUI 则保留它的压缩语义，不把它显示为用户输入。

## 摘要提示词

采用 pi 的摘要协议：

- 专用 system prompt：只允许输出结构化摘要，不继续原会话，也不回答原会话中的问题。
- 首次摘要输出固定章节：Goal、Constraints & Preferences、Progress、Key Decisions、Next Steps、Critical Context。
- 二次压缩把上一摘要放入 `<previous-summary>`，使用更新模板保留既有信息并合并新进展。
- 当前会话序列化后放入 `<conversation>`。
- `/compact` 后的参数只作为 `Additional focus` 追加，不能替换基础摘要协议。
- split-turn 使用独立的 Turn Prefix 模板，并与历史摘要合并。
- 摘要请求禁用 tools，并将最大输出限制为 `min(reserve_tokens * 0.8, model.max_output_tokens)`。
- 空摘要、tool call、取消和 provider 错误均视为压缩失败。

## 存储结构

compaction entry 继续作为当前 lane leaf 的子节点，原始消息和旧 compaction entry 不删除。payload 对齐 pi v4 的核心字段：

```text
type: compaction
summary
retainedTail
tokensBefore
usage?
details?
```

`details` 保留 clawcode 必需的关联信息，并增加 pi 使用的文件操作信息：

- reason
- turnId
- startedAtMs
- endedAtMs
- readFiles
- modifiedFiles

compaction operation records 使用类型化 payload 表达：

- `operation_started`：source leaf、`intent.kind = compaction`、自定义要求和预留的结果 entry id。
- `step_attempt`：attempt、结果 entry id 和 compaction reason。
- `usage`：cause 为 compaction，并关联 run、entry 和 attempt。
- `operation_finished`：completed、aborted 或 failed。

先完成摘要，再追加 compaction entry；任何失败都不能移动 lane leaf。operation failure record 可以持久化诊断，但客户端只接收稳定错误文案。

## 恢复与二次压缩

恢复 Session 时：

1. 完整 transcript 和 Session Tree 仍可看到原始分支。
2. Provider 上下文从最新 compaction entry 开始，只包含压缩摘要、该 entry 的 retained tail 和它之后的消息。
3. 模型看到的摘要使用 compaction-summary 用户消息包装，不使用普通 System 消息。
4. 再次压缩时读取最新 entry 的 `summary` 与 `retainedTail`，生成增量摘要并追加新的 compaction entry。
5. 上一 compaction entry 保留在树中，但只有最新 boundary 参与当前模型上下文。

## 事件与 ACP

`CompactionStart` 保留 run、Turn、reason 和毫秒时间关联。

`CompactionEnd` 使用类型化结果表达：

- completed：完整 CompactionResult，包括 entry id、summary、tokens before、timing 和 usage。
- failed：稳定的客户端错误，不携带 provider 或 Extension 内部细节。
- cancelled：明确取消状态。

实时 ACP 将这些事件映射为产品命名空间扩展 update。Session resume 从 branch entries 重建 message 与 compaction replay items，按照持久化 sequence 发送，因此压缩卡片在刷新后仍处于正确位置。

## WebUI 交互

压缩期间在对话底部显示 Codex 风格的轻量运行提示：

```text
正在压缩上下文…
```

手动压缩允许使用现有停止按钮取消。阈值或 overflow 自动压缩可在副文案中说明原因。

压缩成功后，运行提示替换为持久化卡片：

```text
上下文已压缩
从 N tokens 压缩 · 点击展开
```

卡片默认折叠；展开显示结构化摘要、原因和耗时。卡片由 compaction entry 投影，刷新和 Session resume 后仍可回放。

`/compact` 仍持久化调用消息，但成功时不再生成重复的普通 Slash Command 成功输出。失败时保留稳定的命令失败结果，并显示压缩失败提示。

## 错误处理

- 没有模型可见上下文：返回稳定的“没有可压缩的上下文”，不产生 start/end 运行假象。
- 已位于无新增上下文的 compaction boundary：返回同一稳定错误。
- Provider、Extension 或持久化错误：日志记录完整 trace、run、Turn 和内部错误；ACP 只发送稳定失败状态。
- 取消：记录 aborted operation，发送 cancelled end，不写 compaction entry。
- replay 遇到非法 compaction payload：Store 拒绝打开损坏 Session，不能静默降级为普通消息。

## 测试策略

Rust 使用测试先行：

1. 摘要请求精确包含 pi system prompt、`<conversation>`、结构化模板、previous summary 和 additional focus。
2. `tokensBefore = 0` 或没有可摘要模型消息时不调用 provider、不写 entry。
3. 首次压缩持久化完整 payload、usage、details 和操作 records。
4. 恢复后 provider 请求只包含压缩摘要、retained tail 和后续消息，摘要角色转换正确。
5. 二次压缩使用 previous summary，不重复总结旧原文。
6. split-turn 不丢失 Turn 前缀上下文。
7. 失败和取消不移动 leaf，并发出对应终态事件。
8. ACP 实时事件与 `replay_from=start` 产生同一 compaction card identity 和顺序。

前端按项目规则不增加单元测试。完成后运行：

- Rust workspace 全量测试、Clippy、fmt 和 diff check。
- WebUI TypeScript、ESLint 和生产构建。
- 正式 `app` 后端、真实配置 provider 和 WebSocket WebUI 端到端测试。
- E2E 覆盖压缩中提示、完成卡片、展开摘要、刷新回放、Session Tree payload 和压缩后的下一次真实 provider 请求。

## 非目标

- 不改变 provider 和 config 模块的公共职责。
- 不删除原始历史或改变 fork/navigation 的分支保留规则。
- 不新增 UI Extension hook point。
- 不为前端增加单元测试。
- 不在本阶段重构与 compaction 无关的 Store record。
