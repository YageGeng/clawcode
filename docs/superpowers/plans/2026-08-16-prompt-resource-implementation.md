# Prompt 资源与指令机制实施计划

> **执行约束：** 实施时必须使用 `superpowers:executing-plans` 在当前会话逐任务执行。项目禁止 SubAgent 和 worktree；未经用户明确要求不得创建 commit，因此本计划不包含 commit 步骤。

**目标：** 新增独立 `prompt` crate，对齐本地 pi 0.84.1 的默认 System Prompt、指令、Template、Skill Command 和 ACP 2.0 Available Commands 行为。

**架构：** `protocol` 保存跨 crate 公共类型，`prompt` 只负责资源发现、解析、诊断、Template 展开和 System Prompt 文本构建；Kernel 在 Session 启动时通过 `PromptFactory` 创建不可变快照，并在 Run/Turn 边界与 tools、skill、extension 协作。ACP 使用标准 `available_commands_update`，WebUI 只做命令发现和输入，不在浏览器展开资源。

**技术栈：** Rust 2024、Serde/TOML、Tokio、ACP 2.0 Rust SDK、React 19、TypeScript 6、Zustand、Vite。

## 全局约束

- 所有 spec 和 plan 使用中文；新增函数和方法必须写英文作用注释，修改旧逻辑必须写英文变动原因注释。
- 禁止 SubAgent、worktree 和未经用户明确要求的 commit。
- 测试代码只能放在 crate 的 `tests/` 目录或精确的 `#[cfg(test)] mod tests` 中；正文不得增加测试支撑代码。
- 后端行为按 TDD 实施；前端不要求 TDD，也不新增前端单元测试。
- 单参数且参数为项目自定义类型的逻辑优先实现为该类型的关联方法或 trait，不堆积短小 helper。
- 公共结构体不得跨 crate 重复定义；跨 crate Prompt 类型统一下沉到 `protocol`。
- Cargo 相对路径依赖放在依赖组最前；新依赖先加入 workspace `[workspace.dependencies]`，子 crate 使用 `{ workspace = true }`。
- 项目名称只来自 `protocol::ProductIdentity`，不得新增分散的 `clawcode` 字符串常量。
- 不实现 package/CLI Prompt 资源、热更新、UI Extension Point、ACP Client FS/Terminal Callback。
- Prompt 资源不持久化到 Session JSONL，不新增 `index.jsonl` 或其他索引文件。
- 日志使用 `tracing::info!()`、`tracing::warn!()` 等全路径宏和字符串插值，不导入 tracing 宏，不打印 Prompt 正文。
- 所有 shell 命令使用 `rtk` 前缀。

---

### 任务 1：Workspace、公共 Prompt 类型与 TOML 配置

**文件：**

- 修改：`Cargo.toml`
- 新增：`crates/protocol/src/prompt/mod.rs`
- 新增：`crates/protocol/src/prompt/policy.rs`
- 新增：`crates/protocol/src/prompt/source.rs`
- 新增：`crates/protocol/src/prompt/system.rs`
- 新增：`crates/protocol/src/prompt/template.rs`
- 新增：`crates/protocol/src/prompt/command.rs`
- 修改：`crates/protocol/src/capability.rs`
- 修改：`crates/protocol/src/lib.rs`
- 新增：`crates/config/src/prompt.rs`
- 修改：`crates/config/src/config.rs`
- 修改：`crates/config/src/lib.rs`
- 修改：`crates/config/tests/loading.rs`
- 新增：`crates/protocol/tests/prompt.rs`
- 新增：`crates/prompt/Cargo.toml`
- 新增：`crates/prompt/src/lib.rs`
- 新增：`crates/prompt/src/error.rs`

**接口：**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum PromptContentSource {
    Path(PathBuf),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptPolicy {
    pub load_project_instructions: bool,
    pub load_templates: bool,
    pub system_prompt: Option<PromptContentSource>,
    pub append_system_prompts: Vec<PromptContentSource>,
    pub template_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptResourceRequest {
    pub cwd: PathBuf,
    pub project_resources_allowed: bool,
    pub extension_prompt_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillResourceRequest {
    pub cwd: PathBuf,
    pub project_resources_allowed: bool,
    pub extension_skill_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInstruction {
    pub path: PathBuf,
    pub content: String,
    pub source: PromptSourceInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptSourceKind {
    System,
    AppendSystem,
    Instruction,
    Template,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptSourceScope {
    User,
    Project,
    Config,
    Extension,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSourceInfo {
    pub kind: PromptSourceKind,
    pub scope: PromptSourceScope,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptDiagnosticSeverity {
    Warning,
    Collision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptCollision {
    pub name: String,
    pub winner: PromptSourceInfo,
    pub loser: PromptSourceInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptDiagnostic {
    pub severity: PromptDiagnosticSeverity,
    pub message: String,
    pub source: PromptSourceInfo,
    pub collision: Option<PromptCollision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptTemplateInfo {
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
    pub source: PromptSourceInfo,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPromptContribution {
    pub snippet: Option<String>,
    pub guidelines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemPromptBuildOptions {
    pub cwd: PathBuf,
    pub custom_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub selected_tools: Vec<String>,
    pub tool_contributions: BTreeMap<String, ToolPromptContribution>,
    pub project_instructions: Vec<ProjectInstruction>,
    pub skills: Vec<SkillInfo>,
    pub include_skill_instructions: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableAgentCommand {
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
    pub kind: AvailableAgentCommandKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AvailableAgentCommandKind {
    Extension,
    Skill,
    PromptTemplate,
}
```

这些类型统一使用 `snake_case` Serde 命名；`SystemPromptBuildOptions` 的正文只传给进程内 Extension，不进入日志或 ACP Available Commands。

- [ ] **步骤 1：先写配置和 Serde 失败测试**

在 `crates/config/tests/loading.rs` 增加 TOML 测试，断言：

```rust
let config = config::load_from([path]).expect("load prompt config");
assert!(!config.current().prompt.load_templates);
assert_eq!(
    config.current().prompt.system_prompt,
    Some(PromptContentSource::Path(PathBuf::from("SYSTEM.custom.md")))
);
assert_eq!(config.current().prompt.append_system_prompts.len(), 2);
```

在 `crates/protocol/tests/prompt.rs` 增加 `PromptContentSource` 的 `path`、`text` round-trip，并验证同时缺少 `type` 或 `value` 时反序列化失败。

- [ ] **步骤 2：运行测试并确认失败原因**

运行：

```bash
rtk cargo test -p protocol --test prompt
rtk cargo test -p config --test loading prompt
```

预期：因 `protocol::PromptContentSource`、`AppConfig::prompt` 和新 crate 尚不存在而编译失败。

- [ ] **步骤 3：建立公共类型和 workspace crate**

在 workspace members 和 workspace dependencies 中加入：

```toml
"crates/prompt",
prompt = { path = "crates/prompt" }
```

在 workspace serde 依赖组加入 `serde_yaml_ng = "0.10"`。`prompt/Cargo.toml` 的相对路径依赖组只包含 `protocol = { workspace = true }`，其后按 error、serde、fs 分组使用 `thiserror` 和 `serde_yaml_ng` 的 workspace 依赖。`prompt/src/lib.rs` 此阶段只声明 `mod error;` 并重导出 `PromptError`。Cargo 解析依赖后检查 `Cargo.lock` 只新增 YAML 解析所需传递依赖。

- [ ] **步骤 4：实现 PromptPolicy 默认值和 Config 接入**

默认值必须为：

```rust
Self {
    load_project_instructions: true,
    load_templates: true,
    system_prompt: None,
    append_system_prompts: Vec::new(),
    template_paths: Vec::new(),
}
```

`config::AppConfig` 直接使用 `protocol::PromptPolicy`，`config::prompt` 只重导出该类型并保留配置模块文档，不定义第二份结构体。

- [ ] **步骤 5：运行目标测试和格式检查**

```bash
rtk cargo test -p protocol --test prompt
rtk cargo test -p config --test loading
rtk cargo fmt --all -- --check
```

预期：全部通过，且 `rtk git diff --check` 无空白错误。

---

### 任务 2：Pi 兼容 Prompt Template 引擎

**文件：**

- 新增：`crates/prompt/src/template/mod.rs`
- 新增：`crates/prompt/src/template/catalog.rs`
- 新增：`crates/prompt/src/template/document.rs`
- 新增：`crates/prompt/src/template/expansion.rs`
- 修改：`crates/prompt/src/lib.rs`
- 修改：`crates/prompt/src/error.rs`
- 新增：`crates/prompt/tests/templates.rs`

**接口：**

```rust
pub struct PromptTemplateRoot {
    pub path: PathBuf,
    pub scope: PromptSourceScope,
    pub requirement: PromptTemplateRootRequirement,
}

pub enum PromptTemplateRootRequirement {
    Discovered,
    Required,
}

pub struct MarkdownDocument {
    frontmatter: Option<String>,
    body: String,
}

impl MarkdownDocument {
    pub fn parse(content: &str) -> Self;
    pub fn metadata<T: DeserializeOwned>(&self) -> Result<T, PromptError>;
    pub fn body(&self) -> &str;
}

struct PromptTemplate {
    info: PromptTemplateInfo,
    content: String,
}

pub struct PromptTemplateCatalog {
    templates: Vec<PromptTemplate>,
    by_name: BTreeMap<String, usize>,
    diagnostics: Vec<PromptDiagnostic>,
}

impl PromptTemplateCatalog {
    pub fn discover(roots: &[PromptTemplateRoot]) -> Result<Self, PromptError>;
    pub fn templates(&self) -> Vec<PromptTemplateInfo>;
    pub fn diagnostics(&self) -> &[PromptDiagnostic];
    pub fn expand(&self, input: &str) -> String;
}
```

Catalog 使用插入顺序保存有效 Template，并用名称索引查找；不能用会按键重排来源优先级的单独 `BTreeMap` 作为唯一存储。第一个同名 Template 生效，后续项只产生 collision。

- [ ] **步骤 1：写参数解析和替换失败测试**

在 `templates.rs` 使用 table-driven cases 覆盖：

```rust
let cases = [
    ("/review one two", "one|two|one two"),
    ("/review 'one two' three", "one two|three|one two three"),
    ("/review", "fallback|all-default"),
    ("/review one two three", "two three|two"),
    ("/review '$1'", "$1||$1"),
];
```

测试 Template 正文同时使用 `$1`、`$2`、`$@`、`$ARGUMENTS`、`${1:-fallback}`、`${@:-all-default}`、`${@:2}`、`${@:2:1}`。另写测试验证换行和 Unicode 空白可分隔参数、空引号不生成参数、未闭合引号读到结尾、参数中的 `$1` 不递归展开。

- [ ] **步骤 2：运行模板测试并确认失败**

```bash
rtk cargo test -p prompt --test templates expansion
```

预期：因 Catalog 和 expansion 模块尚不存在而编译失败。

- [ ] **步骤 3：实现单次扫描替换器**

`expansion.rs` 使用一次正则等价扫描或显式状态机，只扫描 Template 正文。匹配优先级固定为：默认值表达式、slice、aggregate/position 简写。`${@:0}` 将 start 归一化为第一个参数；缺失位置参数为空字符串。

参数解析作为 `CommandArguments::from(&str)` 或 `FromStr` 实现，不创建多个单用途 helper。

- [ ] **步骤 4：写发现、frontmatter 和 collision 失败测试**

测试必须创建：

- 全局和项目两个同名 `review.md`，断言全局 winner、项目 loser diagnostic。
- 带 `description` 和 `argument-hint` 的 Markdown。
- 无 description 的 61 字符首行，断言 60 字符加 `...`。
- 子目录 `nested/hidden.md`，断言不递归。
- 普通 `.md` 符号链接和损坏符号链接。
- `.txt` 文件和不可读/非法 frontmatter 文件。
- 带 YAML frontmatter、CRLF 和正文首尾空白的文件，断言换行规范化且正文按 pi 行为执行 `trim()`。

- [ ] **步骤 5：实现 frontmatter 和非递归 Catalog**

`document.rs` 的 `MarkdownDocument::parse` 统一 CRLF/CR 为 LF，识别闭合 delimiter 后对正文执行 `trim()`；`metadata<T>()` 使用 `serde_yaml_ng::from_str`。Template 将 metadata 反序列化为只读取 `description` 和 `argument-hint` 的私有结构。`catalog.rs` 负责文件、目录、符号链接、来源和诊断。自动发现错误进入 warning；传入的显式配置根错误由后续 Factory 标记为 fatal，不在 Catalog 中猜测来源意图。

目录项读取后按文件名排序再加载，确保同一来源内的 winner 和 Available Commands 顺序跨平台稳定。

- [ ] **步骤 6：运行完整 Prompt Template 测试**

```bash
rtk cargo test -p prompt --test templates
rtk cargo clippy -p prompt --all-targets -- -D warnings
```

预期：完整语法、发现、来源和 collision 测试通过。

---

### 任务 3：指令、SYSTEM/APPEND 发现与 PromptFactory

**文件：**

- 新增：`crates/prompt/src/source.rs`
- 新增：`crates/prompt/src/instruction.rs`
- 新增：`crates/prompt/src/factory.rs`
- 新增：`crates/prompt/src/session.rs`
- 修改：`crates/prompt/src/lib.rs`
- 修改：`crates/prompt/src/error.rs`
- 新增：`crates/prompt/tests/resources.rs`

**接口：**

```rust
pub trait PromptFactory: Send + Sync {
    fn create(
        &self,
        request: PromptResourceRequest,
    ) -> Result<PromptSession, PromptError>;
}

pub struct FilesystemPromptFactory {
    global_root: PathBuf,
    policy: PromptPolicy,
}

impl FilesystemPromptFactory {
    pub fn new(global_root: PathBuf, policy: PromptPolicy) -> Self;
}

pub struct PromptSession {
    cwd: PathBuf,
    custom_prompt: Option<String>,
    append_system_prompt: Option<String>,
    instructions: Vec<ProjectInstruction>,
    templates: PromptTemplateCatalog,
    diagnostics: Vec<PromptDiagnostic>,
}

impl PromptSession {
    pub fn templates(&self) -> Vec<PromptTemplateInfo>;
    pub fn diagnostics(&self) -> &[PromptDiagnostic];
    pub fn expand_template(&self, input: &str) -> String;
}
```

- [ ] **步骤 1：写资源优先级失败测试**

`resources.rs` 分别验证：

```text
configured system > project .pi/SYSTEM.md > global SYSTEM.md > default
configured append list > project .pi/APPEND_SYSTEM.md > global APPEND_SYSTEM.md
global instruction > root ancestor > child ancestor > cwd
global prompts > project prompts > configured paths > extension paths
```

配置 Append 测试必须断言多个 `PromptContentSource` 保持配置顺序；`project_resources_allowed = false` 时不得读取 `.pi` 和祖先指令，但仍加载 global 资源。

- [ ] **步骤 2：运行资源测试并确认失败**

```bash
rtk cargo test -p prompt --test resources discovery
```

预期：因 `PromptFactory` 和 `PromptSession` 尚不存在而失败。

- [ ] **步骤 3：实现内容来源解析与 fatal/warning 边界**

配置 `Path` 不存在、不是普通文件或不可读时返回带路径的 `PromptError::ConfiguredResource`。配置 `Text` 保持原文。自动发现的候选读取失败时追加 warning 并尝试下一候选，不阻止 Session。

- [ ] **步骤 4：实现项目指令候选与顺序**

候选常量固定为：

```rust
const INSTRUCTION_CANDIDATES: [&str; 5] = [
    "AGENTS.override.md",
    "AGENTS.md",
    "AGENTS.MD",
    "CLAUDE.md",
    "CLAUDE.MD",
];
```

目录逻辑放入 `InstructionDiscovery` 的 `impl`。规范化 cwd 后按 root-to-cwd 收集；global root 单独最先插入并用规范化路径去重。

- [ ] **步骤 5：实现 linked worktree shadow 测试与逻辑**

集成测试使用真实 `git init`、`git worktree add` 建立嵌套 linked worktree，分别在主工作区和 worktree 放置同名指令，断言主工作区逻辑副本只加载一次。Git 不可用时测试不得静默成功，应明确失败显示环境问题。

实现时读取 `.git`/`commondir` 关系并比较规范化路径，不调用 shell 命令作为生产发现逻辑。

- [ ] **步骤 6：组装 Template Roots 并冻结 PromptSession**

Factory 按全局、项目、配置、Extension 顺序构造 `PromptTemplateRoot`。`load_templates = false` 时跳过全局和项目默认目录，但仍加载显式配置路径和 Extension 路径；这与显式来源优先于禁用自动发现的语义一致。

- [ ] **步骤 7：运行 Prompt crate 全部测试**

```bash
rtk cargo test -p prompt
rtk cargo clippy -p prompt --all-targets -- -D warnings
rtk cargo fmt --all -- --check
```

---

### 任务 4：动态 System Prompt、Tool 贡献与 Skill 规则

**文件：**

- 新增：`crates/prompt/src/system.rs`
- 修改：`crates/prompt/src/session.rs`
- 修改：`crates/prompt/src/lib.rs`
- 新增：`crates/prompt/tests/system_prompt.rs`
- 修改：`crates/tools/src/contract.rs`
- 修改：`crates/tools/src/builtin/read.rs`
- 修改：`crates/tools/src/builtin/write.rs`
- 修改：`crates/tools/src/builtin/edit.rs`
- 修改：`crates/tools/src/builtin/bash.rs`
- 修改：`crates/tools/tests/contract.rs`
- 修改：`crates/tools/tests/builtins.rs`
- 修改：`crates/protocol/src/prompt/system.rs`
- 修改：`crates/protocol/src/capability.rs`
- 修改：`crates/config/src/skills.rs`
- 修改：`crates/skill/src/lib.rs`
- 修改：`crates/skill/Cargo.toml`
- 修改：`crates/skill/tests/discovery.rs`

**接口：**

```rust
pub struct SystemPromptTool {
    pub name: String,
    pub contribution: ToolPromptContribution,
}

pub struct SystemPromptTurnInput {
    pub tools: Vec<SystemPromptTool>,
    pub skills: Vec<SkillInfo>,
    pub include_skill_instructions: bool,
}

pub struct BuiltSystemPrompt {
    pub text: String,
    pub options: SystemPromptBuildOptions,
}

impl PromptSession {
    pub fn build_system_prompt(
        &self,
        input: SystemPromptTurnInput,
    ) -> BuiltSystemPrompt;
}

pub trait AgentTool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    fn prompt_contribution(&self) -> ToolPromptContribution {
        ToolPromptContribution::default()
    }
    fn validate(&self, call: &ToolCall) -> Result<(), ToolError> {
        let definition = self.definition();
        if definition.name == call.name {
            Ok(())
        } else {
            Err(ToolError::NotFound(call.name.clone()))
        }
    }
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError>;
}

impl ToolRegistry {
    pub fn prompt_tools(&self) -> Vec<SystemPromptTool>;
}

impl SkillCatalog {
    pub fn expand_command(&self, input: &str) -> Result<Option<String>, SkillError>;
}

pub trait SkillFactory: Send + Sync {
    fn create(
        &self,
        request: SkillResourceRequest,
    ) -> Result<SkillCatalog, SkillError>;
}

pub struct SkillSelectionRule {
    pub path: Option<PathBuf>,
    pub name: Option<String>,
    pub enabled: bool,
}
```

- [ ] **步骤 1：写 System Prompt 构建失败测试**

覆盖：默认身份含 `ProductIdentity::NAME`、无 snippet 的工具不出现在 Available Tools、没有可见工具时输出 `(none)`、guideline trim/稳定去重、自定义 SYSTEM 仍追加 Append/Context/Skill/cwd、没有 read 时不追加 Skill、`include_skill_instructions = false` 时不追加 Skill、项目路径 XML 属性转义但正文保持原文。

动态 guideline 测试分别传入 `[bash]` 和 `[bash, grep]`：前者必须包含 `Use bash for file operations like ls, rg, find`，后者不得包含该 fallback。

增加多行自定义 snippet 测试：`"  Inspect\nthe   service  "` 必须规范化为 `"Inspect the service"`；全空白 snippet 必须被隐藏。

- [ ] **步骤 2：运行测试确认失败**

```bash
rtk cargo test -p prompt --test system_prompt
```

- [ ] **步骤 3：实现 PromptSession::build_system_prompt**

默认主体使用：

```text
You are an expert coding assistant operating inside {ProductIdentity::NAME}, an agent harness.
```

Available Tools 段后保留 `In addition to the tools above, you may have access to other custom tools depending on the project.`，但不加入 pi 文档路径段。

固定 guidelines 为：

```text
Be concise in your responses
Show file paths clearly when working with files
```

主体后严格按 Append、Project Context、Skill Catalog、cwd 顺序追加。`BuiltSystemPrompt.options` 保存这次构建的完整结构化输入，供 Extension 读取。

- [ ] **步骤 4：写 Tool Contribution 失败测试**

断言四个内置工具返回以下内容：

```text
read:  "Read file contents"
       "Use read to examine files instead of cat or sed."
write: "Create or overwrite files"
       "Use write only for new files or complete rewrites."
edit:  "Make precise file edits with exact text replacement, including multiple disjoint edits in one call"
bash:  "Execute bash commands (ls, grep, find, etc.)"
```

edit 的四条 guideline 使用 pi 当前内容。bash 不复制 `PI_* environment variables` guideline，因为 clawcode 没有暴露该环境能力。自定义/MCP Tool 默认贡献为空。

- [ ] **步骤 5：实现 AgentTool 默认方法和 Registry 投影**

`ToolRegistry::prompt_tools()` 按 Registry 稳定词典顺序返回名称和贡献。不能从 `ToolDefinition.description` 推断 snippet。

- [ ] **步骤 6：写 Skill rules 与命令展开失败测试**

新增测试：

- 后出现的 name/path rule 覆盖先出现规则。
- 同时配置 name 和 path、或两者都未配置的 rule 被跳过并记录 warning。
- `/skill:rust-patterns fix borrow` 返回去除 frontmatter 的 Skill Block 加参数。
- `/skill:rust-patterns\tfix` 和 `/skill:rust-patterns\nfix` 不匹配 Skill Command，保持原文。
- quoted YAML 和多行 description 可被 Skill discovery 解析，命令展开使用与 Template 相同的 `MarkdownDocument::body()`。
- 未知 Skill 返回 `Ok(None)`。
- SKILL.md 调用时不可读返回 `SkillError::Io`。

- [ ] **步骤 7：实现 SkillSelectionRule 和类型驱动展开**

将 `SkillConfigRule` 的共享形状下沉为 `protocol::SkillSelectionRule`，config 直接使用该类型。`skill/Cargo.toml` 在 workspace crates 组增加 `prompt = { workspace = true }`。`FilesystemSkillFactory::new(global_root, rules)` 保存全局根和有序 rules；`create(SkillResourceRequest)` 按全局 `skills/`、允许时的 `cwd/.pi/skills`、`cwd/.agents/skills`、Extension paths 创建 Session Catalog，再应用后出现规则覆盖先出现规则的过滤。Skill metadata 和正文剥离统一使用 `prompt::MarkdownDocument`。`SkillInvocation::try_from(&str)` 解析 `/skill:name args`，`SkillCatalog::expand_command` 负责读取和构建 location/baseDir block。

- [ ] **步骤 8：运行三个 crate 的目标测试**

```bash
rtk cargo test -p tools
rtk cargo test -p skill
rtk cargo test -p prompt
rtk cargo clippy -p tools -p skill -p prompt --all-targets -- -D warnings
```

---

### 任务 5：PromptFactory 的 Session 生命周期接入与旧实现删除

**文件：**

- 修改：`crates/kernel/Cargo.toml`
- 修改：`crates/kernel/src/lib.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 新增：`crates/kernel/src/runtime/prompt.rs`
- 删除：`crates/kernel/src/prompt.rs`
- 删除：`crates/kernel/src/prompt/template.rs`
- 修改：`crates/kernel/tests/system_prompt.rs`
- 修改：`crates/kernel/tests/prompt_templates.rs`
- 修改：`crates/kernel/tests/session_capabilities.rs`
- 修改：`crates/app/Cargo.toml`
- 修改：`crates/app/src/lib.rs`

**接口：**

```rust
pub struct KernelFactory {
    prompt_factory: Arc<dyn prompt::PromptFactory>,
}

struct SessionRuntime {
    prompt: OnceLock<Arc<prompt::PromptSession>>,
    skills: OnceLock<Option<Arc<skill::SkillCatalog>>>,
}

impl Kernel {
    pub fn skills(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<SkillInfo>, KernelError>;

    pub fn invoke_skill(
        &self,
        session_id: &SessionId,
        name: &str,
    ) -> Result<String, KernelError>;

    pub fn prompt_diagnostics(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<PromptDiagnostic>, KernelError>;
}
```

两个 `OnceLock` 只解决 Extension Runtime 必须先构造、Prompt/Skill 资源必须在 trust/resources hooks 后加载的初始化顺序；初始化完成后不提供替换或 reload 方法。删除 Kernel 的全局 `skills` 快照和 SessionRuntime 的 Skill `RwLock`。

- [ ] **步骤 1：写 Session 加载顺序失败测试**

在 `session_capabilities.rs` 注册可返回 `ProjectTrustDecision::No` 和 Extension prompt path 的测试 Extension，断言：

- `No` 时不加载项目 `.pi/prompts` 和 cwd `AGENTS.md`。
- `Yes`/`Undecided` 时加载。
- Extension `resources_discover` 返回后才创建 PromptSession。
- SkillFactory 和 PromptFactory 收到相同的 Session cwd 与项目资源访问决定。
- 创建、Resume 和 Fork 都得到独立不可变 PromptSession。
- Session 活跃期间修改 Template 文件不会热更新；Close 后修改并 Resume 会重新加载。
- `session_transcript` 和 Store JSONL 不包含 System Prompt、Template 正文或 PromptSession 序列化副本。

- [ ] **步骤 2：运行 Kernel 目标测试确认失败**

```bash
rtk cargo test -p kernel --test session_capabilities prompt
```

- [ ] **步骤 3：替换 Kernel Factory 依赖**

workspace `kernel` 和 `app` Cargo 相对依赖组加入 `prompt = { workspace = true }`。`KernelError::Prompt` 改为 `prompt::PromptError`。删除 `SystemPromptFactory`、`PiSystemPromptFactory`、`ProjectContext` 和旧 Template 类型重导出。

`runtime/prompt.rs` 集中实现 PromptSession 初始化读取、每 Turn `BuiltSystemPrompt` 到 System AgentMessage 的物化，以及 `prompt_diagnostics` 只读投影；`session.rs` 只保留生命周期顺序，不重新展开 Prompt 逻辑。

- [ ] **步骤 4：调整 Session 初始化顺序**

`build_session_runtime` 不读取 Prompt 文件，只创建空 `OnceLock`。`start_extensions` 执行顺序固定为：

```text
project_trust
→ session_start
→ resources_discover（项目资源允许时）
→ SkillFactory::create(SkillResourceRequest)
→ PromptFactory::create
→ OnceLock::set
→ model/thinking restore events
```

重复初始化返回 `KernelError::Protocol("session prompt resources already initialized")`，不能静默覆盖。

- [ ] **步骤 5：在 app 组装 FilesystemPromptFactory**

```rust
let prompt_factory = Arc::new(FilesystemPromptFactory::new(
    config_root.clone(),
    snapshot.prompt.clone(),
));
```

KernelFactory 注入该 factory。`FilesystemSkillFactory::new(config_root.clone(), snapshot.skills.rules.clone())` 不再捕获 app 进程 cwd；Kernel 在 Session startup 传入真实 cwd、trust 结果和 Extension skill paths。`snapshot.skills.include_instructions` 由 Kernel 保存为不可变 bool，并传给每次 System Prompt 构建。

- [ ] **步骤 6：迁移旧测试并删除 Kernel Prompt 文件**

纯资源/Template/System 构建测试迁入 `crates/prompt/tests/` 后，Kernel 测试只保留真实 Session/Turn 集成断言。删除旧模块后运行 `rtk rg -n "PiSystemPromptFactory|SystemPromptFactory|ProjectContext|kernel::PromptTemplate" crates`，预期无生产引用。

- [ ] **步骤 7：运行目标测试**

```bash
rtk cargo test -p kernel --test session_capabilities
rtk cargo test -p kernel --test system_prompt
rtk cargo test -p app
rtk cargo clippy -p kernel -p app --all-targets -- -D warnings
```

---

### 任务 6：输入展开顺序、Run 级 Prompt Override 与 Queue

**文件：**

- 修改：`crates/protocol/src/extension/events/agent.rs`
- 修改：`crates/extension/src/runtime/agent.rs`
- 修改：`crates/extension/src/handler.rs`
- 修改：`crates/extension/src/dynamic.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/runtime/run.rs`
- 新增：`crates/kernel/src/runtime/input.rs`
- 修改：`crates/kernel/src/runtime/queue.rs`
- 修改：`crates/kernel/src/runtime/extension/host.rs`
- 修改：`crates/kernel/tests/lifecycle_order.rs`
- 修改：`crates/kernel/tests/turn.rs`
- 修改：`crates/kernel/tests/extensions_host.rs`
- 修改：`crates/kernel/tests/session_capabilities.rs`

**接口：**

```rust
pub struct BeforeAgentStartEvent {
    pub prompt: String,
    pub system_prompt: String,
    pub system_prompt_options: SystemPromptBuildOptions,
    pub skills: Vec<String>,
}

pub struct ExtensionCommandInvocation {
    pub name: String,
    pub arguments: String,
}

enum CommandInputDisposition {
    Handled,
    Continue(String),
}

impl TryFrom<&str> for ExtensionCommandInvocation {
    type Error = DynamicRegistryError;

    fn try_from(input: &str) -> Result<Self, Self::Error>;
}

impl SessionRuntime {
    fn expand_prompt_input(
        &self,
        session_id: &SessionId,
        input: &str,
    ) -> Result<String, KernelError>;
}
```

`DynamicRegistryError` 增加准确变体 `InvalidCommandInvocation(String)`，其他现有变体保持不变。

- [ ] **步骤 1：写完整输入顺序失败测试**

用记录调用顺序的 Extension 和 faux model 断言：

```text
extension command → no input hook / no model
input hook transform → skill expansion → template expansion → before_agent_start
unknown slash command → input hook → unchanged user message
```

另测 `!`/`!!` 仍早于 extension command。测试必须通过实际 `Kernel::run`，不能调用正文私有解析器。

- [ ] **步骤 2：写 Run Prompt Override 失败测试**

faux model 第一个 Turn 返回 ToolCall、第二个 Turn 返回完成消息。Extension 在 `before_agent_start` 返回 `replacement-system`。断言两次 ModelRequest 的第一条 System Message 都是 replacement；下一次独立 Run 恢复基础 Prompt。

- [ ] **步骤 3：运行目标测试确认失败**

```bash
rtk cargo test -p kernel --test lifecycle_order prompt_order
rtk cargo test -p kernel --test turn system_prompt_override
```

- [ ] **步骤 4：实现 ExtensionCommandInvocation 和前置分发**

在获取 Run gate 和创建 Agent TurnId 之前解析 slash input；能解析且 Registry 能唯一 resolve 时调用 `ExtensionCommandHandler::handle(arguments, &Value::Null, context)`。成功后只生成一个用于满足 `RunResult` 类型的 RunId 并返回空 messages/turns，不发出 RunStart、TurnStart 或 Provider 请求。Command 不存在时继续普通输入；短名称歧义返回明确 KernelError，不把歧义命令发给模型。

`ExtensionCommandInvocation::try_from` 只把第一个普通空格 U+0020 视为 name/arguments 分隔符，与 pi 的 Extension Command 行为一致；Tab 和换行保留在 command name 中并按未知命令继续普通输入。

新增 `runtime/input.rs`，以 `CommandInputDisposition::{Handled, Continue(String)}` 表达 slash command 预处理结果。Extension Command 分发先执行；Continue 后 `run.rs` 再建立正常 Run/Turn 关联并调用 input hook，随后由 `SessionRuntime::expand_prompt_input` 执行 Skill 和 Template。组合实现放在该类型及 Kernel/SessionRuntime 方法中；`run.rs` 只按判别结果提前返回或进入 Agent Run，避免继续增大当前已超过 1k 行的运行主文件。

- [ ] **步骤 5：实现 SessionRuntime::expand_prompt_input**

方法按以下顺序；日志中的 SessionId 使用方法参数，不在 SessionRuntime 复制 map key：

```rust
let skill_expanded = match skills.expand_command(input) {
    Ok(Some(expanded)) => expanded,
    Ok(None) => input.to_string(),
    Err(error) => {
        tracing::warn!("failed to expand Skill command for session {}: {}", session_id, error);
        input.to_string()
    }
};
Ok(prompt.expand_template(&skill_expanded))
```

不得打印 expanded content。

- [ ] **步骤 6：调整 Run 顺序和 Override 生命周期**

Input hook transform 后调用 `expand_prompt_input`。第一次构建 `BuiltSystemPrompt` 后将 options 传入 `BeforeAgentStartEvent`。用 Run 局部 `Option<String>` 保存 Extension replacement；每个后续 Turn 有 replacement 时使用该文本，否则使用该 Turn 的新基础 Prompt。局部值随正常、错误或取消退出自然释放，不写入 SessionRuntime。

- [ ] **步骤 7：让 Queue 使用同一展开器**

`Kernel::queue_message` 在创建 Message/Record 前：

- 若 slash input 能 resolve 为 Extension Command，返回 `KernelError::ExtensionCommandCannotQueue`。
- 否则执行 Skill 和 Template 展开。
- 持久化展开后的 User Message。

Steering 和 FollowUp 共用该方法，不能复制展开逻辑。

- [ ] **步骤 8：运行 Kernel 和 Extension 测试**

```bash
rtk cargo test -p extension
rtk cargo test -p kernel --test lifecycle_order
rtk cargo test -p kernel --test turn
rtk cargo test -p kernel --test session_capabilities
rtk cargo clippy -p extension -p kernel --all-targets -- -D warnings
```

---

### 任务 7：Available Commands、动态 Command 与 ACP 2.0

**文件：**

- 修改：`crates/protocol/src/extension/registration.rs`
- 修改：`crates/protocol/src/event.rs`
- 修改：`crates/extension/src/dynamic.rs`
- 修改：`crates/extension/src/context.rs`
- 修改：`crates/extension/src/host.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/kernel/src/runtime/session.rs`
- 新增：`crates/kernel/src/runtime/command.rs`
- 修改：`crates/kernel/src/runtime/extension/host.rs`
- 修改：`crates/extension/tests/dynamic.rs`
- 修改：`crates/extension/tests/context.rs`
- 修改：`crates/kernel/tests/session_capabilities.rs`
- 修改：`crates/kernel/tests/extensions_host.rs`
- 修改：`crates/acp/src/mapping.rs`
- 修改：`crates/acp/src/server.rs`
- 修改：`crates/acp/src/extension.rs`
- 修改：`crates/protocol/src/acp.rs`
- 修改：`crates/protocol/tests/events.rs`
- 修改：`crates/acp/tests/mapping.rs`
- 修改：`crates/acp/tests/extension.rs`

**接口：**

```rust
pub struct ExtensionCommandDefinition {
    pub name: String,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
}

impl Kernel {
    pub fn available_commands(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<AvailableAgentCommand>, KernelError>;

    pub fn available_commands_event(
        &self,
        session_id: &SessionId,
    ) -> Result<AgentEvent, KernelError>;
}

`AgentEventPayload` 增加准确变体：

```rust
AvailableCommandsChanged { commands: Vec<AvailableAgentCommand> }
```
```

- [ ] **步骤 1：写命令合并与冲突失败测试**

在 Kernel 测试注册：

- 一个 unique short Extension Command。
- 两个同 short name 的 Extension Command。
- 一个 `/skill:review`。
- 一个名为 `review` 和一个名为 `skill:review` 的 Template。

断言有效列表遵守真实执行优先级：unique Extension > Skill > Template；歧义 Extension Command 只发布 qualified names；description 和 argument_hint 保留；列表顺序稳定。

- [ ] **步骤 2：扩展动态 Registry 测试**

新增 `DynamicCommandRegistry::remove(&ExtensionId, &str)`，只允许删除所属 Extension 的 qualified command。测试 upsert/remove 后快照版本变化，旧 `Arc<CommandRegistry>` 仍可读取旧值。

`ExtensionContext::unregister_command(name)` 和 `ExtensionHost::unregister_command` 使用 ExtensionInvocation 绑定所有权，不允许删除其他 Extension 的命令。

- [ ] **步骤 3：运行 Extension/Kernel 测试确认失败**

```bash
rtk cargo test -p extension --test dynamic
rtk cargo test -p extension --test context
rtk cargo test -p kernel --test session_capabilities available_commands
```

- [ ] **步骤 4：实现统一 AvailableAgentCommand 投影**

Kernel 从一个 Command Registry snapshot、Session Skill Catalog 和 PromptSession Template metadata 构建完整列表。每个 Extension Command 始终发布 `extension_id/name`；短名称全局唯一时额外发布 short alias。Skill 名称发布为 `skill:{name}`。最终按执行类别再按名称稳定排序，并按“Extension、Skill、Template”的真实执行优先级去重同名项。

该投影和 `available_commands_event` 放在 `runtime/command.rs`；`session.rs` 和 Extension Host 只调用它，不复制命令合并或元数据生成逻辑。`protocol/tests/events.rs` 增加新 payload 的 JSON round-trip。

- [ ] **步骤 5：实现带元数据的 Commands 事件**

`available_commands_event` 使用 Session 的 `event_sequence.fetch_add`、Kernel Clock 和新生成的系统 TurnId 创建 `AgentEvent`。动态 register/unregister 成功后，如果当前 Session 有 live sink，则发送 `AvailableCommandsChanged`；没有 live sink 时只更新 Registry，下一次 Resume 发送完整快照。

- [ ] **步骤 6：写 ACP 标准映射失败测试**

在 `mapping.rs` 测试断言：

```rust
wire::SessionUpdate::AvailableCommandsUpdate(
    wire::AvailableCommandsUpdate::new(vec![
        wire::AvailableCommand::new("review", "Review changes")
            .input(wire::AvailableCommandInput::Text(
                wire::TextCommandInput::new("<path>")
            )),
    ])
)
```

同时断言通知 `_meta` 保留 Product namespace 下的 turnId、字符串 timestampMs 和 sequence。

- [ ] **步骤 7：在 Session New/Resume/Fork 后发送快照**

抽取 `AcpServer` 内聚类型方法 `send_available_commands(session_id, connection)`，内部调用 Kernel event 和 `AcpEventMapper`。New Session response 成功后发送；Resume 的 replay 消息发送完成后发送；Fork 通过现有 extension method 创建新 Session 后向同一 connection 发送。

该方法接收 `&SessionId` 和 `&ConnectionTo<Client>`，不是只包装单参数的自由 helper。

同时把私有 Skill API 改为 Session 级：`AcpSkillParameters` 增加 `session_id`，Skill List 使用 `AcpSessionParameters`；分别调用 `Kernel::invoke_skill(&SessionId, name)` 和 `Kernel::skills(&SessionId)`。这两个 API 与 Available Commands 必须读取同一个 Session Catalog。

- [ ] **步骤 8：运行 ACP、Extension 与 Kernel 目标测试**

```bash
rtk cargo test -p acp --test mapping
rtk cargo test -p acp --test extension
rtk cargo test -p extension
rtk cargo test -p kernel --test extensions_host
rtk cargo clippy -p acp -p extension -p kernel --all-targets -- -D warnings
```

---

### 任务 8：WebUI Command Palette

**文件：**

- 修改：`web/src/domain/model.ts`
- 修改：`web/src/workspace/state.ts`
- 修改：`web/src/workspace/updateDecoder.ts`
- 修改：`web/src/workspace/controller.ts`
- 修改：`web/src/features/conversation/Conversation.tsx`
- 修改：`web/src/features/composer/Composer.tsx`
- 新增：`web/src/features/composer/CommandPalette.tsx`
- 修改：`web/src/theme/workbench.css`

**接口：**

```ts
export type AvailableCommandEntity = Readonly<{
  name: string;
  description: string;
  argumentHint?: string;
}>;

// WorkspaceState
availableCommands: readonly AvailableCommandEntity[];

// WorkspaceAction
{ readonly type: "commands/replaced"; readonly commands: readonly AvailableCommandEntity[] }
```

- [ ] **步骤 1：实现 ACP update 解码和 Workspace 快照**

`SessionUpdateDecoder` 识别 `sessionUpdate === "available_commands_update"`，严格验证 `availableCommands` 数组、name、description 和可选 `input.type === "text"`/`input.hint`。无效单项跳过并增加 diagnostic，不让整条通知破坏 Session。

`session/deactivated` 和 `transcript/cleared` 清空 `availableCommands`，`commands/replaced` 原子替换完整列表。

`WorkspaceController` 的 skill list 和 invoke 请求都发送当前 `sessionId`，确保 Skills Panel、`/skill:name` 和 Available Commands 使用同一个 Session Catalog。

- [ ] **步骤 2：实现 CommandPalette 独立组件**

组件 props：

```ts
export type CommandPaletteProps = Readonly<{
  commands: readonly AvailableCommandEntity[];
  query: string;
  selectedIndex: number;
  onSelect: (command: AvailableCommandEntity) => void;
}>;
```

只在 textarea 当前文本以 `/` 开头且命令 token 中不含空白时展示。过滤规则为 name 的大小写不敏感 prefix match；显示 name、argumentHint、description。

- [ ] **步骤 3：接入键盘与输入状态**

Composer 行为：

- `ArrowDown`/`ArrowUp` 循环选择。
- `Enter` 或 `Tab` 选择候选并写入 `/${name}`；有 hint 时追加空格。
- `Escape` 关闭本次 palette，下一次文本变化后可重新出现。
- `Ctrl/⌘ + Enter` 保持发送，不被 palette 截获。
- 鼠标选择与键盘选择调用同一个 `onSelect`。
- Palette 只写输入框，不展开 Template/Skill，不直接调用 Extension endpoint。

- [ ] **步骤 4：增加亮色样式和可访问性**

使用现有亮色 token，Palette 使用 `role="listbox"`，候选使用 `role="option"` 和 `aria-selected`，textarea 设置 `aria-controls`/`aria-expanded`。最大高度和滚动不遮挡 composer actions，窄屏保持全宽。

- [ ] **步骤 5：运行前端检查和构建**

```bash
cd web
rtk npm run check
rtk npm run build
```

预期：TypeScript、ESLint 和 Vite build 全部通过；不新增前端测试文件。

---

### 任务 9：文档、全量验证与真实后端 WebUI E2E

**文件：**

- 修改：`claw.toml`
- 修改：`README.md`
- 修改：`README_zh.md`
- 修改：`docs/extensions/hooks.md`
- 检查：`Cargo.lock`

- [ ] **步骤 1：补充用户文档和配置示例**

`claw.toml` 增加默认 `[prompt]`：

```toml
[prompt]
load_project_instructions = true
load_templates = true
template_paths = []
```

README 中说明全局/项目 `SYSTEM.md`、`APPEND_SYSTEM.md`、指令候选、`.pi/prompts/*.md`、`/skill:name`、Template 参数语法和“不支持热更新”。Extension 文档补充 `resources_discover.prompt_paths` 与 `before_agent_start.system_prompt_options`。

- [ ] **步骤 2：检查无旧实现、重复类型和正文测试支撑**

运行：

```bash
rtk rg -n "PiSystemPromptFactory|SystemPromptFactory|ProjectContext|PromptTemplateCatalog" crates/kernel/src
rtk rg -n "struct (ProjectInstruction|PromptTemplateInfo|ToolPromptContribution|AvailableAgentCommand)" crates --glob '*.rs'
rtk rg -n "#\[cfg\(test\)\]" crates --glob '*.rs'
```

预期：Kernel 旧 Prompt 类型无结果；公共结构体只在 protocol 定义；新增正文文件没有测试支撑模块。已有合法 `#[cfg(test)] mod tests` 不因本任务删除。

- [ ] **步骤 3：运行 Rust 全量验证**

```bash
rtk cargo fmt --all -- --check
rtk cargo clippy --workspace --all-targets --all-features -- -D warnings
rtk cargo test --workspace
rtk git diff --check
```

所有 error、warning 和 test failure 必须修复，不通过删功能或降低 lint 处理。

- [ ] **步骤 4：运行 Web 全量验证**

```bash
cd web
rtk npm run check
rtk npm run build
```

- [ ] **步骤 5：启动正式后端**

从仓库根目录运行：

```bash
rtk cargo run -p app --bin clawcode -- serve --bind 127.0.0.1:3000 --web-root web/dist
```

使用已修复的真实 `claw.toml` 和真实 Provider，不启动 fixture backend。

- [ ] **步骤 6：使用真实 WebUI 验证默认资源与 Template**

在测试 cwd 创建 `.pi/prompts/review.md`：

```markdown
---
description: Review one selected file
argument-hint: "<path>"
---
Read and review $1. Report exactly which path you reviewed.
```

浏览器打开 `http://127.0.0.1:3000/`，创建该 cwd 的 Session，验证 `/` Palette 显示 review、hint 和 description。提交 `/review README.md`，确认真实模型收到展开后的指令并针对 README.md 回复。

- [ ] **步骤 7：验证 SYSTEM、APPEND、AGENTS 与 Skill**

在同一测试 cwd 创建 `.pi/SYSTEM.md`、`.pi/APPEND_SYSTEM.md`、`AGENTS.md` 和 `.pi/skills/e2e-prompt/SKILL.md`，内容分别要求模型在回复中给出四个不重复标记。创建新 Session 后提交 `/skill:e2e-prompt execute`，确认真实回复包含四类指令效果；删除 `.pi/SYSTEM.md` 后创建另一个 Session，确认恢复 ProductIdentity 默认主体行为。

- [ ] **步骤 8：验证 Session Resume 与 Commands 快照**

刷新页面并 Resume Session，确认 Template 与 Skill Commands 完整快照重新出现，Transcript 顺序未被 Commands Update 扰乱。Extension Command 的“不触发 Provider”行为由任务 6 的真实 Kernel Run 测试和任务 7 的 ACP WebSocket 集成测试覆盖；正式配置只启用现有安全 Guard Extension，不为手工 E2E 临时增加生产 Command。

- [ ] **步骤 9：收尾审查**

停止正式后端，删除仅用于手工 E2E 的测试 cwd 资源。运行 `rtk git status --short`，确认只包含本计划范围内文件；不创建 commit，等待用户明确指令。

## 完成定义

- 独立 `prompt` crate 通过 `PromptFactory` 接入 Kernel，Kernel 旧 Prompt 实现完全删除。
- pi 0.84.1 的 System/Append/Instruction/Template/Skill 核心语义有集成测试覆盖。
- `before_agent_start` replacement 在完整 Run 内有效，输入和 Queue 顺序与 pi 一致。
- ACP 2.0 标准 Available Commands 在 New/Resume/Fork 和动态 Command 变化时发送。
- WebUI Command Palette 使用后端快照，不在客户端展开 Prompt。
- 配置、公共类型、Cargo 分组、日志和测试目录符合 AGENTS.md。
- Rust、Web check/build 和真实 Provider WebUI E2E 全部通过。
