# 流式滚动与压缩上下文恢复实施计划

> **执行约束：** 当前项目禁止 SubAgent、worktree 和未经授权的 commit。本计划由当前会话直接执行；前端不增加单元测试，Rust 测试只放在 crate 的 `tests/` 目录。

**目标：** 让流式输出在用户位于底部时稳定跟随、向上滚动时保持位置，并让压缩后的模型上下文恢复精确 `tokensBefore`。

**架构：** WebUI 使用独立 Hook 管理 transcript 视口状态，Conversation 只传入内容修订并绑定事件。Protocol 类型负责压缩消息的模型文本投影，Provider 仅消费该投影，使实时运行与回放保持一致。

**技术栈：** React 19、TypeScript、CSS、Rust、Kernel provider adapter、ACP WebSocket。

## 全局约束

- 不修改持久化摘要正文和 WebUI 压缩卡片的数据结构。
- 不增加前端单元测试。
- 新增函数和方法写英文作用注释；修改旧逻辑写英文原因注释。
- 不创建 commit。

---

### 任务 1：压缩 token 模型投影

**文件：**

- 修改：`crates/kernel/tests/provider_runtime.rs`
- 修改：`crates/protocol/src/message.rs`
- 修改：`crates/kernel/src/provider.rs`

**接口：**

- `CompactionSummaryMessage::model_text(&self) -> String`

- [x] 修改 provider 集成测试的期望文本，要求包含精确 `42000 tokens`。
- [x] 运行目标测试，确认旧投影缺失 token 数而失败。
- [x] 在协议类型上实现模型文本投影，并让 Provider 调用该方法。
- [x] 重跑目标测试并确认通过。

### 任务 2：流式滚动状态机

**文件：**

- 新增：`web/src/features/conversation/useTranscriptScroll.ts`
- 修改：`web/src/features/conversation/Conversation.tsx`
- 修改：`web/src/theme/workbench.css`

**接口：**

- `useTranscriptScroll(revision) -> { transcriptRef, onScroll }`，Hook 内部捕获原生 `toggle` 事件。
- `TranscriptContentRevision` 包含会触发 transcript 高度变化的实体集合与状态。

- [x] 实现 32 像素底部阈值、切换会话重置和 `useLayoutEffect` 即时跟随。
- [x] 在 Conversation 绑定滚动与详情切换事件，不再无条件滚到底部。
- [x] 移除平滑滚动并增加稳定滚动条槽位。
- [x] 运行 Web 类型检查、ESLint 和生产构建。

### 任务 3：完整验证与真实 E2E

- [x] 运行 `cargo fmt --all -- --check`。
- [x] 运行 provider 目标集成测试。
- [x] 运行 `cargo test --workspace`。
- [x] 运行 `cargo clippy --workspace --all-targets --all-features -- -D warnings`。
- [x] 启动正式 app 后端和 WebUI，使用真实配置与 Provider 产生足够长的流式响应。
- [x] 在流式期间向上滚动，确认位置不被拉回；自行滚到底部，确认恢复跟随且无抖动。
- [x] 展开和折叠 Thinking、工具详情，确认状态与滚动位置稳定。
- [x] 触发或恢复压缩后继续提问，确认模型可读取精确 `tokensBefore`，并核对存储与 WebUI 仍只保存、展示结构化字段。
