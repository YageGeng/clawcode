# ACP WebSocket 实时恢复设计

## 1. 背景

当前 WebUI 将一个 ACP WebSocket 连接同时作为请求通道和 Session 事件投影出口。`session/prompt` 响应后，Kernel Run 已脱离请求任务继续运行，但 `AcpEventSink` 仍永久绑定发起 Prompt 的原连接。连接断开后，事件发送失败会被抑制，Run 虽然继续执行，新连接却无法接管实时事件，只能轮询运行状态并等待 Run 结束后执行持久化回放。

前端重连逻辑还缺少单飞约束和连接代次校验。多个 `close`、失败回调和定时器可以同时启动重连，旧连接的异步结果也可能清空已建立的新连接。断线时 `sessions/invalidated` 会把所有 Session 的 `running` 改为 `false`，使仍在后端运行的 Agent 在界面上表现为已经停止。

本设计不新增 `_clawcode/session/watch` 方法。连接级恢复协商放入标准 `initialize` 的 `_meta`，Session 的历史补偿和实时绑定统一由标准 `session/resume` 完成。

## 2. 目标

1. WebSocket 断开后 Kernel Run 继续运行，不受原连接生命周期影响。
2. 网络恢复后，前端补齐断线期间事件并恢复实时更新，不等待 Run 结束。
3. 同一 Session 允许多个 ACP 连接同时接收事件，断开其中一个连接不影响其他连接。
4. ACP 协议表面不增加新的 watch 方法，恢复能力通过标准初始化、Resume、Replay cursor 和 `_meta` 扩展表达。
5. 前端同一时刻最多存在一个连接尝试和一个重连定时器，旧连接回调不能修改新连接状态。
6. 断线期间保留 Session 投影和运行状态；无法确认 Prompt 是否受理时只标记结果未知，不自动重发。
7. 使用标准 ACP 请求完成空闲连接存活检测，不增加产品 Ping 方法。
8. 使用 ACP v2 支持的标准 JSON-RPC batch 批量发送连续 Session 更新，减少恢复期间的 WebSocket 帧数、前端解码次数和状态渲染次数。
9. 对同一文本块下已经排队的连续 Assistant 文本或思考流式 chunk 合并正文，减少 batch 内重复 JSON-RPC、ACP 和恢复元数据 envelope 的字节开销，不引入等待窗口。

## 3. 非目标

1. 不保证进程崩溃或服务重启后恢复尚未持久化的流式 token；服务重启后回退到持久化全量 Resume。
2. 不修改持久化 JSONL 格式，也不把运行期事件日志写入数据库或 Store。
3. 不把 `session/update` 设计成跨进程消息队列。
4. 不自动重发结果未知的 `session/prompt`，避免重复执行 Agent Run。
5. 不改变 ACP 标准方法名称或 JSON-RPC envelope。
6. 不保证一个客户端跨过两个及以上完整 Operation 后仍恢复所有非持久化 Tool、Retry 和流式事件；超过最近 Operation 恢复窗口时以持久化 transcript 为权威。
7. 不合并 ToolCall、ToolCall result、ToolExecution update、MessageEnd、Operation 生命周期事件或任意非文本 chunk，不改变它们的数量、顺序和状态语义。
8. 不重写 Operation journal，不压缩持久化 transcript，也不以本次传输优化解决 journal 的长期内存占用。

## 4. 协议方案

### 4.1 初始化能力协商

前端在 `initialize` 请求的产品命名空间 `_meta.clawcode.sessionRecovery` 中声明恢复协议版本，并提交当前内存中仍保留投影的 Session 游标。

```json
{
  "protocolVersion": 2,
  "info": {
    "name": "clawcode-web",
    "version": "0.1.0"
  },
  "capabilities": {},
  "_meta": {
    "clawcode": {
      "sessionRecovery": {
        "version": 1,
        "sessions": [
          {
            "sessionId": "session-1",
            "runId": "run-1",
            "nextSequence": 124
          }
        ]
      }
    }
  }
}
```

`runId` 标识前端当前投影所属的 Agent Run 或其他带 RunId 的 Session Operation。`nextSequence` 表示前端尚未应用的第一条 Kernel 事件序号下界。前端只有在已经收到 Operation 起始事件并按单调 sequence 完整应用投影组后才提交该游标；缺少 RunId、sequence 回退或本地投影已清空时不得猜测游标。Available Commands 等不属于 Operation journal 的直接 ACP 更新可以占用 Kernel sequence，因此相邻投影组之间允许存在序号空洞。

后端读取当前 Session 运行状态和运行期日志覆盖范围，在 Initialize 响应的同一命名空间返回恢复计划：

```json
{
  "_meta": {
    "clawcode": {
      "sessionRecovery": {
        "version": 1,
        "sessions": [
          {
            "sessionId": "session-1",
            "mode": "watch",
            "runId": "run-1",
            "nextSequence": 124
          }
        ]
      }
    }
  }
}
```

恢复模式定义如下：

- `watch`：当前或最近已完成 Operation 的 RunId 与前端一致，且内存日志完整覆盖 `nextSequence`。前端保留当前投影，通过增量 Resume 补偿并重新绑定实时流。该模式既可能返回 `running: true`，也可能在 Operation 已完成但日志仍保留时返回 `running: false`。
- `resume`：RunId 不一致、游标回退或超出日志覆盖范围、日志已淘汰，或前端没有可复用投影。前端清空投影，通过 `replayFrom: start` 全量恢复。

Initialize 只返回恢复计划，不在初始化响应前发送任何 Session 更新，也不把连接直接注册为 Session 订阅者。未声明恢复协议的普通 ACP Client 继续获得当前标准行为。

### 4.2 增量 Resume

`watch` 计划仍调用标准 `session/resume`，使用 ACP v2 允许的实现方 Replay cursor：

```json
{
  "sessionId": "session-1",
  "cwd": "/workspace",
  "replayFrom": {
    "type": "_clawcode/event",
    "runId": "run-1",
    "sequence": 124
  }
}
```

Replay cursor 保持 ACP 的 inclusive 语义，`sequence` 指定第一条需要补发的事件。后端必须在同一 ACP Session Projection 临界区内完成以下操作：

1. 校验 Session、cwd、RunId 和日志覆盖范围。
2. 为当前 ACP 连接创建实时订阅接收器。
3. 截取 `sequence` 起始的已投影事件组快照。
4. 按 Kernel sequence 和组内 index 顺序发送快照。
5. 从实时接收器继续发送快照之后的事件组，跳过已经发送的 sequence。
6. 安装连接级 watcher 后返回标准 `ResumeSessionResponse`。

先创建接收器再截取日志，避免快照与订阅之间丢失事件。快照和实时接收器重叠的事件按 sequence 去重。一个 Kernel 事件可能映射为多条 ACP `session/update`；这些通知共享同一个 Kernel sequence，并保持 mapper 返回顺序。

后端在外层 `UpdateSessionNotification._meta.clawcode.sessionRecovery` 中为同一 Kernel 事件映射出的每条通知增加 `runId`、`projectionIndex` 和 `projectionCount`。Index 从零开始，Count 是该组通知总数。Operation 起始事件组额外携带 `operationPhase: "start"`，终止事件组额外携带 `operationPhase: "end"`；其他事件组省略该字段。Agent Run 以 `RunStart` / `AgentSettled` 为边界，Slash Command 以 `SlashCommandStart` / `SlashCommandEnd` 为边界，独立 Compaction 以 `CompactionStart` / `CompactionEnd` 为边界。内嵌 Compaction 不建立新的 Operation 边界。事件自身的 `turnId`、`timestampMs` 和 `sequence` 继续保留在现有 update/content 元数据中，不改变标准 ACP update 结构。

前端只对存在合法分组元数据的通知进行缓冲，旧服务端或直接发送的标准 ACP 通知继续立即应用。前端收齐整组后才原子应用并推进 `nextSequence`。如果连接在组内断开，当前连接代次必须丢弃全部未完成组，但保留已经完整提交的 Session cursor；未完成组不进入 Session 投影，重连后从该 Kernel sequence 完整重放，因此不会重复追加已应用的文本 chunk，也不会漏掉同事件的 Idle 或扩展更新。

如果 Initialize 后 RunId 已被下一 Operation 替换、游标过期或日志不再覆盖请求范围，增量 Resume 在发送任何更新前返回可识别的 cursor unavailable 错误。仅仅 Operation 已经结束不构成错误，只要最近已完成 journal 仍覆盖游标就继续补发。前端收到 cursor unavailable 后清空 Session 投影，并使用 `replayFrom: start` 重试。后端不得静默猜测或在一次请求中混合两种恢复策略。

### 4.3 全量 Resume

`resume` 计划沿用标准请求：

```json
{
  "sessionId": "session-1",
  "cwd": "/workspace",
  "replayFrom": {
    "type": "start"
  }
}
```

空闲或尚未加载且没有 ACP Projection 的 Session 继续使用现有 `Kernel::resume_session` 和持久化 transcript 回放。运行中的 Session 不得等待 operation gate 或重新触发 Session Resume 生命周期，而是先调用 Kernel 的只读 attachment 校验，再执行“全量投影恢复并绑定实时流”。只读校验负责检查 SessionId、cwd 和 lifecycle，不等待 operation gate、不运行 Resume Hook，也不修改 Session。

当 ACP Projection 仍保存当前或最近 Operation 时，全量恢复执行：

1. 读取 Operation 起始事件到达 Projection Sink 时捕获的持久化回放 baseline。
2. 回放 baseline 中的持久化 Session 历史。
3. 回放该 Operation 从起点开始的已投影事件组日志。
4. 切换到当前连接的实时订阅。

`ProjectionSink` 在第一次收到 `RunStart` 或 `SlashCommandStart` 时同步读取一次 `Kernel::session_replay`，并在追加起始事件前保存为 baseline。`CompactionStart` 只有在当前没有进行中的 journal 时才开始一个独立 Compaction Operation；Agent Run、Slash Command 或 Extension 内嵌触发的 Compaction 继续追加到外层 Operation journal，不能替换它。现有 Kernel 会在这些起始事件之后才持久化本 Operation 的消息或 Compaction，因此 baseline 与后续事件组日志不存在重叠。全量恢复只回放 baseline 和该 Operation 日志，当前 Operation 已持久化的用户消息、Assistant 消息和 Compaction 不再从最新 Store 重复读取。

User Bash 当前只在完成并持久化后发送一个 `MessageEnd`，没有可恢复的中间流式事件。它仍通过共享 Projection Sink 实时广播；若恰好断线，则由标准持久化全量 Resume 恢复终态消息。

### 4.4 Resume 响应元数据

成功响应在 `_meta.clawcode.sessionRecovery` 中返回实际恢复结果：

```json
{
  "_meta": {
    "clawcode": {
      "sessionRecovery": {
        "mode": "watch",
        "runId": "run-1",
        "journalTailSequence": 150,
        "running": true
      }
    }
  }
}
```

该元数据只用于诊断，不得覆盖前端 Recovery cursor，也不作为开始应用回放通知的屏障。实时 watcher 可能在 Resume 响应前继续发送更新，因此响应中的 journal tail 只是服务端建立恢复快照时的观察值。前端游标只能由完整应用的投影组推进。ACP Resume 的更新通知可以先于响应到达，因此前端必须在发出 Resume 前安装通知路由。

### 4.5 ACP v2 Multi-Message 批量传输

恢复协议不新增批量通知方法，也不把多个 ACP update 塞入自定义参数。ACP component 与物理 transport 之间增加一个 frame-aware adapter，将当前已经排队且连续的 `session/update` JSON-RPC notification 封装为标准 JSON-RPC 2.0 batch。WebSocket 上一个 batch 表现为一个包含多条完整 JSON-RPC notification 的数组，因此每个成员仍保持标准 ACP v2 `session/update` 方法、参数和 `_meta`。

批量规则如下：

1. 每个 batch 最多包含 `[app.recovery] max_batch_size` 条 notification，避免超大单帧阻塞浏览器主线程；该配置必须为正整数，默认值为 128。
2. adapter 只合并当前已经排队的连续 `session/update`；不使用定时窗口，不为等待更多消息增加实时延迟。
3. 遇到请求、响应、其他 notification、已有 batch 或队列暂时为空时，立即发送当前 batch，再按原顺序发送边界消息。
4. 单条 `session/update` 可以保持单消息 frame；两个及以上连续更新才构造 batch。
5. 除 4.6 节限定的连续文本 chunk 合并外，batch 不改变 Projection Group 的 `runId`、Kernel sequence、`projectionIndex` 或 `projectionCount`。一个 Projection Group 可以跨 batch 边界，前端仍按分组元数据判断完整性。
6. stdio、HTTP 和 WebSocket 共用同一个 frame adapter，确保协议测试和生产 transport 行为一致。

前端 `AcpConnection` 同时接受单个 JSON-RPC 对象和非空 JSON-RPC batch 数组。一个 batch 内的 notification 按数组顺序交给 Controller；响应仍按各自 id 解析。Controller 将同一 transport batch 内所有可完整提交的 Projection Group 解码到临时 WorkspaceState，最后只安装一次已经计算完成的 WorkspaceState，不重复执行同一组 reducer，随后统一推进对应 Recovery cursor。若任一通知解码失败，本批涉及的 Session 都标记为只能全量 Resume，当前连接主动关闭并通过统一重连流程执行恢复，不能提交半批状态。

该优化覆盖 `replayFrom: start`、`_clawcode/event` cursor backlog 和恰好在同一调度轮次产生的实时 Projection Group。实时流没有积压时仍立即发送，不为了批量率牺牲首 token 延迟。

### 4.6 连续文本 chunk envelope 合并

frame adapter 在构造 JSON-RPC batch 前，可以把当前已经排队的连续 `AgentMessageChunk` 或连续 `AgentThoughtChunk` 合并为一条同类型 ACP `session/update` notification。该优化只拼接文本正文，目标是消除同一文本块每个小 delta 重复携带的 JSON-RPC、`session/update`、SessionId、MessageId 和 `_meta` envelope；它不是业务状态压缩，也不修改 Operation journal 中保存的原始 EventGroup。

只有同时满足以下条件的相邻 notification 才可以合并：

1. update 类型同为 `agent_message_chunk`，或同为 `agent_thought_chunk`；两种类型之间不能互相合并。
2. SessionId、RunId、MessageId 和 TurnId 分别相同。
3. 两条 update 都只携带一个文本 ContentBlock、具有相同 annotations，并具有合法的恢复分组元数据；annotations 不同意味着正文语义边界，不能合并。
4. 每个来源 Projection Group 都只包含这一条 update，即 `projectionIndex = 0` 且 `projectionCount = 1`，并且没有 `operationPhase`。
5. 后一条 Kernel sequence 严格大于前一条；允许中间存在没有进入该 Operation journal 的 sequence 空洞。
6. 合并消耗的原始 notification 数量不超过当前 `[app.recovery] max_batch_size`，避免一个合并结果无限增长。

任一条件不满足时，adapter 立即结束当前文本合并段，并按现有顺序处理边界 notification。ToolCall start、ToolCall update、ToolCall end/result、MessageEnd、状态更新、扩展事件、请求、响应和已有 batch 都是不可跨越的边界。adapter 只检查当前已经排队的 notification，不启动 timer、不等待下一条 delta；队列暂时为空时立即发送已经合并的正文，因此没有额外首 token 延迟。

合并后的 notification 继续使用第一条来源事件的 `sequence` 作为 `firstSequence`，并在外层 `_meta.clawcode.sessionRecovery.lastSequence` 中记录最后一条来源事件的 sequence。未携带 `lastSequence` 的普通 Projection Group 等价于 `lastSequence = sequence`。合并结果的 `projectionIndex` 固定为 0、`projectionCount` 固定为 1，并保留第一条来源事件的 RunId；文本内容按来源顺序连接，相同 annotations 保留一份，chunk 级事件 `_meta` 保留区间首事件的数据，恢复终点只由外层 `lastSequence` 表达。用于投影排序和恢复组起点的 sequence 必须保持第一条来源 sequence。

前端把 `[sequence, lastSequence]` 视为一个原子提交区间。只有完整解码并应用该 notification 后，才把 Recovery cursor 推进到 `lastSequence + 1`。如果连接在该 notification 到达前断开，cursor 仍停留在 `sequence`；重连后服务端可以从未压缩的 journal 按原始 EventGroup 重放。实时流和重放流的物理分组允许不同，但拼接后的文本、事件顺序和最终 cursor 必须一致。

由于本优化不修改已经发布或保留的 journal，不引入 journal revision。服务端收到 cursor 后仍按原始 EventGroup sequence 选择 backlog；前端不得产生落在一个已完整提交合并区间内部的 cursor。

## 5. Session 事件流

### 5.1 ACP Projection Registry 所有权

事件日志和广播属于 ACP adapter，不下沉到通用 Kernel `SessionExecution`。`AcpServerFactory` 持有一个 `Arc<ProjectionRegistry>`，HTTP router 在连接工厂闭包之外只创建一次 `AcpServerFactory`，每个 WebSocket component 克隆同一个 Registry。stdio 连接继续使用其 Factory 所属 Registry。

Registry 按 SessionId 保存 `SessionProjection`。Projection 拥有广播 sender、当前或最近 Operation journal、持久化 baseline 和必要的恢复状态，但不持有任何 ACP 连接。`session/new`、`session/resume` 和产品 Fork 成功后为当前连接安装 watcher。

`ProjectionSink` 实现 Kernel `EventSink`，绑定 SessionId、共享 Registry 和只读 Kernel handle。它将一个 Kernel `AgentEvent` 映射为一个 `EventGroup`，然后按以下顺序处理：

1. 在顶层 Operation 起始事件处捕获持久化 baseline，并开始新的 journal；内嵌 Compaction 复用当前 journal。
2. 将完整投影组追加到 journal。
3. 向 Session 广播通道发布同一个投影组。

没有订阅者不是错误，不能中止 Kernel Operation。映射失败或 Projection 内部状态损坏仍返回 `SinkError`，保持当前 EventSink 错误语义。

ACP Prompt、Compact、User Bash 以及其他当前构造连接绑定 `AcpEventSink` 的入口统一改用 Registry 返回的 Session Projection Sink。原连接关闭只会使该连接 watcher 退出，不能关闭 Projection、删除 journal、取消 Token 或释放 ActiveRun lease。Kernel 的 `ActiveRunContext` 继续持有普通 `Arc<dyn EventSink>`，Queue、MCP、Extension diagnostics 和 settlement 事件自然进入同一个 ACP Projection Sink，不需要修改 Kernel 事件分发模型。

### 5.2 日志生命周期

Operation journal 以映射完成的 `EventGroup` 为存储单位，保留一个 Agent Run 从 `RunStart` 到 `AgentSettled` 的完整事件，或一个直接 Slash Command/Compaction 从 Start 到 End 的完整事件。journal 与 baseline 只存在于内存中。

终态事件必须先写入 journal 并广播。Operation 完成后不立即释放 journal，而是作为该 Session 的“最近已完成 Operation”继续保留。它只在以下情况释放或替换：

1. 下一 Operation 起始事件到达；在替换前，新的 baseline 已包含上一 Operation 的持久化 transcript。
2. `session/close` 或 `session/delete` 成功并移除对应 Projection。
3. ACP Server/Kernel 进程结束。

因此客户端在 Agent 刚好于断线期间结束后仍可使用原 RunId 增量补齐 Tool、Retry、Usage 和 Idle。若客户端跨过下一 Operation 才重连，旧 RunId 不再可用，服务端要求持久化全量 Resume。

广播通道允许有限容量，但当前或最近 Operation journal 保留完整投影组，不随广播 lag 淘汰。消费者发生 lag 时根据该 watcher 的 RunId 和 nextSequence 从 journal 重新补偿；RunId 已替换时结束 watcher，使连接下一次恢复走全量 Resume。每个 Session 最多保留一个 Operation baseline 和 journal，避免历史 Operation 在内存中无限累积。

live-only watcher 在发送完整 Operation 起始组后必须记录该组 RunId 和下一 sequence，使 New、Fork 和无 backlog Resume 创建的 watcher 也能在后续 lag 时从 journal 补偿。watcher 发送下一 Operation 的完整起始组后切换到新的 RunId；若在尚未发送新起始组时发生 lag 且旧 RunId 已被替换，则该 watcher 不得猜测 journal。

### 5.3 多连接语义

`session/new`、产品 Fork 和成功的 `session/resume` 都为当前 ACP 连接安装一个 Session watcher。每个连接对同一 Session 最多存在一个 watcher；同一 Session 可以同时拥有多个连接 watcher。连接级 watcher Registry 使用 SessionId 到取消句柄的映射，重复 Resume 原子替换旧 watcher，避免两个任务向同一连接重复投影。

Session 空闲时 watcher 保持订阅，下一次 Prompt 创建 Run 后所有已绑定连接都会收到实时事件。连接关闭只回收该连接创建的 watcher。`session/close` 和 `session/delete` 仍是显式 Session 生命周期操作，可以取消或释放 Session。

## 6. 前端连接状态机

### 6.1 单飞和连接代次

`WorkspaceController` 增加以下连接级状态：

- 单调递增的 connection generation；
- 当前 connect Promise；
- 唯一 reconnect timer；
- 当前 heartbeat timer；
- 当前连接最后接收消息时间；
- 每个 Session 的 Recovery cursor。

每次连接尝试捕获局部 `connection` 和 generation。`connect`、Initialize、Session list、Resume、关闭和错误回调在修改 Controller 状态前都校验两者仍为当前值。旧连接的回调只清理旧连接自身资源，不能清空或关闭新连接。

同一时刻最多一个连接请求处于执行中。调度重连前先检查当前 connect Promise 和 reconnect timer；connect Promise 尚未结束时把调度延后到其清理完成，已有 timer 时不再创建。首次建连、Initialize 或 Session 恢复期间的连接关闭都必须传播为本次尝试失败并进入同一重连流程。连接成功并完成所有 Session 恢复后清除 timer、归零 attempt 并进入 Ready。

### 6.2 重连顺序

重连严格执行：

1. 建立 WebSocket。
2. 安装 notification、diagnostic 和 close handler。
3. 发送带 Session 游标的 Initialize。
4. 校验 ACP 能力和恢复协议版本。
5. 调用 `session/list` 刷新 Session 摘要。
6. 根据 Initialize 恢复计划，对内存中已有 workspace 的 Session 调用标准 Resume。
7. 增量 Resume 失败且确认为 cursor unavailable 时，清空该 Session 投影并全量 Resume。
8. 刷新 Tree、Pending、Skill、MCP 等原子快照。
9. 所有恢复完成后进入 Ready 并允许用户操作。

整个流程使用局部连接实例发请求，不在中途调用可能返回另一代连接的 `requireConnection()`。

没有 retained journal 的持久化 Resume 也必须先建立 Session Projection 实时接收器，再执行 durable replay。另一个 ACP 连接在 Resume 生命周期结束后立即启动 Operation 时，新连接只能观察到 replay/live 重叠，不能落入“回放完成但订阅尚未建立”的事件空窗；重叠部分继续按 sequence 去重。

### 6.3 断线状态

断线只更新全局 Connection 状态，不再派发会把所有 Session `running` 清零的 `sessions/invalidated`。现有 transcript、工具状态和 Running 标记保留到 Resume 返回权威结果。

已经发送但尚未收到响应的 Prompt 在连接关闭时进入 `outcomeUnknown`。前端不自动重发；重连后的 watch 或全量 Resume 决定最终状态。其他普通读取请求可以由调用方重新发起，但不得跨连接复用旧 Promise。

### 6.4 心跳和网络恢复

浏览器 WebSocket API 不暴露协议级 Ping，因此前端只在连接至少 20 秒没有收到任何 ACP 消息时发送一次标准 `session/list` 请求作为存活探测。探测结果不刷新界面；该心跳请求使用独立的 5 秒超时，且同一连接最多一个心跳请求处于执行中。超时后 Controller 显式关闭当前连接并调度重连，不依赖 `AcpConnection.close()` 的显式关闭回调。

每次收到合法 JSON-RPC 消息都更新最后活跃时间。浏览器 `online` 事件或页面从隐藏恢复为可见时，Ready 连接立即执行一次存活检测；尚在建连或恢复的连接继续当前单飞尝试；连接已断开时取消当前退避并立即尝试重连。所有 ACP request 在 WebSocket 不可用时都通过 rejected Promise 报错，不得在 Promise 建立前同步抛出。组件关闭时清除所有 timer 和 DOM listener。

重连使用带抖动的指数退避并设置上限。成功连接后重置退避；同一失败只记录一次诊断，避免多个回调重复刷屏。

## 7. 事件游标与前端去重

前端为每个 Session 保存：

- 当前可恢复 `runId`；
- 下一个期望的 Kernel sequence；
- 当前 sequence 的临时投影组，包括 `lastSequence`、`projectionCount` 和已经收到的 `projectionIndex`。

收到带合法 sessionRecovery 分组元数据且 `operationPhase` 为 `start` 的完整 Operation 起始组后建立 Run cursor。之后只有 RunId 一致且 sequence 单调前进、`lastSequence >= sequence` 的完整投影组才可以按 `projectionIndex` 顺序一次应用，并把 `nextSequence` 推进到已提交 `lastSequence` 加一；缺少 `lastSequence` 时使用当前 sequence。同一个 Kernel 事件映射出的多条 ACP 更新共享 sequence；组未收齐时不得修改正式 Session 投影。WebSocket 和 watcher 都按序发送，广播 lag 也先从 journal 补偿，因此观察到更大 sequence 表示中间序号属于已经合并提交的文本 chunk、直接 ACP 更新或没有 mapper 输出，可以安全前进；sequence 回退、`lastSequence` 回退、RunId 变化但缺少合法起始组、重复 index、越界 index、互相矛盾的 count 或非法 phase 则丢弃当前临时组，记录诊断并把当前 Session 标记为只能全量 Resume。

收到带 `operationPhase: "end"` 的完整组后先应用终态，再清除活跃 Run cursor。Session 的通用 transcript 排序继续使用现有 `receivedOrder`，恢复 cursor 不替代 UI 排序模型。

## 8. 错误与日志

后端增加以下有意义的生命周期日志：

- Initialize 为多少个 Session 选择 watch 或 resume；
- Session watcher 建立、替换和退出；
- 增量 Resume 的 RunId、请求 sequence 和补发事件数量；
- watcher lag 后从哪个 sequence 补偿；
- 游标失效并要求全量 Resume；
- Operation journal 开始、完成、替换和删除；
- 连接 watcher 发送失败，但 Kernel Operation 继续执行。

日志使用完整可读正文参数，不使用 tracing event fields，不记录 Prompt、事件正文、认证头、Cookie、Token 或其他凭据。所有错误返回前记录足够的 SessionId、RunId 和 sequence 上下文。

前端诊断只记录连接代次、重连次数、恢复模式和错误摘要，不记录 ACP 请求正文。

## 9. 测试策略

Rust 修改遵循 TDD，测试代码只放在 crate 级 `tests/` 或标准 `#[cfg(test)] mod tests` 中。

### 9.1 Kernel 集成测试

1. 只读 attachment 校验在 Session 运行时立即返回，不等待 operation gate。
2. attachment 校验拒绝 cwd 不匹配、尚未完成启动或正在关闭的 Session。

### 9.2 ACP Projection 与协议测试

1. 两个 ACP Session watcher 同时收到同一 Run 的完整有序投影组。
2. 释放一个 watcher 不取消 Run，也不影响另一个 watcher。
3. 从 RunId 和 sequence 恢复时先补发 journal，再接收实时事件，边界事件只出现一次。
4. 广播 lag 后可以从 journal 恢复；RunId 已替换时返回明确错误。
5. AgentSettled 进入 journal 和广播后，已完成 journal 仍可增量恢复。
6. 下一 Operation 捕获包含上一 Operation transcript 的 baseline 后才替换旧 journal。
7. 全量恢复严格组合 Operation 前 baseline 和当前/最近 journal，不重复消息。
8. 没有 watcher 时 Projection Sink 仍成功，Kernel Operation 不受连接数量影响。

### 9.3 ACP 集成测试

1. Initialize 根据客户端游标返回 watch 或 resume 计划。
2. 未声明 sessionRecovery 的客户端保持兼容行为。
3. 自定义 `replayFrom: _clawcode/event` 补发缺失更新并持续接收实时更新。
4. 游标失效时，在发送任何 `session/update` 前返回可识别错误。
5. `replayFrom: start` 可以在 Run 运行期间完成，不等待 Run settlement。
6. 同一 Session 的两个 ACP component 都收到实时更新；断开其中一个后 Run 继续。
7. Prompt 发起连接断开后，重连 component 可以补齐并收到 Idle 终态。
8. 一个 Kernel 事件映射为多条 ACP 更新时携带连续的 projection index 和一致的 projection count。
9. Run 在断线期间完成后，重连 component 仍可从最近已完成 journal 补齐非持久化事件。
10. Prompt、Compact、Slash Command 和 User Bash 都不再把事件出口绑定到原连接。
11. 现有断线测试继续证明释放 ACP Client 不取消 Kernel Run。
12. Operation 起止组携带明确 phase，前端只在完整起始组后建立 cursor，并在完整终止组应用后清除 cursor。

### 9.4 前端和真实 WebUI 验收

前端不要求增加单元测试，但必须运行 TypeScript 类型检查、Lint 和生产构建。

ACP adapter 增加 frame batching 单元测试，覆盖单消息透传、2 到配置上限条 notification 合并、超过配置上限一条时分片、非 Session 消息刷新边界和消息顺序，并验证 `max_batch_size = 1` 的合法边界。前端通过静态检查和真实 WebUI 验收确认单对象与 batch 数组都能解码，并且一个恢复 batch 只触发一次 Workspace 状态提交。

连续文本 chunk 合并测试必须覆盖：同一 Agent message chunk 正文按序拼接、同一 Agent thought chunk 正文按序拼接、不同 MessageId、chunk 类型或 annotations 不合并、ToolCall 和其他 update 刷新合并边界、非文本 ContentBlock 不合并、来源组不是单 update 时不合并、合并数量受 `max_batch_size` 限制，以及合并结果携带首 sequence 和正确 `lastSequence`。前端验收必须确认提交合并通知后 cursor 使用 `lastSequence + 1`，而未携带 `lastSequence` 的现有通知仍使用 `sequence + 1`。

WebUI E2E 使用生产 `app` 后端和当前配置 Provider，不使用 fixture backend：

1. 发起一个持续时间足以断线的真实 Agent Run。
2. 中途断开 WebSocket，确认界面仍保持 Running，后端 Run 未取消。
3. 恢复网络，确认无需刷新页面即可补齐事件并继续实时显示。
4. 在两个标签页打开同一 Session，确认两边都收到最终 Assistant 输出和 Idle。
5. 让服务器短暂不可用，确认每个页面只有一个重连链路，恢复后只有一个当前连接。
6. 在 Prompt 响应结果未知的窗口断线，确认前端不重复发送 Prompt。
7. 以本项目目录创建真实 Session，让当前 Provider 执行代码审查；Run 期间断开并恢复浏览器网络，确认 Run 未取消、Running 未错误清零、断线 backlog 通过 JSON-RPC batch 补齐，最终 Assistant 内容和 Idle 各出现一次。
8. 检查恢复流中的 WebSocket frame，确认包含多个标准 `session/update` 成员的 JSON-RPC batch，且单个 batch 不超过当前 `[app.recovery] max_batch_size` 配置值。
9. 检查同一 Assistant 文本块产生积压时，至少一个发送 notification 拼接了多个来源 delta，ToolCall 和终态消息仍保持独立顺序；断线重连后最终文本无重复、无缺失，恢复 cursor 位于合并区间末尾之后。

## 10. 实施边界

实现期间不创建 worktree，不创建 commit。所有新增函数提供英文函数级注释，非平凡代码添加英文原因注释。超过三个字段的 Rust struct 使用 `typed-builder` 和 builder 构造，`Arc` 字段使用显式 `Arc::clone`。日志遵循项目 tracing 约束。

本次不增加数据库 migration 或第三方依赖；优先复用现有 `tokio::sync::broadcast`、ACP `_meta`、Replay cursor、Kernel `EventSink` 和 ACP Server Factory。Kernel 只增加只读 attachment 校验，不承担 ACP watcher、journal 或连接生命周期。
