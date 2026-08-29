# Shell 工具流式输出实现计划

> **执行方式：** 在当前会话内逐项执行并复核；项目禁止 SubAgent 和 worktree，且本次未获 commit 授权。

**目标：** 让 `exec_command` 与 `write_stdin` 在执行期间持续更新工具卡片，并在有界滚动框中实时展示 Shell 输出。

**架构：** 终端管理层在单次交互期间生成不消费 Agent 游标的输出快照，由工具适配层转换成现有的可替换 `ToolResult` 更新。ACP 继续复用 `tool_call_update`，WebUI 从每次快照提取 Shell 输出并覆盖当前显示，最终结果负责收敛状态而不重复拼接内容。

**技术栈：** Rust、Tokio、ACP、React、TypeScript、CSS。

**设计依据：** 当前会话中已确认的调用级流式快照方案。

## 全局约束

- 所有新增函数和非平凡逻辑使用英文注释。
- Rust 行为变更必须先写失败测试；前端不要求单元测试。
- 不增加全局终端原始输出通知，避免高频输出污染进程快照事件。
- 输出保留现有容量和 token 上限；最终 Agent 输出只消费一次。
- WebUI E2E 必须使用生产 `app` 后端及其配置的真实 provider。
- 不创建 commit，不使用 SubAgent，不使用 worktree。

---

### 任务一：终端交互快照

**文件：**
- 修改：`crates/tools/src/terminal/mod.rs`
- 修改：`crates/tools/src/terminal/buffer.rs`
- 修改：`crates/tools/src/terminal/process.rs`
- 修改：`crates/tools/src/terminal/manager.rs`
- 测试：`crates/tools/tests/terminal_tools.rs`

**接口：**
- 输入：一次 `spawn` 或 `interact` 的调用级输出观察器。
- 输出：有界、非消费式的当前未读输出快照；最终 `drain_output` 行为保持不变。

- [x] 增加集成测试，断言命令结束前可收到至少一个输出快照，且最终输出完整、不重复。
- [x] 运行定向测试并确认因缺少观察器接口而失败。
- [x] 为终端请求增加默认可丢弃的观察器，并实现非消费式输出快照。
- [x] 在 `wait_and_collect` 中以合并后的进程变更发布快照。
- [x] 运行定向测试并确认通过。

### 任务二：Shell 工具增量更新

**文件：**
- 修改：`crates/tools/src/builtin/exec_command.rs`
- 修改：`crates/tools/src/builtin/write_stdin.rs`
- 测试：`crates/tools/tests/terminal_tools.rs`

**接口：**
- 输入：终端交互输出快照。
- 输出：与最终 JSON 结果兼容的 `ToolResult` 局部快照，通过现有 `ToolUpdateSink` 发布。

- [x] 扩展失败测试，断言 `exec_command` 和 `write_stdin` 的更新按工具调用关联。
- [x] 运行测试并确认更新尚未发布。
- [x] 为两个工具增加调用级观察器适配，保持错误、session id 和最终状态格式不变。
- [x] 运行工具与 kernel/ACP 定向测试，确认更新可到达 `tool_call_update`。

### 任务三：Shell 专用输出卡片

**文件：**
- 修改：`web/src/domain/model.ts`
- 修改：`web/src/workspace/updateDecoder.ts`
- 修改：`web/src/features/conversation/ToolCallCard.tsx`
- 修改：`web/src/theme/workbench.css`

**接口：**
- 输入：`exec_command`/`write_stdin` 的可替换 ACP 内容快照。
- 输出：固定最大高度、双轴可滚动、保留终端空白格式的实时输出框。

- [x] 复核 decoder 的内容覆盖语义，并在 Shell 输出组件中提取 JSON 的 `output` 字段。
- [x] 增加 Shell 输出组件：位于底部时自动跟随，用户向上滚动后暂停，回到底部后恢复。
- [x] 使用语义化 `role="log"`、可见焦点和主题 token 完成样式。
- [x] 运行 TypeScript 检查和生产构建。

### 任务四：回归与真实端到端验证

**文件：**
- 复核：本计划涉及的所有文件及当前工作区已有 PTY 改动。

- [x] 运行 Rust 定向测试、全量测试、格式检查和 Clippy。
- [x] 运行 WebUI 类型检查与生产构建。
- [x] 启动生产 `app` 和真实 WebUI，使用真实 provider 执行分段输出命令。
- [x] 验证命令结束前卡片出现中间输出、滚动跟随可暂停/恢复、最终输出无重复。
- [x] 检查工作区 diff，确认没有无关修改且不创建 commit。
