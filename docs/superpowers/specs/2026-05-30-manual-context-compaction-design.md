# 手动上下文压缩设计方案

**日期**: 2026-05-30
**状态**: 待审核

参考实现:

- Codex: `/home/isbest/Documents/WorkSpace/codex`
- opencode: `/home/isbest/Documents/WorkSpace/opencode`

---

## 1. 背景

当前 `clawcode` 已经有上下文管理的抽象入口，但没有真正实现压缩：

- `crates/kernel/src/context.rs` 中 `ContextManager::should_compact()` 默认返回 `false`。
- `ContextManager::compact()` 是 no-op。
- `InMemoryContext` 只保存 `Vec<Message>`，每轮请求都把完整 `context.history()` 传给模型。
- `store` 目前只持久化普通 `MessageRecord`，恢复 session 时会重放所有历史消息。
- slash command 当前没有 `/compact`。

这意味着长会话只能持续累积上下文，无法手动释放模型上下文窗口。

---

## 2. 目标

1. 实现手动 `/compact`，用户主动触发上下文压缩。
2. 压缩语义更靠近 Codex：压缩成功后替换 live history，而不是只追加摘要消息。
3. 压缩提示词参考 opencode：使用 anchored summary，结构化输出 Markdown。
4. 压缩后保留最近少量原文上下文，旧历史通过摘要承载。
5. 持久化 compaction checkpoint；恢复 session 时仍重放完整 transcript，但 live model history 只从最新 checkpoint 的 replacement history 加后续消息构建。
6. 第一版增加 `compaction.retained_turns` 配置，默认保留最近 2 个 user turns。
7. 第一版不实现自动压缩，不在 token 阈值触发。

---

## 3. 非目标

1. 不实现自动 compaction。
2. 不实现 OpenAI `/responses/compact` 专用远端接口。
3. 不实现复杂 token tokenizer；继续使用项目现有粗略 token 估算。
4. 不压缩正在运行中的 turn；`/compact` 只在 session 空闲时作为独立任务执行。
5. 不引入 SQLite 或额外索引层。
6. 不改动 provider 协议的底层 streaming 形状。

---

## 4. 设计原则

1. **Live history 可控替换**: 压缩成功后，`ContextManager` 中的消息应变成 `summary + retained_tail`，下一轮模型请求不再携带完整旧历史。
2. **持久化可恢复**: JSONL 中必须记录 replacement history；恢复时完整 transcript 仍用于审计和 UI，live model history 从最新 checkpoint materialize。
3. **摘要可继续更新**: 后续再次 `/compact` 时，应识别上一份摘要，将其作为 anchored summary 更新，而不是简单叠加多份摘要。
4. **提示词稳定**: 压缩输出固定 Markdown section，便于后续模型继续任务。
5. **手动优先**: 第一版只暴露 `/compact`，降低交互和失败恢复复杂度。

---

## 5. 用户体验

### 5.1 Slash Command

新增 slash command：

```text
/compact
```

别名暂不增加。opencode 支持 `/summarize`，但本项目第一版保持命令面收敛，只支持 `/compact`。

### 5.2 执行反馈

用户提交 `/compact` 后：

1. TUI 不把 `/compact` 当普通 prompt 发给模型。
2. Kernel 启动 compaction turn。
3. UI 显示上下文压缩开始和结束事件。
4. 压缩成功后，下一条普通 prompt 使用压缩后的 history。

第一版可以使用普通 system message 或 `ItemStarted`/`ItemCompleted` 表示 compaction 状态。若实现 `TurnItem::ContextCompaction`，TUI/ACP 可以像 Codex 一样渲染独立生命周期项。

---

## 6. 核心架构

```text
TUI / ACP
   │
   │ /compact
   ▼
protocol::Op::Compact
   │
   ▼
kernel::session::run_loop
   │
   │ spawn standalone compaction turn
   ▼
kernel::compaction::run_manual_compaction()
   │
   ├─ snapshot current ContextManager history
   ├─ split into summary_input + retained_tail
   ├─ call Llm::completion() with compaction prompt
   ├─ build replacement history
   ├─ context.replace_compacted_history(...)
   └─ persist PersistedPayload::Compaction(...)
```

---

## 7. 数据模型

### 7.1 protocol::Op

新增：

```rust
Compact {
    session_id: SessionId,
}
```

`Compact` 不携带用户文本。它是独立 session 操作，不是普通 prompt。

### 7.2 store::PersistedPayload

新增：

```rust
Compaction(CompactionRecord)
```

`CompactionRecord` 字段：

```rust
pub struct CompactionRecord {
    pub turn_id: String,
    pub summary: String,
    pub replacement_history: Vec<Message>,
    pub retained_message_count: usize,
}
```

说明：

- `summary` 用于审计和展示。
- `replacement_history` 是恢复 live model history 的 checkpoint，不是完整 transcript 的替代品。
- `retained_message_count` 便于调试保留 tail 的行为。
- 结构体字段超过 3 个，按项目规则必须使用 `typed-builder`。

### 7.3 ContextManager

扩展 trait：

```rust
fn replace(&mut self, messages: Vec<Message>);
```

或者更明确：

```rust
fn replace_compacted(&mut self, messages: Vec<Message>);
```

推荐使用 `replace()`，因为未来 rollback、fork、replay 也可能需要替换上下文。实现时应在函数级注释中说明它会整体替换当前 history。

---

## 8. 压缩提示词

### 8.1 System Prompt

参考 opencode 的 compaction agent prompt，第一版内置为常量：

```text
You are an anchored context summarization assistant for coding sessions.

Summarize only the conversation history you are given. The newest turns may be kept verbatim outside your summary, so focus on the older context that still matters for continuing the work.

If the prompt includes a <previous-summary> block, treat it as the current anchored summary. Update it with the new history by preserving still-true details, removing stale details, and merging in new facts.

Always follow the exact output structure requested by the user prompt. Keep every section, preserve exact file paths and identifiers when known, and prefer terse bullets over paragraphs.

Do not answer the conversation itself. Do not mention that you are summarizing, compacting, or merging context. Respond in the same language as the conversation.
```

### 8.2 User Prompt

如果没有 previous summary：

```text
Create a new anchored summary from the conversation history above.
```

如果已有 previous summary：

```text
Update the anchored summary below using the conversation history above.
Preserve still-true details, remove stale details, and merge in the new facts.
<previous-summary>
{previous_summary}
</previous-summary>
```

然后追加固定模板：

```text
Output exactly the Markdown structure shown inside <template> and keep the section order unchanged. Do not include the <template> tags in your response.
<template>
## Goal
- [single-sentence task summary]

## Constraints & Preferences
- [user constraints, preferences, specs, or "(none)"]

## Progress
### Done
- [completed work or "(none)"]

### In Progress
- [current work or "(none)"]

### Blocked
- [blockers or "(none)"]

## Key Decisions
- [decision and why, or "(none)"]

## Next Steps
- [ordered next actions or "(none)"]

## Critical Context
- [important technical facts, errors, open questions, or "(none)"]

## Relevant Files
- [file or directory path: why it matters, or "(none)"]
</template>

Rules:
- Keep every section, even when empty.
- Use terse bullets, not prose paragraphs.
- Preserve exact file paths, commands, error strings, and identifiers when known.
- Do not mention the summary process or that context was compacted.
```

---

## 9. History 选择策略

第一版使用简单、可测试的策略：

1. 找出当前 history 中最近的 compaction summary message。
2. 如果存在，把它作为 `previous_summary`。
3. 将最近 `compaction.retained_turns` 条用户 turn 作为 retained tail 原文保留。
4. 将 retained tail 之前的消息作为 summary input。
5. 压缩请求发送 `summary_input + compaction prompt`。
6. replacement history 为：

```text
Message::User(summary_marker_text)
retained_tail...
```

其中 `summary_marker_text` 推荐为：

```text
Another model previously summarized the conversation. Use this summary as authoritative context for older turns:

{summary}
```

第一版新增配置项：

```toml
[compaction]
retained_turns = 2
```

配置语义：

- `retained_turns` 表示压缩后原样保留的最近 user turns 数量。
- 默认值为 `2`。
- `0` 是合法值，表示压缩后只保留 summary marker，不额外保留最近 turn 原文。
- user turn 的范围从一条非 compaction 用户消息开始，到下一条非 compaction 用户消息之前结束，包含其中的 assistant/tool-result 消息。

---

## 10. 对话恢复语义

恢复必须区分两个视图：

1. **完整 transcript replay**: 顺序读取 JSONL 中所有 `MessageRecord`、`TurnContextRecord`、`CompactionRecord` 等记录，保留完整历史用于 UI、审计、session 列表、调试和未来导出。
2. **live model history materialization**: 给下一次模型请求使用的 `ContextManager` 只从最新 compaction checkpoint 开始构建。

加入 `CompactionRecord` 后，replay 逻辑应输出一个包含双视图的恢复结果：

```rust
pub struct ReplayedSession {
    pub meta: SessionMetaRecord,
    pub messages: Vec<Message>,
    pub live_messages: Vec<Message>,
    pub usage: Option<Usage>,
    pub agent_edges: Vec<AgentEdgeRecord>,
}
```

语义：

1. `messages` 始终追加所有普通 `MessageRecord`，不会因为 compaction 丢弃旧历史。
2. `live_messages` 初始为空；没有 checkpoint 时与当前行为一致，最终等于完整 `messages`。
3. 遇到普通 `MessageRecord`：append 到 `messages`；同时 append 到当前 `live_messages`。
4. 遇到 `CompactionRecord`：完整 replay 继续保留 checkpoint 前已经读取的所有历史；仅将 `live_messages` 替换为 `replacement_history`。
5. checkpoint 后续普通消息继续 append 到 `messages` 和 `live_messages`。

这样 session 文件仍然 append-only，恢复后仍能看到完整历史；只有发给模型的 live context 从最新 checkpoint 开始，避免重新携带压缩前的旧历史。

旧 JSONL 没有 compaction record，继续按现有逻辑完整 replay，且 `live_messages == messages`。

---

## 11. 错误处理

1. 空 history 或只有 1 条用户消息时，`/compact` 应返回成功但不替换 history，并向 UI 提示没有足够历史可压缩。
2. LLM completion 失败时，至少重试 3 次；若初始尝试加 3 次重试后仍失败，不修改 context，不写 `CompactionRecord`。
3. 摘要为空时，视为失败，不替换 context。
4. 压缩请求超过上下文窗口时，第一版先失败并提示用户；不做递归裁剪。
5. 如果持久化 compaction record 失败，但内存替换已发生，应记录 warning。更稳妥的实现顺序是先 append 成功，再替换内存。

重试规则：

- 只重试摘要生成请求，不重试持久化写入。
- 每次重试使用同一份 history snapshot，避免重试期间新消息改变压缩输入。
- retry attempt 之间使用短退避，例如 200ms、500ms、1000ms。
- 所有尝试失败后向 UI 发出失败事件或错误文本，保留原 live history。

推荐顺序：

```text
generate summary with up to 3 retries
build replacement_history
persist CompactionRecord
replace live ContextManager history
emit completed event
```

---

## 12. 测试策略

### 12.1 context 单元测试

- `replace()` 会整体替换旧 history。
- `token_count()` 在 replacement 后按新 history 估算。
- 最近 assistant text 在 replacement 后读取新 history。

### 12.2 compaction 单元测试

使用 fake `Llm`：

- 手动 compaction 会用固定 prompt 调 `completion()`。
- 没有 previous summary 时使用 `Create a new anchored summary...`。
- 有 previous summary 时使用 `<previous-summary>`。
- replacement history 包含 summary marker 和 `compaction.retained_turns` 指定数量的最近 user turns。
- summary input 不包含 retained tail。
- LLM completion 前 3 次失败、第 4 次成功时，compaction 成功并替换 live history。
- LLM completion 初始尝试加 3 次重试全部失败时，不替换 live history，不写 `CompactionRecord`。

### 12.3 config 单元测试

- 未配置 `[compaction]` 时，`retained_turns` 默认值为 `2`。
- 配置 `retained_turns = 0` 时，history 选择不会保留 tail 原文。
- 配置 `retained_turns = 3` 时，history 选择保留最近 3 个 user turns。

### 12.4 store replay 测试

- replay 始终保留完整 `messages`，不会因为 `CompactionRecord` 删除旧历史。
- replay 遇到 `CompactionRecord` 时只替换 `live_messages`。
- compaction record 后的普通消息会同时 append 到 `messages` 和 `live_messages`。
- 旧格式 session 无 compaction record 时行为不变。

### 12.5 kernel/session 测试

- `Op::Compact` 在空闲 session 中触发 compaction。
- `Op::Compact` 不创建普通 user prompt message。
- compaction 成功后下一轮 `CompletionRequest.chat_history` 使用 replacement history。
- 正在 turn 中收到 `Op::Compact` 时记录 debug 并忽略，或返回 unavailable。第一版推荐忽略并提示 UI 后续重试。

### 12.6 slash command 测试

- `SlashCommand::Compact` 可以解析 `/compact`。
- `/compact` 不支持 inline args。
- TUI 提交 `/compact` 时发送 compact operation，而不是普通 prompt。

---

## 13. 实现边界

建议新增文件：

```text
crates/kernel/src/compaction.rs
```

职责：

- compaction prompt 常量。
- history split/retained tail 选择。
- summary marker 构造。
- `run_manual_compaction()` 主流程。

建议修改文件：

```text
crates/protocol/src/op.rs
crates/protocol/src/event.rs
crates/protocol/src/item.rs
crates/config/src/config.rs
crates/config/src/lib.rs
crates/kernel/src/context.rs
crates/kernel/src/session.rs
crates/kernel/src/lib.rs
crates/kernel/src/command/slash_command.rs
crates/store/src/record.rs
crates/store/src/replay.rs
crates/tui/src/app.rs
crates/acp/src/agent.rs
```

第一版可以不改 ACP schema，只通过现有 prompt path 支持 TUI `/compact`。如果要让 ACP 客户端也支持独立 compact API，则需要扩展 `AgentKernel` trait；这可以作为第二阶段。

---

## 14. 与 Codex / opencode 的取舍

### 14.1 采用 Codex 的点

- 压缩成功后替换 live history。
- 持久化 replacement history checkpoint。
- 下一轮请求不携带完整旧历史。
- 手动 `/compact` 是独立任务，不是普通用户消息。

### 14.2 采用 opencode 的点

- anchored summary 提示词。
- previous summary 更新策略。
- 固定 Markdown section。
- 通过配置保留最近少量 turn 原文，旧历史只摘要。

### 14.3 暂不采用的点

- Codex 的远端 `/responses/compact`。
- Codex 的自动 token window compaction。
- opencode 的 auto-continue。
- opencode 的插件 hook 自定义 compaction prompt。
- opencode 的 tool output pruning。

---

## 15. 开放问题

1. `/compact` 是否需要支持 root session 和 subagent session 都可用？
   - 推荐：都支持，因为每个 session 都有独立 `ContextManager`。
2. compaction 状态是否需要独立 UI cell？
   - 推荐：第一版实现 `TurnItem::ContextCompaction`，但 ACP 转换可以先返回 `None`，避免过早扩展外部协议。

---

## 16. 验收标准

1. 用户在 TUI 输入 `/compact` 后，会触发手动上下文压缩。
2. 压缩成功后，内存中的 context history 被替换为摘要和保留 tail。
3. 新的普通 prompt 不再携带完整压缩前历史。
4. session JSONL 中存在 compaction checkpoint。
5. load session 后，完整 transcript 仍被重放；live `ContextManager` 使用最新 checkpoint 的 replacement history 加后续消息。
6. 压缩提示词采用 anchored summary 结构，并保留固定 Markdown section。
7. `compaction.retained_turns` 可配置，默认值为 2。
8. 摘要生成失败时至少重试 3 次；全部失败后保留原 live history。
9. 相关单元测试和 kernel/session 测试通过。
