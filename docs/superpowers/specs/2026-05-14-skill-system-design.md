# Skill 系统设计

**日期**: 2026-05-14
**状态**: 待审核

参考实现: Codex `core-skills` crate（`/Users/isbset/Documents/codex/codex-rs/core-skills/src/`）

---

## 1. 目标

为 clawcode 实现独立的 skill 模块，使 LLM 能够在系统提示词中看到可用 skill 目录，并通过 `$skill-name` 提及语法按需加载 SKILL.md 主体内容。采用渐进式信息披露策略：目录始终在上下文中 → 主体仅在提及时加载。

与 Codex 的关键差异：
- **仅 `.agents/skills/` 目录** — 不扫描多来源（无插件、无系统级、无配置层）
- **独立 crate** — 与 `crates/tools` 同级，作为 `crates/skills`
- **无依赖声明** — skill 不声明 MCP/env_var 工具依赖
- **无隐式调用** — 不实现 Codex 的 implicit invocation 分析

## 2. 模块总览

```
crates/config/src/skills_config.rs     # 新增 — SkillsConfig, SkillConfigRule
crates/config/src/config.rs            # 修改 — AppConfig 加 skills 字段
crates/skills/                         # 新增 crate
  Cargo.toml
  src/
    lib.rs                             # SkillMetadata, SkillRegistry, SkillScope（公共 API）
    loader.rs                          # 发现：扫描 .agents/skills/**/SKILL.md
    render.rs                          # 渲染：目录渲染为 skills_xml 文本
    injection.rs                       # 匹配：$skill-name 提及检测 + 主体加载
    config_rules.rs                    # 新增 — 按名称/路径的启用禁用规则解析

crates/kernel/src/prompt/mod.rs        # 已有 — skills_xml 字段已声明，直接连接
crates/kernel/src/turn.rs              # 修改 — execute_turn() 中调用 SkillRegistry
crates/kernel/Cargo.toml               # 修改 — 添加 skills 依赖
```

**不改动**: `protocol/` 全部、`context.rs`、`tools/` 全部

## 3. 核心类型

### 3.1 SkillScope

`skills/src/lib.rs`:

```rust
/// Skill 的来源范围，决定优先级和显示名称。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkillScope {
    /// 仓库级 — 来自当前项目的 .agents/skills/
    Repo = 0,
    /// 用户级 — 来自 $HOME/.agents/skills/
    User = 1,
}
```

### 3.2 SkillMetadata

```rust
/// 从 SKILL.md 解析出的 skill 元数据。
#[derive(Debug, Clone)]
pub struct SkillMetadata {
    /// skill 名称，来自 frontmatter 的 name 字段，缺省为父目录名。
    /// 用作 $skill-name 提及匹配的 key。
    pub name: String,
    /// skill 描述，来自 frontmatter 的 description 字段。
    /// 渲染到技能目录中供 LLM 判断是否使用。
    pub description: String,
    /// SKILL.md 文件的规范绝对路径。
    pub path: PathBuf,
    /// 来源范围。
    pub scope: SkillScope,
}
```

### 3.3 SkillRegistry

```rust
/// 技能注册表 — 发现、索引、渲染、匹配。
///
/// 创建后不可变（技能在 turn 开始时加载，turn 内不变）。
/// 使用静态缓存避免重复扫描文件系统（类似 Instructions::load 的模式）。
#[derive(Debug, Clone)]
pub struct SkillRegistry {
    skills: Vec<SkillMetadata>,
}

impl SkillRegistry {
    /// 从 cwd 向上遍历，发现所有 .agents/skills/ 目录中的 SKILL.md 文件。
    ///
    /// 扫描策略：
    /// 1. 从 cwd 向上遍历到文件系统根
    /// 2. 对每个目录，检查 `<dir>/.agents/skills/` 是否存在
    /// 3. 若存在，BFS 遍历其下所有子目录（最大深度 4 层），查找 SKILL.md
    /// 4. Repo 级 skill 来自项目根附近的 .agents/skills/
    /// 5. User 级 skill 来自 $HOME/.agents/skills/
    /// 6. 同名 skill 按 scope 优先级（Repo > User）去重
    ///
    /// 结果被缓存（以 cwd 的规范绝对路径为 key），避免每次 turn 重复扫描。
    pub fn discover(cwd: &Path) -> Self;

    /// 返回发现的 skill 总数。
    pub fn len(&self) -> usize;

    /// 是否未发现任何 skill。
    pub fn is_empty(&self) -> bool;

    /// 将 skill 目录渲染为系统提示词文本。
    /// 当 registry 为空时返回 None。
    /// 输出格式见 §4。
    pub fn render_catalog(&self) -> Option<String>;

    /// 从文本中提取 $skill-name 提及，返回匹配的 skill 列表。
    /// 按名称精确匹配，不区分大小写。无匹配时返回空 Vec。
    pub fn resolve_mentions(&self, text: &str) -> Vec<&SkillMetadata>;

    /// 加载指定 skill 的 SKILL.md 全文。
    /// 返回 None 表示文件无法读取。
    pub fn load_body(&self, name: &str) -> Option<String>;
}
```

## 4. 配置层启用/禁用规则

与 Codex 的 `config_rules.rs` 一致：配置层（`claw.toml` 的 `[skills]` section）可以按名称或路径选择性启用/禁用 skill。规则按配置优先级叠加，后匹配的规则覆盖先匹配的规则。

### 4.1 配置类型

`config/src/skills_config.rs`:

```rust
/// 顶层 skill 配置，放在 AppConfig 中。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillsConfig {
    /// 是否在系统提示词中注入 skill 目录块。
    /// 默认 true。
    #[serde(default = "default_include_instructions")]
    pub include_instructions: bool,

    /// 逐 skill 的启用/禁用规则列表。
    /// 越靠后的规则优先级越高（后匹配覆盖先匹配）。
    #[serde(default)]
    pub rules: Vec<SkillConfigRule>,
}

fn default_include_instructions() -> bool { true }

/// 单条启用/禁用规则。path 和 name 互斥。
#[derive(Debug, Clone, Deserialize)]
pub struct SkillConfigRule {
    /// 按 SKILL.md 文件绝对路径匹配。
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// 按 skill 名称匹配。
    #[serde(default)]
    pub name: Option<String>,
    /// true = 启用，false = 禁用。
    pub enabled: bool,
}
```

### 4.2 AppConfig 集成

`config/src/config.rs` 的 `AppConfig` 增加字段：

```rust
pub struct AppConfig {
    // ... 现有字段 ...
    /// Skill 子系统配置。
    #[serde(default)]
    pub skills: SkillsConfig,
}
```

### 4.3 claw.toml 示例

```toml
[skills]
include_instructions = true

# 按名称禁用特定 skill
[[skills.rules]]
name = "deprecated-skill"
enabled = false

# 按路径启用特定 skill（路径形式用于精确控制）
[[skills.rules]]
path = "/home/user/.agents/skills/experimental/SKILL.md"
enabled = true

# 按名称批量禁用
[[skills.rules]]
name = "skill-creator"
enabled = false
```

### 4.4 规则解析

`skills/src/config_rules.rs`:

```rust
/// 从 SkillsConfig 解析出禁用的 skill 路径集合。
///
/// 算法：
/// 1. 遍历 rules 列表（保持配置中的顺序）
/// 2. 对于 path 选择器：若 enabled=false，将路径加入禁用集合；若 enabled=true 且之前被禁用则从禁用集合中移除
/// 3. 对于 name 选择器：遍历所有匹配名称的 skill，同上处理
/// 4. 同名/同路径规则按配置顺序后发覆盖
/// 5. 同时有 path 和 name 的规则 → 警告并忽略
/// 6. 既无 path 也无 name 的规则 → 警告并忽略
pub fn resolve_disabled_paths(
    skills: &[SkillMetadata],
    config: &SkillsConfig,
) -> HashSet<PathBuf> { ... }
```

### 4.5 SkillRegistry 集成

`SkillRegistry` 在 `discover()` 时接收 `&SkillsConfig`，内部调用 `resolve_disabled_paths` 计算禁用集合：

```rust
impl SkillRegistry {
    /// 从 cwd 向上遍历，发现所有 .agents/skills/ 目录中的 SKILL.md 文件，
    /// 并应用 config 中的启用/禁用规则。
    pub fn discover(cwd: &Path, config: &SkillsConfig) -> Self { ... }

    /// 检查指定 skill 是否已启用。
    pub fn is_enabled(&self, name: &str) -> bool { ... }

    /// 返回所有已启用的 skill（目录渲染仅包含启用的 skill）。
    fn enabled_skills(&self) -> Vec<&SkillMetadata> { ... }
}
```

规则只在 `discover()` 时计算一次，存入 registry 内部字段 `disabled_paths: HashSet<PathBuf>`。后续 `render_catalog()` 和 `resolve_mentions()` 自动跳过禁用项。

## 5. SKILL.md 格式

### 5.1 文件布局

```
.agents/skills/
  skill-creator/
    SKILL.md           # 必需 — YAML frontmatter + Markdown body
    scripts/           # 可选 — 可执行脚本
    references/        # 可选 — 按需加载的文档
    assets/            # 可选 — 模板、图标等
```

### 5.2 SKILL.md 结构

```markdown
---
name: skill-creator
description: Guide for creating effective skills
---

# Skill Creator

This skill helps you create new skills...

## Usage
...
```

- `name` — 必需。skill 的唯一标识符，用于 `$name` 提及匹配。缺省为父目录名。
- `description` — 必需。单行描述，渲染到目录中。最多 1024 字符。
- YAML frontmatter 由 `---` 分隔。缺少 frontmatter 的 SKILL.md 视为解析错误，跳过该 skill。

### 5.3 解析规则

`skills/src/loader.rs`:

```rust
#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

/// 从 SKILL.md 内容中提取 YAML frontmatter。
/// 返回第一个 `---` 和第二个 `---` 之间的内容。
/// 文件不以 `---` 开头时返回 None。
fn extract_frontmatter(contents: &str) -> Option<String> { ... }

/// 从 SKILL.md 路径和内容解析 SkillMetadata。
/// name 缺失时回退为父目录名。
/// description 缺失时回退为空字符串。
fn parse_skill_file(path: &Path) -> Result<SkillMetadata, SkillError> { ... }
```

## 6. 发现与加载流程

`skills/src/loader.rs`:

### 6.1 根目录确定

```rust
/// 从 cwd 向上遍历，收集所有存在的 .agents/skills/ 目录。
///
/// 遍历顺序：从 cwd 开始，向父目录逐层查找。
/// 这保证了 Repo 级 skill（离项目更近）优先于 User 级。
///
/// 每个目录被标记为 Repo 或 User scope：
/// - 位于 $HOME 之外的目录 → Repo
/// - 位于 $HOME 之内的目录 → User
fn find_skill_roots(cwd: &Path) -> Vec<SkillRoot> { ... }

struct SkillRoot {
    path: PathBuf,       // <dir>/.agents/skills/
    scope: SkillScope,
}
```

### 6.2 目录扫描

```rust
/// BFS 遍历 skill root 下的子目录，查找 SKILL.md 文件。
///
/// 参数：
/// - max_depth: 4（root 下的最大子目录深度）
/// - 跳过以 . 开头的目录
/// - 不跟随符号链接
///
/// 返回所有找到的 SKILL.md 路径列表。
fn discover_skills_under_root(root: &SkillRoot) -> Vec<PathBuf> { ... }
```

### 6.3 去重规则

```rust
/// 按名称去重，scope 优先级 Repo > User。
/// 当 Repo 和 User 中存在同名 skill 时，保留 Repo 版本。
///
/// 结果按 scope（Repo 优先）、名称排序。
fn deduplicate(skills: Vec<SkillMetadata>) -> Vec<SkillMetadata> { ... }
```

### 6.4 缓存策略

```rust
use std::sync::OnceLock;
use std::collections::HashMap;
use std::sync::Mutex;

static CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<SkillRegistry>>>> = OnceLock::new();

impl SkillRegistry {
    pub fn discover(cwd: &Path) -> Self {
        let key = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        // 检查缓存 → 未命中时加载 → 存入缓存 → 返回 Arc::clone
        ...
    }
}
```

缓存以 cwd 的规范绝对路径为 key。当 cwd 不变时，多次 `discover()` 调用共享同一份结果。

## 7. 目录渲染

`skills/src/render.rs`:

### 7.1 输出格式

采用 XML 格式（与 OpenCode 参考实现一致），包含 `<available_skills>` 包装和每个 skill 的 `<skill>` 条目：

```xml
<available_skills>
  <skill>
    <name>skill-creator</name>
    <description>Guide for creating effective skills</description>
    <location>file:///path/to/skill-creator/SKILL.md</location>
    <scope>repo</scope>
  </skill>
  <skill>
    <name>my-skill</name>
    <description>Custom user-level workflow</description>
    <location>file:///home/user/.agents/skills/my-skill/SKILL.md</location>
    <scope>user</scope>
  </skill>
</available_skills>

### How to use skills
- Discovery: The list above shows available skills (name + description + file path).
- Trigger: If the user names a skill (with `$SkillName` or plain text), use that skill.
- Usage: After deciding to use a skill, read its SKILL.md with the Read tool.
- Paths: Relative paths in SKILL.md resolve relative to the skill directory.
```

每个 `<skill>` 包含四个子元素：
- `<name>` — skill 名称，用于 `$name` 提及匹配
- `<description>` — 单行描述，帮助 LLM 判断是否适用
- `<location>` — SKILL.md 的 `file://` URL
- `<scope>` — 来源范围（`repo` 或 `user`）

### 7.2 渲染函数

```rust
/// 渲染 skill 目录。registry 为空时返回 None。
///
/// 不对描述做预算截断（项目级 skill 数量有限，通常不超过 10 个）。
pub fn render_catalog(skills: &[SkillMetadata], roots: &[SkillRoot]) -> String { ... }
```

### 7.3 scope 元素

| scope | 值 | 含义 |
|-------|------|------|
| Repo | `<scope>repo</scope>` | 项目级，位于当前仓库的 .agents/skills/ |
| User | `<scope>user</scope>` | 用户级，位于 $HOME/.agents/skills/ |

## 8. 提及检测与匹配

`skills/src/injection.rs`:

### 8.1 提及语法

```rust
/// 从文本中提取所有 $skill-name 模式的提及。
///
/// 匹配规则：
/// - `$` 后跟字母、数字、连字符、下划线
/// - 排除常见环境变量名: $PATH, $HOME, $USER, $SHELL, $PWD, $LANG, $TERM
/// - 最大匹配长度: 64 字符
///
/// 返回去重后的 skill 名称列表（保持首次出现顺序）。
fn extract_skill_mentions(text: &str) -> Vec<&str> { ... }
```

### 8.2 名称解析

```rust
impl SkillRegistry {
    /// 将提及名称解析为 SkillMetadata。
    ///
    /// 匹配策略（与 Codex 一致）：
    /// 1. 精确名称匹配（不区分大小写）
    /// 2. 仅当名称无歧义时才返回（同名计数 == 1）
    /// 3. 无匹配 → 跳过（不报错）
    pub fn resolve_mentions(&self, text: &str) -> Vec<&SkillMetadata> { ... }
}
```

### 8.3 主体加载

```rust
impl SkillRegistry {
    /// 读取指定 skill 的 SKILL.md 全文。
    /// 用于在 LLM 提及 skill 后将详细指令注入上下文。
    pub fn load_body(&self, name: &str) -> Option<String> { ... }
}
```

在 P1 阶段，主体加载由 LLM 通过现有的 `read` 工具按需完成（渐进式信息披露的第三步）。`load_body` 方法预留，后续阶段可用于自动注入。

## 9. 注入点与数据流

### 9.1 execute_turn() 注入

`kernel/src/turn.rs`，在 `execute_turn()` 中：

```
execute_turn()
  │
  ├─ 1. 构建 EnvironmentInfo（已有）
  ├─ 2. 加载指令文件（已有）
  ├─ 3. SkillRegistry::discover(&ctx.cwd, &ctx.skills_config)   ← 新增，传入 config
  ├─ 4. 构造 SystemPrompt {
  │        agent_prompt,
  │        environment,
  │        instructions,
  │        skills_xml: registry.render_catalog(),                ← 新增：连接现有字段
  │        user_prompt,
  │    }
  ├─ 5. system_prompt.render()
  ├─ 6. CompletionRequest::builder()
  │      .preamble(rendered)
  │      .chat_history(history)
  │      .tools(...)
  │      .build()
  └─ 7. llm.stream(request)
```

### 9.2 TurnContext 扩展

`kernel/src/turn.rs` 的 `TurnContext` 增加字段：

```rust
pub(crate) struct TurnContext {
    // ... 现有字段 ...
    /// Skill 配置（来自 AppConfig.skills）。
    #[builder(default)]
    pub skills_config: SkillsConfig,
}
```

### 9.3 配置传递路径

```
AppConfig.skills
  → Kernel::new_session() 时读取
  → spawn_thread() 时注入 Thread
  → run_loop() 中构建 TurnContext 时传入
  → execute_turn() 中传给 SkillRegistry::discover()
```

### 9.4 skills_xml 字段连接

`prompt/mod.rs` 中的 `SystemPrompt.skills_xml: Option<String>` 字段已声明、已渲染（第 94-96 行），仅需在 `execute_turn()` 中填充。无需修改 `SystemPrompt` 类型本身。

## 10. Crate 结构

### 10.1 Cargo.toml

`crates/skills/Cargo.toml`:

```toml
[package]
name = "skills"
edition.workspace = true
version.workspace = true
description = "Skill discovery, loading, and rendering"

[dependencies]
config = { path = "../config" }

serde = { workspace = true, features = ["derive"] }
serde_yaml = "0.9"
thiserror = { workspace = true }
```

### 10.2 工作空间注册

`Cargo.toml`（根）添加 `skills = { path = "crates/skills" }` 到 `[workspace.dependencies]`。

### 10.3 kernel 依赖

`crates/kernel/Cargo.toml` 添加 `skills = { workspace = true }`。

## 11. 依赖关系

```
config/skills_config   ──→ serde（纯数据，无内部依赖）
skills/lib             ──→ skills/loader + skills/render + skills/injection + skills/config_rules
skills/loader          ──→ serde_yaml（YAML frontmatter 解析）
skills/render          ──→ 无外部依赖（纯字符串拼接）
skills/injection       ──→ 无外部依赖（纯字符串匹配）
skills/config_rules    ──→ config/skills_config（读取 Rules）
kernel/turn            ──→ skills（调用 discover + render_catalog）+ config（传入 SkillsConfig）
kernel/session         ──→ config（读取 AppConfig.skills 并传给 TurnContext）
kernel/prompt/mod      ──→ 无新增依赖（skills_xml 字段已存在）
```

## 12. 不变式与错误语义

- **发现不阻塞**: 任何 .agents/skills/ 目录不存在或无法读取时，返回空 registry。不影响 turn 执行。
- **解析软失败**: 单个 SKILL.md 解析失败（缺 frontmatter、YAML 格式错误）时，跳过该 skill，不阻塞其他 skill 的加载。
- **配置语法错不崩溃**: `SkillsConfig` 中无效的 rules 条目（同时有 path 和 name，或两者都没有）→ 警告并忽略该条目。非法的 toml 结构 → 使用默认 `SkillsConfig`。
- **目录为空不注入**: `registry.enabled_skills()` 为空时，`render_catalog()` 返回 `None`，`skills_xml` 为 `None`，系统提示词不包含 skills section。
- **提及无匹配不报错**: 用户输入中的 `$unknown-skill` 被静默忽略。禁用的 skill 即使被提及也不匹配。
- **名称唯一性**: 同名 skill 按 Repo > User 去重，确保 `$name` 解析唯一。
- **缓存正确性**: 缓存以 cwd 规范路径为 key。cwd 不变时复用，cwd 变更时重新扫描。config 变更通过缓存 key 的 config 哈希部分触发重扫。
- **默认启用**: 无 config rules 时，所有发现的 skill 均处于启用状态。
- **规则后发覆盖**: 同名/同路径的多条规则，列表中靠后者生效（与 TOML 数组语义一致）。

## 13. 测试点

- `SkillMetadata`: frontmatter 正确解析；name 缺省回退为目录名；description 缺省为空字符串；缺少 frontmatter 时返回 SkillError
- `SkillRegistry::discover()`: 找到 .agents/skills/ 下的 SKILL.md；无 .agents/skills/ 时返回空；同名去重（Repo 优先）；缓存命中
- `render_catalog()`: 空 registry 返回 None；非空时输出包含 skill 名称和描述；Repo/User scope 标记正确；禁用 skill 不出现在目录中
- `resolve_mentions()`: 精确匹配；不区分大小写；无匹配返回空；排除 $PATH/$HOME 等环境变量；禁用 skill 不参与匹配
- `load_body()`: 正确读取 SKILL.md 全文；文件不存在返回 None
- `SkillsConfig`: 空配置默认所有 skill 启用；按 name 禁用生效；按 path 禁用生效；无效 rules 条目触发警告不崩溃；后发规则覆盖先发规则
- `config_rules::resolve_disabled_paths()`: name 选择器匹配所有同名 skill；path 选择器精确匹配；enabled=true 的规则重新启用之前的禁用
- 集成: `execute_turn()` 的 preamble 在 skill 存在时包含 `<available_skills>` 块；无 skill 时不包含；`include_instructions = false` 时不包含
