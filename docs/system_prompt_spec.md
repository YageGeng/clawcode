# Pi Agent — System Prompt Spec

## 概述

System prompt 在会话初始化时由 `buildSystemPrompt()` 函数组装，不是外部模板文件。根据是否存在自定义 prompt 走两条路径。

## 组装结构

### 路径 A：有自定义 system prompt（`SYSTEM.md` 存在或 SDK 注入）

```
{customPrompt 全文}

{appendSystemPrompt}

# Project Context
Project-specific instructions and guidelines:

## /path/to/child/AGENTS.md
{子目录 AGENTS.md 内容}

## /path/to/ancestor/AGENTS.md
{祖先 AGENTS.md 内容}

<available_skills>
  <skill>
    <name>skill-name</name>
    <description>What it does</description>
    <location>/path/SKILL.md</location>
  </skill>
</available_skills>

Current date: YYYY-MM-DD
Current working directory: /path/to/cwd
```

### 路径 B：默认 system prompt（无 `SYSTEM.md`）

```
You are an expert coding assistant operating inside pi, a coding agent harness.
You help users by reading files, executing commands, editing code, and writing new files.

Available tools:
- tool_a: one-line description
- tool_b: one-line description

In addition to the tools above, you may have access to other custom tools depending on the project.

Guidelines:
- {根据工具组合推导}
- {工具注入的 guidelines}
- Be concise in your responses
- Show file paths clearly when working with files

{关于 Pi 本身的文档指针：README、docs、examples 路径，各主题索引}

{appendSystemPrompt}

# Project Context
Project-specific instructions and guidelines:

## /path/to/AGENTS.md
{内容}

<available_skills>
  <skill>
    <name>skill-name</name>
    <description>What it does</description>
    <location>/path/SKILL.md</location>
  </skill>
</available_skills>

Current date: YYYY-MM-DD
Current working directory: /path/to/cwd
```

## 各段说明

### 1. 角色声明

- **路径 A**: 无，由 `SYSTEM.md` 替换。
- **路径 B**: 固定文本，声明 "expert coding assistant operating inside pi" 及核心能力。

### 2. Available Tools

- **来源**: 工具注册时的 `promptSnippet` 字段。
- **渲染**: 每个有 snippet 的工具一行 `- name: snippet`。无 snippet 的工具不出现。
- **兜底**: 无任何 snippet 时显示 `(none)`。
- **固定后缀**: 提示可能还有其他自定义工具。

### 3. Guidelines

由三个来源合并去重：
- **内置推导**: 根据可用工具组合自动生成（如有 bash 且有 grep，优先 grep）
- **工具注入**: 每个工具注册时可附带 `promptGuidelines`
- **固定项**: "Be concise in your responses"、"Show file paths clearly when working with files"

### 4. Pi 文档指针

- **仅路径 B**。
- 列出 README、docs、examples 的绝对路径及主题→文件映射。

### 5. appendSystemPrompt

- **来源**: 仅来自运行时显式注入的 `appendSystemPrompt` / SDK 选项。
- **行为**: 直接追加到 system prompt，两条路径都生效。
- **不再支持**: 不读取项目内 `.pi/APPEND_SYSTEM.md`，也不读取 `~/.pi/agent/APPEND_SYSTEM.md`。

### 6. Project Context（AGENTS.md / CLAUDE.md）

- **发现**: 从 cwd 向上逐级递归到根目录，收集所有 `AGENTS.md` 或 `CLAUDE.md`。
- **排序**: 子目录（更具体）排在前面，去重。
- **渲染**: `## /path/to/file` + 全文。
- **不再支持**: 不读取 `~/.pi/agent/` 下的全局 `AGENTS.md` / `CLAUDE.md`。

### 7. Skills 列表（`<available_skills>`）

- **来源**: 所有已发现 skill 的 `SKILL.md` frontmatter。
- **包含字段**: `name`、`description`、`location`（文件路径）。
- **不包含**: SKILL.md 正文。正文仅在使用 `/skill:name` 时展开到对话消息中。
- **过滤**: `disableModelInvocation: true` 的 skill 被隐藏。
- **条件**: Read 工具可用且有可见 skill。
- **格式**:
  ```xml
  The following skills provide specialized instructions for specific tasks.
  Use the read tool to load a skill's file when the task matches its description.
  When a skill file references a relative path, resolve it against the skill directory...

  <available_skills>
    <skill>
      <name>xxx</name>
      <description>xxx</description>
      <location>/path/SKILL.md</location>
    </skill>
  </available_skills>
  ```

### 8. 日期 + 工作目录

```
Current date: YYYY-MM-DD
Current working directory: /absolute/path
```

运行时动态生成，始终存在。

## 数据来源全景

```
buildSystemPrompt()
  ├── customPrompt       ← SYSTEM.md 文件或 SDK systemPrompt 选项
  ├── selectedTools      ← 当前活跃工具名列表
  ├── toolSnippets       ← 工具注册时的 promptSnippet
  ├── promptGuidelines   ← 工具注册时的 promptGuidelines + 内置推导
  ├── appendSystemPrompt ← 运行时 / SDK 显式注入
  ├── cwd                ← 会话工作目录
  ├── contextFiles       ← AGENTS.md / CLAUDE.md（递归向上收集）
  └── skills             ← Skill[]（formatSkillsForPrompt 转为 XML）
```

## 重建时机

System prompt 不是每轮对话都重建，只在以下三个时机：

| # | 触发 | 原因 |
|---|------|------|
| 1 | 会话初始化 | 扩展和工具绑定完成后，首次构建 |
| 2 | 工具集变更 | 扩展通过 `setActiveTools` 改变活跃工具 |
| 3 | 扩展注入资源 | startup/reload 时扩展贡献新的 skill/prompt/theme |

普通对话轮次中，system prompt 保持缓存不变。

## 与 Skill 的关系

| 位置 | 内容 | 生命周期 |
|------|------|---------|
| System prompt 中 | skill 的 name + description + location | 每轮都在（轻量，仅元数据） |
| 对话消息中 | `/skill:name` 展开的完整 SKILL.md 正文 | 注入后跟随对话历史，受 compaction 影响 |

System prompt 中的 skill 列表只是索引，正文靠模型自行 Read 或用户 `/skill:name` 按需加载。
