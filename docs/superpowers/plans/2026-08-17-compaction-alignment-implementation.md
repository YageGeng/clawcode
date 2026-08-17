# 上下文压缩与 pi v4 对齐实施计划

> **执行要求：** 在当前工作区按任务顺序执行；Rust 行为变更必须使用 TDD。项目规则禁止 SubAgent 和 worktree，因此不使用计划模板建议的并行执行方式。

**目标：** 将 clawcode 的压缩提示、领域类型、持久化、恢复、ACP 回放和 WebUI 状态完整对齐已确认的 pi v4 设计。

**架构：** `protocol` 定义压缩摘要、终态和回放公共类型；`kernel` 从 Session 分支准备压缩输入并在 provider 边界转换摘要；`store` 持久化 pi v4 风格 payload 与 operation records；`acp` 投影实时和回放事件；WebUI 根据同一压缩 identity 渲染临时状态与持久化卡片。原始历史不删除，最新 compaction entry 是模型上下文边界。

**技术栈：** Rust、Tokio、Serde、ACP WebSocket、React、TypeScript、Vite。

## 全局约束

- 不修改 `provider` 和 `config` crate 的公共职责。
- 所有 Rust 测试只能放在 `tests/` 集成测试目录或精确的 `#[cfg(test)] mod tests` 中；正文不能包含测试支撑代码。
- 前端不增加单元测试，最终使用正式 `app` 后端和真实 provider 做 WebUI 端到端验证。
- 每条消息继续携带字符串 `turn_id` 和字符串毫秒时间戳，避免数值精度损失。
- 新增函数必须有英文函数级注释；非直观修改必须说明原因。
- 避免无意义的短 helper；单个自定义类型参数的转换优先实现 `From`、`TryFrom` 或该类型的方法。
- 超过三个字段的结构体使用 `typed-builder`，`Option` 字段使用 `builder(default)`。
- 不创建提交，除非用户另行明确要求。

---

### Task 1：用集成测试锁定压缩请求和空上下文行为

**文件：**
- 修改：`crates/kernel/tests/compaction.rs`
- 修改：`crates/kernel/src/runtime/compaction.rs`

**接口：**
- 消费：`Kernel::compact_session`、现有测试 provider、Session tree 查询接口。
- 产出：`CompactionPreparation` 生成 pi 风格请求；空模型上下文返回稳定错误且没有副作用。

- [ ] **Step 1：编写空上下文失败测试**

  构造只包含 Slash Command 消息的 Session，调用 `compact_session`，断言返回“没有可压缩的上下文”、测试 provider 调用次数为零、lane leaf 未改变且没有 compaction entry。

- [ ] **Step 2：运行定向测试并确认 RED**

  运行 `rtk cargo test -p clawcode-kernel --test compaction compact_rejects_session_without_model_context -- --exact --nocapture`，预期旧实现错误地产生 provider 请求或 compaction entry。

- [ ] **Step 3：实现模型可见消息校验**

  让 `CompactionPreparation` 基于模型可见消息和上一份摘要判断是否可压缩；Slash Command 调用和结果不计入 token，也不进入摘要请求。校验在发出运行事件和写入 operation record 之前完成。

- [ ] **Step 4：编写首次摘要请求契约测试并确认 RED**

  在测试 provider 中记录真实 `ModelRequest`，断言第一条消息是专用 System 指令，第二条是包含 `<conversation>` 和六个固定摘要章节的 User 消息；自定义参数表现为 `Additional focus:`，不替换基础协议。

- [ ] **Step 5：实现首次压缩提示并确认 GREEN**

  在 `compaction.rs` 中用常量定义 system prompt、首次摘要模板和 conversation 包装，禁用 tools，并按 `min(reserve_tokens * 0.8, model.max_output_tokens)` 设置摘要最大输出。运行整个 `crates/kernel/tests/compaction.rs`。

### Task 2：建立压缩摘要和终态的公共协议类型

**文件：**
- 修改：`crates/protocol/src/message.rs`
- 修改：`crates/protocol/src/capability.rs`
- 修改：`crates/protocol/src/event.rs`
- 修改：`crates/protocol/src/lib.rs`
- 修改：`crates/protocol/tests/events.rs`
- 修改：所有穷举匹配 `MessageContent` 的 Rust 调用点。

**接口：**
- 产出：`CompactionSummaryContent`；`MessageContent::CompactionSummary`；`CompactionOutcome::{Completed, Failed, Cancelled}`；包含 summary、tokens、usage 和 timing 的 `CompactionResult`。

- [ ] **Step 1：编写协议序列化失败测试**

  使用手写 JSON 期望值验证压缩摘要消息和三种 `CompactionEnd` 终态的 tagged serialization，确保 `turnId`、`startedAtMs`、`endedAtMs` 均为字符串。

- [ ] **Step 2：运行协议测试并确认 RED**

  运行 `rtk cargo test -p clawcode-protocol --test events -- --nocapture`，预期因新 variant/type 尚不存在而编译失败。

- [ ] **Step 3：实现类型并修复穷举映射**

  将共享结构放入 `protocol`，不在 kernel、ACP、store 重复定义；所有转换使用类型方法或标准转换 trait。完成后运行 protocol 全部测试。

### Task 3：持久化 pi v4 风格 compaction entry 和 operation records

**文件：**
- 修改：`crates/store/src/model.rs`
- 修改：`crates/store/src/session.rs`
- 修改：`crates/kernel/src/runtime/compaction.rs`
- 修改：`crates/kernel/tests/compaction.rs`

**接口：**
- 消费：Task 2 的 `CompactionResult` 和 usage 类型。
- 产出：`CompactionData { summary, retained_tail, tokens_before, usage, details }`；类型化 compaction intent/attempt/usage/finished records。

- [ ] **Step 1：编写成功持久化失败测试**

  执行一次真实 kernel 压缩，读取 branch records，按字面值断言 compaction payload、`readFiles`、`modifiedFiles`、usage cause、source leaf、result entry id、attempt 和 completed outcome。

- [ ] **Step 2：运行测试并确认 RED**

  运行对应单测名，预期缺少 usage/details 或 operation payload 不匹配。

- [ ] **Step 3：实现原子成功路径**

  摘要成功后先准备完整 payload，再追加 compaction entry 并移动 leaf；provider、extension、取消和持久化失败都只追加诊断 operation records，不移动 leaf。

- [ ] **Step 4：增加失败与取消测试并完成 GREEN**

  分别让 provider 返回错误和取消信号，断言 leaf 不变、没有 compaction entry，并收到 `Failed` 或 `Cancelled` 的唯一终态事件。运行 kernel compaction 测试全集。

### Task 4：恢复模型上下文并支持增量和 split-turn 压缩

**文件：**
- 修改：`crates/kernel/src/runtime/session.rs`
- 修改：`crates/kernel/src/runtime/compaction.rs`
- 修改：`crates/kernel/src/provider.rs`
- 修改：`crates/kernel/tests/compaction.rs`
- 修改：`crates/kernel/tests/provider_runtime.rs`

**接口：**
- 消费：`MessageContent::CompactionSummary` 和最新 compaction boundary。
- 产出：provider 边界的 User `<summary>` 包装；二次压缩的 `<previous-summary>`；split-turn prefix 摘要。

- [ ] **Step 1：编写恢复角色失败测试**

  压缩并重新打开 Session，再发起模型请求；断言请求只包含压缩摘要、retained tail 和 boundary 后消息，摘要在 provider 边界是 User 角色且带 `<summary>`，旧原文不重复出现。

- [ ] **Step 2：运行测试并确认 RED**

  运行定向 kernel 测试，预期旧实现把摘要映射为 System 或包含 boundary 前原文。

- [ ] **Step 3：实现 boundary 恢复与 provider 转换**

  Session 恢复保留压缩摘要语义；仅 `kernel::provider` 在构造 provider 请求时转为 User 包装。原始树不删除。

- [ ] **Step 4：编写二次压缩和 split-turn 测试并确认 RED**

  二次压缩断言请求包含 `<previous-summary>` 且旧原文不在 `<conversation>`；构造超过 retained budget 的单 Turn，断言前缀摘要与 retained tail 都保留必要上下文。

- [ ] **Step 5：实现增量准备并完成 GREEN**

  从最新 compaction entry 读取上一摘要和 retained tail，按完整 Turn 选择边界；只有单 Turn 超限时启用 prefix 摘要。运行 kernel compaction、provider runtime 和 session capability 测试。

### Task 5：统一 ACP 实时事件与 Session 回放

**文件：**
- 修改：`crates/acp/src/mapping.rs`
- 修改：`crates/acp/src/server.rs`
- 修改：`crates/acp/tests/mapping.rs`
- 修改：`crates/acp/tests/extension.rs`
- 按需修改：`crates/protocol/src/session.rs`

**接口：**
- 消费：Task 2 的压缩终态和 Task 3 的持久化 entry。
- 产出：按 branch sequence 排序的 message/compaction replay items；实时与回放使用相同 compaction identity。

- [ ] **Step 1：编写实时映射失败测试**

  对 completed、failed、cancelled 事件分别断言 ACP 产品扩展 update 的 JSON 内容，完成态必须携带可直接渲染卡片的 summary、tokens、reason 和 timing。

- [ ] **Step 2：编写回放顺序失败测试**

  创建“消息 A → compaction → 消息 B”的 Session，使用 `replay_from=start`，断言 ACP 顺序为 A、compaction card、B，且实时和回放 card id 相同。

- [ ] **Step 3：运行 ACP 测试并确认 RED**

  运行 `rtk cargo test -p clawcode-acp --test mapping -- --nocapture` 和 `rtk cargo test -p clawcode-acp --test extension -- --nocapture`。

- [ ] **Step 4：实现类型化映射和 branch replay 投影**

  Kernel/Store 返回公共 replay item；ACP server 不再只遍历 transcript messages。非法 compaction payload 继续由 Store 拒绝，不在 ACP 静默降级。

- [ ] **Step 5：运行 ACP 和 Store 测试确认 GREEN**

  运行 `rtk cargo test -p clawcode-acp` 和 `rtk cargo test -p clawcode-store`。

### Task 6：实现 WebUI 压缩运行提示和持久化卡片

**文件：**
- 修改：`web/src/domain/model.ts`
- 修改：`web/src/workspace/state.ts`
- 修改：`web/src/workspace/updateDecoder.ts`
- 修改：`web/src/shell/AppShell.tsx`
- 修改：`web/src/features/conversation/MessageView.tsx`
- 创建：`web/src/features/conversation/CompactionCard.tsx`
- 修改：`web/src/theme/workbench.css`

**接口：**
- 消费：ACP compaction start/end/replay updates。
- 产出：临时“正在压缩上下文…”状态；默认折叠、可展开且可回放的压缩卡片。

- [ ] **Step 1：扩展前端领域状态**

  用判别联合表示 running/completed/failed/cancelled；completed 数据包含 entry id、summary、tokens、reason 和字符串 timing。按 compaction identity 幂等合并实时与回放 update。

- [ ] **Step 2：实现卡片和运行提示**

  运行中在对话底部显示轻量提示；完成卡片显示“上下文已压缩”和“从 N tokens 压缩 · 点击展开”，展开后显示摘要、原因和耗时；失败和取消显示稳定状态，不泄露内部错误。

- [ ] **Step 3：移除重复成功输出**

  `/compact` 调用消息继续保留；成功路径不再产生普通 Slash Command 成功卡片，失败命令仍显示稳定错误。

- [ ] **Step 4：执行前端静态验证**

  运行项目定义的 TypeScript、ESLint 和生产构建命令，修复所有错误和警告；不增加前端单元测试。

### Task 7：全量验证与真实 WebUI 端到端测试

**文件：**
- 验证：整个 workspace 与正式运行配置。

**接口：**
- 消费：Task 1–6 的完整链路。
- 产出：可复现的测试结果和真实 WebUI 行为证据。

- [ ] **Step 1：执行 Rust 质量门禁**

  依次运行 `rtk cargo fmt --all -- --check`、`rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`、`rtk cargo test --workspace`，完整检查输出。

- [ ] **Step 2：启动正式后端**

  停止旧的 3000 端口进程，使用真实 `claw.toml` 启动 `app`；确认 WebSocket、MCP 和 provider 初始化日志没有阻塞错误。

- [ ] **Step 3：执行真实 WebUI 压缩流程**

  在浏览器新建 Session，发送足够上下文后执行 `/compact`；观察运行提示、完成卡片和展开摘要，再发送后续问题验证真实 provider 能基于摘要继续回答。

- [ ] **Step 4：验证刷新回放和存储结构**

  刷新或重新选择 Session，确认卡片顺序和 identity 不变；读取对应 Session JSONL，核对 payload、operation records、usage、timing 和 lane leaf；确认不存在 `index.jsonl`。

- [ ] **Step 5：检查最终差异**

  运行 `rtk git diff --check` 和 `rtk git status --short`，只报告本次相关变更，不覆盖工作区中已有的 Slash Command 改动。
