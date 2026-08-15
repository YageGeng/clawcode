# 静态扩展加载设计

## 背景

当前 `app` 在组合 `KernelFactory` 时使用空的 `StaticExtensionFactory`，因此已经实现的扩展生命周期只能由测试或 Cargo example 手工装配，正式后端无法加载这些扩展。现有 `hook-examples` 同时包含观察型示例、请求变换示例和危险命令拦截示例；如果整体默认启用，会修改系统提示词、Provider 请求体及请求头，不适合作为生产默认行为。

本阶段将扩展框架与扩展实现分离：`extension` crate 继续提供生命周期、注册器和 Factory 抽象；新的 `extensions` crate 保存随二进制静态编译的扩展实现；`extensions` 的 `build.rs` 读取构建配置并生成本次构建可用的扩展目录；运行时 TOML 只能使用已经编译进该目录的扩展。

## 目标

1. 正式后端能够加载静态编译的 Rust 扩展，并为每个 Session 创建独立扩展实例。
2. 默认只编译并启用 `command-guard` 扩展。
3. `command-guard` 同时拦截模型发起的 Bash 工具调用和用户直接发起的 Bash 命令中的 `rm -rf`。
4. 保留全部 Hook 示例，但默认构建配置不编译、不启用有副作用的 `hook-examples`。
5. WebUI 不增加扩展管理界面或 Hook 调用轨迹，只展示扩展作用后产生的正常 ACP 消息。
6. 使用正式后端和真实 Provider 完成 WebUI 端到端验收。

## 非目标

- 不支持动态库、WASM、脚本扩展或运行时下载扩展。
- 不支持配置热更新。
- 不允许 TOML 启用本次二进制未编译的扩展。
- 不增加 Hook started/completed 等 Pi 中不存在的协议事件。
- 不提供 WebUI 扩展管理、扩展配置或扩展专用 UI 接入点。
- 本阶段不实现扩展私有配置参数。

## 模块边界

### `extension` crate

继续作为扩展运行时框架，负责：

- `ExtensionModule`、`ExtensionFactory` 和 `ExtensionModuleFactory`。
- Hook 注册与冲突检查。
- 描述已编译扩展的类型化目录，以及按运行时配置选择扩展。
- 将选中的静态注册信息和 Session 级模块 Factory 合成为 `StaticExtensionFactory`。

该 crate 不依赖具体扩展实现，也不读取应用 TOML。

### `extensions` crate

新增工作区 crate，保存内置扩展实现：

- `command_guard`：独立的生产扩展。
- `hook_examples`：保留其余 Pi Hook 接入点的真实示例。

`build.rs` 只为配置列出的扩展生成模块声明，因此未列出的扩展源码不进入本次 crate 编译。`hook_examples` 不再重复实现 `rm -rf` 防护，避免同时启用两个扩展时重复拦截。

### `config` crate

新增 `ExtensionsConfig`：

```toml
[extensions]
enabled = ["command-guard"]
```

`enabled` 在类型层面区分两种状态：

- 整个配置项缺省：使用默认值 `["command-guard"]`。
- `enabled = ["..."]`：严格按照列表顺序编译并启用指定扩展；空列表表示不编译、不启用任何扩展。

构建脚本只解析 `extensions.enabled`，不解析、输出或嵌入 Provider 配置及密钥。运行时的 config crate 使用同一个字段；如果二进制运行时换用了不同配置，则应用组合阶段验证运行时列表不得包含未编译扩展。

### `app` crate

`app` 是运行时组合根，`extensions` 是构建期扩展组合根：

- 项目根目录的 `claw.toml` 是默认构建配置。
- 配置路径作为 `BuildConfig` 的构造参数传入，不耦合在解析或代码生成逻辑中。
- `build.rs` 默认使用 `ProductIdentity::CONFIG_FILE_NAME`，并允许通过 `ProductIdentity::CONFIG_PATH_ENV` 指定其他绝对路径；当前值分别为 `claw.toml` 和 `CLAW_CONFIG`，后续改名只修改公共身份常量。覆盖路径必须是绝对路径，避免 Cargo 构建进程和运行进程使用不同工作目录时解析出不同文件。
- `build.rs` 读取 `extensions.enabled`，校验扩展 ID 和对应源码路径，并生成 `OUT_DIR/compiled_extensions.rs`。
- 正式代码通过 `include!` 使用生成目录，不手写重复的扩展 ID 或 Factory 列表。
- `ApplicationFactory` 使用 TOML 选择目录条目并把结果传给 `KernelFactory`。

`build.rs` 为每个配置 ID 生成对应的模块声明和 Factory 登记；Rust 编译器只看到这些模块，因此只编译指定扩展。构建脚本通过 `cargo:rerun-if-changed` 跟踪实际配置文件，并通过 `cargo:rerun-if-env-changed` 跟踪配置路径参数。改变扩展列表后重新执行 Cargo 构建即可得到新的静态扩展集合，不需要 Cargo features。

## 类型设计

扩展目录使用类型表达构建产物，不使用松散字符串和并行数组。核心条目包含：

- `ExtensionDescriptor`：稳定 ID、名称和版本。
- `StaticExtensionRegistration`：模型、Provider 或 Flag 等启动前声明。
- `ExtensionModuleFactory`：为每个 Session 创建新模块。
- 构建顺序：与 `extensions.enabled` 的声明顺序一致。

条目构造由具体扩展模块提供，`build.rs` 只生成模块声明及对这些构造入口的引用。扩展 ID 到源码文件采用受限命名约定：合法的 kebab-case ID 转换为 snake_case 模块名，并且解析后的源码必须位于 `extensions/src/available` 下。具体扩展仍通过公共常量声明自身 ID；目录创建时校验模块 descriptor 与配置 ID 一致，避免生产运行时依赖散落的字符串常量。

目录生成和运行时选择保持 TOML 顺序。选择过程中一次性检查：

1. 配置 ID 是否重复。
2. ID 是否存在于本次构建目录。
3. 目录条目声明的 descriptor 是否与模块实例返回值一致。
4. 静态 Flag、Provider 等注册信息是否冲突。

任一检查失败都会终止应用启动，不进行部分加载。

## `command-guard` 行为

扩展 ID 为 `command-guard`。当前规则保持明确而有限：检测命令文本中的 `rm -rf`，不扩展为通用 Shell 安全分析器。

扩展注册两个 Pi 兼容 Hook：

1. `ToolCallPoint`：仅检查名为 `bash` 的工具及其字符串 `command` 参数；命中后返回非终止型 `ToolBlock`，不执行工具。
2. `UserBashPoint`：命中后直接返回退出码 `126` 的 `UserBashResult`，不启动服务端 Shell。

两条路径使用同一个策略类型的方法判断命令，避免重复规则和自由 helper。拦截原因保持稳定、可读，并通过现有工具结果或 Bash 执行消息进入 ACP，不新增扩展专用事件。

## `hook-examples` 处理

现有示例按 Hook 语义继续拆分在 startup、session、agent、provider、model 和 tool 文件中，并迁移为可静态编译的扩展模块。原 Cargo example 改为复用该实现，不复制结构体和 Hook 注册代码。需要使用示例时，在构建所读取的 `claw.toml` 中加入 `hook-examples` 后重新构建。

其中危险命令拦截逻辑迁移至 `command_guard`；其余示例行为保持，包括资源发现、输入变换、系统提示词变换、Provider 请求变换和生命周期观察。由于这些行为可能影响真实请求，`hook-examples` 默认不编译、不启用。

## 错误行为

应用启动错误应明确区分：

- 配置引用了未编译扩展。
- 配置重复启用同一扩展。
- 扩展描述符与目录声明不一致。
- 扩展静态注册冲突。
- 扩展模块创建或注册失败。

错误中包含扩展 ID，但不包含 API Key、Provider Header 或其他敏感数据。

## 测试策略

### Rust 集成测试

遵循项目规则，测试只放在各 crate 的 `tests/` 目录或标准 `#[cfg(test)] mod tests` 中，生产代码不增加测试支撑分支。

覆盖以下行为：

- TOML 缺省、空列表和有序列表的解析。
- 构建配置默认值、顺序保持、重复 ID、非法 ID、源码越界和不存在的扩展错误。
- 运行时配置引用未编译扩展时的启动错误。
- `command-guard` 放行普通 Bash 工具调用。
- `command-guard` 阻止包含 `rm -rf` 的 Bash 工具调用。
- `command-guard` 放行普通用户 Bash，使 Kernel 继续执行默认路径。
- `command-guard` 接管包含 `rm -rf` 的用户 Bash 并返回退出码 `126`。
- 默认 `claw.toml` 构建包含 `command-guard`，不包含 `hook-examples`。

所有生产行为按 TDD 实现：先增加失败的集成测试并确认失败原因，再写最小实现使其通过。

### WebUI 真实后端端到端测试

启动正式 `app` 后端，使用用户已修复的真实配置和真实 Provider，不使用 fixture：

1. 创建新 Session。
2. 执行无破坏性的用户 Bash，确认正常完成并按事件顺序渲染。
3. 执行指向临时测试路径的 `!rm -rf ...`，确认返回退出码 `126` 和扩展拦截文本，且目标仍存在。
4. 向模型发送要求调用 Bash `rm -rf` 临时测试路径的提示，确认工具调用被标记为阻止且目标仍存在。
5. 刷新或重新进入 Session，确认拦截消息可由持久化记录正确回放。
6. 删除端到端测试创建的 Session；临时测试路径使用可恢复、精确限定的目标，不执行真实破坏命令。

WebUI 不需要为此新增组件；如果现有正常 ACP 工具结果或 Bash 消息无法正确呈现，则只修复通用消息渲染逻辑。

## 验收标准

- 默认 `claw.toml` 只静态编译并登记 `command-guard`。
- 未配置 `[extensions]` 时构建和运行时都默认启用 `command-guard`。
- `enabled = []` 构建不包含任何扩展，运行时也关闭全部扩展。
- `CLAW_CONFIG` 可以通过绝对路径把构建配置切换到其他 TOML 文件，路径处理逻辑无需修改。
- 配置未编译扩展时应用拒绝启动并给出明确错误。
- 每个 Session 获得独立扩展模块实例。
- 两条 Bash 路径中的 `rm -rf` 均在执行前被阻止。
- 其余 Hook 示例仍可通过构建配置显式编译并启用。
- Rust 格式化、Clippy、工作区测试及真实后端 WebUI 端到端测试全部通过。
