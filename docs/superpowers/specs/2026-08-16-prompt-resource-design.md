# Prompt 资源与指令机制设计

## 1. 目标

本阶段为 clawcode 建立独立的 `prompt` crate，对齐本地 pi 0.84.1 的核心 Prompt 机制，同时保留 clawcode 的 Rust 类型系统、Factory 注入、ACP 2.0、Session 隔离和项目身份约束。

完成后，系统应能够：

- 动态构建与当前 Turn 工具、Skill、工作目录和项目指令一致的默认 System Prompt。
- 通过 `SYSTEM.md` 完整替换默认主体，通过 `APPEND_SYSTEM.md` 追加指令。
- 按 pi 的优先级发现全局及项目级 `AGENTS.md`、`CLAUDE.md` 和 Prompt Template。
- 完整支持 pi 的 Prompt Template frontmatter、参数解析、默认值和切片语法。
- 支持 `/skill:name args` 展开，并保持 extension command、input hook、Skill 和 Template 的 pi 顺序。
- 通过 ACP 2.0 原生 `available_commands_update` 向客户端发布 Template、Skill 和 Extension Command。
- 在 WebUI 提供命令发现和参数提示，但由后端作为唯一的命令解析与展开来源。

## 2. 行为基准

本设计以本地 pi 0.84.1 的以下实现为行为基准：

- `packages/coding-agent/src/core/system-prompt.ts`
- `packages/coding-agent/src/core/prompt-templates.ts`
- `packages/coding-agent/src/core/resource-loader.ts`
- `packages/coding-agent/src/core/skills.ts`
- `packages/coding-agent/src/core/agent-session.ts`
- `packages/coding-agent/docs/prompt-templates.md`

clawcode 不复制 pi 的 TypeScript 类结构，也不复制 pi 专属身份和文档路径。默认身份必须使用 `protocol::ProductIdentity`，项目名称不得作为分散的字符串常量出现。

## 3. 范围

### 3.1 本阶段包含

- 独立 `prompt` crate 及 `PromptFactory`。
- 全局、项目和 Extension Prompt 资源发现。
- `SYSTEM.md`、`APPEND_SYSTEM.md` 和项目指令加载。
- 默认及自定义 System Prompt 构建。
- Tool Prompt Snippet 和 Guideline 贡献。
- Prompt Template 加载、元数据、冲突诊断和完整参数展开。
- `/skill:name` 展开。
- `before_agent_start` 的结构化构建参数和 Run 级 Prompt 覆盖。
- ACP 2.0 Available Commands 映射。
- WebUI 命令提示和真实后端端到端验证。

### 3.2 本阶段不包含

- pi package 中的 Prompt、Skill 或其他资源。
- Prompt 或项目资源 CLI 参数。
- Prompt、Template、Skill 或指令热重载。
- Theme、TUI 和 UI Extension Point。
- ACP Client 文件系统或 Terminal Callback。
- 客户端执行 Template 或 Skill 展开。
- 复制 pi 专属文档索引到 clawcode 默认 System Prompt。

## 4. 总体架构

### 4.1 依赖方向

```text
                  protocol
        ┌────────────┼────────────┐
        ↓            ↓            ↓
     prompt        tools      skill / extension
        └────────────┼────────────┘
                     ↓
                   kernel
                     ↓
                    acp
                     ↓
                    app ← config
                     ↓
                    web
```

- `protocol` 定义跨 crate 使用的 Prompt、指令、工具贡献和命令元数据。
- `prompt` 负责文件发现、解析、优先级、诊断、Template 展开和 System Prompt 文本构建。
- `tools` 和 `skill` 提供 Prompt 构建所需的类型化贡献，不读取或组装 System Prompt。
- `extension` 继续提供资源路径和 `before_agent_start`，不自行扫描默认目录。
- `kernel` 在 Session 启动时创建不可变 Prompt 资源快照，在 Run 和 Turn 边界调用该快照。
- `acp` 只映射标准 ACP 类型，不重新实现命令优先级或展开规则。
- `app` 将不可变配置转换为 Factory 参数并完成依赖组装。

`prompt` 只依赖 `protocol` 和文件解析依赖，不直接依赖 `tools`、`skill`、`extension` 或 `kernel`。Kernel 将这些模块产生的协议快照传给 PromptSession，避免形成循环依赖。`skill` 可以单向复用 `prompt::MarkdownDocument` 的 YAML frontmatter 解析和正文剥离能力，避免 Template 与 Skill 各自维护一份解析器；`prompt` 不反向依赖 `skill`。

Skill Catalog 与 PromptSession 使用相同的 Session cwd 和项目资源访问决定创建。SkillFactory 不得在 app 进程启动 cwd 提前冻结全局 Catalog，否则不同 cwd 的 Session 会看到错误的 `.pi/skills` 和 `.agents/skills`。

### 4.2 Prompt Factory

`prompt` crate 提供：

```rust
pub trait PromptFactory: Send + Sync {
    fn create(
        &self,
        request: PromptResourceRequest,
    ) -> Result<PromptSession, PromptError>;
}
```

`FilesystemPromptFactory` 持有全局配置目录和不可变 Prompt Policy。`PromptResourceRequest` 持有 Session cwd、项目资源访问决定，以及 Extension 按注册顺序贡献的 Prompt 路径。

`PromptSession` 是 Session 级不可变快照，包含：

- 已选择的 System Prompt 主体来源。
- 已选择的 Append System Prompt 内容。
- 有序项目指令。
- Prompt Template Catalog。
- 资源来源和诊断。

System Prompt 构建作为 `PromptSession` 的类型方法实现。Kernel 不再持有独立 `SystemPromptFactory`、`ProjectContext` 和 `PromptTemplateCatalog`，也不使用 `RwLock` 保存不会热更新的 Prompt 资源。

## 5. 公共类型

以下跨 crate 类型下沉到 `protocol`，其他 crate 不重复定义同义结构体：

- `PromptPolicy`
- `PromptContentSource`
- `PromptResourceRequest` 中需要跨边界的值类型
- `PromptSourceInfo`
- `PromptSourceKind`
- `PromptSourceScope`
- `PromptDiagnostic`
- `PromptDiagnosticSeverity`
- `ProjectInstruction`
- `PromptTemplateInfo`
- `ToolPromptContribution`
- `SystemPromptBuildOptions`
- `AvailableAgentCommand`
- `AvailableAgentCommandKind`

Template 正文和内部索引属于 `prompt` crate 私有实现，不进入 ACP 或 WebUI 公共状态。ACP schema 类型只存在于 `acp` crate 边界，WebUI 使用自己的传输 DTO，不把外部 ACP crate 类型复制到 Rust 公共协议。

## 6. 配置

`AppConfig` 增加类型化 `[prompt]` 配置，并直接复用 `protocol::PromptPolicy`，不在 config 和 prompt crate 分别定义同义配置结构。内容来源使用带判别字段的枚举，不通过“字符串对应的文件存在时视为路径，否则视为正文”进行猜测：

```toml
[prompt]
load_project_instructions = true
load_templates = true
template_paths = []

# system_prompt = { type = "path", value = "/path/to/SYSTEM.md" }
# append_system_prompts = [
#   { type = "path", value = "/path/to/APPEND_SYSTEM.md" },
#   { type = "text", value = "Additional instruction" },
# ]
```

规则如下：

- `system_prompt` 为空时执行默认文件发现。
- `append_system_prompts` 非空时替代自动发现的单个 Append 文件，并按配置顺序拼接。
- `template_paths` 中的相对路径相对于 Session cwd 解析；绝对路径保持不变。
- 配置在进程启动时读取一次，不支持热更新。
- 现有 `skills.include_instructions` 控制 Skill Catalog 是否进入 System Prompt。
- 现有 Skill rules 必须在 Skill Catalog 构建时实际生效，避免配置存在但运行时忽略。

## 7. 资源发现与优先级

### 7.1 项目资源访问

Extension `project_trust` 明确返回 `No` 时，不读取项目 `.pi` 资源和 cwd 祖先指令；全局资源仍可用。返回 `Yes` 或保持当前项目默认的 `Undecided` 时允许读取项目资源，维持现有无交互信任 UI 的行为。

Prompt 资源必须在 `project_trust` 和 `resources_discover` 完成后创建。当前在 Session Runtime 构建期间提前读取 `ProjectContext` 的顺序需要调整。

### 7.2 System Prompt 主体

按以下顺序选择第一个有效来源：

1. 配置中的 `prompt.system_prompt`。
2. 允许项目资源时的 `cwd/.pi/SYSTEM.md`。
3. 全局配置目录中的 `SYSTEM.md`。
4. clawcode 默认 System Prompt。

`SYSTEM.md` 是默认主体的完整替换，不与默认身份段落拼接。

### 7.3 Append System Prompt

按以下规则选择：

1. 配置的 `append_system_prompts` 非空时，仅使用该有序列表。
2. 否则使用允许项目资源时的 `cwd/.pi/APPEND_SYSTEM.md`。
3. 否则使用全局配置目录中的 `APPEND_SYSTEM.md`。
4. 都不存在时不追加。

### 7.4 项目指令

每个目录只加载第一个匹配文件，候选顺序固定为：

1. `AGENTS.override.md`
2. `AGENTS.md`
3. `AGENTS.MD`
4. `CLAUDE.md`
5. `CLAUDE.MD`

最终顺序为：

1. 全局配置目录中的一个指令文件。
2. 从文件系统根目录到 Session cwd 的祖先指令。

发现过程使用规范化路径去重，并实现 pi 的 linked worktree shadow 规则，避免嵌套 linked worktree 同时加载主工作区和 worktree 的同一逻辑指令。

`prompt.load_project_instructions = false` 时跳过 cwd 祖先指令，但不影响全局指令、System Prompt、Append Prompt 和 Template。

### 7.5 Prompt Template

默认及附加来源顺序为：

1. 全局配置目录的 `prompts/*.md`。
2. 允许项目资源时的 `cwd/.pi/prompts/*.md`。
3. `prompt.template_paths` 的配置顺序。
4. Extension `resources_discover` 返回的 `prompt_paths` 顺序。

同名 Template 由先发现者生效。后续同名资源保留为 collision 诊断，不静默覆盖。

目录扫描不递归。显式指向 Markdown 文件时加载单文件；指向目录时只扫描直接子文件。指向普通 Markdown 文件的符号链接可用，损坏链接跳过并记录 warning。

## 8. System Prompt 构建

### 8.1 默认主体

默认主体包含：

- 使用 `ProductIdentity` 生成的 Agent 身份和职责。
- 当前 Turn 可用工具中的 Prompt Snippet。
- 根据当前 Turn 工具贡献生成并去重的 Guidelines。
- 启用 bash 且没有独立 grep/find/ls 工具时，增加 `Use bash for file operations like ls, rg, find`。
- 固定的简洁响应和清晰文件路径规则。

不包含 pi 专属文档路径。若 clawcode 未来建立稳定的内置文档资源，再通过独立设计增加文档索引。

### 8.2 固定追加顺序

无论主体来自默认内容还是 `SYSTEM.md`，都按以下顺序追加：

1. Append System Prompt。
2. `<project_context>` 包装的项目指令。
3. Skill Catalog。
4. 当前工作目录。

项目指令的路径属性必须转义 XML 元字符；文件正文保持原文，不对 Markdown 内容做破坏性转义。

只有当前 Turn 启用 `read` 工具且 `skills.include_instructions = true` 时才追加 Skill Catalog。

### 8.3 Tool Prompt Contribution

工具公共接口增加可选贡献：

```rust
pub struct ToolPromptContribution {
    pub snippet: Option<String>,
    pub guidelines: Vec<String>,
}
```

- pi 对应的内置工具提供与实际能力一致的 Snippet 和 Guidelines。
- MCP 和 Extension Tool 默认不提供贡献；它们可以通过工具接口显式提供。
- Available Tools 段只展示有 Snippet 的当前激活工具。
- Snippet 将换行和连续空白规范化为单个空格并 trim；规范化后为空则不展示。
- Guidelines 先按激活工具顺序收集，再规范化、去空和稳定去重。
- 同一 Turn 的 System Prompt、Provider Tool Definitions 和 Tool 执行必须使用同一个 Tool Registry 快照。

### 8.4 Before Agent Start

`BeforeAgentStartEvent` 增加完整的 `SystemPromptBuildOptions`，让 Extension 能检查已经加载的来源、工具贡献、项目指令和 Skill 元数据，而不重新扫描文件系统。

Extension 返回的 System Prompt 替换按注册顺序链式应用，并在整个 Kernel Run 内保持有效，包括 Tool Call 触发的后续 Turn。Run 正常结束、失败或取消后清除替换；下一次 Run 从新的基础 Prompt 开始。

基础 Prompt 仍在每个 Turn 使用当前 Tool 和 Skill 快照重建。存在 Run 级 Extension 替换时，后续 Turn 使用替换文本，符合 pi 的覆盖语义。

## 9. Prompt Template

### 9.1 Frontmatter

支持：

- `description`
- `argument-hint`

文件名去掉 `.md` 后成为命令名。Frontmatter 按 YAML 解析。解析前统一把 CRLF/CR 规范化为 LF；存在有效 frontmatter 时，对其后的正文执行 `trim()`，与 pi 的 `parseFrontmatter` 一致。缺少 description 时，使用处理后正文第一条非空行，最多保留 60 个字符，超出时追加 `...`。

### 9.2 参数解析

参数解析支持单引号和双引号。引号只用于将空白保留在同一参数内，不实现转义语法。未加引号时，所有 Unicode 空白字符都视为分隔符；空引号不产生空参数；未闭合引号收集到输入末尾。

### 9.3 替换语法

完整支持：

- `$1`、`$2` 和多位数字位置参数。
- `$@`、`$ARGUMENTS`。
- `${N:-default}`。
- `${@:-default}`、`${ARGUMENTS:-default}`。
- `${@:N}`。
- `${@:N:L}`。
- `${@:0}` 按 pi 规则等价于从第一个参数开始。

替换只扫描 Template 原文一次。参数值和默认值中的 `$1`、`$@` 等内容不得被递归展开。缺失的位置参数替换为空字符串；未知 Template 返回原始输入。

### 9.4 Skill 命令

`/skill:name args` 将目标 `SKILL.md` 去除 frontmatter 后包装为带 name、location 和引用基准目录的 Skill Block，再追加 args。Skill 不存在时保留原始输入；文件读取失败时保留原始输入并记录带 Session/Run/Turn 关联的 warning。

与 pi 一致，Skill Command 和 Extension Command 只使用第一个普通空格 U+0020 分隔命令名与参数；参数随后 trim。Tab 和换行不会作为这两类命令的名称分隔符。Prompt Template 仍使用全部空白字符分隔命令名和参数。

Skill 展开使用 Session 级 Skill Catalog，不能调用 Kernel 的全局启动快照，以确保 Extension 贡献的 Session Skill 可用。

SkillFactory 的请求包含 Session cwd、项目资源访问决定和 Extension skill paths。默认来源为全局配置目录 `skills/`，以及项目资源允许时的 `cwd/.pi/skills/`、`cwd/.agents/skills/`；Extension paths 最后追加。Skill list 和显式 invoke 的 Kernel/ACP API 都必须携带 SessionId。

## 10. 输入与 Run 生命周期

普通用户输入按以下顺序处理：

```text
! / !! user bash
  → extension slash command
  → input extension hook
  → /skill:name 展开
  → /template 展开
  → before_agent_start
  → Agent Run 和 Turn
```

规则如下：

- Extension slash command 在 input hook 之前执行，执行后不进入 Agent Run。
- Input hook 看到未展开的原始命令文本，并可 handled 或 transform。
- Skill 在 Template 之前展开。
- 未识别的 slash command 原样进入用户消息。
- Steer 和 Follow-up 复用同一 Skill/Template 展开器，但不允许把 Extension Command 排入队列。
- Expansion 发生在消息持久化之前，因此 Store 和 Provider 都看到最终展开文本。
- Prompt 展开失败不创建半完成消息或 Turn；可恢复的自动发现问题使用诊断并保留原始输入。

## 11. Available Commands 与 ACP 2.0

Kernel 生成统一的 `AvailableAgentCommand` 列表，来源包括：

- Extension Command。
- `/skill:name`。
- Prompt Template。

名称冲突按真实执行顺序解决：Extension Command 优先，其次 Skill Command，最后 Prompt Template。Template 自身同名冲突已在发现阶段按先发现者解决。

`acp` 将列表映射到 ACP 2.0：

- `AvailableCommandsUpdate.available_commands`
- `AvailableCommand.name`
- `AvailableCommand.description`
- `AvailableCommandInput::Text`
- `TextCommandInput.hint`

Template 的 `argument-hint`、Skill 的通用可选参数提示和 Extension Command 的输入提示映射到 `hint`。不需要 ACP 自定义扩展消息。

Session 创建、恢复和 Fork 完成资源加载后发送一次完整列表。Extension 动态注册或注销 Command 后重新发送完整快照。每次更新继续使用项目现有 ACP 元数据规则携带 TurnId、字符串毫秒时间戳和稳定序列号。

## 12. WebUI

WebUI 解码并保存最近一次 `available_commands_update` 完整快照。在输入框键入 `/` 时展示：

- Command 名称。
- Argument Hint。
- Description。

选择命令只向输入框写入 `/name` 和必要空格，不在浏览器展开 Template 或 Skill。提交后仍由后端按第 10 节的顺序处理。

该界面属于 Agent 核心能力，不是 Extension UI Point。前端不要求 TDD，也不新增前端单元测试；最终通过真实后端 E2E 验证。

## 13. 错误与诊断

### 13.1 启动错误

以下情况阻止 Session 启动：

- 配置明确指定的 System、Append 或 Template 路径不存在或不可读取。
- 配置的 Prompt Content Source 类型或值非法。
- Prompt Factory 内部不变量被破坏。

### 13.2 可恢复诊断

以下情况记录类型化 warning 或 collision，并继续启动：

- 自动发现的文件不可读取。
- 自动发现的 Template frontmatter 无法解析。
- 损坏的符号链接。
- 同名 Template 被较早来源覆盖。
- Skill 命令在调用时读取失败。

诊断至少包含 severity、message、path、source kind 和可选 collision winner/loser。日志使用 `tracing::warn!()` 全路径宏和项目既有字符串插值风格，不导入 tracing 宏，不打印 Template 或 System Prompt 正文。

本阶段通过 Kernel 只读查询和日志保留诊断，不新增 ACP 私有诊断事件。Available Commands 只发布有效资源。

Session 启动期间产生的资源诊断保存在不可变 PromptSession 中。Skill 命令调用期间的读取失败只记录带 Session、Run 和 Turn 关联信息的 warning，并保留原始输入，不修改不可变诊断集合。

## 14. 并发、快照与持久化

- Prompt 资源在 Session 启动时加载一次，之后使用 `Arc<PromptSession>` 共享。
- 不为不可变 Catalog 增加 `Mutex` 或 `RwLock`。
- 文件读取不跨 `await` 持有 Kernel、Store 或 Extension 锁。
- System Prompt 每个 Turn 从不可变资源和 Turn 工具快照构建。
- Template 和 Skill 展开结果作为最终 User Message 持久化；原始 slash 输入不额外持久化。
- System Prompt 不写入普通 transcript，继续作为 Provider 请求上下文临时构建。
- Session Resume 重新从当前文件系统加载 Prompt 资源，不把资源正文复制到 Session JSONL。
- Fork 为新 Session 按相同 cwd 和当前资源重新加载，不继承旧 Session 的内存 Catalog。

## 15. 模块与文件边界

新增 `crates/prompt/`，按语义拆分：

```text
crates/prompt/src/
├── lib.rs
├── error.rs
├── factory.rs
├── session.rs
├── system.rs
├── instruction.rs
├── source.rs
└── template/
    ├── mod.rs
    ├── catalog.rs
    ├── document.rs
    └── expansion.rs
```

约束如下：

- `lib.rs` 只提供模块声明和稳定重导出。
- 文件发现、Template 解析、参数展开和 System Prompt 组装分离。
- 单参数且参数为项目自定义类型的逻辑优先放入对应类型的 `impl`。
- 不创建大量单调用点短 helper；简单逻辑直接内联。
- 跨 crate 公共结构体只定义在 `protocol`。
- 测试代码只放在各 crate 的 `tests/` 集成测试目录，不在正文增加测试支撑代码。

主要修改边界：

- `Cargo.toml` 和新增 `crates/prompt/Cargo.toml`
- `crates/protocol/src/prompt/`
- `crates/config/src/prompt.rs`
- `crates/config/src/config.rs`
- `crates/tools/src/`
- `crates/skill/src/`
- `crates/extension/src/runtime/agent.rs`
- `crates/protocol/src/extension/events/agent.rs`
- `crates/kernel/src/runtime/session.rs`
- `crates/kernel/src/runtime/run.rs`
- `crates/kernel/src/runtime/prompt.rs`
- `crates/kernel/src/runtime/input.rs`
- `crates/kernel/src/runtime/command.rs`
- `crates/kernel/src/runtime/queue.rs`
- `crates/acp/src/`
- `crates/app/src/lib.rs`
- `web/src/`
- 对应 crate 的 `tests/` 集成测试目录

现有 `crates/kernel/src/prompt.rs` 和 `crates/kernel/src/prompt/` 在迁移完成后删除，不保留兼容层。

## 16. 测试策略

### 16.1 Prompt crate

集成测试覆盖：

- System、Append 和指令优先级。
- 项目资源禁止和允许。
- 候选文件优先级与祖先顺序。
- linked worktree shadow。
- 自定义主体仍追加 Append、Context、Skill 和 cwd。
- Tool Snippet 和 Guideline 动态构建、去重和顺序。
- Template 默认目录、显式文件、目录、符号链接和非递归扫描。
- YAML frontmatter description、argument-hint、换行规范化和正文 trim。
- 完整参数、默认值、切片、Unicode 空白、引号和非递归替换。
- 同名冲突、可恢复 warning 和明确配置路径错误。

### 16.2 Kernel 与 Extension

集成测试覆盖：

- Trust 和 Resource Discover 之后才创建 PromptSession。
- Input Hook 在 Skill 和 Template 展开之前。
- Extension Command 在 Input Hook 之前。
- Skill 在 Template 之前。
- Before Agent Start 替换持续整个 Run，并在下一 Run 清除。
- 每个 Turn 使用同一个工具快照构建 Prompt 和 Provider Request。
- Session 级 Extension Skill 和 Prompt 路径生效。
- Steer 和 Follow-up 使用相同展开语义。

### 16.3 ACP 与 WebUI

`crates/acp/tests/` 验证：

- Available Commands 的原生 ACP 2.0 映射。
- description、hint、顺序、去重和动态 Command 更新。
- Session 创建、Resume 和 Fork 后发送完整命令列表。
- 更新包含 TurnId、字符串毫秒时间戳和稳定序列。
- 带测试 Extension 的真实 Kernel/ACP 链路中，Extension Command 执行后不触发 Provider 请求。

前端运行 check/build，不新增单元测试。最终启动正式 app 后端，通过 WebUI 和真实 Provider 验证：

1. `/` 可展示当前正式配置实际提供的 Template 和 Skill Command。
2. Argument Hint 和 Description 正确显示。
3. Template 参数展开后的文本进入真实模型。
4. `/skill:name args` 内容进入真实模型。
5. 刷新和恢复 Session 后 Available Commands 重新出现。

正式配置只启用现有安全 Guard Extension，若它不贡献 Command，手工 E2E 不为测试临时增加生产 Command；Extension Command 的端到端行为由 Kernel/ACP 集成测试覆盖。

不使用 fixture backend，不使用 ACP Client FS/Terminal callback。

## 17. 验收标准

- `prompt` 成为独立 crate，并通过 `PromptFactory` 接入 Kernel。
- Kernel 不再包含 Prompt 文件发现和 Template 解析实现。
- clawcode 在没有 `SYSTEM.md` 时具有动态默认 System Prompt。
- `SYSTEM.md`、`APPEND_SYSTEM.md`、项目指令和 Skill Catalog 的组合顺序与 pi 一致。
- Tool Snippet 和 Guideline 与当前 Turn 激活工具一致。
- Prompt Template 完整支持 pi 0.84.1 的公开语法和发现行为。
- Extension Prompt 替换在完整 Run 内有效。
- ACP 2.0 Available Commands 可被 WebUI 使用，无自定义命令协议。
- Prompt 资源不热更新、不复制到 Session JSONL、不增加索引文件。
- 公共类型不在多个 crate 重复定义。
- Rust fmt、Clippy、workspace tests、Web check/build 和真实后端 WebUI E2E 全部通过。
