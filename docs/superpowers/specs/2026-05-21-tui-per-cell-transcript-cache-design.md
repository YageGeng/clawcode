# TUI Per-Cell Transcript Cache 与 Active Tail 设计

## 背景

当前 TUI 已经把 transcript 软换行拆到 `crates/tui/src/ui/transcript/`，并引入了 `width + transcript_revision` 级别的全局 wrapped rows cache。这个修复解决了滚轮滚动时反复全量软换行的问题：当 transcript 没有变化时，滚动只复用缓存并截取 viewport 可见行。

但这个设计仍有一个高成本路径：只要 ACP streaming 到来一个新 token、tool output 或 tool status 更新，`AppState::transcript_revision` 就会递增，导致整个 transcript wrapped rows cache 失效。长会话中历史 transcript 很大时，模型持续输出会反复重算全部历史，仍可能阻塞 TUI 事件循环，表现为滚动卡顿、输入延迟、甚至 Ctrl-C 响应变慢。

Codex 的 TUI 采用更细粒度的结构：历史 cell 作为 committed transcript 保存，当前仍在变化的内容放在 active cell / live tail 中。稳定历史不随 streaming token 重新渲染，只有 active tail 会频繁更新。

## 目标

1. 将 transcript 渲染缓存从“全局 revision”升级为“per-cell revision + width”缓存。
2. 引入 committed transcript 与 active tail 的概念，使 streaming 更新只重算当前活跃 cell。
3. 保留当前本地 TUI 的用户可见行为：滚动、tail-follow、tool preview、diff preview、状态栏和 approval overlay 不改变。
4. 保持 ACP 协议边界不变，不要求 kernel/acp 层改变事件形状。
5. 为后续进一步靠近 Codex 的 `HistoryCell` 风格保留清晰模块边界。

## 非目标

1. 不在本版本引入 Codex 的完整 terminal scrollback/reflow 系统。
2. 不重写 markdown renderer、diff renderer 或 tool cell 的展示规则。
3. 不引入 Codex 的完整 `HistoryCell` 类型层级，但本版本会把本地 transcript cell 升级为 trait object，以便渲染缓存和 active tail 能按 cell 边界工作。
4. 不引入新的第三方 wrapping crate；继续使用当前 `ui/transcript/wrap` 模块内的算法。
5. 不优化所有 `String` 构造，只处理滚动和 streaming 期间的重复渲染热路径。

## 现状问题

当前关键路径如下：

```text
ACP event
  -> AppState mutates transcript
  -> bump global transcript_revision
  -> render_transcript cache miss
  -> every TranscriptCell display_lines(width, theme)
  -> every logical line soft-wrap
  -> whole transcript rows cached
  -> viewport slice rendered
```

这个结构对“滚动但 transcript 不变”有效，对“长历史 + streaming 持续追加”仍然昂贵，因为最后一个 assistant cell 变化会让所有历史 cell 重新渲染。

## Codex 对照

Codex 的相关设计点：

1. `HistoryCell` 是显示单元，每个 cell 暴露 `transcript_lines(width)` 和 `desired_height(width)`。
2. committed history cells 与当前 active cell 分开管理。
3. active cell 变化时只 bump `active_cell_revision`。
4. active cell 可以在完成时 flush/commit 成 history cell。
5. transcript overlay 会把 committed cells 与 active live tail 拼接，但 live tail 有独立 cache key。

本项目不需要一次性复制 Codex 全量架构，但应该采用同样的职责边界。

## 设计概览

新增一个 transcript render model：

```text
AppState
  committed transcript cells
  active tail cell id / index
  per-cell revision

ViewState / transcript cache
  cell_id + width + cell_revision -> wrapped lines
  active_tail_key + width + active_revision -> wrapped lines

render_transcript
  collect committed cached rows
  append active tail cached rows
  compute viewport
  render visible rows
```

本版本将 `TranscriptCell` 从 enum 调整为 trait object。entry 保存稳定 id、revision、状态和具体 cell 实例：

```rust
pub struct TranscriptEntry {
    id: TranscriptEntryId,
    revision: u64,
    state: TranscriptEntryState,
    cell: Box<dyn TranscriptCell>,
}

pub enum TranscriptEntryState {
    Committed,
    Active,
}
```

`TranscriptEntryId` 使用单调递增 `u64`，只作为 TUI 内部 cache key，不暴露给 ACP。

本地 trait 应参考 Codex 的 `HistoryCell` 重新设计。Codex 的核心点是：cell 自己产生 rich display lines、raw lines、transcript overlay lines，并提供默认高度计算；downcast 通过 `impl dyn HistoryCell` 提供，而不是把 `as_any` 写进 trait 必选方法。

本项目保留 `Theme` 参数，因为当前 TUI theme 不是全局常量。建议形状如下：

```rust
pub enum TranscriptRenderMode {
    Rich,
    Raw,
}

pub trait TranscriptCell: std::fmt::Debug + Send + Sync + std::any::Any {
    fn display_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>>;

    fn raw_lines(&self) -> Vec<Line<'static>>;

    fn display_lines_for_mode(
        &self,
        width: u16,
        theme: &Theme,
        mode: TranscriptRenderMode,
    ) -> Vec<Line<'static>> {
        match mode {
            TranscriptRenderMode::Rich => self.display_lines(width, theme),
            TranscriptRenderMode::Raw => self.raw_lines(),
        }
    }

    fn desired_height(&self, width: u16, theme: &Theme) -> u16 {
        self.desired_height_for_mode(width, theme, TranscriptRenderMode::Rich)
    }

    fn desired_height_for_mode(
        &self,
        width: u16,
        theme: &Theme,
        mode: TranscriptRenderMode,
    ) -> u16 {
        Paragraph::new(Text::from(self.display_lines_for_mode(width, theme, mode)))
            .wrap(Wrap { trim: false })
            .line_count(width)
            .try_into()
            .unwrap_or(0)
    }

    fn transcript_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        self.display_lines(width, theme)
    }

    fn desired_transcript_height(&self, width: u16, theme: &Theme) -> u16 {
        let lines = self.transcript_lines(width, theme);
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .line_count(width)
            .try_into()
            .unwrap_or(0)
    }

    fn text(&self) -> &str;

    fn is_stream_continuation(&self) -> bool {
        false
    }

    fn transcript_animation_tick(&self) -> Option<u64> {
        None
    }
}

impl dyn TranscriptCell {
    pub fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    pub fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
```

和 Codex 的差异：

## 数据模型

### TranscriptEntryId

`TranscriptEntryId(u64)` 是 TUI 内部稳定 id。每次创建新的 transcript entry 时递增。它解决两个问题：

1. Vec index 可能因为将来删除/压缩历史而变化，不适合作为长期 cache key。
2. 同样内容的 cell 也必须区分，否则 cache 可能错误复用。

### TranscriptEntry

`TranscriptEntry` 包含：

```text
id
revision
state
Box<dyn TranscriptCell>
```

当 cell 内容变化时，只 bump 该 entry 的 revision。历史 committed entry 不再因为 active tail 变化而失效。

由于 trait object 不能自动 derive `Clone`、`PartialEq`、`Eq`，本版本应同步调整依赖这些 derive 的测试和结构：

1. `AppState` 不再 derive `Clone`、`PartialEq`、`Eq`。
2. `TranscriptEntry` 不要求 clone；测试通过公开只读访问器或 rendered lines 验证行为。
3. 如果个别测试确实需要复制 cell 内容，应构造新的 fixture，而不是 clone production state。

### Active Tail

Active tail 是“当前仍可能变化的最后一个 transcript entry”。第一版规则：

1. Assistant message chunk 会追加到最后一个 active assistant entry；如果不存在，则创建 active assistant entry。
2. Agent thought chunk 会追加到最后一个 active reasoning entry；如果角色变化，则先 commit 旧 active，再创建新的 active entry。
3. Tool call / tool update 对应的 tool entry 在 status 为 `Pending` 或 `InProgress` 时视为 active；到 `Completed` 或 `Failed` 后 commit。
4. User message、system error 默认直接 committed，因为它们创建后不再流式变化。
5. Turn finish 时，将仍存在的 assistant/reasoning active entry commit。

如果存在多个并发 tool call，第一版不强行只有一个 active entry。每个 tool entry 自己有 revision；“active tail”在实现上可以是多个 mutable entries，但缓存仍是 per-entry。命名上保留 `Active` 是为了表达“未稳定、可能继续变化”。

## 渲染缓存

新增 `TranscriptRenderCache`，替代当前单个 `TranscriptLinesCache`。

建议结构：

```rust
pub struct TranscriptRenderCache {
    width: u16,
    entries: HashMap<TranscriptEntryId, CachedEntryLines>,
}

pub struct CachedEntryLines {
    revision: u64,
    lines: Vec<Line<'static>>,
}
```

缓存策略：

1. width 变化时清空所有 entry cache。
2. entry revision 不变时直接复用 wrapped lines。
3. entry revision 变化时只重算该 entry。
4. entry 被删除或 archive 时，清理对应 cache。
5. viewport 渲染仍只 clone 可见 rows，保持当前行为。

缓存应调用 `TranscriptCell::transcript_lines(width, theme)` 作为 transcript 主渲染来源，而不是直接调用 `display_lines`。这和 Codex 一致：大多数 cell 的 transcript lines 等于 display lines，但 tool/exec 类 cell 可以在 transcript 中提供更适合回看或复制的展开形态。

为了避免每帧构造完整 `Vec<Line>`，可以采用两阶段：

1. 第一版：构造 `Vec<&[Line]>` 或轻量 row spans 后计算 viewport，再 clone 可见行。
2. 如果实现复杂，可以先保留拼接 `Vec<Line>`，但必须保证只从 per-cell cache clone，而不是重新 wrap 全部 cell。

第一版验收标准是“不重新 wrap 全部历史”；不是完全零拷贝。

## 状态更新规则

### UserMessageChunk

创建 committed user entry。它不进入 active 状态。

### AgentMessageChunk

如果最后一个 active entry 是 assistant，则 append 并 bump 该 entry revision。否则创建 active assistant entry。

这会替代当前 `append_to_last_or_push(&mut transcript, text, TextRole::Assistant)` 的全局 revision 行为。

### AgentThoughtChunk

同 assistant，但角色为 reasoning。assistant 与 reasoning 不应合并到同一个 entry。

### ToolCall

根据 `ToolCallId` 找到或创建 tool entry。更新 name、arguments、status、content 后 bump entry revision。

如果 status 是 terminal 状态，则将 entry 标记为 committed。

### ToolCallUpdate

只 bump 对应 tool entry revision。不要 bump 全局 transcript revision。

如果 update.status 变成 terminal 状态，则标记 committed。

### FinishPrompt

将所有 active assistant/reasoning entries 标记为 committed。Tool entry 是否 committed 仍以 tool status 为准。

## 模块组织

建议调整为：

```text
crates/tui/src/ui/transcript/
  mod.rs        // render_transcript 入口
  cache.rs      // TranscriptRenderCache, CachedEntryLines
  entry.rs      // TranscriptEntry, TranscriptEntryId, TranscriptEntryState
  viewport.rs   // viewport offset 与 visible row selection
  wrap/
    mod.rs      // soft wrap 对外入口
    line.rs     // 单个 styled logical line 的 wrap
    chars.rs    // StyledChar flattening 与 span rebuild
    boundary.rs // wrap boundary、trim、skip whitespace
```

`ui/state.rs` 负责 ACP event -> transcript entry mutation。为了避免 `state.rs` 继续变大，可在第二步拆出：

```text
crates/tui/src/ui/transcript/state.rs
```

但第一版可以先不拆，等 entry API 稳定后再迁移。

### Wrap 模块边界

当前 `ui/transcript/wrap.rs` 已经包含四类职责：

1. 多 logical lines 到 terminal rows 的入口。
2. 单行按 display width 软换行。
3. `Line` / `Span` 到 `StyledChar` 的 flatten。
4. wrap boundary、尾部空白裁剪、前导空白跳过、span 重建。

per-cell cache 实现会继续调用并测试这些能力，如果仍放在一个 `wrap.rs` 中，文件会很快变成新的大 helper 集合。设计上应把 `wrap.rs` 升级为 `wrap/` 目录，`wrap/mod.rs` 只暴露 `wrap_display_lines` 这一类入口，其余细节拆到 `line.rs`、`chars.rs`、`boundary.rs`。这样后续如果引入 URL-aware wrapping、range wrapping 或 incremental wrapping，也可以继续按职责扩展，而不是继续加长单文件。

第一版实现时，如果改动量需要控制，可以先创建 `wrap/` 目录并做机械搬迁，不改变算法。per-cell cache 的逻辑不应直接依赖 `StyledChar` 或 boundary helper，只依赖 `wrap::wrap_display_lines`。

## 测试策略

### 单元测试

1. `TextCell` 和 `ToolCallCell` 实现 `TranscriptCell` trait。
2. `display_lines_for_mode(Raw)` 委托到 `raw_lines`。
3. `transcript_lines` 默认等于 `display_lines`，并允许 tool 类 cell 覆写。
4. `AgentMessageChunk` 连续追加时，只 bump active assistant entry revision。
5. user message 创建 committed entry，不影响已有 assistant cache key。
6. tool update 只 bump 对应 tool entry revision。
7. terminal tool status 会把 tool entry 标记为 committed。
8. width 不变且 entry revision 不变时，cache 不重算 entry。
9. width 变化时，cache 清空并重算 entries。
10. viewport 只渲染可见 rows，保持 manual scroll 与 follow-tail 语义。

### 渲染回归测试

保留当前已有 render 测试：

1. long assistant lines wrap。
2. streaming keeps latest wrapped line live。
3. long unbroken streaming text wrap。
4. manual scroll shows older output。
5. tool output carriage return rendering。
6. apply_patch diff preview。

### 性能型测试

新增一个轻量计数测试，不做 wall-clock 断言：

1. 构造 100 个 committed text entries 和 1 个 active assistant entry。
2. 首次 render 触发 101 次 wrap。
3. active entry append 后再次 render，只允许 active entry 重算。
4. scroll-only render 不允许任何 entry 重算。

该测试需要让 cache build path 可以注入计数器，或把 `TranscriptRenderCache::render_entry_lines` 拆成可测试 helper。

## 风险与取舍

1. trait object 会增加动态分发和 downcast 复杂度，但它让 cell 边界与 Codex 的 `HistoryCell` 风格一致，也为后续新增 cell 类型降低 enum churn。
2. tool call updates 当前依赖 `tool_call_indices: HashMap<String, usize>`。引入 entry 后需要确保 index 指向 entry vec，而不是旧 transcript vec。
3. active/committed 状态必须和 ACP status 一致，否则可能出现工具仍在流式输出但被错误缓存为稳定状态。
4. 如果未来支持删除/折叠 transcript，entry id 比 index 更安全。
5. per-cell cache 会增加内存占用，但这是用内存换 CPU。缓存只存当前 width 的 rows，resize 后清空，成本可控。
6. trait object 不适合继续保留 enum 的派生比较测试；测试应转向行为断言，例如 rendered text、entry state、entry revision 和 cache rebuild count。

## 验收标准

1. 滚轮滚动不触发任何 soft wrap 重算。
2. assistant streaming 只重算当前 assistant active entry，不重算旧历史。
3. tool output streaming 只重算对应 tool entry。
4. resize 后能正确重算所有可见 transcript 内容。
5. 当前 TUI render 测试全部通过。
6. `rtk cargo test -p tui -- --nocapture` 通过。
7. `rtk cargo clippy --all-targets --all-features --locked -- -D warnings` 通过。
8. `rtk pre-commit run --all-files` 通过。

## 实施顺序建议

1. 引入 `TranscriptCell` trait，并让现有 `TextCell`、`ToolCallCell` 实现该 trait。
2. 引入 `TranscriptEntry`、`TranscriptEntryId`、`TranscriptEntryState`，entry 持有 `Box<dyn TranscriptCell>`。
3. 将 `AppState.transcript: Vec<TranscriptCell enum>` 替换为 `Vec<TranscriptEntry>`，同步调整 tool index。
4. 将全局 revision cache 替换为 `TranscriptRenderCache`。
5. 将 `render_transcript` 改为按 entry 获取 cached rows。
6. 加入 active/committed 状态转换规则。
7. 添加计数型性能测试。
8. 跑全量验证。

## Open Questions

1. assistant/reasoning 在同一 turn 内是否允许多个 active entries 并存？建议第一版允许，但不同角色分开 entry。
2. tool call terminal status 后是否可能继续收到 late output？如果 ACP 可能出现 late update，则 terminal 状态后收到更新仍应 bump revision，但保持 committed。
3. 是否需要 LRU 或最大缓存行数？第一版不需要，只缓存当前 transcript 与当前 width。
