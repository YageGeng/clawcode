# Apply Patch Tool Spec

## 概述

`apply_patch` 是一个文件编辑工具，使用自定义的 patch 格式在一次调用中完成多文件的增、删、改、重命名操作。核心思路：解析 → 验证（预演，确保所有 old 行都能匹配）→ 权限检查 → 写入 → 后处理（格式化 + LSP 诊断）。

---

## 1. 工具定义

### 注册信息

| 字段 | 值 |
|---|---|
| **name** | `apply_patch` |
| **label** | `apply_patch` |

### 参数 Schema

```
{
  patchText: string   // 必填。完整的 patch 文本，描述所有文件变更
}
```

仅有一个字符串参数 `patchText`，将整个 patch 内容作为原始文本传入。

### Description（注入系统提示的完整描述）

> Use the `apply_patch` tool to edit files. Your patch language is a stripped-down, file-oriented diff format designed to be easy to parse and safe to apply. You can think of it as a high-level envelope:
>
> ```
> *** Begin Patch
> [ one or more file sections ]
> *** End Patch
> ```
>
> Within that envelope, you get a sequence of file operations.
> You MUST include a header to specify the action you are taking.
> Each operation starts with one of three headers:
>
> - `*** Add File: <path>` — create a new file. Every following line is a `+` line (the initial contents).
> - `*** Delete File: <path>` — remove an existing file. Nothing follows.
> - `*** Update File: <path>` — patch an existing file in place (optionally with a rename).
>
> Example patch:
>
> ```
> *** Begin Patch
> *** Add File: hello.txt
> +Hello world
> *** Update File: src/app.py
> *** Move to: src/main.py
> @@ def greet():
> -print("Hi")
> +print("Hello, world!")
> *** Delete File: obsolete.txt
> *** End Patch
> ```
>
> It is important to remember:
>
> - You must include a header with your intended action (Add/Delete/Update)
> - You must prefix new lines with `+` even when creating a new file

### 执行函数签名

```
execute(params: { patchText: string }, ctx: ToolContext) => Promise<ToolResult>
```

返回结构：

```
{
  title: string           // 摘要行
  output: string          // 完整输出（含 LSP 诊断）
  metadata: {
    diff: string          // 全部文件的 unified diff 拼接
    files: Array<{        // 每个文件的变更详情，供 UI 渲染
      filePath: string
      relativePath: string
      type: "add" | "update" | "delete" | "move"
      patch: string
      additions: number
      deletions: number
      movePath?: string
    }>
    diagnostics: Record<string, Diagnostic[]>
  }
}
```

---

## 2. Patch 格式

### 外层信封

```
*** Begin Patch
[一个或多个文件操作]
*** End Patch
```

必须包含 `*** Begin Patch` 和 `*** End Patch` 标记。两个标记之间的内容是解析范围，即使输入包含额外的对话文本也能正确定位。

### 三种文件操作

#### Add File（新建）

```
*** Add File: path/to/file.ext
+第一行内容
+第二行内容
```

- 每行以 `+` 开头，`+` 后为实际内容
- 末尾自动加换行符
- 自动创建父目录
- 如果文件已存在则覆写

#### Delete File（删除）

```
*** Delete File: path/to/file.ext
```

- 无后续行，仅头部
- 文件必须存在

#### Update File（修改）

```
*** Update File: path/to/file.ext
*** Move to: path/to/new.ext           ← 可选，重命名
@@ some context line
 unchanged line
-removed line
+added line
```

- `@@` 开头的行是 context 提示（可选，用于定位匹配起点）
- 空格开头：保持行（同时出现在 old 和 new 中）
- `-` 开头：删除行（仅 old）
- `+` 开头：添加行（仅 new）
- `*** Move to:` 跟在 Update File 后表示重命名
- 一个 Update File 可包含多个 `@@` hunk，按顺序匹配
- `*** End of File` 表示 hunk 应从文件末尾匹配（EOF 锚定）

### 完整示例

```
*** Begin Patch
*** Add File: hello.txt
+Hello world
*** Update File: src/app.py
*** Move to: src/main.py
@@ def greet():
-print("Hi")
+print("Hello, world!")
*** Delete File: obsolete.txt
*** End Patch
```

---

## 3. 数据结构

### Hunk 类型

| 类型 | 字段 | 含义 |
|---|---|---|
| `add` | `path`, `contents` | 新建文件及其内容 |
| `delete` | `path` | 删除文件 |
| `update` | `path`, `move_path?`, `chunks[]` | 修改文件，可选重命名 |

### UpdateFileChunk

```
{
  old_lines: string[]    // 原文件中要匹配删除的行
  new_lines: string[]    // 替换后的新行
  change_context?: string // @@ 后的上下文提示
  is_end_of_file?: boolean // 是否 EOF 锚定
}
```

---

## 4. 解析流程

```
patchText
  │
  ├─ stripHeredoc()         // 剥离 bash heredoc 包装（如 bash -lc 'apply_patch <<"EOF"...EOF'）
  │
  ├─ 定位 *** Begin Patch / *** End Patch 标记
  │   └─ 缺失或顺序错误 → 抛出 "missing Begin/End markers"
  │
  ├─ 遍历标记间行，按头部识别操作类型
  │   ├─ parsePatchHeader()     → 提取文件路径、Move to
  │   ├─ parseAddFileContent()  → 收集 + 行作为内容
  │   └─ parseUpdateFileChunks()→ 解析 @@ hunk 中的 old/new 行
  │
  └─ 返回 Hunk[]
```

---

## 5. 验证流程（预演，提交前完成）

验证阶段对所有 hunk 做"干跑"，确保变更可应用后再实际写入。任一文件的验证失败，整个 patch 不会应用。

### Add 验证

- 无需原文件存在
- 内容末尾确保换行符

### Delete 验证

- 读取原文件内容（用于生成 diff 展示）
- 文件不存在 → 失败

### Update 验证：`deriveNewContentsFromChunks()`

```
原文件 → 按行切分
  │
  ├─ computeReplacements()
  │   │
  │   ├─ 对每个 chunk:
  │   │   ├─ 如果有 change_context → 用模糊匹配定位 context 行作为搜索起点
  │   │   ├─ 如果 old_lines 为空 → 纯插入，定位到文件末尾
  │   │   ├─ 否则: seekSequence() 在文件中查找 old_lines
  │   │   │   ├─ 先精确匹配
  │   │   │   ├─ 失败则去掉尾部空行重试
  │   │   │   └─ 仍失败 → 抛出 "Failed to find expected lines"
  │   │   ├─ 找到后记录 [startIdx, oldLen, newSlice]
  │   │   └─ lineIndex 前移到匹配点之后（保证后续 hunk 顺序匹配）
  │   │
  │   └─ 按索引排序（从小到大）
  │
  ├─ applyReplacements()
  │   └─ 逆序应用替换（避免索引偏移）
  │
  ├─ 确保末尾换行
  │
  └─ 返回 { unified_diff, content, bom }
```

### 模糊匹配算法：`seekSequence()`

按优先级递降尝试 4 种比较器：

| 优先级 | 方式 | Comparator |
|--------|------|-----------|
| 1 | 精确匹配 | `a === b` |
| 2 | rstrip | `a.trimEnd() === b.trimEnd()` |
| 3 | trim | `a.trim() === b.trim()` |
| 4 | Unicode 规范化 | `normalizeUnicode(a.trim()) === normalizeUnicode(b.trim())` |

Unicode 规范化将以下字符转为 ASCII 等价物：

| Unicode | 替换为 |
|---------|-------|
| `'‘' '` `'’' '` `'‚'` `'‛'` | `'` |
| `'“' "` `'”' "` `'„'` `'‟'` | `"` |
| `'‐'–'―'` | `-` |
| `'…'` | `...` |
| `' '` | ` ` |

### EOF 锚定

当 chunk 标记 `is_end_of_file: true` 时，匹配优先从文件末尾倒数 `pattern.length` 行开始尝试，失败再正向搜索。

---

## 6. 权限检查

验证通过后、写入前，检查文件是否在工作区可编辑范围内：

- 外部目录（不在工作区内）→ 拒绝
- 调用 `ctx.ask({ permission: "edit", patterns: [...] })` 请求用户授权

---

## 7. 写入阶段

按 hunk 类型分别写入：

```
Add    → 递归创建父目录 → writeFile(path, content)
Delete → unlink(path)
Update → writeFile(path, newContent)
Move   → writeFile(newPath, newContent) → unlink(oldPath)
```

所有写入是**同步批量的**（在一次工具调用中完成）。

---

## 8. 后处理

写入完成后：

1. **自动格式化**：对每个修改/新增的文件触发格式化
2. **LSP 诊断**：通知 LSP 文件变更，收集诊断结果
3. **BOM 同步**：保持原文件的 BOM 设置
4. **文件变更事件**：发布文件系统事件通知 UI 刷新

---

## 9. 输出

成功时返回简洁摘要：

```
Success. Updated the following files:
M src/main.py
A hello.txt
D obsolete.txt
```

如果有 LSP 诊断错误，追加到输出末尾：

```
LSP errors detected in src/main.py, please fix:
[诊断信息]
```

失败时返回错误描述，如：
- `"apply_patch verification failed: missing Begin/End markers"`
- `"apply_patch verification failed: Failed to find expected lines in path/to/file"`
- `"apply_patch verification failed: Failed to read file to update: path/to/file"`
- `"patch rejected: empty patch"`

---

## 10. 隐式调用检测

除了作为显式工具调用，系统还检测以下隐式调用模式，防止模型绕过工具直接用 bash：

| 模式 | 示例 | 处理 |
|------|------|------|
| 直接文本 | 输入本身就是一个合法 patch | 返回 ImplicitInvocation 错误 |
| heredoc | `bash -lc 'apply_patch <<"EOF"\n...\nEOF'` | 提取 heredoc 内容解析 |

---

## 11. 与单文件 edit 工具的对比

| | apply_patch | 单文件 edit |
|---|---|---|
| 跨文件 | 一次调用支持 N 个文件 | 一次一个文件 |
| 操作类型 | Add / Delete / Update / Move | 仅 Update（替换） |
| 匹配方式 | 4 级模糊匹配 | 精确匹配 |
| 新文件 | 支持 | 不支持 |
| 删除文件 | 支持 | 不支持 |
| 重命名 | 支持 | 不支持 |
| 格式标记 | `*** Begin/End Patch` 信封 | 无 |
| 后处理 | format + LSP diagnostics | 无 |
| 权限模型 | 工具级权限检查 | 无 |

## 12. 生命周期总览

```
parse → verify (dry-run) → permission check → write → post-process → output
```

所有验证在写入前完成，任一失败则整个 patch 不应用（原子性）。
