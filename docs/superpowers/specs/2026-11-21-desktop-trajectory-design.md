# Desktop 轨迹视图（M4a）设计

本文是 M4 的第一个子项目。它参照 deepseek-harness 的 Trajectory 视图（`packages/client/ui-trajectory`，调研版本 `2e39e704`），在桌面端加一个「轨迹」视图，展示 agent 循环的完整过程。[Desktop 客户端设计](./2026-11-19-desktop-gpui-kit-design.md)和 [M3b 外壳重构设计](./2026-11-21-desktop-shell-redesign-design.md)中没有被本文提到的部分仍然有效。

## 1. 背景与目标

现在桌面端和 WebUI 只能看到对话和原始 ACP 事件，看不到：

- 每次请求模型实际看到了什么：系统提示词、工具清单、请求选项；
- 时间花在哪里：等首 token、生成、工具执行各用了多久；
- 每次请求的 token 用量和缓存命中；
- 重试、插话、排队消息、压缩发生在循环的哪个位置。

目标：对一个会话打开「轨迹」，能逐轮、逐次请求地看到这些信息，体验接近 deepseek-harness 的 Trajectory。

成功标准：第 9 节的测试全部通过；第 9 节的实机验收清单逐项截图确认。

## 2. 已确认的决策

| # | 决策 |
| --- | --- |
| 1 | 先做桌面端；WebUI 以后复用同一个后端方法。 |
| 2 | 放在主卡片标题栏的「对话 / 轨迹」切换里，不做成右侧面板标签。 |
| 3 | 属于 M4，作为子项目 M4a，包含后端数据、记录表与详情、时间总览三部分。 |
| 4 | 不考虑历史数据兼容：日志格式可以直接修改，请求快照是必有字段，不为旧会话补数据或降级。 |
| 5 | 记录详情复用右侧面板，打开一个「记录详情」标签。 |
| 6 | JSONL 仍是唯一的数据来源；SQLite 历史库是查询层，新增的表都能从 JSONL 重建。请求快照写进 JSONL，不只存在 SQLite。 |
| 7 | 只显示当前分支：从根到当前分支末端的条目，以及产生它们的运行。 |
| 8 | 实时更新只在消息完成、工具调用完成、一轮结束时从历史库补取；进行中的请求或工具显示为末尾的一行，不逐字显示输出。 |
| 9 | 轨迹记录在后端整理（`state` 库），ACP 返回整理好的记录，客户端只负责画。 |

## 3. 数据层

**术语对应：** clawcode 的 turn 是一次模型请求（`step_attempt` 记录一条），运行（run）是一个完整的 agent 循环。界面上的「第 N 轮」对应运行，「请求 #N」对应 turn，与 deepseek-harness 的 Turn / Step 一致。

### 3.1 JSONL：请求快照

`protocol::AssistantMetadata` 新增字段 `request: Option<RequestSnapshot>`，记录这次助手尝试实际发出的请求。agent 循环里的每次尝试都必须带上它；压缩、fork、MCP 采样等不经过 agent 循环请求的助手消息为 `None`（这是语义上没有请求，不是兼容旧数据）。字段如下：

| 字段 | 内容 |
| --- | --- |
| `system_prompt` | 系统提示词文本的 SHA-256（十六进制，`ContentHash` 类型） |
| `tools` | 工具定义列表（名称、描述、参数 Schema，按发送顺序）序列化后的 SHA-256 |
| `options` | 提供方、模型、思考等级、最大 token、温度、提示缓存保留策略（不含缓存键） |
| `message_count` | 请求带的历史消息条数（不含系统提示词） |

快照在 `crates/kernel/src/runtime/run/turn.rs` 组装请求的最后一步之后取：扩展的 `context` 钩子改写、`adapt_input` 都已完成，也就是真正发给提供方的那份。每次尝试（包括重试和溢出恢复后的重新请求）各带一份。

新增记录类型 `RecordKind::RequestContent`，载荷 `{requestContent: {hash, kind: system_prompt | tools, content}}`（记录载荷会展开到 JSONL 变更行里，内容自身的 `kind` 不能放在顶层，否则与变更行的 `kind` 冲突）：

- 同一会话里一个哈希第一次出现时，在对应的助手消息条目之前写入；
- 会话运行时保存已写过的哈希集合，从日志加载会话时由已有记录恢复；
- `tools` 的 `content` 是工具定义数组的 JSON，`system_prompt` 的 `content` 是文本。

### 3.2 SQLite 历史库

新增迁移 `crates/state/migrations/history/0003_trajectory.sql`：

```sql
-- Every lane record mirrored verbatim, for queue marks, retries, and request contents.
CREATE TABLE records (
    session_id   TEXT NOT NULL REFERENCES projection_state(session_id) ON DELETE CASCADE,
    record_id    TEXT NOT NULL,
    sequence     INTEGER NOT NULL,
    lane         TEXT NOT NULL,
    run_id       TEXT,
    kind         TEXT NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    record_json  TEXT NOT NULL,
    PRIMARY KEY (session_id, record_id)
);
CREATE INDEX idx_records_by_run ON records(session_id, run_id, sequence);
CREATE INDEX idx_records_by_kind ON records(session_id, kind, sequence);

-- Request contents addressed by hash within one Session.
CREATE TABLE request_contents (
    session_id TEXT NOT NULL REFERENCES projection_state(session_id) ON DELETE CASCADE,
    hash       TEXT NOT NULL,
    kind       TEXT NOT NULL,
    byte_len   INTEGER NOT NULL,
    content    TEXT NOT NULL,
    PRIMARY KEY (session_id, hash)
);
```

两张表由现有的历史投影（`crates/state/src/projector/history_sink.rs`）写入，与 `entries`、`runs` 同一个事务，走现有的恢复和重建流程。

### 3.3 轨迹记录类型

在 `protocol` 新增 `trajectory.rs`：

```
TrajectoryPage {
    records: Vec<TrajectoryRecord>,
    leaf_id: Option<EntryId>,        // 当前分支末端；每次追加条目都会变，不能用来判断分支切换
    branch_changed: bool,            // 仅 after 查询：游标已不在当前分支上，本页是新分支的末尾一页
    has_more_before: bool,
    has_more_after: bool,            // 仅 after 查询：还有超出 limit 的新记录
    first_sequence / last_sequence,  // 本页的游标
    usage_before: ModelUsage,        // 本页之前当前分支的累计用量（复用已有的 ModelUsage）
}

TrajectoryRecord {
    id: String,                      // 稳定 id：条目 id，或 "request:{message_id}" 等
    sequence: u64,
    run: Option<RunRef>,             // run_id 和运行序号 N（界面上的「第 N 轮」）；运行之外的条目为 None
    turn_id: Option<TurnId>,         // clawcode 的 turn 即一次模型请求
    started_at_ms / ended_at_ms: Option<TimestampMs>,
    kind: TrajectoryRecordKind,
}

TrajectoryRecordKind
  User        { text, image_count, file_count, origin: input | steering | follow_up | skill }
  Request     { index, snapshot: RequestSnapshot, prompt_changed: bool,
                tools_added: Vec<String>, tools_removed: Vec<String>, first_in_branch: bool }
  Assistant   { request_index, reasoning, text, tool_call_ids, usage, requested_at_ms,
                first_token_at_ms, last_token_at_ms, stop_reason, error, cancelled }
  Tool        { call_id, name, arguments (JSON), result_text, image_count, is_error,
                cancelled }
  Compaction  { summary, tokens_before }
  BranchSummary { summary }
  Setting     { change: model | thinking_level | active_tools }
  Extension   { extension_id, custom_type, text }
```

`Request` 和 `Assistant` 都来自同一条助手消息：`Request` 在前，表示一次请求的开始（记录表中的请求分隔行），`Assistant` 是它的输出。`Tool` 合并助手消息里的调用块和对应的工具结果条目。提示词和工具清单的完整内容不放进 `TrajectoryPage`，由 3.5 的第二个方法按哈希取。

### 3.4 查询与分页

实现放在 `crates/state/src/history/trajectory.rs`，是 `StateRuntime` 的方法。整理逻辑使用一个持有查询上下文的小结构体（会话、分支条目清单、已读取的快照），遵守 AGENTS.md 第 15 条。

- **分支：** 末端取自 `lanes` 表的主分支线。用递归查询沿父条目往回走，只取 `entry_id`、`sequence`、`message_type`，不取 `entry_json`。这份清单用于计算请求序号、轮次序号，以及确定页的边界。
- **分页参数：** `before: Option<u64>`、`after: Option<u64>`（二选一，或都不给）、`limit`（默认 80 条分支条目，上限 200）。
  - 都不给：返回末尾一页。
  - `before`：返回该顺序号之前的一页。
  - `after`：返回该顺序号之后的全部新记录，不超过 `limit`。超出时设置 `has_more_after`，客户端接着取。
- **页边界：** 页的起点只能是用户消息、助手消息或非消息条目，不能是工具结果。因此工具的调用和结果总在同一页。
- **跨页比较：** 一页里的第一个请求需要和上一次请求比较时，服务端沿分支往回找到上一条助手条目，读出它的快照。只有在哈希不同时，才读取两份工具清单、计算增减的名称。
- **插话和排队标记：** 用 `records` 表里的 `queue_enqueued` 记录（载荷带 `entryId` 和 `target`），把对应的用户条目标成 `steering` 或 `follow_up`。
- **累计用量：** `usage_before` 用一条聚合查询，对分支上本页之前的助手条目汇总 `entry_json` 里的用量。
- **读前追赶：** 沿用历史库「读取前先追上日志」的约定，所以通知到达后的查询一定能看到对应的条目。

### 3.5 内核与 ACP

- 内核新增 `session_trajectory(session_id, cursor)` 和 `session_request_content(session_id, hash)`，转调 `StateRuntime`。历史库不可用（关闭或未就绪）时返回 `KernelError::HistoryUnavailable`。
- ACP 新增两个扩展方法：
  - `_clawcode/session/trajectory`：参数 `{sessionId, before?, after?, limit?}`，返回 `TrajectoryPage`；
  - `_clawcode/session/trajectory/content`：参数 `{sessionId, hash}`，返回 `RequestContent`（`{hash, kind, content}`），不存在时返回 `null`。
- 不可用错误映射为带 `data.reason = "historyUnavailable"`（与现有的 `questionClosed` 同一风格） 的 ACP 错误，客户端据此显示提示。
- 两个方法都是只读的，按 AGENTS.md 第 13 条只在 DEBUG 记录，INFO 不记参数。

## 4. 「对话 / 轨迹」切换

- 主卡片标题栏（`crates/desktop/src/ui/shell/card.rs` 的 `render_card_header`）左侧是会话标签条，所以切换放在右侧工具组的最前面（状态文字之前），用分段样式的 `TabBar`，包含「对话」和「轨迹」。
- 每个主标签页记住自己当前的视图，只在内存里保存，默认是「对话」。
- 新增动作 `ToggleTrajectory`，放进「视图」菜单，快捷键 `Ctrl+Shift+T`（macOS 为 `Cmd+Shift+T`，与现有绑定不冲突），作用范围与其他外壳快捷键相同。
- 轨迹视图保留底部输入框。
- 草稿标签没有会话，不显示切换。

## 5. 轨迹视图

### 5.1 布局

自上而下依次是：工具栏、时间总览（第 6 节）、记录表。只有记录表滚动。

### 5.2 工具栏

- **左侧：**
  - 时长显示切换（实际 / 等宽，只影响时间总览）；
  - 「轮次」「调用」两个折叠开关；
  - 时间总览的显示开关。
- **右侧：** 汇总，例如「3 轮 · 12 次请求 · 1.2 万 tok · 缓存命中 85%」，统计范围是已加载的记录。
- **时间区间：** 时间总览选了区间时，工具栏显示「时间范围 +3.2s–+9.0s」（从已加载的第一条记录算起；工作区没有时区库，不显示钟表时间），带清除按钮。
- **窄窗口：** 折叠开关收进一个「更多」菜单，汇总保留。

### 5.3 记录表

使用 gpui-kit 的 `Table`，所有行等高，使用它的虚拟滚动和键盘选择。列依次为：

| 列 | 内容 |
| --- | --- |
| 类型 | 类型标记；轮次标题行和请求分隔行在这一列放标题 |
| 内容 | 单行摘要，占剩余宽度，过长显示省略号 |
| 输入 / 输出 / 思考 | token 数，右对齐，等宽数字 |
| 耗时 | 时长，右对齐 |

行的种类：

| 行 | 显示 |
| --- | --- |
| 加载更早 | 表头下第一行，点击或回车加载上一页，加载中显示转圈 |
| 轮次标题 | 「第 N 轮」，即 clawcode 的一次运行（一个完整的 agent 循环）；本轮总耗时、token 合计；点击折叠这一轮。运行之外的条目（例如运行开始前写入的扩展消息）归在「运行之间」标题下 |
| 请求分隔 | 「请求 #N · 模型 · 首 token 0.8s」；提示词或工具有变化时带变化标记 |
| 用户 | 文本摘要；附件数、插话、排队、skill 展开用标签标出 |
| 系统 | 提示词变化，或「+2 工具 −1 工具」；会话中的第一次请求显示「初始系统提示词」 |
| 助手 | 正文摘要；只有工具调用时显示「（仅工具调用）」；失败或取消用错误色并标出 |
| 工具 | `工具名 参数摘要 → 结果摘要`；出错用错误色 |
| 压缩 / 分支摘要 / 设置 / 扩展 | 各自的单行摘要 |
| 进行中 | 表的最后一行：进行中的请求或工具、开始时间、已用时长（每秒更新） |

- **「调用」折叠：** 每次请求下的工具行合成一行「N 个工具调用 · 合计时长 · M 个失败」，点击展开这一组。
- **选择：**
  - 鼠标点击或上下方向键选择；回车打开记录详情（第 7 节）。
  - 选中按记录 id 保存，加载更早的页或补取新记录后都保留。
  - 悬停、键盘焦点、选中三种状态在视觉上分开。
- **搜索：** 暂不提供（工具栏放不下，实现后已移除）。
- **加载全部：** 还有更早的记录时，工具栏提供「加载全部」按钮：持续向前分页，直到没有更早的记录。
- **跟随：**
  - 滚动位置在底部时，新记录到达后自动滚到底部。
  - 向上滚动后停止跟随，右下角出现「回到最新」按钮。

### 5.4 状态

- **首次加载：** 显示表格形状的骨架。
- **空会话：** 显示「还没有记录。发送消息后，这里会显示 agent 的每一步。」
- **历史库不可用或请求失败：** 在表格区域显示原因和「重试」按钮，工具栏仍然可用。
- **补取失败：** 不清空已有内容，在表格底部显示一行错误和「重试」。

### 5.5 数据加载与补取

- 每个会话标签持有一个 `TrajectoryState`：已加载的记录、游标、分支末端、选中项、折叠状态、时间区间、跟随状态。它在第一次切到「轨迹」时创建并加载末尾一页。
- **补取时机：** 轨迹视图正在显示时，在以下三种会话更新之后补取：一条消息完成、一个工具调用完成、一轮结束。150ms 内的多次通知合并成一次 `after` 查询。轨迹没有显示时不补取；重新显示时用 `after` 一次取齐。
- **分支变化：** `after` 查询的游标不在当前分支上时，服务端改为返回新分支的末尾一页并置 `branch_changed`；客户端据此丢弃已加载内容，选中项尽量按 id 保留。只比较 `leaf_id` 不可行：同一分支上每追加一个条目，末端就会变。
- **进行中行：** 由现有的会话投影（运行状态、执行中的工具调用）生成，不进历史库。对应的记录补取回来后，这一行由真实记录替换。

## 6. 时间总览

- 位于工具栏和记录表之间，高度约 6 rem。是否显示保存在 `desktop.json` 的布局状态里，所有会话共用。
- 用 gpui 的自绘画布实现，不新增依赖，颜色只取主题的图表色和语义色。
- **时间轴：** 横轴范围是已加载的第一条到最后一条记录。左边还有未加载的记录时，在左端标「更早的记录未加载」。
- **三条泳道：**
  - **请求：** 每次请求一根横条；从发出到首 token 用浅色，从首 token 到末 token 用实色；失败或取消用错误色。
  - **工具：** 每个工具调用一根横条。重叠的调用按贪心分配放到最多 3 个子行，放不下的合并成一根带数量的横条。出错用错误色。
  - **事件：** 用户消息、插话、提示词或工具变化、压缩、设置变化画成细竖线标记。
- **轮次和刻度：** 轮次边界画成贯穿三条泳道的分隔线，上方标「第 N 轮」，下方是时间刻度。
- **等宽模式：** 每条记录按先后顺序占相同宽度，不按真实时长。
- **进行中：** 从开始时间画一根虚线横条到当前时刻，每秒延长。
- **交互：**

| 操作 | 结果 |
| --- | --- |
| 悬停约 500ms | 浮出开始时间、时长；请求另外显示首 token 延迟、生成时长、吞吐量 |
| 单击横条 | 在记录表中选中这条记录并滚动到它 |
| 拖动 | 选一个时间区间，记录表只显示与区间重叠的记录 |
| 右键单击，或工具栏的清除按钮 | 清除区间 |
| 滚轮 | 以鼠标位置为中心缩放 |
| Shift+滚轮，或右键拖动 | 缩放后平移 |
| 双击 | 恢复全部范围 |
| 左右方向键（获得焦点时） | 按时间顺序移动选中 |
| Esc | 清除区间 |

- **性能：** 只绘制视口内的横条；相邻的、宽度不足 1px 的横条合并后再画。
- **纯函数：** 泳道分配、缩放平移换算、区间筛选、横条合并写成不依赖界面的纯函数，以便单独测试。

## 7. 记录详情

- 新增面板标签 `PanelTabKey::Record`。每个会话面板最多一个，标题「记录详情」。它显示轨迹中当前选中的记录，标签会随面板状态一起保存。
- 重启后如果找不到原来选中的记录，显示「在轨迹中选择一条记录」。
- **顶部：**
  - 类型标记、标题（例如「请求 #12 · deepseek-flash」）、状态（完成 / 失败 / 进行中）；
  - 「上一条」「下一条」按钮，与记录表的选中同步。
- **内容分页：** 用分段 `TabBar` 切换，按记录类型提供：

| 记录 | 分页 |
| --- | --- |
| 请求 / 助手 | 概述、计时、用量、系统提示词、工具、选项、原始内容 |
| 工具 | 概述、参数、结果、计时、原始内容 |
| 用户 | 概述、原始内容 |
| 压缩 / 分支摘要 | 概述、原始内容 |
| 系统 / 设置 / 扩展 | 概述、原始内容 |

- **各页内容：**
  - **概述（请求 / 助手）：** 可折叠的思考、正文（用现有的 Markdown 渲染）、这次请求发起的工具调用列表，点击跳到对应的工具记录。
  - **计时：** 发出、首 token、末 token 三个时刻，以及首 token 延迟、生成时长、吞吐量（输出 token ÷ 生成时长）。重试的请求注明是第几次尝试。
  - **用量：** 输入、缓存读取、输出、推理 token；分「本次请求」和「会话累计」两栏。
  - **系统提示词：** 第一次打开时按哈希调用 `_clawcode/session/trajectory/content`。和上一次请求不同时提供「完整 / 差异」切换；差异用 `similar` 计算，用 gpui-kit 的差异组件显示。
  - **工具：** 工具名列表，展开某一项可以看到参数 Schema。顶部标出与上一次请求相比增减了哪些。
  - **选项、参数、原始内容：** 格式化的 JSON，代码高亮，只读，带复制按钮。
- 面板宽度不够时，沿用 M3b 的浮层显示规则，Esc 关闭。

## 8. 代码组织

| 位置 | 内容 |
| --- | --- |
| `crates/protocol/src/trajectory.rs` | 请求快照、轨迹记录、分页类型 |
| `crates/kernel/src/runtime/run/` | 取请求快照、写 `RequestContent` 记录、会话内的哈希集合 |
| `crates/store` | `RecordKind::RequestContent` |
| `crates/state/migrations/history/0003_trajectory.sql`、`projector/history_sink.rs`、`history/trajectory.rs` | 镜像与查询 |
| `crates/acp/src/` | 两个扩展方法 |
| `crates/desktop/src/backend/client.rs` | 两个请求方法 |
| `crates/desktop/src/ui/trajectory/` | `mod.rs`（视图与 `TrajectoryState`）、`toolbar.rs`、`table.rs`、`timeline.rs`、`timeline_math.rs`（纯函数） |
| `crates/desktop/src/ui/panel/record.rs` | 记录详情标签 |

新结构体超过 3 个字段时使用 `typed-builder`。测试只放在各库的 `tests/` 目录中。

## 9. 测试与验收

### 9.1 自动测试

| 库 | 测试 |
| --- | --- |
| `state` | 用 JSONL 样本构造会话：只返回当前分支；页边界不拆开工具调用和结果；请求序号和轮次序号；提示词变化和工具增减（包括跨页）；插话和排队标记；失败重试；压缩；`before` 和 `after` 游标；`after` 游标离开当前分支时返回新分支末尾并置 `branch_changed`，同一分支追加时不置；`usage_before` 正确；`request_contents` 镜像和按哈希读取 |
| `kernel` | 同一哈希只写一次内容记录，且写在助手条目之前；从日志加载后不重复写入；快照取的是扩展改写之后的请求；每次重试都有快照 |
| `acp` | 两个方法的请求和响应往返；历史库不可用时的错误 |
| `desktop` | 视图切换和每个标签页记住视图；各种行的显示；轮次和调用折叠；选中后打开记录详情、上一条和下一条；补取后跟随最新；分支变化后重新加载；时间总览纯函数（泳道分配、缩放平移、区间筛选、横条合并） |

### 9.2 实机验收

用生产后端和真实的 DeepSeek 提供方跑一个会话，覆盖：多次工具调用（含并行和出错）、子 agent、运行中插话、手动压缩、导航到另一个分支。逐项截图确认：

1. 记录表的分组、折叠和列数据；
2. 请求的计时和用量与 ACP 推送的数据一致；
3. 系统提示词的完整视图和差异视图、工具增减；
4. 时间总览的泳道、悬停浮层、区间筛选和缩放；
5. 运行中的「进行中」行和虚线横条，以及完成后被真实记录替换；
6. 切换分支后轨迹跟着变；
7. 窄窗口下工具栏和记录详情浮层的表现。

## 10. 不做的内容

- 父会话轨迹里嵌套子 agent 的轨迹；
- 跨会话搜索；
- WebUI 版本（以后复用第 3.5 节的方法）；
- 生成中逐字显示输出；
- 轨迹导出和回放。
