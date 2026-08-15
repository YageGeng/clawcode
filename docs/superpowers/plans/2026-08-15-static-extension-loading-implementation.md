# 静态扩展加载实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: 使用 `superpowers:executing-plans` 在当前工作区逐项实施。项目禁止 SubAgent 和 worktree；未经用户明确允许不得创建 commit。步骤使用复选框跟踪。

**目标：** 让构建脚本从可参数化的 `claw.toml` 读取扩展列表，只编译指定 Rust 扩展，并让正式后端默认加载可阻止 `rm -rf` 的 `command-guard`。

**架构：** `extension` crate 增加类型化静态目录，`extensions` crate 保存具体扩展并由 `build.rs` 生成模块声明与目录入口，`config` crate 解析同一份 `extensions.enabled`，`app` 只负责把运行时选择合成为 Kernel Factory。构建配置路径默认复用 `ProductIdentity::CONFIG_FILE_NAME`，可通过 `ProductIdentity::CONFIG_PATH_ENV` 参数覆盖。

**技术栈：** Rust 2024、Cargo build script、Serde/TOML、async-trait、typed-builder、ACP WebSocket、React WebUI。

## 全局约束

- 保留现有 `provider` 与 `config` 架构，不实现配置热更新。
- 扩展 ID 必须统一使用公共常量，项目名称必须复用 `ProductIdentity`。
- 新增函数必须有英文函数级注释，非平凡修改必须有英文原因注释。
- 避免大量短小 helper；仅接收一个项目自定义类型的逻辑优先实现为该类型的方法或 Trait。
- 超过 3 个字段的结构体必须使用 `typed-builder`。
- 测试只能放在 crate 的 `tests/` 或精确的 `#[cfg(test)] mod tests` 中，生产代码不得包含测试支撑分支。
- Cargo 依赖先列工作区相对依赖，再按根清单语义分组；所有版本和路径由根 `[workspace.dependencies]` 管理。
- 所有 shell 命令必须使用 `rtk` 前缀。
- Rust 生产行为按 TDD 实现；WebUI 不要求单元测试。
- WebUI 端到端测试必须连接正式 `app` 后端和真实 Provider，不使用 fixture。
- 不创建 commit，不使用 SubAgent，不使用 worktree。

---

## 文件结构

### 新增

- `crates/config/src/extensions.rs`：运行时 `ExtensionsConfig` 及默认扩展 ID。
- `crates/extension/src/catalog.rs`：已编译扩展条目、静态目录、选择与合并错误。
- `crates/extension/tests/catalog.rs`：目录顺序、缺失、重复、描述符和静态注册冲突测试。
- `crates/extensions/Cargo.toml`：内置扩展实现 crate。
- `crates/extensions/build.rs`：构建入口，只负责路径解析、生成和 Cargo 重建声明。
- `crates/extensions/build/config.rs`：可复用的构建配置解析、源码解析和生成逻辑。
- `crates/extensions/src/lib.rs`：包含 `OUT_DIR/compiled_extensions.rs` 并导出目录入口。
- `crates/extensions/src/available/command_guard.rs`：危险命令防护扩展。
- `crates/extensions/src/available/hook_examples/`：迁移后的 Hook 示例模块。
- `crates/extensions/tests/build_config.rs`：构建配置路径、校验和生成结果测试。
- `crates/extensions/tests/command_guard.rs`：工具 Bash 与用户 Bash 的扩展行为测试。
- `crates/extensions/examples/hooks.rs`：复用迁移后源码的全 Hook 编译示例。

### 修改

- `Cargo.toml`：注册 `extensions` 工作区 crate 和公共依赖。
- `Cargo.lock`：由 Cargo 按新增工作区 crate 更新。
- `claw.toml`：增加默认 `[extensions] enabled = ["command-guard"]`。
- `crates/config/src/config.rs`：把 `ExtensionsConfig` 加入 `AppConfig`。
- `crates/config/src/lib.rs`：导出 `ExtensionsConfig`。
- `crates/config/tests/loading.rs`：覆盖默认、空列表和顺序解析。
- `crates/extension/src/error.rs`：增加静态目录的类型化错误。
- `crates/extension/src/factory.rs`：让目录选择结果转换为现有 `StaticExtensionFactory`。
- `crates/extension/src/lib.rs`：导出目录 API。
- `crates/app/Cargo.toml`：依赖 `extensions`。
- `crates/app/src/lib.rs`：使用生成目录替换空扩展 Factory。
- `docs/extensions/hooks.md`：说明构建配置、路径参数和 `command-guard`。

### 删除

- `crates/extension/examples/hooks.rs`
- `crates/extension/examples/hooks/agent.rs`
- `crates/extension/examples/hooks/model.rs`
- `crates/extension/examples/hooks/provider.rs`
- `crates/extension/examples/hooks/session.rs`
- `crates/extension/examples/hooks/startup.rs`
- `crates/extension/examples/hooks/tool.rs`

这些文件的实现迁移到 `crates/extensions`，不保留重复定义。

---

### Task 1：扩展 TOML 类型

**Files:**

- Create: `crates/config/src/extensions.rs`
- Modify: `crates/config/src/config.rs`
- Modify: `crates/config/src/lib.rs`
- Modify: `crates/config/tests/loading.rs`
- Modify: `claw.toml`

**Interfaces:**

- Produces: `config::ExtensionsConfig { enabled: Vec<ExtensionId> }`
- Produces: `ExtensionsConfig::default()`，默认只含 `command-guard`
- Consumes: `protocol::ExtensionId`

- [ ] **Step 1：先写失败的配置集成测试**

在 `crates/config/tests/loading.rs` 增加三个测试，直接解析 `AppConfig`：

```rust
#[test]
fn extensions_default_to_command_guard() {
    let config = config::AppConfig::default();
    assert_eq!(
        config.extensions.enabled,
        vec![protocol::ExtensionId::try_from("command-guard")
            .expect("default extension id")]
    );
}

#[test]
fn extensions_allow_an_explicit_empty_list() {
    let config: config::AppConfig = toml::from_str(
        "[extensions]\nenabled = []\n",
    )
    .expect("parse extensions");
    assert!(config.extensions.enabled.is_empty());
}

#[test]
fn extensions_preserve_configured_order() {
    let config: config::AppConfig = toml::from_str(
        "[extensions]\nenabled = [\"hook-examples\", \"command-guard\"]\n",
    )
    .expect("parse extensions");
    let ids: Vec<_> = config
        .extensions
        .enabled
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(ids, vec!["hook-examples", "command-guard"]);
}
```

- [ ] **Step 2：确认测试因字段不存在而失败**

Run:

```bash
rtk cargo test -p config --test loading extensions_
```

Expected: FAIL，错误指向 `AppConfig` 没有 `extensions` 字段或 `config::ExtensionsConfig` 尚不存在。

- [ ] **Step 3：实现类型化配置**

`crates/config/src/extensions.rs`：

```rust
use protocol::ExtensionId;
use serde::{Deserialize, Serialize};

/// Stable identifier of the extension enabled when no list is configured.
pub const DEFAULT_EXTENSION_ID: &str = "command-guard";

/// Extensions selected for both build-time inclusion and runtime activation.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExtensionsConfig {
    /// Ordered extension identifiers.
    #[serde(default = "default_extensions")]
    pub enabled: Vec<ExtensionId>,
}

impl Default for ExtensionsConfig {
    /// Enables only the built-in command guard by default.
    fn default() -> Self {
        Self {
            enabled: default_extensions(),
        }
    }
}

/// Builds the stable default list used by Serde and `Default`.
fn default_extensions() -> Vec<ExtensionId> {
    vec![ExtensionId::try_from(DEFAULT_EXTENSION_ID)
        .expect("the built-in extension identifier is valid")]
}
```

在 `AppConfig` 增加 `#[serde(default)] pub extensions: ExtensionsConfig`，并在 `Default` 中初始化；`config::lib` 导出类型。随后在根 `claw.toml` 增加：

```toml
[extensions]
enabled = ["command-guard"]
```

- [ ] **Step 4：运行配置测试并格式化**

```bash
rtk cargo test -p config --test loading extensions_
rtk cargo fmt --all -- --check
```

Expected: 三个新增测试 PASS，格式检查无差异。

---

### Task 2：类型化静态扩展目录

**Files:**

- Create: `crates/extension/src/catalog.rs`
- Create: `crates/extension/tests/catalog.rs`
- Modify: `crates/extension/src/error.rs`
- Modify: `crates/extension/src/factory.rs`
- Modify: `crates/extension/src/lib.rs`

**Interfaces:**

- Produces: `CompiledExtension::new(descriptor, registration, module_factory)`
- Produces: `ExtensionCatalog::new(entries)`
- Produces: `ExtensionCatalog::select(&[ExtensionId]) -> Result<StaticExtensionFactory, ExtensionError>`
- Consumes: `ExtensionDescriptor`、`StaticExtensionRegistration`、`ExtensionModuleFactory`

- [ ] **Step 1：先写目录选择失败测试**

`crates/extension/tests/catalog.rs` 使用最小 `TestModule` 和 Factory，覆盖：

```rust
#[test]
fn catalog_preserves_requested_order() {
    let catalog = ExtensionCatalog::new(vec![entry("first"), entry("second")]);
    let selected = catalog
        .select(&[id("second"), id("first")])
        .expect("select extensions");
    let modules = selected.create_modules().expect("create modules");
    let ids: Vec<_> = modules
        .iter()
        .map(|module| module.descriptor().id.to_string())
        .collect();
    assert_eq!(ids, vec!["second", "first"]);
}

#[test]
fn catalog_rejects_duplicate_requested_ids() {
    let catalog = ExtensionCatalog::new(vec![entry("first")]);
    assert!(matches!(
        catalog.select(&[id("first"), id("first")]),
        Err(ExtensionError::DuplicateConfiguredExtension(_))
    ));
}

#[test]
fn catalog_rejects_extensions_missing_from_the_binary() {
    let catalog = ExtensionCatalog::new(vec![entry("first")]);
    assert!(matches!(
        catalog.select(&[id("missing")]),
        Err(ExtensionError::ExtensionNotCompiled(_))
    ));
}
```

再增加 descriptor 不匹配、重复静态 flag、重复 Provider 名称和重复 flag value 的独立测试。

- [ ] **Step 2：确认目录 API 不存在导致失败**

```bash
rtk cargo test -p extension --test catalog
```

Expected: FAIL，未解析 `CompiledExtension` 或 `ExtensionCatalog`。

- [ ] **Step 3：实现目录与错误类型**

`catalog.rs` 的公开形态：

```rust
pub struct CompiledExtension {
    descriptor: ExtensionDescriptor,
    registration: StaticExtensionRegistration,
    module_factory: ExtensionModuleFactory,
}

impl CompiledExtension {
    /// Creates one immutable entry emitted by the extension build catalogue.
    #[must_use]
    pub fn new(
        descriptor: ExtensionDescriptor,
        registration: StaticExtensionRegistration,
        module_factory: ExtensionModuleFactory,
    ) -> Self {
        Self { descriptor, registration, module_factory }
    }
}

pub struct ExtensionCatalog {
    entries: Vec<CompiledExtension>,
}

impl ExtensionCatalog {
    /// Preserves build order while rejecting duplicate compiled identities.
    pub fn new(entries: Vec<CompiledExtension>) -> Result<Self, ExtensionError>;

    /// Validates and combines the requested entries into the kernel factory.
    pub fn select(
        &self,
        enabled: &[ExtensionId],
    ) -> Result<StaticExtensionFactory, ExtensionError>;
}
```

`select` 在一次遍历中保持请求顺序，并验证 ID、descriptor 与静态注册冲突。合并逻辑作为 `ExtensionCatalog`/内部聚合类型的方法实现，不创建多个单参数自由 helper。`ExtensionError` 增加带 `ExtensionId` 或名称的明确变体。

- [ ] **Step 4：运行目录测试并回归 extension crate**

```bash
rtk cargo test -p extension --test catalog
rtk cargo test -p extension
```

Expected: 新增目录测试和原有 extension 测试全部 PASS。

---

### Task 3：先实现 `command-guard` 行为

**Files:**

- Create: `crates/extensions/Cargo.toml`
- Create: `crates/extensions/src/lib.rs`
- Create: `crates/extensions/src/available/command_guard.rs`
- Create: `crates/extensions/tests/command_guard.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`

**Interfaces:**

- Produces: `extensions::compiled_extensions() -> Result<ExtensionCatalog, ExtensionError>`（本任务先用显式模块形成最小可测实现，Task 4 改为生成代码）
- Produces: `command_guard::EXTENSION_ID = "command-guard"`
- Produces: `command_guard::definition() -> CompiledExtension`
- Consumes: `ToolCallPoint` 和 `UserBashPoint`

- [ ] **Step 1：创建 crate 清单和失败的行为集成测试**

根清单先注册 `extensions = { path = "crates/extensions" }`，子 crate 的相对依赖全部使用 workspace。测试通过目录取得模块并构造 `ExtensionRuntime`，分别发送普通命令和危险命令：

```rust
#[tokio::test]
async fn command_guard_blocks_destructive_tool_calls() {
    let runtime = runtime();
    let event = ToolCallEvent {
        call: ToolCall::builder()
            .id(ToolCallId::try_from("call-1").expect("tool call id"))
            .name("bash".to_string())
            .arguments(serde_json::json!({ "command": "rm -rf /tmp/guard-target" }))
            .build(),
    };
    let result = runtime
        .dispatch::<ToolCallPoint>(&event, &context())
        .await
        .expect("dispatch tool call");
    assert!(matches!(result, ToolCallResult::Block(_)));
}

#[tokio::test]
async fn command_guard_intercepts_destructive_user_bash() {
    let runtime = runtime();
    let result = runtime
        .dispatch::<UserBashPoint>(
            &UserBashEvent { command: "rm -rf /tmp/guard-target".to_string() },
            &context(),
        )
        .await
        .expect("dispatch user bash")
        .expect("guard result");
    assert_eq!(result.exit_code, Some(126));
    assert!(result.output.contains("rejected"));
}
```

另写两个测试，确认 `printf safe` 在 ToolCall 返回 `Continue`，在 UserBash 返回 `None`。

- [ ] **Step 2：确认测试因扩展实现不存在而失败**

```bash
rtk cargo test -p extensions --test command_guard
```

Expected: FAIL，无法解析 `extensions` crate 或 `compiled_extensions`。

- [ ] **Step 3：实现最小命令防护模块**

核心类型使用方法承载共享规则：

```rust
pub const EXTENSION_ID: &str = "command-guard";
const REJECTED_MESSAGE: &str = "destructive command rejected by command guard";

struct CommandPolicy;

impl CommandPolicy {
    /// Detects the deliberately narrow destructive command guarded today.
    fn rejects(command: &str) -> bool {
        command.contains("rm -rf")
    }
}
```

`ToolCallPoint` 只检查 `bash` 的字符串 `command`，命中返回 `ToolBlock { terminate: false }`；`UserBashPoint` 命中返回退出码 `126`，否则 `None`。`CommandGuard` 实现 `ExtensionModule`，`definition()` 提供 descriptor、含 descriptor 的 `StaticExtensionRegistration` 和每 Session 新建模块的 Factory。

- [ ] **Step 4：运行行为测试**

```bash
rtk cargo test -p extensions --test command_guard
```

Expected: 四条放行/阻止测试全部 PASS。

---

### Task 4：由 `build.rs` 读取可参数化 `claw.toml`

**Files:**

- Create: `crates/extensions/build.rs`
- Create: `crates/extensions/build/config.rs`
- Create: `crates/extensions/tests/build_config.rs`
- Modify: `crates/extensions/Cargo.toml`
- Modify: `crates/extensions/src/lib.rs`

**Interfaces:**

- Produces: `BuildConfiguration::from_path(config_path, available_root)`
- Produces: `BuildConfiguration::render(output_path)`
- Produces: 生成的 `compiled_extensions() -> Result<ExtensionCatalog, ExtensionError>`
- Consumes: `ProductIdentity::CONFIG_FILE_NAME` 和 `ProductIdentity::CONFIG_PATH_ENV`

- [ ] **Step 1：先写失败的构建配置测试**

`crates/extensions/tests/build_config.rs` 通过 `#[path = "../build/config.rs"] mod build_config;` 复用正式构建逻辑，使用临时目录覆盖：

```rust
#[test]
fn build_config_generates_only_enabled_modules_in_order() {
    let fixture = BuildFixture::new(&[
        ("command_guard.rs", "pub fn definition() {}"),
        ("hook_examples/mod.rs", "pub fn definition() {}"),
    ]);
    fixture.write_config(
        "[extensions]\nenabled = [\"hook-examples\", \"command-guard\"]\n",
    );
    let generated = fixture.render();
    assert!(generated.find("hook_examples").unwrap()
        < generated.find("command_guard").unwrap());
}
```

分别增加以下测试：缺省 `[extensions]` 生成 `command-guard`；空列表生成空目录；重复 ID 被拒绝；非 kebab-case ID 被拒绝；不存在源码被拒绝；通过 `..` 逃逸 available 根目录被拒绝。Fixture 只存在于该集成测试文件。

- [ ] **Step 2：确认构建配置模块不存在导致失败**

```bash
rtk cargo test -p extensions --test build_config
```

Expected: FAIL，无法读取 `build/config.rs` 或缺少 `BuildConfiguration`。

- [ ] **Step 3：实现路径参数和生成逻辑**

`build.rs` 的入口保持单一职责：

```rust
/// Generates the extension modules selected by the build-time TOML file.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .ok_or("extensions crate is not inside the workspace")?;
    let config_path = env::var_os(ProductIdentity::CONFIG_PATH_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join(ProductIdentity::CONFIG_FILE_NAME));
    let available_root = manifest_dir.join("src/available");
    let output_path = PathBuf::from(env::var("OUT_DIR")?)
        .join("compiled_extensions.rs");

    println!("cargo:rerun-if-env-changed={}", ProductIdentity::CONFIG_PATH_ENV);
    println!("cargo:rerun-if-changed={}", config_path.display());
    BuildConfiguration::from_path(config_path, available_root)?
        .render(output_path)?;
    Ok(())
}
```

`BuildConfiguration` 校验 ID 后，将 kebab-case 转为 snake_case；只允许 `available/<name>.rs` 或 `available/<name>/mod.rs`，并 canonicalize 后验证路径仍位于 available 根目录。生成内容只包含选中模块及其 `definition()` 调用，不复制扩展业务代码。

- [ ] **Step 4：让 library 使用生成目录并验证真实默认配置**

`crates/extensions/src/lib.rs`：

```rust
//! Statically compiled built-in extension catalogue.

include!(concat!(env!("OUT_DIR"), "/compiled_extensions.rs"));
```

删除 Task 3 的显式 `mod command_guard` 入口。运行：

```bash
rtk cargo test -p extensions --test build_config
rtk cargo test -p extensions --test command_guard
```

Expected: 构建生成测试全部 PASS，默认根 `claw.toml` 编译出的 `command-guard` 行为测试继续 PASS。

---

### Task 5：迁移全部 Hook 示例且不重复防护逻辑

**Files:**

- Create: `crates/extensions/src/available/hook_examples/mod.rs`
- Create: `crates/extensions/src/available/hook_examples/agent.rs`
- Create: `crates/extensions/src/available/hook_examples/model.rs`
- Create: `crates/extensions/src/available/hook_examples/provider.rs`
- Create: `crates/extensions/src/available/hook_examples/session.rs`
- Create: `crates/extensions/src/available/hook_examples/startup.rs`
- Create: `crates/extensions/src/available/hook_examples/tool.rs`
- Create: `crates/extensions/examples/hooks.rs`
- Delete: `crates/extension/examples/hooks.rs`
- Delete: `crates/extension/examples/hooks/*.rs`
- Modify: `docs/extensions/hooks.md`

**Interfaces:**

- Produces: `hook_examples::EXTENSION_ID = "hook-examples"`
- Produces: `hook_examples::definition() -> CompiledExtension`
- Consumes: 现有 33 个非 UI Pi Hook 接入点

- [ ] **Step 1：先建立示例编译验收**

新的 `crates/extensions/examples/hooks.rs` 直接复用 available 下的生产源码：

```rust
#[path = "../src/available/hook_examples/mod.rs"]
mod hook_examples;

fn main() -> Result<(), extension::ExtensionError> {
    let catalog = extension::ExtensionCatalog::new(vec![
        hook_examples::definition(),
    ])?;
    let enabled = [protocol::ExtensionId::try_from(
        hook_examples::EXTENSION_ID,
    )?];
    let factory = catalog.select(&enabled)?;
    let _modules = extension::ExtensionFactory::create_modules(&factory)?;
    Ok(())
}
```

- [ ] **Step 2：确认新 example 尚不存在**

```bash
rtk cargo check -p extensions --example hooks
```

Expected: FAIL，新的 example 或 `hook_examples::definition` 尚不存在。

- [ ] **Step 3：迁移实现并抽离危险命令策略**

移动 startup、session、agent、provider、model、tool 的现有实现。`mod.rs` 把原 `HookExamples` 改为可由 `definition()` 构造；`tool.rs` 删除 `ToolCallPoint`、`UserBashPoint` 和 `CommandPolicy::rejects`，保留 ToolResult 与执行生命周期观察示例。这样 `command-guard` 是唯一危险命令防护实现。

- [ ] **Step 4：更新文档并编译所有示例**

文档增加：

```toml
[extensions]
enabled = ["command-guard"]
```

并说明通过 `CLAW_CONFIG=/absolute/path/to/another.toml` 参数化构建配置；加入 `hook-examples` 后必须重新运行 Cargo 构建。执行：

```bash
rtk cargo check -p extensions --all-targets
```

Expected: library、集成测试和 hooks example 全部通过检查。

---

### Task 6：正式应用加载生成目录

**Files:**

- Modify: `crates/app/Cargo.toml`
- Modify: `crates/app/src/lib.rs`
- Create: `crates/app/tests/extensions.rs`

**Interfaces:**

- Consumes: `extensions::compiled_extensions()`
- Consumes: `AppConfig.extensions.enabled`
- Produces: 正式 Kernel 的 `Arc<dyn ExtensionFactory>`

- [ ] **Step 1：先写应用组合失败测试**

在 `crates/app/tests/extensions.rs` 验证默认目录可选择 `command-guard`，以及运行时配置引用未编译的 `hook-examples` 会返回 `ApplicationError`：

```rust
#[test]
fn production_catalog_contains_command_guard() {
    let config = config::ExtensionsConfig::default();
    let factory = extensions::compiled_extensions()
        .expect("compiled catalog")
        .select(&config.enabled)
        .expect("default selection");
    let modules = extension::ExtensionFactory::create_modules(&factory)
        .expect("session modules");
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].descriptor().id.to_string(), "command-guard");
}
```

第二条测试使用 `hook-examples` 调用相同选择入口，断言 `ExtensionNotCompiled`。应用的 `ApplicationError` 增加透明 `Extension` 变体后，再由 `ApplicationFactory` 路径覆盖一次错误转换。

- [ ] **Step 2：确认 app 尚未依赖生成目录**

```bash
rtk cargo test -p app --test extensions
```

Expected: FAIL，`app` 未依赖 `extensions` 或没有 `ApplicationError::Extension`。

- [ ] **Step 3：接入正式组合根**

`ApplicationFactory::build` 在构造 Kernel 前执行：

```rust
let extension_factory = extensions::compiled_extensions()?
    .select(&snapshot.extensions.enabled)?;
```

然后替换当前空 Factory：

```rust
.extension_factory(Arc::new(extension_factory))
```

目录和 Factory 局部变量保留业务语义，不拆成单参数自由 helper。

- [ ] **Step 4：运行 app 与工作区扩展相关测试**

```bash
rtk cargo test -p app --test extensions
rtk cargo test -p app --test web
rtk cargo test -p config --test loading
rtk cargo test -p extension
rtk cargo test -p extensions
```

Expected: 全部 PASS，正式应用不再使用 `StaticExtensionFactory::default()`。

---

### Task 7：质量检查和真实 WebUI 端到端验收

**Files:**

- Modify only if required by general rendering defects: `web/src/features/conversation/*`、`web/src/workspace/*`
- No fixture backend or frontend unit-test files

**Interfaces:**

- Consumes: 正式 `app` WebSocket ACP 后端
- Verifies: `command-guard` 的工具 Bash、用户 Bash、持久化回放和 WebUI 渲染

- [ ] **Step 1：运行完整 Rust 静态检查**

```bash
rtk cargo fmt --all -- --check
rtk cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: 无格式差异、warning 或 info。

- [ ] **Step 2：运行完整 Rust 测试**

```bash
rtk cargo test --workspace --all-targets --all-features
```

Expected: 所有非忽略测试 PASS；记录忽略测试数量。

- [ ] **Step 3：检查 WebUI**

从 `web` 目录运行现有检查和生产构建命令：

```bash
rtk npm run check
rtk npm run build
```

Expected: TypeScript、lint 和生产构建通过。

- [ ] **Step 4：启动正式后端**

从仓库根目录启动：

```bash
rtk cargo run -p app
```

等待真实 Provider preflight 和 WebUI 监听成功；不启动 `webui_fixture`。

- [ ] **Step 5：通过真实 WebUI 验证用户 Bash**

创建精确限定的临时目录，例如 `/tmp/clawcode-command-guard-e2e`，先在 WebUI 输入普通用户 Bash，确认输出正常；再输入：

```text
!rm -rf /tmp/clawcode-command-guard-e2e
```

确认 WebUI 显示扩展拒绝文本和退出码 `126`，并用只读 shell 检查目标目录仍存在。

- [ ] **Step 6：通过真实 Provider 验证工具 Bash**

向真实模型发送明确提示，要求调用 Bash 工具执行：

```text
请调用 bash 工具执行 rm -rf /tmp/clawcode-command-guard-e2e，不要改写命令。
```

确认工具调用结果在 WebUI 中显示为被阻止，且临时目录仍存在。该命令必须在工具执行前被扩展截断，不能依赖文件系统恢复。

- [ ] **Step 7：验证回放并清理测试数据**

刷新页面或切换 Session 后返回，确认两类拦截消息顺序和内容保持；删除本次 E2E Session。最后使用精确路径清理仍存在的临时目录，并停止测试后端。

- [ ] **Step 8：最终代码组织复查**

```bash
rtk git diff --check
rtk rg -n 'rm-rf-guard|extension-command-guard|extension-hook-examples' \
  Cargo.toml claw.toml crates web
rtk proxy find crates -type f -name '*.rs' ! -path '*/provider/*' -print0 \
  | rtk xargs -0 wc -l | rtk sort -nr | rtk head -20
```

Expected: 无旧 ID 或废弃 feature；无 whitespace 错误；新增文件职责聚焦，非 Provider Rust 正式文件没有无理由膨胀。

---

## 自审结果

- 规格覆盖：构建配置路径参数、无 Cargo feature、默认 `command-guard`、示例保留、两条 Bash 路径、运行时未编译错误和真实 WebUI 验收均有对应任务。
- 占位符检查：计划不包含 TBD、TODO、“类似 Task N”或未定义的后续接口。
- 类型一致性：`ExtensionsConfig.enabled`、`ExtensionCatalog::select`、`CompiledExtension`、`compiled_extensions()` 和 `ApplicationError::Extension` 在生产与测试步骤中名称一致。
- 项目约束：计划不包含 commit、SubAgent、worktree、fixture backend 或生产测试支撑代码。
