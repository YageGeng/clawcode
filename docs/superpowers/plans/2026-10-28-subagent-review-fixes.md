# Subagent 评审修复计划

## 范围与约束

修复评审的 6 项问题，并统一提交结果语义、将后台动作收敛为类型化分发。直接使用当前工作区，不创建 worktree 或 commit；保留此前实现与用户已有改动。测试使用真实 Kernel 的本地依赖替身，WebUI 验收继续使用生产 app 和已获授权的 DeepSeek 官方 API。

## Pi 参考与取舍

参考目录：`/Volumes/Yage/Documents/WorkSpace/pi/packages/durable`。

- `src/harness/submissions.ts`：按 submission ID 注册、等待与撤回，避免读取“最新结果”串到别人的输入。
- `src/harness/live.ts:endRun`：由 run 的最终结算统一完成名下输入，取消不会退回读取最后一个失败 Turn。
- `src/storage/sqlite/storage.ts`：会话元数据和答案 entry 按 ID 读取，不通过完整 transcript 查找。
- `src/harness/harness.ts`：会话句柄来自已有记录，读写在 Session 的串行提交线上完成，不存在多个 child 各自重建同一 owner runtime 的模式。Rust 需要显式等待已有会话构建预留完成。
- `test/examples/21-subagent-foreground.ts`：示例直接返回完整文本，没有输出限额；本项目使用已有 `ToolOutputPolicy::Bounded`。
- 不移植 Pi 的持久任务调度器或 SQL 存储：内存回执、既有生命周期预留、JSONL 文件名与 header 查询即可覆盖当前边界。

## Task 1：统一实际 run 的提交结算（评审 1、2、6）

- [x] 将评审临时测试纳入 crate 集成测试，复现 B 得到 C 答案、重试期间取消误报 Failed。
- [x] 为内部排队输入附加一次性回执；消费时归入当前 run，撤回时取消，run 结束时统一结算。
- [x] `RunSettlement` 直接形成 `SubmissionAnswer`，直接提交和排队提交使用同一份结果；删除 `last_settlement` 与 `stored_message` 全量历史扫描。
- [x] 覆盖消费前/后取消、异常退出、滞留队列恢复与后续 run 抢先运行；执行 subagent、retry、queue/lifecycle 相关回归。

## Task 2：共用 owner 的恢复协调（评审 3）

- [x] 加入并行 child 恢复测试，确认原实现出现 SessionStarting。
- [x] 将现有 pending session 预留改为可等待的完成信号；已在启动的 owner 先等待，成功后共享；失败保持错误语义。
- [x] 检查 ownership 环，避免把递归等待误当成并发恢复；不持有同步锁跨 await。
- [x] 执行 ownership、MCP 和生命周期回归。

## Task 3：轻量存在性查询（评审 4）

- [x] 为 StoreFactory 增加兼容默认实现的 `exists`；JsonlStoreFactory 按既有文件名定位，只读取匹配文件的 header。
- [x] 冷会话 runtime 状态使用 exists，保留不存在时 NotFound、已持久化未启动时 idle 的公开语义。
- [x] 测试正常/缺失/错误 header，证明查询不读取 transcript 正文；无新增元数据索引或缓存失效机制。

## Task 4：输出预算与后台动作整理（评审 5、clean code）

- [x] 加入 30 KiB 前台答案测试，启用现有 Bounded 输出策略，完整答案保留在 child transcript。
- [x] 用 `BackgroundAction` + `TryFrom<&ToolCall>` 承载已校验参数；动作执行以 match 分发，保持普通参数错误文案和 reporter 语义。
- [x] 执行后台、前台与参数边界回归；完整 workspace tests、clippy、fmt、前端 check/build。

## 验证边界

不改变外部 RunResult、JSONL schema、取消的后台边界或扩展事件顺序。每个确认的评审问题都应有失败到通过的证据。新增逻辑测试只放 crate tests 或标准 tests 模块；对不修改的前端不增加单测。
