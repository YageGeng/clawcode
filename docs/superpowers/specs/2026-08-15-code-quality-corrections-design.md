# Clawcode 代码质量修复设计

## 目标

修复代码审查确认的状态一致性、运行收尾、会话隔离、扩展组合、超时和输出完整性问题，并把本轮涉及的大文件按业务语义拆分。保持 `provider` 与 `config` 的既有职责，不恢复客户端文件系统或终端回调能力，不增加 Pi v4 之外的能力。

## 设计

### Store 原子状态归约

JSONL 写入使用“候选记录 -> 持久化并同步 -> 内存归约”的顺序。序列号只从当前状态计算，不提前修改状态。实时写入和重放共用 `StoreState::apply`，保证同一条记录在两条路径上产生相同状态。写入或同步失败时，内存状态保持不变，重试不会产生序列号缺口。

### Kernel 运行结果与统一收尾

运行主流程拆成执行阶段和收尾阶段。执行阶段返回类型化 `RunOutcome`，区分成功、用户取消和失败；外层无论预检、provider、工具或压缩在哪一步失败，都统一发出并持久化 `OperationFinished`、`RunEnd`、`AgentEnd`、`AgentSettled`。错误仍返回调用方，但不会留下半完成生命周期。

### WebUI 会话隔离

工作区按 `SessionId` 保存独立会话状态。通知先根据 `notification.sessionId` 路由，再更新对应会话；视图只读取当前活动会话。异步打开会话使用递增请求代次，旧请求完成后不能覆盖较新的选择。

### Extension 效果组合与接入点

扩展管线返回 `ExtensionEffects`，分别累积注入消息、元数据和控制指令，不再用“最后一个非 Continue 覆盖前者”的语义。Kernel 在模型选择、思考级别选择、工具执行更新、会话信息变化和会话切换边界分发对应事件。`UserBash` 仅在存在用户 Bash 入口时接入；当前协议没有该入口，因此保留类型但不制造额外能力。

### MCP、Bash 与 ACP 结果类型

MCP 工具从运行配置携带 `tool_timeout_sec`，调用同时监听取消和 `tokio::time::timeout`。Bash 正常退出后等待 stdout/stderr reader 到 EOF；只在子进程退出但继承管道长期不关闭时使用有界宽限。协议把布尔成功值替换为 `AgentOutcome::{Succeeded, Cancelled, Failed}`，ACP 仅把明确取消映射为 `Cancelled`，失败映射为错误结束。

### 模块边界

本轮只拆分实际修改且职责混杂的文件：`store` 拆为模型、状态归约和 JSONL 实现；`kernel/runtime` 拆出运行收尾；`extension` 拆为事件、契约和管线；WebUI controller 拆出通知解码和会话命令。其余大文件不做无行为收益的机械迁移。

## 错误处理与验证

后端每项先增加可观察行为的失败测试，再实现最小修复。前端按项目规则不增加单元测试，但运行 TypeScript、ESLint、构建和真实 WebUI 多会话端到端测试。最终运行 Rust 格式化检查、全 workspace 测试与 Clippy。
