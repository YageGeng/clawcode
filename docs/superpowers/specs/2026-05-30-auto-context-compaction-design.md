# 自动上下文压缩设计

## 背景

当前 `/compact` 只支持手动触发。DeepSeek 等长上下文模型在工具循环中会反复携带 live history、系统提示和工具结果，累计 usage 很快升高；更重要的是，单次请求的 live history 可能接近 `context_tokens` 上限。自动压缩需要在发起模型请求前主动缩短 live history，并继续沿用现有 compaction checkpoint 持久化与恢复机制。

## 目标

- 支持用户通过配置开启自动上下文压缩。
- 支持配置触发比例，例如达到模型 `context_tokens` 的 `90%` 时自动触发。
- 自动压缩复用现有 `ContextManager::compact()` 和 checkpoint 持久化路径。
- 自动压缩失败不阻断当前 turn；失败只记录日志，本次请求继续执行。

## 非目标

- 不改变手动 `/compact` 的用户可见行为。
- 不按累计 session usage 触发自动压缩。
- 不引入精确 tokenizer；第一版继续使用现有 `ContextManager::token_count()` 粗估算，并额外计入当前 turn 的 preamble 粗估算。
- 不把自动压缩本身的 LLM usage 纳入当前 usage 统计。

## 配置

新增配置字段：

```toml
[compaction]
retained_turns = 2
auto = false
trigger_ratio = 0.9
```

- `retained_turns`：保留最近多少个普通用户 turn，沿用现有默认值 `2`。
- `auto`：是否开启自动压缩，默认 `false`，避免升级后无感知地产生额外 LLM 请求。
- `trigger_ratio`：触发比例，默认 `0.9`。有效范围为 `(0.0, 1.0]`；超出范围时自动压缩跳过并记录 warning。

## 触发规则

在每次 `execute_turn` 内真正构造并发送 `CompletionRequest` 前检查：

```text
estimated_request_tokens = context.token_count() + preamble.len() / 4
threshold = context_tokens * trigger_ratio
estimated_request_tokens >= threshold
```

其中 `context_tokens` 来自当前 provider/model 的配置。若当前模型未配置 `context_tokens`，自动压缩跳过。

## 执行流程

1. 用户消息先进入 live history 并持久化。
2. 每次工具循环发起 LLM 请求前，先 drain inter-agent 输入。
3. 若启用自动压缩且超过阈值，调用 `ContextManager::compact()`。
4. 若压缩返回 `Some(output)`：
   - 先持久化 `CompactionRecord`。
   - 持久化成功后用 `replacement_history` 替换 live history。
   - 若替换后估算仍超过阈值，本 turn 内暂停后续自动压缩，避免重复压缩同一批 retained tail。
5. 若压缩返回 `None` 或失败：
   - 记录 warning。
   - 本 turn 内暂停后续自动压缩。
   - 当前 LLM 请求继续执行。

## 测试要求

- 配置默认值：`auto=false`、`trigger_ratio=0.9`、`retained_turns=2`。
- TOML 能读取 `auto` 和 `trigger_ratio`。
- 自动压缩在超过阈值时发起，并在真实 LLM 请求前替换 live history。
- 自动压缩失败后当前 turn 继续发送原始请求。
- 未配置 `context_tokens` 或未开启 `auto` 时不触发。
