# TUI Tool Call Display Design

## 背景

当前 TUI 的 tool call 显示有两个问题：

1. `AppState` 将普通对话放在 `transcript`，但将 tool call 放在独立的 `tool_calls` map。
2. `render_transcript` 先渲染 user/assistant/system，再把所有 tool call 统一追加到下方。

这个结构会导致运行中 tool call 像一个全局尾部面板，而不是对话历史的一部分。完成后的历史显示也难以和运行中显示保持一致。

Codex 的 TUI 参考实现把 command execution 建模成一个 history cell：运行中是 active cell，完成后同一个 cell flush 到 history。live 和 history 使用同一套 `display_lines`。本设计借鉴这个方向，但不完整移植 Codex 的 command parser、grouping 和 transcript overlay，先做适合 clawcode ACP tool event 的轻量版本。

## 目标

1. tool call 必须作为 transcript 中的有序 cell 渲染，不能再作为全局 map 统一追加到 assistant 后面。
2. live tool call 和 history tool call 使用同一套显示规则。
3. 默认只显示 output 预览，最多前 5 行。
4. 不同类别工具需要有清晰的标题、参数摘要和 output 预览规则。
5. 运行中输出更新时原地刷新对应 cell，不重复追加完整 tool call。
6. 保留当前 borderless Codex 风格，不引入新的框线容器。
7. 第一版不提供 tool call 折叠/展开快捷键；selection/focus 与单个 tool call 展开后续单独设计。

## 非目标

1. 不在本阶段实现完整 output pager。
2. 不在本阶段实现全局或 per-tool-call 折叠状态。
3. 不在本阶段移植 Codex 的 read/search/list grouping。
4. 不在本阶段实现 transcript selection/focus。
5. 不修改 ACP 协议或 kernel event 语义。
6. 不改变工具执行结果传给模型的内容，只调整 TUI 渲染。

## 总体方案

### 状态模型

将 `TranscriptCell` 扩展为：

```rust
enum TranscriptCell {
    Assistant(String),
    Reasoning(String),
    User(String),
    System(String),
    ToolCall(ToolCallView),
}
```

`ToolCallView` 继续保存 `call_id`、`name`、`arguments`、`output`、`status`。为了高效原地更新，`AppState` 内保留一个 `tool_call_indices: HashMap<String, usize>`，它只作为 transcript cell 的索引，不再作为独立渲染来源。

事件处理规则：

1. 收到 `SessionUpdate::ToolCall`：
   - 如果 `call_id` 已存在，更新对应 `ToolCall(ToolCallView)`。
   - 如果不存在，在 transcript 尾部插入新的 `ToolCall` cell，并记录索引。
2. 收到 `SessionUpdate::ToolCallUpdate`：
   - 如果 `call_id` 已存在，原地更新 title/raw input/output/status。
   - 如果不存在，插入 pending placeholder，再原地更新。
3. 收到 assistant/user/reasoning chunk：
   - 继续按原规则追加或合并对应文本 cell。
   - 不把 tool output 合并到 assistant 文本。

### 渲染结构

`render_transcript` 只遍历 `state.transcript()`，不再遍历 `state.tool_calls()`。

tool call 的基础格式：

```text
• Running <summary>
  └ <output preview line 1>
    <output preview line 2>
```

完成成功：

```text
• Ran <summary>
  └ <output preview line 1>
```

失败：

```text
• Failed <summary>
  └ <error/output preview line 1>
```

无输出：

```text
• Ran <summary>
  └ (no output)
```

超过 5 行：

```text
• Ran <summary>
  └ line 1
    line 2
    line 3
    line 4
    line 5
    ... +N lines
```

第一版始终显示同一种 preview：标题加最多前 5 行 output。不存在全局折叠模式，也不存在单个 tool call 展开状态。

## Tool 分类与显示规则

### 1. Shell 工具

匹配工具名：`shell`

参数：

- `command`
- `cwd`

标题摘要：

```text
<command>
```

如果 `cwd` 存在并且不是当前 session cwd，追加：

```text
 · cwd: <cwd>
```

运行中预览：

```text
• Running cargo test -p tui
  └ compiling tui...
```

完成预览：

```text
• Ran cargo test -p tui
  └ exit code: 0
    stdout:
    test result: ok
```

失败预览：

```text
• Failed cargo test -p tui
  └ exit code: 101
    stderr:
    error[E0425]: cannot find value
```

特殊处理：

- 复用当前 terminal output normalize 逻辑：去 ANSI control sequence，处理 carriage return 覆盖。
- 不再展示完整 stdout/stderr，只取前 5 个显示行。
- shell 输出通常已经包含 `exit code/stdout/stderr`，预览直接基于 normalized output。

### 2. 文件读取工具

匹配工具名：`read_file`

参数：

- `path`
- `offset`
- `limit`

标题摘要：

```text
Read <path>
```

如果存在 `offset/limit`：

```text
Read <path> · lines <offset>..<offset + limit>
```

完成预览：

```text
• Ran Read crates/tui/src/ui/render.rs · lines 120..180
  └ fn render_transcript(...)
    let max_scroll = ...
```

特殊处理：

- output 可能是文件内容，默认最多前 5 行。
- 不加 `args:` 块，路径和 range 已经在标题里表达。

### 3. 文件写入工具

匹配工具名：`write_file`

参数：

- `path`
- `content`

标题摘要：

```text
Write <path>
```

运行中：

```text
• Running Write docs/spec.md
```

完成：

```text
• Ran Write docs/spec.md
  └ wrote 2048 bytes to /repo/docs/spec.md
```

特殊处理：

- 不预览 `content` 参数，避免大段内容刷屏。
- output 通常是简短确认文本，仍按最多 5 行处理。

### 4. 精确编辑工具

匹配工具名：`edit`

参数：

- `filePath`
- `oldString`
- `newString`
- `replaceAll`

标题摘要：

```text
Edit <filePath>
```

如果 `replaceAll = true`：

```text
Edit <filePath> · replace all
```

完成：

```text
• Ran Edit crates/tui/src/ui/state.rs
  └ edited /repo/crates/tui/src/ui/state.rs: replaced 1 occurrence(s)
```

失败：

```text
• Failed Edit crates/tui/src/ui/state.rs
  └ oldString not found in content
```

特殊处理：

- 不预览 `oldString/newString` 全文。
- 第一版不在标题中追加 `oldString/newString` 预览，只显示文件路径和执行结果。

### 5. Apply Patch 工具

匹配工具名：`apply_patch`

参数：

- `patchText`

标题摘要：

```text
Apply patch
```

完成 output 来自 prepared hunk summary：

```text
• Ran Apply patch
  └ add docs/spec.md
    update crates/tui/src/ui/render.rs
```

失败：

```text
• Failed Apply patch
  └ failed to read crates/tui/src/ui/render.rs: ...
```

特殊处理：

- 默认不直接预览 `patchText`，因为 patch 可能很长。
- 如果 output 为空但参数中能解析到 patch headers，可以显示 patch header 预览：

```text
• Running Apply patch
  └ *** Update File: crates/tui/src/ui/render.rs
    @@
```

实现阶段先以 output 预览为主，patch header 预览可作为 P2。

### 6. Skill 工具

匹配工具名：`skill`

参数：

- `name`

标题摘要：

```text
Load skill <name>
```

完成：

```text
• Ran Load skill rust-best-practices
  └ <skill_content name="rust-best-practices">
    # Skill: rust-best-practices
```

特殊处理：

- skill output 通常很长，默认最多前 5 行。

### 7. Subagent 管理工具

匹配工具名：

- `spawn_agent`
- `send_message`
- `followup_task`
- `wait_agent`
- `list_agents`
- `close_agent`

显示规则：

`spawn_agent`：

```text
• Running Spawn agent explorer: inspect-tui
```

```text
• Ran Spawn agent explorer: inspect-tui
  └ {"agent_path":"...","nickname":"..."}
```

`send_message`：

```text
• Ran Send message to Tesla
  └ message sent
```

`followup_task`：

```text
• Ran Follow up Tesla
  └ followup sent
```

`wait_agent`：

```text
• Running Wait agent Tesla
```

```text
• Ran Wait agent Tesla
  └ ["Tesla: completed"]
```

`list_agents`：

```text
• Ran List agents
  └ ["Tesla","Galileo"]
```

`close_agent`：

```text
• Ran Close agent Tesla
  └ agent Tesla closed
```

特殊处理：

- 消息内容只进入标题的短摘要，不显示完整 content。
- content 标题预览最多 80 个字符，超出追加 `...`。
- 输出按最多 5 行处理。

### 8. MCP 工具

匹配工具名：

```text
mcp__<server>__<tool>
```

标题摘要：

```text
MCP <server>/<tool>
```

如果参数里有常见目标字段，追加目标：

- `path`
- `file`
- `query`
- `url`
- `name`

示例：

```text
• Running MCP filesystem/read_file · README.md
```

完成：

```text
• Ran MCP filesystem/read_file · README.md
  └ # Project
    ...
```

失败：

```text
• Failed MCP github/create_issue · clawcode
  └ permission denied
```

特殊处理：

- MCP schema 不稳定，不能按具体工具硬编码。
- 使用 tool name 解析 server/tool，用常见字段生成摘要。
- output 统一按最多前 5 行处理。

### 9. 未知工具

匹配规则：任何未被上述类别识别的 tool name。

标题摘要：

```text
<tool name> <compact args>
```

示例：

```text
• Running custom_tool {"id":"123"}
```

完成：

```text
• Ran custom_tool {"id":"123"}
  └ output line 1
```

特殊处理：

- 参数摘要最多 120 个字符。
- output 统一最多前 5 行。

## Output 预览规则

1. 将 tool output 先转换为 display lines：
   - 去 ANSI control sequences。
   - 处理 `\r` 覆盖行。
   - 按 `\n` 切分。
   - 保留空输出为 `(no output)`。
2. 第一版始终显示最多前 5 行 preview。
3. 超过 5 行时追加：

```text
    ... +N lines
```

4. output 行统一使用 dim style；标题使用 bold。
5. 不显示完整 `arguments` 块，除非工具没有可识别摘要。

## 状态到文案映射

| ACP status | Header verb |
| --- | --- |
| `Pending` | `Queued` |
| `InProgress` | `Running` |
| `Completed` | `Ran` |
| `Failed` | `Failed` |
| unknown | `Tool` |

颜色建议：

- running/pending：默认色或 dim bullet。
- completed：green bullet。
- failed：red bullet。

## 与历史回放的一致性

完成后的 tool call 不迁移到另一个结构，也不重新格式化成另一种历史文本。它仍然是 transcript 中同一个 `ToolCall` cell，只是 status/output 已更新。

这保证：

1. 运行中看到的位置就是历史中保留的位置。
2. assistant 文本不会被 tool output 覆盖。
3. terminal resize、scroll、mouse wheel 都只面对一条统一 transcript。

## 测试计划

1. `AppState`：
   - `ToolCall` 插入 transcript cell。
   - `ToolCallUpdate` 原地更新已有 transcript cell。
   - update 先于 snapshot 到达时创建 pending placeholder。
   - assistant chunk 不合并到 tool cell。
2. `render`：
   - shell tool 只显示前 5 行 output。
   - read_file 标题显示 path/range。
   - write_file 不显示 content 参数。
   - edit 不显示 oldString/newString 全文。
   - apply_patch 显示 output summary。
   - subagent 工具显示目标和短消息摘要。
   - MCP 工具解析 `mcp__server__tool`。
   - unknown tool 使用 fallback。
   - 所有 tool call 都显示最多前 5 行 preview。
3. 回归：
   - terminal output `\r` 覆盖逻辑仍生效。
   - transcript scroll 仍能看到历史 tool cell。
   - borderless transcript/composer 测试保持通过。

## 实施顺序

1. 添加 `TranscriptCell::ToolCall` 和索引更新逻辑。
2. 将 `AppState` 的 tool call event 应用改为 transcript-first。
3. 提取 `ToolDisplayKind` / `ToolDisplaySummary`，集中处理 tool 分类和摘要。
4. 替换 `append_tool_call_lines`，让它只接收单个 `ToolCallView` 并使用新 preview 规则。
5. 删除 `render_transcript` 中的全局 `state.tool_calls()` 追加逻辑。
6. 更新旧测试并补齐各类工具渲染测试。
7. 运行 `cargo fmt --check -p tui`、`cargo test -p tui`、`cargo clippy -p tui --all-targets -- -D warnings`。

## 设计决策

1. 第一版移除 `Ctrl+T` tool preview toggle。
2. `apply_patch` 运行中不解析完整 `patchText`；第一版以 output summary 为准，patch header 预览放到 P2。
3. 第一版不实现完整 output pager，只保留最多前 5 行预览。
4. 单个 tool call 展开、selection、focus、鼠标命中测试作为后续同一批能力设计。
