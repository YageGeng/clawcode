# Skill 运行时对齐设计

## 1. 目标

本阶段补全 clawcode 的 Skill 机制，使资源发现、来源优先级、元数据校验、冲突处理、命令展开和诊断行为与本地 pi v4 coding-agent 保持一致，同时保留 clawcode 的 Rust 类型系统、Factory 注入、Session 隔离、ACP 2.0 和项目可信边界。

完成后，系统应能够：

- 从显式配置、项目目录、祖先 `.agents` 目录、用户目录和 Extension 路径加载 Skill。
- 使用与 pi 一致的来源优先级、目录扫描规则和 canonical path 去重规则。
- 校验 Skill frontmatter，并用结构化诊断表达损坏资源、无效元数据和名称冲突。
- 在 Session 内保存不可变 Skill Catalog，在实际调用时重新读取 Skill 正文。
- 通过 ACP 和 WebUI 展示 Skill 来源、手动调用限制和诊断信息。

## 2. 行为基准

本设计以本地 pi v4 当前实现的以下文件为行为基准：

- `packages/coding-agent/src/core/skills.ts`
- `packages/coding-agent/src/core/package-manager.ts`
- `packages/coding-agent/src/core/resource-loader.ts`
- `packages/coding-agent/src/core/agent-session.ts`
- `packages/coding-agent/src/core/system-prompt.ts`
- `packages/coding-agent/test/skills.test.ts`
- `packages/coding-agent/test/resource-loader.test.ts`

clawcode 不复制 pi 的 TypeScript 类结构，也不引入 npm package manager、CLI 专属资源选项、热更新、TUI 或 UI Extension Point。本设计取代 `2026-08-16-prompt-resource-design.md` 中关于 Skill 默认来源和优先级的旧描述；Prompt、Template 和项目指令设计不受影响。

## 3. 范围

### 3.1 本阶段包含

- `protocol` 中唯一的 Skill 公共类型定义。
- `[skills]` 显式文件和目录路径配置。
- 项目、祖先、用户和 Extension Skill 发现。
- `.pi/skills` 与 `.agents/skills` 的不同扫描语义。
- 项目可信边界。
- Skill frontmatter 校验。
- canonical path 去重和名称冲突诊断。
- Session 级不可变 Skill Catalog。
- System Prompt Skill 清单。
- `/skill:name` 输入展开和显式 Skill 调用。
- ACP Skill 列表、诊断和调用映射。
- WebUI Skill 来源、手动调用限制和诊断展示。
- Rust 集成测试和正式后端 WebUI 端到端验证。

### 3.2 本阶段不包含

- pi 的 npm package、package manifest 或 package 安装能力。
- Skill 热更新或文件监控。
- CLI 专属 `--skill`、`--no-skills` 等参数。
- TUI、Theme 或 UI Extension Point。
- 新增 pi 中不存在的 Skill Hook 事件。
- ACP Client 文件系统或 Terminal Callback。
- 客户端自行扫描、解析或展开 Skill。

## 4. 总体架构

### 4.1 依赖方向

```text
                     protocol
          ┌──────────────┼──────────────┐
          ↓              ↓              ↓
        config         skill         extension
          └──────────────┼──────────────┘
                         ↓
                       kernel
                         ↓
                        acp
                         ↓
                        app
                         ↓
                        web
```

- `protocol` 是 Skill 公共类型的唯一来源。
- `config` 只定义顶层配置容器，选择规则等公共值类型直接复用 `protocol`。
- `skill` 负责来源解析、文件发现、frontmatter 解析、校验、去重、冲突处理、规则过滤和 Catalog。
- `extension` 只贡献 Skill 路径，不扫描默认目录，也不实现 Skill 生命周期。
- `kernel` 只通过 `SkillFactory` 创建 Session Catalog，不感知具体文件系统规则。
- `acp` 只做协议映射，不重复定义 Skill 领域结构体。
- `web` 只消费后端返回的 Skill 和诊断数据，不实现第二套发现或展开逻辑。

### 4.2 类型驱动的来源模型

`SkillFactory` 先把配置和 Session 请求转换为有序的 `SkillSource`，再交给 Discovery。来源类型至少表达：

```rust
pub struct SkillSource {
    pub kind: SkillSourceKind,
    pub scope: SkillSourceScope,
    pub root: PathBuf,
    pub origin_base_dir: PathBuf,
    pub discovery_mode: SkillDiscoveryMode,
}
```

`SkillSourceKind` 区分 Configured、Pi、Agents 和 Extension；`SkillSourceScope` 区分 Configured、Project、User 和 Extension；`SkillDiscoveryMode` 区分允许根 Markdown 的 Pi 模式和只接受嵌套 `SKILL.md` 的 Agents 模式。`origin_base_dir` 表示来源配置的相对路径基准，不等同于 Skill 正文引用目录。来源类型负责决定扫描模式和展示元数据，Discovery 不通过字符串、布尔标志或路径片段反推来源。

实现中避免把来源构建拆成大量单次调用的小 helper。只接收一个自定义类型的行为应优先实现为该类型的方法或标准 Trait。

### 4.3 Factory 与 Catalog

保留现有 Factory 边界：

```rust
pub trait SkillFactory: Send + Sync {
    fn create(&self, request: SkillResourceRequest)
        -> Result<SkillCatalog, SkillError>;
}
```

`FilesystemSkillFactory` 持有全局目录、显式配置路径和有序选择规则。`SkillResourceRequest` 持有 Session cwd、项目资源访问决定和 Extension Skill 路径。

`SkillCatalog` 是 Session 级不可变快照，保存已启用 Skill 的元数据索引和启动诊断。Catalog 不缓存 Skill 正文；调用时重新读取文件，以保留 pi coding-agent 的调用语义。不同 cwd 的 Session 必须独立创建 Catalog。

## 5. 公共类型

以下跨 crate 类型统一下沉到 `protocol`，其他 crate 不允许定义同义镜像结构体：

- `SkillInfo`
- `SkillSource`
- `SkillSourceKind`
- `SkillSourceScope`
- `SkillDiscoveryMode`
- `SkillDiagnostic`
- `SkillDiagnosticSeverity`
- `SkillDiagnosticCode`
- `SkillCollision`
- `SkillSelectionRule`
- `SkillResourceRequest`
- ACP 和 WebUI 都需要的 Skill 列表响应值类型

`SkillInfo` 至少包含：

```rust
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub reference_dir: PathBuf,
    pub source: SkillSource,
    pub disable_model_invocation: bool,
}
```

`reference_dir` 固定为 Skill 文件所在目录，用于解析正文中的相对引用；`source.origin_base_dir` 用于解释来源和配置，不参与 Skill 正文引用解析。Skill 正文、frontmatter 中间结构和 Discovery 索引属于 `skill` crate 私有实现，不进入公共协议。

## 6. 配置

`SkillsConfig` 增加显式路径：

```toml
[skills]
include_instructions = true
paths = [
  "./skills",
  "/absolute/path/SKILL.md",
]
```

规则如下：

- `paths` 支持目录和单个 Markdown 文件。
- `~` 展开为当前用户主目录。
- 相对路径基于 Session cwd 解析，绝对路径保持不变。
- 路径不存在、无法读取或不是 Markdown 文件时产生结构化诊断，不阻止 Session 创建。
- 配置在应用启动时读取一次，不支持热更新。
- `include_instructions` 只控制模型可见的 System Prompt Skill 清单，不禁用手动调用和 ACP 列表。
- 现有 `rules` 保留；后出现的匹配规则覆盖先出现的规则。
- 每条规则必须只配置 `name` 或 `path` 之一，否则产生结构化诊断。
- 规则路径使用与显式 Skill 路径相同的 `~`、相对路径和 canonical path 解析语义。

## 7. 来源发现与优先级

### 7.1 固定优先级

来源按以下顺序加载，同名 Skill 由先加载者获胜：

1. `[skills].paths` 中的显式路径，保持配置顺序。
2. `cwd/.pi/skills`。
3. `cwd/.agents/skills`。
4. 从 cwd 的父目录逐级向上查找 `.agents/skills`。
5. 用户配置目录下的 `skills`。
6. `~/.agents/skills`。
7. Extension 贡献的 Skill 路径，保持 Hook 返回顺序。

如果 cwd 位于 Git 仓库内，祖先 `.agents/skills` 查找到 Git 仓库根为止；如果不在 Git 仓库内，则查找到文件系统根。当前目录来源因此覆盖祖先来源，项目来源覆盖用户来源。用户目录与祖先结果相同时必须按 canonical path 去重，不能把 `~/.agents/skills` 当作项目资源重复加载。

### 7.2 项目可信边界

只有 `project_resources_allowed = true` 时，才读取：

- `cwd/.pi/skills`
- cwd 和祖先目录中的 `.agents/skills`

显式配置、用户目录和 Extension 路径不因项目可信决定被隐式移除。Extension 自身负责决定是否在 `resources_discover` 中返回项目路径；SkillFactory 不通过路径猜测 Extension 的权限。

### 7.3 扫描模式

- `.pi/skills`、显式目录和 Extension 目录允许根目录中的普通 `*.md`，并递归发现嵌套 `SKILL.md`。
- `.agents/skills` 只递归发现嵌套 `SKILL.md`，不加载根目录普通 `*.md`。
- 目录包含 `SKILL.md` 后，不继续扫描其子目录，避免把一个 Skill 内的引用文档识别为其他 Skill。
- 隐藏项和 `node_modules` 不参与发现。
- `.gitignore`、`.ignore` 和 `.fdignore` 规则生效。
- 跟随符号链接，但使用 canonical path 防止同一真实文件重复加载。
- 同一真实文件通过多个来源出现时静默去重，不产生名称冲突。

## 8. Frontmatter 与元数据校验

Skill 使用 YAML frontmatter，支持：

- `name`
- `description`
- `disable-model-invocation`

未知字段允许存在。行为规则如下：

- `name` 缺失时使用 Skill 文件父目录名。
- `name` 与父目录名不同合法，不产生诊断。
- `name` 最长 64 字符。
- `name` 只允许小写 ASCII 字母、数字和单个连字符。
- `name` 不允许以连字符开头或结尾，也不允许连续连字符。
- 非法名称产生 warning，但 Skill 仍加载，与 pi 一致。
- `description` 必须存在且 trim 后非空；缺失或空值时产生诊断并跳过 Skill。
- `description` 最长 1024 字符；超长产生 warning，但 Skill 仍加载。
- `disable-model-invocation` 缺失时默认为 `false`。
- Frontmatter 缺失或 YAML 无法解析时产生诊断并跳过 Skill。
- 单个 Skill 损坏不能使整个 Session 创建失败。

## 9. 去重、冲突与选择规则

Discovery 同时维护按 canonical path 的已加载集合和按名称的 Skill 索引：

1. 先 canonicalize 候选文件。
2. canonical path 已存在时静默跳过。
3. 路径未出现但名称已存在时，保留先加载者，并产生 collision 诊断。
4. 路径和名称都未出现时加入 Catalog。

名称冲突诊断必须携带资源类型、Skill 名称、胜出路径和被忽略路径，便于 ACP 和 WebUI 准确展示。

全部来源发现完成后应用选择规则。规则按配置顺序执行，后匹配者覆盖先匹配者。按路径匹配时比较 canonical path；按名称匹配时比较最终 Skill 名称。禁用规则从有效 Catalog 移除 Skill，但保留发现阶段诊断。

## 10. 结构化诊断

`SkillDiagnostic` 至少包含：

```rust
pub struct SkillDiagnostic {
    pub severity: SkillDiagnosticSeverity,
    pub code: SkillDiagnosticCode,
    pub message: String,
    pub path: Option<PathBuf>,
    pub source: Option<SkillSource>,
    pub collision: Option<SkillCollision>,
}
```

诊断代码覆盖：

- `PathNotFound`
- `UnsupportedPath`
- `FileInfoFailed`
- `DirectoryReadFailed`
- `FileReadFailed`
- `FrontmatterInvalid`
- `MetadataInvalid`
- `NameCollision`
- `SelectionRuleInvalid`

可恢复的资源问题使用 warning。只有 Factory 无法解析 Session cwd 等导致整个发现过程没有可靠基准的错误才返回 `SkillError`。诊断消息用于人类阅读，调用方必须根据代码和字段判断语义，不能解析消息字符串。

## 11. System Prompt 与调用展开

### 11.1 模型可见清单

System Prompt 仅列出启用且 `disable_model_invocation = false` 的 Skill。名称、描述和路径属性必须进行 XML 转义。提示内容说明：

- 使用 Skill 前读取完整文件。
- Skill 内相对引用基于 `reference_dir` 解析。

`disable_model_invocation = true` 的 Skill 仍出现在 ACP 和 WebUI 列表中，并允许用户手动调用。

### 11.2 输入顺序

普通用户输入保持以下 pi 顺序：

```text
Extension input hook
  → /skill:name 展开
  → Prompt Template 展开
  → Agent Turn
```

不增加 pi 中不存在的 Skill Hook 事件。

### 11.3 Skill 命令

- 只识别 `/skill:<name>`。
- 使用第一个普通空格 U+0020 分隔命令名和参数。
- 参数随后 trim；Tab 和换行不作为命令名分隔符。
- 找不到 Skill 时保留原始输入。
- 找到 Skill 后重新读取文件，剥离 frontmatter，并包装为包含 name、location 和引用基准目录的 `<skill>` 块。
- 读取失败时保留原始输入，同时产生带 Session、Run 和 Turn 关联信息的结构化诊断事件。

Catalog 的展开接口应返回类型化结果，明确区分“不属于 Skill 命令”“展开成功”和“展开失败且保留原文”，而不是依赖空字符串、布尔值或消息文本表达状态。

## 12. ACP 与 WebUI

### 12.1 ACP

- 保留 Session 级 Skill 列表和显式调用能力。
- Skill 列表响应包含 `skills` 和 `diagnostics`。
- ACP 映射直接消费 `protocol` 中的 Skill 类型，不在 `acp` crate 重复定义领域结构体。
- 显式调用失败返回协议化错误。
- Agent 输入自动展开失败时不拒绝原始用户消息，而是发送诊断事件。
- 所有事件继续遵守现有 TurnId、字符串毫秒时间戳和 trace_id 规则。

### 12.2 WebUI

Skill 面板增加：

- Configured、Project、User、Extension 来源标签。
- Skill 路径和描述。
- `disable_model_invocation` 对应的“仅手动调用”状态。
- 诊断列表，包括严重级别、错误代码、消息、路径和名称冲突胜出者。

WebUI 保持现有点击调用交互。客户端不扫描文件、不解析 frontmatter、不决定优先级，也不增加 UI Extension Point。

## 13. 错误处理与日志

- 单个来源、目录或文件失败记录结构化诊断，并使用适量的 `tracing::warn!()` 或 `tracing::debug!()` 记录上下文。
- Factory 整体失败和显式 Skill 调用失败记录 error 级日志。
- 日志使用完整 `tracing::level!()` 路径和格式化字符串，不导入宏，不使用 tracing field 风格。
- 日志包含足够定位问题的 SessionId、TurnId、trace_id、来源和路径，但不重复打印大段 Skill 正文。
- 正常扫描成功不为每个文件打印 info，避免无意义日志。

## 14. 测试与验证

Rust 测试只放在各 crate 的 `tests/` 集成测试目录或精确的 `#[cfg(test)] mod tests` 中。生产代码不得增加 `#[cfg(test)]` 测试支撑分支。

测试至少覆盖：

- 显式、项目、祖先、用户和 Extension 来源优先级。
- Git 仓库根和无 Git 仓库时的祖先边界。
- 项目不可信时跳过项目来源。
- `.pi` 与 `.agents` 的根 Markdown 扫描差异。
- ignore 文件、隐藏目录、`node_modules` 和嵌套 Skill 停止规则。
- 符号链接 canonical path 去重。
- 名称冲突胜出路径和结构化诊断。
- 名称、描述、Frontmatter 和未知字段行为。
- 显式文件、目录、`~`、不存在路径和不支持路径。
- 选择规则顺序和无效规则诊断。
- `disable-model-invocation` 的 System Prompt 和手动调用差异。
- `/skill:name` 普通空格语义、正文重读和失败保留原文。
- ACP Skill 列表、诊断和显式调用。

后端按 TDD 实现：先增加失败的集成测试，再完成最小实现并重构。前端不要求 TDD 或单元测试。全部实现完成后：

1. 运行 Rust 格式化、静态检查和相关集成测试。
2. 运行 Web 前端检查。
3. 启动正式后端和真实配置的 WebUI。
4. 在真实 Session 中验证 Skill 来源、仅手动调用标记、诊断展示和显式调用。

不使用 fixture 后端，不为测试增加生产正文支撑代码。

## 15. 非目标与后续演进

本阶段不建立通用 Resource Manager。若未来 Prompt、Skill 和其他资源的来源模型继续收敛，可在不改变本设计行为语义的前提下抽取公共资源发现内核。该抽取必须由真实重复逻辑驱动，不能预先引入 pi package manager 的全部复杂度。
