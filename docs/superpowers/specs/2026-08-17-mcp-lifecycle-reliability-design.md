# MCP 生命周期可靠性修复设计

## 目标

修复 MCP discovery 无界等待、Modern subscription 结束后失联、并发生命周期操作竞态、动态 Tool 禁用偏好丢失，以及 WebUI 切换或重连后无法恢复待处理 elicitation 的问题。

## 设计

### Server 生命周期

`McpServerRuntime` 是单个 Server 生命周期的唯一所有者。它增加一个异步 lifecycle 互斥，串行执行 reconnect、OAuth continuation 和 shutdown，避免多个 ACP 请求交错修改 client、watcher、catalog 与状态机。

初始 discovery 和动态 catalog refresh 通过 `McpServerRuntime` 的类型方法执行。该方法同时监听 lifecycle cancellation，并应用配置中的 `request_timeout`。初始超时进入 Discovery failure；动态刷新超时进入 Degraded，同时保留最后一次成功 catalog。shutdown 取消 lifecycle 后不再被挂起的 discovery 阻塞。

### Modern subscription

Modern subscription 不再用“槽位是否为 `Some`”代表运行状态。已结束任务必须释放运行标记，后续 discovery 才能重新建立订阅。`SubscriptionEnded` 触发一次有界完整刷新；刷新会重新订阅，失败则把 Server 标记为 Degraded。

### Tool 偏好

`disabled_preferences` 表示用户显式禁用过的名称，而不是“当前可见但未启用”的临时差集。更新 active Tool 时，仅修改当前可见名称对应的偏好，保留当前不可见 Tool 的历史偏好。MCP Tool 暂时消失并恢复后，仍遵循原有选择。

### Elicitation 恢复

Kernel 的 pending elicitation 项同时保留请求和 response sender，并提供 Session 级只读快照。ACP 增加产品命名空间下的 elicitation list 请求。WebUI 在 Session resume 和 WebSocket reconnect 后读取快照；实时通知即使属于非当前 Session，也按 Session 保存。界面只展示 active Session 的待处理请求，响应继续携带 SessionId。

Elicitation 仍是运行时状态，不写入 transcript JSONL；Session shutdown 或 Turn cancellation 继续以 Cancel 结束等待。

## 错误处理

- discovery 超时统一返回 `RequestTimeout`，并由调用阶段映射为 Discovery 或 Synchronization failure。
- lifecycle cancellation 返回 `RequestCancelled`，shutdown 不把预期取消记录为故障。
- 重复 OAuth continuation 在 lifecycle 锁内重新检查状态，只允许一个请求消费 pending authorization。
- 已结束 subscription 的重新建立失败时保留最后一次 catalog，并标记 Degraded。

## 测试

- MCP 集成测试覆盖初始 discovery 超时、动态 discovery 取消后 shutdown 可结束、subscription 结束后重新订阅、并发 OAuth continuation 串行化。
- Kernel 测试覆盖不可见 MCP Tool 的禁用偏好保持和 pending elicitation 快照。
- ACP 集成测试覆盖 elicitation list 的 Session 隔离与严格参数解析。
- WebUI 不新增单元测试；通过生产 app 后端、真实 provider 和真实 MCP Server 运行端到端回归。

