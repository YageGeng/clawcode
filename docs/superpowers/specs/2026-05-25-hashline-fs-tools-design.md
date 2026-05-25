# Hashline FS Tools 设计

## 背景

当前 `crates/tools/src/builtin/fs` 直接暴露 `read`、`write`、`edit` 和 `apply_patch` 模块。注册时固定注册 `read_file`、`write_file`，再根据 `is_anthropic` 在 Anthropic `edit` 与非 Anthropic `apply_patch` 之间二选一。

目标实现来自 `/home/isbest/Documents/WorkSpace/mcp-hashline-edit-server` 的 hashline 方案。它的核心是让读文件输出 `LINE:HASH|content`，编辑时用 `LINE:HASH` 做行级乐观锁。这样模型不需要复述旧文本，只要引用最近一次读取得到的 anchor，就能做精确行编辑；文件变化后，工具返回带 `>>>` 标记的新 anchor，模型可以据此重试。

本项目第一版需要把现有 fs tools 收纳进子模块，并新增 hashline 子模块。注册时 legacy 与 hashline 二选一。第一版不实现 grep，也不做 arguments consumer 阶段的参数流式预览；hashline edit 的 `execute_streaming()` 只需返回模型可见文本，不发出 `FileChangeItem`。

## 目标

1. 将当前 fs tools 移动到 `crates/tools/src/builtin/fs/legacy/` 下。
2. 新增 `crates/tools/src/builtin/fs/hashline/`，hashline 相关工具、算法和测试都放在该目录下。
3. 让注册入口支持 legacy 与 hashline 两套 fs tools 二选一。
4. hashline 第一版替代当前 `read_file`、`write_file`、文件编辑工具的对外工具名，不新增并行的 `hashline_read_file` 名称。
5. hashline 第一版支持丰富单元测试，覆盖行 hash、anchor 解析、读文件格式、单行替换、范围替换、插入、批量编辑、hash mismatch、空行、前缀剥离、重复编辑、no-op 等边界。

## 非目标

1. 第一版不实现 hashline grep。
2. 第一版不实现 hashline edit 的 `arguments_consumer()` 参数流式预览。
3. 第一版不把现有 `apply_patch` 协议改成 hashline，也不删除 legacy 实现。
4. 第一版不引入新的模型可见工具名；hashline 通过注册选择替代现有工具名。
5. 第一版 hash 算法与 `mcp-hashline-edit-server` 保持一致：对去空白后的单行内容计算 xxHash32，再 `% 256` 输出 2 位小写 hex。

## 源实现行为研究

`mcp-hashline-edit-server` 的 `src/hashline.ts` 负责核心算法：

- `computeLineHash` 对单行去掉所有空白，并去掉行尾 `\r`，再计算短 hash。
- `formatHashLines` 输出 `LINE:HASH|content`，行号从 1 开始。
- `parseLineRef` 接受 `LINE:HASH`，也会容忍从 `LINE:HASH|content` 中复制出来的引用。
- `applyHashlineEdits` 先解析 edits，再预校验所有 anchor hash。
- hash 不匹配时，如果目标 hash 在当前文件中唯一出现，会自动重定位到新行号。
- 如果无法重定位，返回 `HashlineMismatchError`，错误文本展示 mismatch 附近上下文，并用 `>>>` 标记 stale anchor 对应的当前行。
- 批量编辑会去重，并按从文件底部到顶部的顺序应用，避免前面的 splice 改变后续行号。
- 编辑内容会剥离模型误带的 `LINE:HASH|` 前缀或 diff `+` 前缀。
- `insert_after` 会剥离被模型重复写进新内容开头的 anchor line。
- `replace_lines` 会剥离被模型重复写进新内容首尾的范围外上下文。
- 单行替换会恢复旧行缩进，并保留空行。

本项目应复刻这些可观察行为，但实现要贴合 Rust 项目边界：算法层不依赖 MCP/Bun 运行时，工具层通过现有 `FsBackend` 读写。

## 模块结构

目标文件结构：

```text
crates/tools/src/builtin/fs/mod.rs
crates/tools/src/builtin/fs/legacy/mod.rs
crates/tools/src/builtin/fs/legacy/read.rs
crates/tools/src/builtin/fs/legacy/write.rs
crates/tools/src/builtin/fs/legacy/edit.rs
crates/tools/src/builtin/fs/legacy/apply_patch/mod.rs
crates/tools/src/builtin/fs/legacy/apply_patch/stream_parser.rs
crates/tools/src/builtin/fs/hashline/mod.rs
crates/tools/src/builtin/fs/hashline/read.rs
crates/tools/src/builtin/fs/hashline/write.rs
crates/tools/src/builtin/fs/hashline/edit.rs
crates/tools/src/builtin/fs/hashline/format.rs
crates/tools/src/builtin/fs/hashline/model.rs
```

说明：

- `legacy` 只做当前实现的搬迁，行为不变。
- `hashline::format` 放 line hash、line ref parsing、hashline formatting。
- `hashline::model` 放 edit 输入类型、解析后的 edit spec、apply result 和错误类型。
- `hashline::edit` 放 `HashlineEditFile` 工具和编辑应用逻辑。
- `hashline::read` 和 `hashline::write` 放模型可见工具。`write_file` 第一版可以保持当前语义，只迁移到 hashline 模块，输出文案可后续统一。

## 注册设计

新增注册选项：

```rust
pub enum FsToolSet {
    Legacy,
    Hashline,
}
```

注册入口保留当前默认行为，同时新增显式选择入口：

```rust
impl ToolRegistry {
    /// Register the default built-in file-system tools.
    pub fn register_fs_tools(&self, is_anthropic: bool);

    /// Register built-in file-system tools using the selected tool set.
    pub fn register_fs_tools_with_set(&self, is_anthropic: bool, set: FsToolSet);

    /// Register built-in file-system tools using the selected tool set and backend.
    pub fn register_fs_tools_with_backend_and_set(
        &self,
        is_anthropic: bool,
        backend: Arc<dyn FsBackend>,
        set: FsToolSet,
    );
}
```

兼容性：

- `register_fs_tools` 和 `register_fs_tools_with_backend` 默认继续使用 `FsToolSet::Legacy`，避免影响现有调用。
- 需要替代当前实现的位置显式传入 `FsToolSet::Hashline`。
- hashline tool set 注册的模型可见名称仍为 `read_file`、`write_file`、`edit_file`。
- legacy tool set 继续保持现有 `read_file`、`write_file`，并根据 `is_anthropic` 注册 `edit` 或 `apply_patch`。

## Hashline 工具行为

### `read_file`

参数：

```json
{
  "path": "string",
  "offset": "integer optional, 1-indexed",
  "limit": "integer optional",
  "plain": "boolean optional"
}
```

行为：

- 默认 `offset = 1`，默认 `limit = 2000`。
- 返回头部：`File: <path> (<total> lines)`。
- 截断时追加 `[showing lines A-B]` 和 `(<N> more lines below)`。
- 默认正文使用 `LINE:HASH|content`。
- `plain = true` 时正文使用 `LINE|content`，只用于阅读，不用于编辑。
- 第一版可以只支持文本文件；读失败直接返回错误字符串。

### `edit_file`

参数：

```json
{
  "path": "string",
  "edits": [
    { "set_line": { "anchor": "LINE:HASH", "new_text": "string" } },
    {
      "replace_lines": {
        "start_anchor": "LINE:HASH",
        "end_anchor": "LINE:HASH",
        "new_text": "string"
      }
    },
    { "insert_after": { "anchor": "LINE:HASH", "text": "string" } }
  ]
}
```

第一版不实现源项目的 fuzzy `replace`。这样边界更小，也避免和当前 legacy `edit` 的多级 exact/fuzzy matching 语义混在一起。后续如果需要，再把 `replace` 作为 hashline edit 的 fallback 增量加入。

行为：

- 读入文件后统一按 `\n` 归一化处理；写回时第一版可保持 LF。
- `set_line` 替换一个 anchor 行；`new_text = ""` 表示删除该行。
- `replace_lines` 替换闭区间范围；`new_text = ""` 表示删除该范围。
- `insert_after` 在 anchor 行后插入文本；空文本返回错误。
- 所有 anchor 在应用前统一校验，任何不可恢复 mismatch 都阻止写盘。
- hash mismatch 返回上下文诊断，包含 `>>> LINE:HASH|content`。
- 多个 edit 按底部到顶部应用。
- 完全重复的 edit 去重。
- 最终内容与原内容一致时返回 no-op 错误，避免模型误以为已完成变更。
- `execute()` 返回模型文本；`execute_streaming()` 同样返回模型文本，不发出结构化 item。

### `write_file`

hashline tool set 的 `write_file` 第一版可以复用 legacy 语义：写入完整文件内容并返回写入字节数。它放在 `hashline` 模块下，是为了注册时整套替换，而不是表示它必须使用 hashline anchor。

## Hash 算法

第一版使用与源项目一致的 hash 语义：

- 输入：单行内容。
- 处理：去掉行尾 `\r`，删除所有 Unicode whitespace。
- 计算：对处理后的字符串执行 xxHash32。
- 输出：`xxHash32(value) % 256`，格式化为 2 位小写 hex。

原因：

- 用户要求与 `mcp-hashline-edit-server` 保持一致，避免同一 `LINE:HASH` 在两个实现之间不兼容。
- hashline 的安全性来自“行号 + hash + mismatch 诊断”，不是来自 hash 空间足够大。
- 2 位 hash 可能碰撞，所以编辑逻辑仍必须保留“唯一 hash 重定位”和“重复 hash 不重定位”的保护。

实现时应优先使用 Rust xxHash32 crate，并用固定测试向量锁定与 `Bun.hash.xxHash32(value) % 256` 的输出一致。

## 错误处理

错误文本面向模型，优先可操作：

- 无效 anchor：说明期望格式是 `LINE:HASH`。
- 行号越界：说明文件当前行数。
- hash mismatch：返回上下文和 quick fix 映射。
- range start 大于 end：返回 `must be <=`。
- no-op：说明 replacement 与当前内容相同，并提示重新读取。
- 空 edits：返回明确错误，避免静默成功。

## 测试计划

测试全部放在 hashline 模块附近，优先单元测试纯算法，少量 tokio 测试覆盖工具读写。

核心测试：

1. `format_hash_lines` 使用 1-indexed line number，并能从指定 start line 开始。
2. `parse_line_ref` 支持 `5:ab`、`5:ab|content`、`5 : ab`，拒绝 0 行和非法 hash。
3. hash 对空白不敏感，对内容变化敏感。
4. `read_file` 默认返回 hashline fenced block 和文件头。
5. `read_file` 支持 1-indexed offset、limit、plain mode。
6. `set_line` 替换单行、删除单行、扩展为多行。
7. `replace_lines` 替换范围、删除范围、拒绝 start > end。
8. `insert_after` 插入单行、多行、最后一行后插入、拒绝空文本。
9. wrong hash 返回 `>>>` 和 updated `LINE:HASH|content`。
10. hash 唯一重定位可以在行移动后找到目标行。
11. hash 重复时不重定位，返回 mismatch。
12. 多个 edits 在一个调用中 bottom-up 应用。
13. 完全重复 edit 被去重。
14. 替换内容误带 `LINE:HASH|` 前缀时会剥离。
15. 替换内容误带 diff `+` 前缀时会剥离。
16. `insert_after` 剥离 echo 的 anchor line，但不剥离空白行巧合。
17. `replace_lines` 剥离 echo 的边界上下文，但保留真实空行。
18. 相同内容替换返回 no-op 错误，不写盘。
19. `execute_streaming()` 返回模型文本，不发出 `FileChangeItem`。
20. hashline edit 不提供 `arguments_consumer()`。
21. registry 使用 `FsToolSet::Legacy` 时仍注册当前实现。
22. registry 使用 `FsToolSet::Hashline` 时 `read_file` 输出 hashline，`edit_file` 使用 hashline schema。

## 实施顺序

1. 先写 registry 与 legacy 搬迁测试，确保移动当前 fs tools 不改变行为。
2. 移动当前 fs tools 到 `legacy` 子模块，并修正引用路径。
3. 新增 `hashline::format` 和 `hashline::model` 的 failing tests，再实现纯算法。
4. 新增 `hashline::read` 工具测试，再实现 hashline read。
5. 新增 `hashline::edit` 工具测试，按 set/range/insert/mismatch/batch/edge cases 分批 TDD 实现。
6. 新增 `hashline::edit` streaming 测试，锁定模型文本输出，同时确认没有 arguments consumer 和 `FileChangeItem`。
7. 新增 registry hashline 选择测试，并接入注册入口。
8. 跑 `cargo fmt`、`cargo test -p tools`，必要时再跑更广的 workspace 检查。

## 风险与取舍

- 2 位 hash 天然有碰撞风险。设计通过行号校验、唯一 hash 重定位和重复 hash 禁止重定位来降低误编辑风险。
- 第一版 `execute_streaming()` 只返回模型文本，不做结构化 file-change diff。TUI 不会在工具执行完成后看到结构化 diff，只会看到文本输出。
- 第一版需要兼容 Bun hash 输出。测试应覆盖若干源项目 hashline 输出样例，防止后续误改算法。
- 移动 legacy 模块会产生较多路径 diff，但应避免重写其内部逻辑。
