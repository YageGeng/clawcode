# Agent Run Token 用量结算实施计划

**目标：** 让 WebUI 在 `RunEnd/agent_end` 后展示按 `run_id` 汇总的完整 agent loop Token 用量，并让实时与历史回放保持一致。

**架构：** Kernel 从持久化 `OperationFinished` 恢复 Agent Run 终态，ACP 将其投影为 `run_end`；前端在每个 `turn_end` 增量累计 usage，在 `run_end` 到达后冻结并插入唯一的 Agent Usage 行。

**规格：** `docs/superpowers/specs/2026-08-22-turn-usage-settlement-design.md`

## 全局约束

- 不使用 SubAgent 或 worktree。
- 未经用户明确允许不创建 commit。
- Rust 新增函数必须有英文函数注释，非平凡逻辑必须有英文注释。
- WebUI 端到端测试必须连接 production `app` backend 及其配置的真实 provider。

## 任务 1：恢复 Agent Run 结束边界

- [x] 扩展 ACP 集成测试，持久化 `OperationFinished` 并断言回放在最后一个 `turn_end` 后产生 `run_end`。
- [x] 运行目标测试，确认旧实现因只回放四条更新而失败。
- [x] 为 `SessionReplayItem` 增加 Run 结算项，并从 active branch Turn 关联 Agent `OperationFinished`。
- [x] ACP 将回放项映射为带原始 `run_id`、终态和结算时间的 `RunEnd`。
- [x] 重新运行目标测试并确认通过。

## 任务 2：按 Run 聚合前端 Usage

- [x] Decoder 从 `turn_end` 提取 `turn_id` 与 `run_id`，只执行增量累计。
- [x] Decoder 从 `run_end` 产生完整 Agent Run 结算动作。
- [x] Workspace 通过 `turn_id -> run_id` 去重，并按 `run_id` 增量保存精确 usage。
- [x] transcript 仅在 `run_end` 后插入 `agent_usage` entry。
- [x] 将组件和 UI 文案从 Turn Usage 调整为 Agent Usage，并保持细粒度订阅。
- [x] 运行 TypeScript 与 ESLint 检查。

## 任务 3：回归验证

- [x] 运行 Rust 格式、相关 crate 测试和 Clippy。
- [x] 运行 WebUI production build。
- [x] 使用 production `app` 与真实 provider 验证多 Turn 工具场景及刷新回放。
- [x] 执行 `git diff --check` 并复核最终差异。
