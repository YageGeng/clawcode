# Agent Run Token 用量结算设计

## 目标

WebUI 在一次完整 Agent Run 结束后仅展示一条 Token 用量统计。事件语义遵循 Pi：`TurnStart` 到 `TurnEnd` 表示一次 LLM 响应及其工具调用；`agent_start` 到 `agent_end` 才表示完整 agent loop。在 clawcode 中，后者对应 `RunStart` 到 `RunEnd`。

## 实时事件语义

- `UsageUpdated` 继续更新当前上下文窗口，并携带精确的模型 Token 分类数据。
- 每个 `TurnEnd` 只把同一 `turn_id` 下全部 Assistant usage 累加到该 Turn 所属的 `run_id`，不得创建展示行。
- `RunEnd` 是完整 Agent Run 的结算边界。前端仅在该事件到达后冻结累计值，并创建唯一的 Agent Usage 行。
- `AgentSettled` 不作为本统计的边界；它用于表示自动压缩等后续工作也已结束。
- Agent Usage 行使用 `RunEnd` 的事件顺序，因此必须位于该 Run 的全部消息、工具卡和 Turn 之后。

## 持久化回放

- Kernel 已将每个 `TurnRecord` 持久化为当前 lane 的 `StepAttempt` record，并将 Agent Run 终态持久化为 `OperationFinished` record。
- Session replay 必须合并 active branch entries、属于 active branch 的 `TurnRecord` 和对应的 Agent `OperationFinished`，并按 Store 共享 sequence 排序。
- ACP 回放保持实时语义：Assistant `MessageEnd`、`UsageUpdated`、工具结果、各个 `TurnEnd`，最后是唯一的 `RunEnd`。
- Agent `OperationFinished` 通过 active branch 中已选 Turn 的 `run_id` 关联；不得把 Compaction 的同类 record 误投影成 Agent `RunEnd`。
- 分支切换后，不得回放已离开 active branch 的 Turn 或其孤立 Run 结算。
- Session 恢复后的 live event sequence 必须高于全部合成回放事件。

## 前端状态与性能

- 前端保存 `turn_id -> run_id` 去重映射，防止回放或重连重复累计同一 Turn。
- 前端按 `run_id` 保存增量累计值；每个 Turn 仅扫描一次自身 Assistant 消息，`RunEnd` 不再扫描完整 transcript。
- transcript 新增按 `run_id` 唯一标识的 `agent_usage` entry。
- Conversation 使用细粒度订阅渲染已冻结的 Agent Usage，流式 frame 不触发该统计行重渲染。

## 验收

- 无工具单 Turn Run：统计行在最终 Assistant 消息之后出现。
- 多工具、多 Turn Run：中间 `TurnEnd` 不显示统计行，唯一统计行在完整 agent loop 结束后出现。
- 统计值等于同一 `run_id` 下全部 Turn 的 input、output、cache read、cache write、reasoning 和 total Token 之和。
- 历史 Session 刷新后恢复相同的统计行、顺序和数值。
- WebSocket 重连或重复事件不会重复累计或创建多条统计行。
