# Skill 运行时对齐实施计划

> **执行约束：** 实施时使用 `superpowers:executing-plans` 在当前会话逐任务执行。项目禁止 SubAgent 和 worktree；未经用户明确要求不得创建 commit，因此本计划不包含 commit 步骤。

**目标：** 在保留现有 `SkillFactory` 和 Session 级 Catalog 边界的前提下，补齐与本地 pi v4 一致的 Skill 来源发现、优先级、校验、诊断、调用展开、ACP 和 WebUI 能力。

**架构：** `protocol` 保存唯一公共类型；`config` 提供显式路径和选择规则；`skill` 使用有序 `SkillSource` 驱动发现、校验、canonical path 去重和冲突处理；`kernel` 保存不可变 Catalog 并负责运行时诊断事件；`acp` 映射列表和调用；WebUI 只展示服务端结果。

**技术栈：** Rust 2024、Serde、serde_yaml_ng、ignore、typed-builder、ACP 2.0 Rust SDK、React 19、TypeScript 6、Zustand、Vite。

## 全局约束

- 新增函数和方法必须写英文作用注释；修改旧逻辑必须写英文变动原因注释。
- 后端按 TDD 执行：先写失败的集成测试并确认失败原因，再实现最小代码，最后重构。
- 测试只能放在 crate 的 `tests/` 目录或精确的 `#[cfg(test)] mod tests` 中；正文不得出现测试支撑分支。
- 前端不要求 TDD，不新增前端单元测试，但必须通过 TypeScript、ESLint、构建和真实后端端到端验证。
- 避免大量短小 helper；只有一个自定义类型参数的行为优先实现为关联方法、`From`、`TryFrom` 或 `FromStr`。
- 超过三个字段的新增 Rust 结构体使用 `typed-builder`；可选 builder 字段提供默认值。
- 跨 crate Skill 类型只定义在 `protocol`；`config`、`skill`、`kernel` 和 `acp` 不重复定义同义结构体。
- Cargo 相对路径依赖位于依赖列表前部；新依赖先进入 workspace，再由子 crate 使用 workspace 依赖。
- 项目名称只使用 `protocol::ProductIdentity`，不得新增散落的项目名字符串。
- 日志使用完整 `tracing::level!()` 路径和格式化字符串，不导入 tracing 宏，不使用 tracing field 风格。
- 不实现 package manager、CLI Skill 参数、热更新、UI Extension Point、ACP Client FS/Terminal Callback 或新 Skill Hook。
- 所有 shell 命令使用 `rtk` 前缀。

---

### 任务 1：建立 Skill 公共协议类型

**文件：**

- 新增：`crates/protocol/src/skill.rs`
- 修改：`crates/protocol/src/capability.rs`
- 修改：`crates/protocol/src/lib.rs`
- 新增：`crates/protocol/tests/skill.rs`

**公共接口：**

```rust
pub enum SkillSourceKind { Configured, Pi, Agents, Extension }
pub enum SkillSourceScope { Configured, Project, User, Extension }
pub enum SkillDiscoveryMode { Pi, Agents }
pub enum SkillDiagnosticSeverity { Warning, Error }
pub enum SkillDiagnosticCode {
    PathNotFound,
    UnsupportedPath,
    FileInfoFailed,
    DirectoryReadFailed,
    FileReadFailed,
    FrontmatterInvalid,
    MetadataInvalid,
    NameCollision,
    SelectionRuleInvalid,
}

pub struct SkillSource {
    pub kind: SkillSourceKind,
    pub scope: SkillSourceScope,
    pub root: PathBuf,
    pub origin_base_dir: PathBuf,
    pub discovery_mode: SkillDiscoveryMode,
}

pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub reference_dir: PathBuf,
    pub source: SkillSource,
    pub disable_model_invocation: bool,
}

pub struct SkillCollision {
    pub name: String,
    pub winner_path: PathBuf,
    pub loser_path: PathBuf,
}

pub struct SkillDiagnostic {
    pub severity: SkillDiagnosticSeverity,
    pub code: SkillDiagnosticCode,
    pub message: String,
    pub path: Option<PathBuf>,
    pub source: Option<SkillSource>,
    pub collision: Option<SkillCollision>,
}

pub struct SkillListResult {
    pub skills: Vec<SkillInfo>,
    pub diagnostics: Vec<SkillDiagnostic>,
}
```

- [ ] **步骤 1：先写 Serde 和类型语义失败测试**

在 `crates/protocol/tests/skill.rs` 验证 `SkillInfo`、`SkillSource`、collision 和 `SkillListResult` 的 JSON round-trip、camelCase 字段、Option 缺省行为和枚举 wire 值。

- [ ] **步骤 2：运行测试并确认因类型缺失失败**

```bash
rtk cargo test -p protocol --test skill
```

- [ ] **步骤 3：实现公共类型并移除旧定义**

将现有 `SkillInfo` 和 `SkillSelectionRule` 从 `capability.rs` 移入 `skill.rs`，由 `protocol::lib` 统一重导出。大结构体使用 builder；所有公开字段和新增函数写英文文档注释。

- [ ] **步骤 4：运行目标测试**

```bash
rtk cargo test -p protocol --test skill
rtk cargo test -p protocol
rtk cargo fmt --all -- --check
```

---

### 任务 2：补充 TOML 显式 Skill 路径和应用装配

**文件：**

- 修改：`crates/config/src/skills.rs`
- 修改：`crates/config/tests/loading.rs`
- 修改：`crates/app/src/lib.rs`
- 修改：`crates/skill/src/factory.rs`
- 修改：`crates/kernel/tests/lifecycle_order.rs`
- 修改：`crates/kernel/tests/session_capabilities.rs`
- 修改：`crates/acp/tests/extension.rs`

- [ ] **步骤 1：先写配置失败测试**

在 `crates/config/tests/loading.rs` 增加 `[skills].paths` 的相对路径、绝对路径、空列表和默认值测试。

- [ ] **步骤 2：确认测试因 `paths` 缺失失败**

```bash
rtk cargo test -p config --test loading skill_paths
```

- [ ] **步骤 3：实现配置与 Factory 构造参数**

`SkillsConfig` 增加 `paths: Vec<PathBuf>`。`FilesystemSkillFactory` 构造参数改为全局配置目录、显式路径和选择规则；`app` 传入不可变配置快照。同步更新测试中所有 Factory 构造点，不增加兼容旧构造器。

- [ ] **步骤 4：运行配置和编译检查**

```bash
rtk cargo test -p config --test loading
rtk cargo check -p app -p kernel -p acp
```

---

### 任务 3：实现来源模型和 pi 兼容路径解析

**文件：**

- 新增：`crates/skill/src/source.rs`
- 修改：`crates/skill/src/factory.rs`
- 修改：`crates/skill/src/lib.rs`
- 修改：`crates/skill/src/error.rs`
- 修改：`crates/skill/tests/discovery.rs`

- [ ] **步骤 1：先写来源顺序和可信边界失败测试**

测试创建同名 Skill，验证固定 winner 顺序：显式配置、项目 `.pi`、当前 `.agents`、祖先 `.agents`、用户配置目录、`~/.agents`、Extension。另验证：

- 当前目录 `.agents` 覆盖父目录。
- Git 仓库内只查找到仓库根。
- 无 Git 仓库时查到文件系统根，但测试使用隔离路径避免读取宿主资源。
- 不可信项目不读取项目 `.pi` 和祖先 `.agents`。
- 用户 `~/.agents` 始终作为用户来源加载。
- Extension 路径保持传入顺序。

- [ ] **步骤 2：运行来源测试并确认失败**

```bash
rtk cargo test -p skill --test discovery source_
```

- [ ] **步骤 3：实现类型驱动的来源构建**

`SkillSource` 负责解析自己的 root、扫描模式和来源元数据。Factory 根据 canonical cwd 查找 Git 根并构造有序来源。`~` 只在路径首组件完整等于 `~` 时展开，不能替换普通文件名中的波浪号。

用户 home 作为 Factory 的不可变依赖注入，生产由 `app` 使用 `dirs::home_dir()` 提供；测试直接传临时目录。`skill` crate 不直接读取进程全局 home，也不新增 `dirs` 依赖。该依赖是正常运行时能力，不使用 `#[cfg(test)]` 支撑代码。Factory 因字段超过三个改用 builder，不保留旧构造器。

- [ ] **步骤 4：运行 Skill 目标测试**

```bash
rtk cargo test -p skill --test discovery source_
rtk cargo clippy -p skill --all-targets -- -D warnings
```

---

### 任务 4：重构 Discovery 扫描、去重和结构化诊断

**文件：**

- 修改：`crates/skill/src/discovery.rs`
- 新增：`crates/skill/src/metadata.rs`
- 修改：`crates/skill/src/error.rs`
- 修改：`crates/skill/src/lib.rs`
- 修改：`crates/skill/tests/discovery.rs`

- [ ] **步骤 1：先写扫描和诊断失败测试**

覆盖：

- Pi 模式加载根 `*.md`，Agents 模式忽略根 `*.md`。
- 两种模式都发现嵌套 `SKILL.md`。
- 遇到 `SKILL.md` 后停止扫描子目录。
- 忽略隐藏目录、`node_modules`、`.gitignore`、`.ignore` 和 `.fdignore`。
- 同一真实文件的符号链接静默去重。
- 同名不同文件保留先加载者，并返回 winner/loser collision。
- 路径不存在、非 Markdown 文件、属性读取、目录遍历和文件读取失败返回对应 code。

- [ ] **步骤 2：确认旧字符串诊断和扫描模式导致测试失败**

```bash
rtk cargo test -p skill --test discovery discovery_
rtk cargo test -p skill --test discovery diagnostic_
```

- [ ] **步骤 3：实现有序 Discovery**

Discovery 维护插入顺序 Skill 列表、名称索引和 canonical path 集合。候选文件先做真实路径去重，再处理名称冲突。文件系统局部失败转为 `SkillDiagnostic`；只有 cwd 无法建立可靠基准时返回 `SkillError`。

不要使用按名称排序的 `BTreeMap` 作为唯一存储，以免覆盖来源顺序。对外 descriptors 可在返回前按名称稳定排序，不改变 winner 决策。

- [ ] **步骤 4：运行扫描测试和 lint**

```bash
rtk cargo test -p skill --test discovery discovery_
rtk cargo test -p skill --test discovery diagnostic_
rtk cargo clippy -p skill --all-targets -- -D warnings
```

---

### 任务 5：对齐 frontmatter 校验和选择规则

**文件：**

- 修改：`crates/skill/src/metadata.rs`
- 修改：`crates/skill/src/discovery.rs`
- 修改：`crates/skill/src/factory.rs`
- 修改：`crates/skill/tests/discovery.rs`

- [ ] **步骤 1：先写元数据和规则失败测试**

测试表覆盖：名称缺省、64 字符边界、超长名称、大写、下划线、首尾连字符、连续连字符、名称与目录不同、描述缺失或空白、1024 字符边界、超长描述、未知字段、非法 YAML 和 `disable-model-invocation`。

断言非法名称和超长描述产生 warning 但仍加载；描述缺失、空白和 frontmatter 无法解析时跳过。规则测试覆盖 name、canonical path、后规则覆盖前规则，以及同时配置或都不配置 selector 的 `SelectionRuleInvalid`。

- [ ] **步骤 2：运行测试并确认失败**

```bash
rtk cargo test -p skill --test discovery metadata_
rtk cargo test -p skill --test discovery selection_rule_
```

- [ ] **步骤 3：实现元数据值类型和规则应用**

名称校验实现为 `SkillNameValidation` 的类型方法或 Trait，不拆成多个单用途 helper。Frontmatter 复用 `prompt::MarkdownDocument`。选择规则在所有来源完成冲突处理后应用，禁用 Skill 从有效 Catalog 移除但不删除诊断。

- [ ] **步骤 4：运行完整 Skill Discovery 测试**

```bash
rtk cargo test -p skill --test discovery
rtk cargo clippy -p skill --all-targets -- -D warnings
```

---

### 任务 6：类型化 Catalog、System Prompt 和调用失败语义

**文件：**

- 修改：`crates/skill/src/catalog.rs`
- 修改：`crates/skill/src/lib.rs`
- 修改：`crates/kernel/src/runtime/input.rs`
- 修改：`crates/kernel/src/runtime/prompt.rs`
- 修改：`crates/kernel/src/runtime.rs`
- 修改：`crates/protocol/src/event.rs`
- 修改：`crates/kernel/tests/session_capabilities.rs`
- 修改：`crates/kernel/tests/lifecycle_order.rs`

**内部结果类型：**

```rust
pub enum SkillCommandExpansion {
    NotSkillCommand,
    Expanded(String),
    Failed {
        original: String,
        diagnostic: SkillDiagnostic,
    },
}
```

- [ ] **步骤 1：先写 Catalog 和 Kernel 失败测试**

验证：

- descriptors 包含仅手动调用 Skill，但 System Prompt 清单过滤它。
- System Prompt XML 转义名称、描述和路径，并使用 `reference_dir` 说明引用基准。
- `/skill:name` 只按第一个 U+0020 分隔。
- 调用时重新读取正文并剥离 frontmatter。
- 未知 Skill 保留原文。
- 自动展开读取失败保留原文并产生 Skill diagnostic event。
- 显式 Kernel 调用读取失败返回错误。
- 输入顺序仍为 Extension input hook、Skill、Template。

- [ ] **步骤 2：运行测试并确认失败**

```bash
rtk cargo test -p skill --test discovery invocation_
rtk cargo test -p kernel --test session_capabilities skill_
rtk cargo test -p kernel --test lifecycle_order skill_
```

- [ ] **步骤 3：实现类型化展开和事件**

Catalog 的命令展开返回 `SkillCommandExpansion`。Kernel 自动展开失败时通过现有 Event sink 产生携带 `SkillDiagnostic` 的事件，事件 envelope 继续由 Kernel 注入 TurnId、字符串毫秒时间戳和 sequence；trace_id 沿用当前 Run 日志上下文。不要让 `skill` crate 依赖 Kernel 的 Session、Run 或 Turn 类型。

- [ ] **步骤 4：运行 Skill 与 Kernel 测试**

```bash
rtk cargo test -p skill --test discovery
rtk cargo test -p kernel --test session_capabilities
rtk cargo test -p kernel --test lifecycle_order
rtk cargo clippy -p kernel -p skill --all-targets -- -D warnings
```

---

### 任务 7：扩充 ACP Skill 响应和诊断映射

**文件：**

- 修改：`crates/acp/src/extension.rs`
- 修改：`crates/acp/src/mapping.rs`
- 修改：`crates/acp/tests/extension.rs`
- 修改：`crates/acp/tests/mapping.rs`
- 修改：`crates/kernel/src/runtime.rs`

- [ ] **步骤 1：先写 ACP 失败测试**

通过真实 ACP transport 验证：

- `skill/list` 返回 `{ skills, diagnostics }`，不再返回裸数组。
- Skill 字段包括来源、referenceDir 和仅手动调用状态。
- collision diagnostic 保留 winner/loser。
- `skill/invoke` 成功返回 name/content，失败返回 JSON-RPC 错误。
- Kernel Skill diagnostic event 映射为带现有产品命名空间 `_meta` 的 ACP 2.0 扩展 Session Update，并保留 TurnId 和 timestampMs。

- [ ] **步骤 2：运行 ACP 测试并确认失败**

```bash
rtk cargo test -p acp --test extension skill_
rtk cargo test -p acp --test mapping skill_
```

- [ ] **步骤 3：实现协议映射**

`Kernel::skills` 返回 `protocol::SkillListResult`。ACP 直接序列化公共类型，不定义本地响应镜像。动态诊断使用现有 ACP 2.0 扩展消息机制，不添加 pi 不存在的 Hook。

- [ ] **步骤 4：运行 ACP 完整测试**

```bash
rtk cargo test -p acp --test extension
rtk cargo test -p acp --test mapping
rtk cargo clippy -p acp --all-targets -- -D warnings
```

---

### 任务 8：更新 WebUI Skill 状态和展示

**文件：**

- 修改：`web/src/domain/model.ts`
- 修改：`web/src/workspace/state.ts`
- 修改：`web/src/workspace/controller.ts`
- 修改：`web/src/features/skills/SkillsPanel.tsx`
- 修改：`web/src/shell/AppShell.tsx`
- 修改：`web/src/theme/workbench.css`

- [ ] **步骤 1：更新前端 wire/domain 类型**

增加 `SkillSource`、`SkillDiagnostic`、`SkillCollision` 和 `SkillListResult`。Workspace state 分别保存 Skill 与 Skill diagnostics；打开 Session 时原子替换两者，停用 Session 和清空 transcript 时不得遗留上一 Session 的 Skill 诊断。

- [ ] **步骤 2：更新 Controller**

`skill/list` 按对象响应解码并分发。保持 `invokeSkill` 使用后端返回正文，不在客户端重新读取或展开 Skill。

- [ ] **步骤 3：拆分可读的 Skill 面板组件**

`SkillsPanel.tsx` 只保留页面组合；若文件增长明显，将卡片和诊断列表按逻辑语义拆为 `SkillCard.tsx`、`SkillDiagnostics.tsx`。不要为了缩短文件创建只有一次调用的短 helper。

展示来源标签、路径、描述、reference directory、仅手动调用状态、诊断 code/path 和 collision winner。保持现有亮色主题和调用交互。

- [ ] **步骤 4：运行前端检查和构建**

```bash
cd web
rtk npm run check
rtk npm run build
```

---

### 任务 9：更新配置示例和中英文文档

**文件：**

- 修改：`README.md`
- 修改：`README_zh.md`

- [ ] **步骤 1：增加 `[skills]` 配置示例**

在 README 的示例代码块中记录 `include_instructions`、`paths` 和 `rules`，说明相对路径基于 Session cwd、项目可信边界和来源优先级。不得修改包含真实 provider 配置的仓库 `claw.toml`。

- [ ] **步骤 2：补充运行时行为说明**

中英文 README 同步说明 `.pi/skills`、祖先 `.agents/skills`、用户目录、Extension、手动调用限制和结构化诊断。项目名继续复用既有文档身份，不在 Rust/TypeScript 正文增加硬编码常量。

- [ ] **步骤 3：运行文档和格式检查**

```bash
rtk git diff --check
rtk cargo fmt --all -- --check
```

---

### 任务 10：全量验证与真实 WebUI 端到端测试

**文件：**

- 不新增 fixture 或测试支撑生产代码。

- [ ] **步骤 1：运行 Rust 全量验证**

```bash
rtk cargo fmt --all -- --check
rtk cargo check --workspace --all-targets
rtk cargo clippy --workspace --all-targets --all-features -- -D warnings
rtk cargo test --workspace
rtk git diff --check
```

- [ ] **步骤 2：运行 Web 全量验证**

```bash
cd web
rtk npm run check
rtk npm run build
```

- [ ] **步骤 3：准备真实 Skill 场景**

仅在仓库或用户现有配置允许的目录创建可回收的测试 Skill；不得改写用户已有 Skill。场景至少包含正常 Skill、`disable-model-invocation` Skill 和可恢复诊断 Skill。测试完成后只删除本次创建且目标明确的临时文件。

- [ ] **步骤 4：启动正式后端**

```bash
rtk cargo run -p app --bin clawcode -- serve --web-root web/dist
```

使用真实 `claw.toml` 和已配置 provider，不启动 `webui_fixture`。

- [ ] **步骤 5：通过浏览器完成真实端到端验证**

验证：

- 创建或恢复真实 Session 后 Skill 列表成功加载。
- 来源标签、reference directory、仅手动调用状态正确。
- 可恢复诊断和 collision 信息正确展示。
- 点击 Skill 调用后，真实 Agent 收到展开后的 Skill 正文和用户请求。
- Thinking、消息和 Tool 事件顺序未回归。
- 后端日志包含 ACP 参数、Session/Turn/trace 上下文，不打印 Skill 正文。

- [ ] **步骤 6：最终状态审计**

```bash
rtk git status --short
rtk git diff --stat
rtk git diff --check
```

只报告本次变更和验证结果，不创建 commit。
