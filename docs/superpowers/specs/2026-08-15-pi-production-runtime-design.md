# pi 生产运行语义对齐设计

## 1. 目标

本阶段将 clawcode 当前可运行的 Agent 主流程提升为可用于真实 Provider 的生产运行时。功能来源严格限定为本地 pi 0.84.1 已存在的行为；不因为一般性的“生产化”概念额外引入 pi 没有的机制。

本阶段完成后，真实 Provider 的一次 Agent 运行应具备以下语义：

- 完整表示并持久化 Provider、模型、停止原因、错误和 token usage。
- 支持 Provider 请求级重试和 Agent 响应级自动重试。
- 根据模型 context window 和真实 usage 自动触发 Compact。
- 对 context overflow 执行最多一次 Compact 后恢复运行。
- 在 Prompt 前验证模型和认证状态。
- 生成与可用工具、Skills、工作目录和项目指令一致的系统提示词。
- Cancel 可以中断请求、流、退避等待和 Compact。
- 只有 retry、Compact 和队列延续全部结束后，Agent 才进入 settled/Idle。

后续 B 阶段负责文件读写、编辑和 bash 工具，并单独参考 pi 的 `read.ts`、`write.ts`、`edit.ts`、`bash.ts` 设计。本阶段不提前实现这些工具，但系统提示词和 Kernel 接口需要允许 B 阶段直接注册工具而不修改主流程。

## 2. 行为基准

本设计以以下 pi 0.84.1 实现为主要行为来源：

- `packages/ai/src/utils/retry.ts`：错误分类、有限重试和可取消退避。
- `packages/ai/src/utils/provider-retry.ts`：Provider 请求级重试和服务端 retry delay 上限。
- `packages/agent/src/agent-loop.ts`：消息流、Turn、工具执行和 Agent 结束条件。
- `packages/coding-agent/src/core/agent-session.ts`：Agent 级自动重试、自动 Compact、取消和 settled。
- `packages/coding-agent/src/core/compaction/compaction.ts`：context usage 估算、阈值和压缩边界。
- `packages/coding-agent/src/core/system-prompt.ts`：根据工具、Skills、工作目录和项目上下文生成系统提示词。
- `packages/coding-agent/src/core/resource-loader.ts`：项目指令文件发现顺序。
- `packages/coding-agent/src/core/settings-manager.ts`：retry 和 compaction 默认配置。

TypeScript 的类结构不作为移植目标。行为通过 clawcode 已确认的 Rust crate、Factory 和类型边界实现。

## 3. 范围

### 3.1 本阶段包含

- Provider Final 结果和异常断流的统一表示。
- Assistant 完整消息的模型元数据和 usage。
- 两层重试及对应事件。
- 自动 Compact、overflow 恢复和摘要请求重试。
- pi 风格系统提示词和项目指令发现。
- active model、模型元数据、认证和配置交叉校验。
- ACP v2 原生 usage/state 映射及产品扩展事件。
- WebUI 中 usage、重试、自动 Compact、模型错误和 settled 展示。
- 确定性 Rust 测试、浏览器验收和可选真实 Provider 冒烟流程。

### 3.2 本阶段不包含

- 备用 Provider 或模型自动切换。
- 熔断器、主动健康探测或后台探活。
- TOML 热更新和配置编辑。
- 运行时模型切换或模型选择 UI。
- TUI、登录 UI 或认证文件编辑。
- 新的 UI extension 接入点。
- 客户端文件系统或 terminal callback。
- read、write、edit、bash 等 B 阶段工具。
- pi 中不存在的重试算法或恢复策略。

## 4. 架构

### 4.1 依赖方向

生产运行链路保持以下单向边界：

```text
config/provider
      ↓
ApplicationFactory
      ↓
KernelFactory → Kernel → Store
      ↓             ↓
     ACP ← protocol events/messages
      ↓
    WebUI
```

- `config` 只负责不可变 TOML 类型、加载和交叉字段校验。
- `provider` 继续负责 API、认证客户端、请求级 retry 和 Provider Final 信息。
- `protocol` 定义跨 crate 的 Assistant 结果、usage、重试和压缩事件。
- `kernel` 负责系统提示词、Agent 级 retry、自动 Compact、队列和 settled。
- `store` 只持久化已经形成的领域消息、事件关联信息和 pi v4 entries。
- `acp` 只做领域类型到 ACP v2 的映射，不重新实现运行策略。
- `web` 只消费 ACP 状态，不直接读取配置、Store 或 Provider。

### 4.2 Factory 接入

`ApplicationFactory` 从同一个不可变 `ConfigHandle` 构造以下依赖：

- `ProviderModelFactory`：解析 active model，并产生带稳定 `ModelProfile` 的 Model。
- `SystemPromptFactory`：按 Session cwd 和当前能力生成系统提示词。
- `RetryPolicy`：Agent 级重试策略。
- `ProviderRetryPolicy`：Provider 请求级重试策略。
- `CompactionPolicy`：自动和手动 Compact 共用策略。

Kernel 不读取 TOML，不查找 API key，也不判断 Provider 类型。Provider 不操作 Session、Store、ACP 或 WebUI。

## 5. 公共类型

### 5.1 ModelProfile

active model 在 Application 构造时转换为稳定的 `ModelProfile`：

```text
provider_id
model_id
display_name
context_tokens
max_output_tokens
```

`context_tokens` 和 `max_output_tokens` 必须大于零。Kernel 使用该类型进行 Compact 判断和事件输出，不再从字符串 `provider/model` 反复拆分。

### 5.2 Usage

领域 `Usage` 包含：

```text
input_tokens
output_tokens
cache_read_tokens
cache_write_tokens
reasoning_tokens (optional)
total_tokens
```

Rust 内部使用 `u64`。ACP 原生 `UsageUpdate` 按标准字段发送 context used/window；完整 breakdown 放入 namespaced metadata 时使用十进制字符串，前端保留字符串原值。

### 5.3 AssistantMessage

Assistant 使用独立消息内容类型，不在 User/System 消息上增加无效的模型字段。完整 Assistant 结果包含：

```text
blocks
provider_id
model_id
stop_reason
raw_stop_reason (optional)
usage
error (optional)
```

标准化 `stop_reason` 至少覆盖 `stop`、`length`、`tool_use`、`error` 和 `cancelled`。错误文本和 Provider 私有 raw response 分离；raw response 不进入公共协议或 Store。

所有完整消息继续携带 MessageId、TurnId 和字符串毫秒时间。Assistant 的创建时间、首 token 时间和结束时间保持完整；没有收到 token 时，首 token 时间按现有完整消息约束使用开始时间。

### 5.4 Model 流

Provider adapter 产生明确的模型流终态：

- 文本 delta。
- 推理 delta。
- 完整工具调用。
- Final outcome。
- Stream failure。

正常 EOF 不能替代 Final。缺失 Final、损坏的工具参数和异常断流必须转换为完整 Error Assistant 消息，不能静默产生 `EndTurn`。

## 6. 配置

### 6.1 Retry

新增 pi 对应的不可变配置：

```toml
[retry]
enabled = true
max_retries = 3
base_delay_ms = 2000

[retry.provider]
# timeout_ms = 120000
# max_retries = 0
max_retry_delay_ms = 60000
```

- `max_retries` 表示初始调用之外的重试次数。
- `base_delay_ms` 使用指数退避：2s、4s、8s。
- Provider `max_retries` 未配置时，共享请求重试包装层按零次额外重试处理；已有专用 Provider 客户端保留其明确声明的请求默认值。
- retry delay 必须可取消。
- 零次重试是合法配置；非法溢出值和无法表示的 Duration 在配置边界拒绝。

### 6.2 Compaction

Compaction 配置改为 pi 对应语义：

```toml
[compaction]
enabled = true
reserve_tokens = 16384
keep_recent_tokens = 20000
```

当前 `auto`、`trigger_ratio` 和 `retained_turns` 不再作为生产策略，也不保留兼容解析。手动 Compact 与自动 Compact 使用同一个 `keep_recent_tokens` 选择保留尾部。

### 6.3 交叉字段校验

配置加载时必须拒绝：

- active model 不是 `provider/model`。
- active provider 不存在。
- active model 不在 provider 的 models 中。
- 重复的 provider id 或同一 provider 下重复 model id。
- active model 缺少或配置了零值 context/max output tokens。
- retry/compaction 数值超出运行时可表示范围。
- `reserve_tokens >= context_tokens`。
- 认证配置缺失、环境变量无法解析或认证文件不可读取。

认证错误不得包含 API key、Authorization header 或认证文件正文。

## 7. 系统提示词与项目上下文

### 7.1 SystemPromptFactory

`SystemPromptFactory` 接收一个类型化上下文：

- `ProductIdentity` 提供的产品名称。
- Session 绝对 cwd。
- 当前实际注册的工具定义和简述。
- 有效 Skills 描述。
- 已发现的项目指令文件。

Factory 返回一个 System `AgentMessage`，由 Kernel 在每次模型请求前作为上下文首项使用。该消息使用当前模型调用的 TurnId 和字符串毫秒时间。系统提示词不写入用户可见对话，但其版本和来源可以记录在 operation details 中用于诊断。

### 7.2 工具与 Skills

提示词只声明实际存在的工具。编码工具注册 read/write/edit/bash 后，提示词根据 ToolRegistry 自动变化，不声明已删除的过渡工具。

Skills 只在模型能够读取 Skill 文件时加入自动调用说明。A 阶段没有 read 工具，因此仍保留 ACP 显式 Skill 调用，但不诱导模型自行读取文件。B 阶段启用 read 后自动加入 pi 风格 Skill 列表。

### 7.3 项目指令发现

参考 pi，从 cwd 向祖先目录发现以下候选：

- `AGENTS.override.md`
- `AGENTS.md`
- `AGENTS.MD`
- `CLAUDE.md`
- `CLAUDE.MD`

同一目录按 pi 的覆盖规则选择，不重复注入同一逻辑作用域。还可读取产品配置目录中的全局项目指令。文件读取发生在服务端资源发现阶段，不使用 ACP 客户端 FS callback，也不提供热更新。

## 8. Provider 请求级重试

Provider 请求级 retry 只包装“建立请求并取得响应流”的阶段。它处理 pi 已覆盖的瞬时状态：408/409/429、5xx、连接失败和 Provider 明确要求重试的响应。

行为规则：

- 遵守 `Retry-After` 和 `Retry-After-Ms`。
- 服务端要求的等待超过 `max_retry_delay_ms` 时立即失败，并交给 Agent 级 retry。
- 每次等待和请求都响应同一个取消信号。
- 认证失败、参数错误、quota/billing exhaustion 不重试。
- 一旦流已交给 Kernel，该层不负责重放已产生的 delta。

## 9. Agent 响应级重试

### 9.1 分类

Kernel 使用独立 `RetryClassifier`，分类规则与 pi 的 `isRetryableAssistantError` 对齐。可重试类别包括 overloaded、rate limit、429、500/502/503/504/524、网络/连接中断、timeout、WebSocket 异常关闭和提前结束的 Provider 流。

quota、billing、无效认证、请求参数错误、用户取消和 context overflow 不进入普通 retry。

### 9.2 执行语义

一次初始模型调用和其自动重试共享 RunId，但每次模型调用拥有新的 TurnId。

每个失败尝试都按以下顺序结算：

1. 完成 Assistant Error 消息时间。
2. 持久化完整失败消息。
3. 发出 TurnEnd 和 RetryScheduled。
4. 从下一次模型请求的活动上下文排除该失败 Assistant。
5. 执行可取消指数退避。
6. 发出 RetryStart，并创建新的 Turn。

失败消息只从活动模型上下文排除，不从 pi v4 会话树删除。resume/replay 后仍能查看失败尝试。

重试成功时发出 `RetryEnd { success: true }` 并重置计数；重试耗尽时发出最终失败事件。Backoff 期间 Session 仍是 Running，follow-up 可以入队。Cancel 中断退避并使当前 Run 以 cancelled 结算，队列保持不变。

## 10. 自动 Compact

### 10.1 Usage 估算

Context token 计算优先使用最近一个非 error、非 cancelled 且非零的 Assistant usage。该消息之后的 User、Assistant 和 ToolResult 使用与 pi 一致的保守字符估算补充。

最近 compaction 边界之前的 usage 不参与判断，避免刚压缩后立即再次触发。

### 10.2 阈值触发

触发条件为：

```text
context_tokens > context_window - reserve_tokens
```

普通阈值在成功 Assistant 结算后触发 Compact，但不重新运行已经成功的响应。Prompt 提交前也检查最近一次 aborted/旧响应，避免新 Prompt 进入已接近上限的上下文。

### 10.3 Overflow 恢复

以下情况进入 overflow 恢复而不是普通 retry：

- Provider 明确返回 context overflow。
- length stop 发生在模型原始期望输出上限之前，符合 pi 的可恢复条件。

Kernel 从活动上下文移除失败或截断的 Assistant，执行 Compact，然后最多恢复一次模型调用。同一个 Run 不能无限 compact-and-retry；第二次 overflow 直接失败并给出明确错误。

### 10.4 Compact 事务

自动和手动 Compact 共用现有事务语义：

- 摘要请求使用同一个 retry policy。
- Cancel 可以中断摘要流和 retry delay。
- 失败不移动 leaf，不替换内存上下文。
- 成功继续写入 pi v4 `summary`、`retainedTail`、`tokensBefore` 和 `details`。
- `details` 保存 TurnId、字符串开始/结束时间和 threshold/overflow/manual 原因。

## 11. Prompt Preflight、Settled 与队列

### 11.1 Prompt Preflight

每次开始新的用户 Run 前，Kernel 通过 Model 的类型化 preflight 接口确认：

- active `ModelProfile` 仍与已构造 Model 一致。
- Provider 所需认证可以解析；短期凭证由 Provider 自身刷新。
- Session 当前没有另一个活动请求、retry delay 或 Compact。

Preflight 不发送模型请求，不消耗 token。失败时不持久化用户消息，也不创建一个无法运行的 Run。运行已经开始后的认证或网络失败按完整 Assistant Error 消息结算。

### 11.2 Settled 与队列

`TurnEnd` 不等价于 ACP Idle。一次用户 Run 的 retry、自动 Compact 和自动恢复共享 RunId；`RunEnd` 只在这条自动链路结束时发送。Kernel 随后只有在以下条件全部满足后才发出 `AgentSettled`：

- 没有活动 Provider 请求或流。
- 没有 retry delay。
- 没有自动或手动 Compact。
- 没有当前 Run 应继续自动消费的 steering/follow-up。

Retry 和 Compact 期间保持同一个运行租约，防止第二个 Prompt 绕过串行锁。运行期间提交的新消息仍按上一阶段规则进入持久化队列。Cancel 只取消当前自动链路，不清空队列；取消后保留但不再由本次 Run 自动消费的队列不阻止 `AgentSettled`。

## 12. ACP v2 映射

### 12.1 原生映射

- Assistant text/reasoning 使用原生 AgentMessage/AgentThought 更新。
- ToolCall 使用原生 ToolCallUpdate。
- Session 运行状态使用原生 StateUpdate。
- Context used/window 使用原生 `UsageUpdate`。
- 最终 Idle 使用 ACP 标准 StopReason 映射 stop、max tokens、cancelled 和 error。

### 12.2 扩展映射

ACP 没有完整原生对应的数据继续使用集中定义的产品扩展更新：

- RetryScheduled。
- RetryStart。
- RetryEnd。
- CompactionStart/End 及触发原因。
- AgentSettled diagnostics。

扩展 payload 和所有 namespaced metadata 继续携带 TurnId、字符串毫秒时间戳和 sequence。Provider/model、完整 usage breakdown 和 raw stop reason 附着于完整 Assistant 更新的 metadata；不得把 Provider 私有 raw response 发送给客户端。

Replay 恢复完整消息和 usage，但不重放已经完成的 retry 倒计时。ACP 映射层不自行判断是否 retry 或 Compact。

## 13. WebUI

现有亮色工作台增加以下只读状态：

- Header 显示 context used/window。
- Assistant diagnostics 显示 Provider、model、stop reason、usage、TurnId 和三段字符串时间。
- Retry 状态显示 attempt/max、错误摘要和剩余等待；剩余时间由 RetryScheduled 的开始时间和 delay 在浏览器本地计算，不要求服务端发送计时 tick。Cancel 可立即中断。
- 自动 Compact 显示 threshold/overflow 原因、tokensBefore 和成功/失败结果。
- Provider 错误使用独立错误卡片，不伪装为普通 Assistant 正文或工具错误。
- Retry、Compact 和队列延续期间保持 Running；仅 AgentSettled 后显示 Idle。

不增加模型选择、配置编辑、认证表单、Provider fallback 或 UI extension 接入点。前端不增加单元测试。

## 14. 错误处理

错误类型至少区分：

- Config validation。
- Model unavailable/auth unavailable。
- Provider request failure。
- Provider stream failure。
- Protocol conversion failure。
- Retry exhausted/cancelled。
- Context overflow recovery exhausted。
- Compaction failure/cancelled。
- Store persistence failure。

Store 失败优先于继续运行：如果失败 Assistant、retry operation 或 compaction entry 无法持久化，Kernel 不开始下一次自动请求。错误显示可以包含 Provider id、model id、HTTP 状态和安全摘要，但必须清除 API key、Authorization、Cookie 和认证文件正文。

## 15. 验证策略

### 15.1 Rust TDD

使用 deterministic Model、Provider、Clock、IdGenerator 和 Tokio 虚拟时间覆盖：

- Provider Final usage 和 stop reason 映射。
- 缺失 Final、异常断流和损坏工具参数。
- 可重试/不可重试错误分类。
- 2/4/8 秒退避、次数上限和 Cancel。
- 失败 Assistant 持久化、活动上下文排除和 replay 恢复。
- usage 估算、threshold Compact 和压缩边界后旧 usage 排除。
- overflow 最多一次 Compact + retry。
- Compact 摘要请求重试和失败原子性。
- active model、模型元数据、认证和配置数值校验。
- SystemPromptFactory 的工具、Skill 和项目指令条件组合。
- ACP 原生 UsageUpdate、StateUpdate 和扩展 retry/compact 映射。

默认测试不读取真实 API key，不访问付费 Provider。

### 15.2 前端与浏览器

- `npm run check`。
- `npm run build`。
- fixture 验收 retry、usage、threshold Compact、overflow 恢复、Cancel 和 replay。
- 真实浏览器检查控制台、消息去重、Running/Idle 时序和窄窗口布局。
- 前端不采用 TDD，不创建前端单元测试。

### 15.3 真实 Provider 冒烟

提供显式、可选的真实 Provider 冒烟流程：使用用户已有 TOML 和环境变量提交一个最小 Prompt，确认模型认证、流式文本、usage、stop reason、Store 和 ACP/WebUI 全链路。该流程不属于默认测试，不自动执行，也不打印认证值。

### 15.4 最终质量门槛

```sh
cd web && npm run check && npm run build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
git diff --check
```

## 16. 成功标准

- 使用真实 active model 可以从 WebUI 或 stdio 完成流式 Agent 运行。
- Assistant 完整消息和 replay 包含稳定 stop reason、usage、Provider/model、TurnId 和字符串时间。
- 瞬时错误按 pi 规则有限重试，Cancel 能中断退避。
- Context threshold 和 overflow 按 pi 规则触发 Compact，且不会无限恢复。
- 失败尝试保留在 Store，但不会污染后续模型上下文。
- ACP Running/Idle 与实际 settled 状态一致。
- 系统提示词只声明实际能力，后续 B 阶段可以通过 ToolFactory 接入 read/write/edit/bash。
- 不包含本设计非目标中的额外生产机制。
