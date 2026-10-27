# MCP 工具发现、结果预算与 Pi 协议协商实现计划

> **执行状态（2026-10-27）：** 用户后续已授权实施，实现及核心验收已完成。保持禁止 SubAgent、worktree、commit 的约束。复选框按实际结果更新；9.6 的外部人工授权部分未验证，详见[验证记录](../reports/2026-10-27-mcp-tool-discovery-verification.md)。

**目标：** 落实六组 spec 能力：Pi 四版本协商、稳定工具名、元数据、结果预算、模型资源工具，以及 deferred/tool_search 与完整选择状态恢复。

**架构：** MCP 管协议、远端身份和资源访问，tools 提供借用式内容投影，Kernel 管可见集合、搜索和持久化。保留现有 crate、AgentTool、每轮不可变 registry 与 JSONL，按 docparse 风格让具体操作 struct 引用必要上下文，通过关联方法和标准 trait 完成工作。

**技术栈：** Rust、Tokio、rmcp 3、serde、typed-builder、现有 tempfile/sha2 依赖体系、ACP 2、React/TypeScript。

**Spec：** [MCP 工具发现、结果预算与 Pi 协议协商设计](../specs/2026-10-27-mcp-tool-discovery-and-pi-negotiation-design.md)。本计划直接实现该文档；历史 MCP plan 中的精确版本与 Modern 流程不能覆盖它。

**基线：** clawcode d14d189；Pi db14038bd；docparse fd41de7。开始规划时已有本轮 spec 未跟踪文件，必须保留。参考代码与行为以 spec 的源码链接为准。

## 1. 全局约束

- 规划阶段仅交付 Markdown；获得用户后续实施授权后，在当前工作区执行构建、测试和正式 app/provider 验收，结果另存验证记录。
- 后续实现顺序为阶段 1 至 9。每阶段完成后进行本地自审；阶段检查通过是增量验收，不代表中间版本可以发布。
- Rust 非平凡行为按“新增失败测试 → 确认因目标行为失败 → 最小实现 → 目标测试通过”推进。缺少依赖、端口冲突、权限错误不能冒充预期失败。
- 前端不要求 TDD 或单元测试；WebUI 验收必须使用正式 app 和配置 provider。可控 MCP 参考服务器可以作为外部服务，不能用它替换 app 或模型 provider。
- 测试仅位于 crate/tests 或精确的 #[cfg(test)] mod tests；生产代码不增加测试专用字段、模块或 cfg 分支。
- 所有新增函数有英文函数级注释；旧代码的非平凡修改用英文解释原因。spec/plan 为中文；代码注释不改为中文。
- 超过三个字段的 struct 使用 typed-builder；Option 字段使用 builder(default)，按调用端数据决定 strip_option。显式 Arc::clone。
- 标准 trait 遵守 orphan rule；protocol 不依赖 rmcp，不为凑 From/TryFrom 增加空包装。
- 依赖只在根 workspace.dependencies 声明，子 crate 继承；沿用分组和排序。只显式引入 lockfile 已有的 sha2，不引入搜索框架、模型检索或新 crate。
- 日志只在协商、状态变化和错误等必要边界添加；使用完整 tracing 路径与普通格式化消息，不记录查询全文、完整参数/结果和凭据。
- 所有 shell 命令使用 rtk 前缀。没有用户新授权，不创建 commit，也不增加 git add/push 步骤。

### 1.1 不得漂移的产品数值

| 项目 | 固定值 |
|---|---|
| 协议集合 | 2025-11-25、2025-06-18、2025-03-26、2024-11-05 |
| 默认协商 | Auto，initialize 请求 2025-11-25；不逐版本重试，不使用 rmcp Discover/Auto |
| 普通请求总时限 | 默认 300 秒；活动时限继续使用 request_timeout_sec |
| public name | 最多 64 ASCII 字节；改写名为最多 47 字节前缀、下划线、16 个小写摘要字符 |
| 摘要输入 | clawcode-mcp-tool-v1、零字节、server 长度 u64 大端、server 原字节、remote 长度 u64 大端、remote 原字节 |
| 结果文本 | 合计 20 KiB，含投影说明、URI、文件位置及结构化文本 |
| 内联媒体 | 最多 4 块，编码数据合计最多 8 MiB，不切割媒体 |
| 完整文件 | 最多 64 MiB 序列化字节 |
| progress | 文本最多 1 KiB，不落完整文件 |
| 资源分页 | 默认 50 项，limit 1..=200，同时满足 20 KiB 文本限制 |
| 搜索 | query trim 后 1..=256 个 Unicode 标量；limit 默认 8，范围 1..=20 |
| 检索索引 | 每工具最多 16 KiB 文本；参数 schema 遍历深度最多 8 |
| 排序 | BM25 k1=1.2、b=0.75；精确名称优先，同分按 public name |
| 搜索加载预算 | 单次新增定义合计最多 32 KiB，不截断 schema |
| 默认 exposure | Direct；逐工具覆盖只匹配原始 remote_name 精确键 |

### 1.2 五项重点风险与测试归属

| 编号 | 需要防止的真实失败 | 负责阶段与测试 |
|---|---|---|
| F1 | rmcp 提前 initialized，或 HTTP 404 内部重新握手绕过版本检查 | 阶段 1：rejects_version_before_initialized、expired_http_session_does_not_reinitialize |
| F2 | 新摘要名与另一合法原名碰撞，或目录重排改变原工具路由 | 阶段 2：rejects_generated_and_literal_name_collision、names_survive_catalog_reordering |
| F3 | 扩展重新放大输出，落盘失败使远端写操作被误判并重跑 | 阶段 4：bounds_hook_expanded_result、spool_failure_does_not_repeat_remote_call |
| F4 | 两个搜索、目录刷新和 Store 故障交错，造成丢加载或虚假成功 | 阶段 5/7：concurrent_loads_merge、failed_selection_sync_does_not_publish、search_revalidates_changed_catalog |
| F5 | 旧游标被跨 Session、不同资源种类或新目录使用，造成串数据/漏项 | 阶段 6：rejects_cursor_context_mismatch、rejects_stale_catalog_cursor |

## 2. 文件与对象组织

### 2.1 文件边界

已有文件按原职责修改；下表“新增”仅在以后执行相应阶段时创建。

| 位置 | 操作与责任 | 阶段 |
|---|---|---|
| crates/protocol/src/mcp/identity.rs、status.rs；crates/config/src/mcp.rs | 修改：版本、配置策略、实际协商值、迁移错误 | 1 |
| crates/mcp/src/client/negotiation.rs、initialize.rs、request.rs | 新增：握手保护、统一 initialize 客户端、普通请求驱动 | 1 |
| crates/mcp/src/client/{core,handler,mod}.rs、transport.rs、config.rs、error.rs、event.rs、lib.rs、server/{mod,lifecycle}.rs | 修改：协议迁移、生命周期/能力限制、清理导出 | 1 |
| crates/mcp/src/client/{legacy,modern,mrtr,task}.rs | 将仍需要的普通行为迁移后删除 Modern/旧 wrapper 实现 | 1 |
| crates/mcp/examples/reference_server/mod.rs、reference_stdio.rs、reference_http.rs | 更新共用参考服务器；两个新名称替代 legacy_stdio/modern_http | 1、9 |
| crates/protocol/src/tool.rs、mcp/catalog.rs、extension/events/tool.rs；crates/tools/src/contract.rs | 修改：稳定名称、注解、宿主元数据及扩展上下文 | 2 |
| crates/mcp/src/client/catalog.rs、tool.rs、session.rs、config.rs | 修改：带服务器上下文的映射、有效策略、发布前验证 | 2、6 |
| crates/tools/src/output.rs | 新增：预算、借用式模型文本视图、受限完整文件写入 | 3 |
| crates/protocol/src/message.rs；crates/kernel/src/provider.rs、provider/content.rs、runtime/tools/batch.rs | 修改：最终结果投影、历史字段和 Provider 共用文本实现 | 3、4 |
| crates/kernel/src/runtime/tools/state.rs、selection.rs | 修改状态；新增选择提交/重放对象，避免 state.rs 同时承担 I/O | 5 |
| crates/kernel/src/runtime/{session,tree,transcript}.rs、extension/host.rs、session/replay.rs | 修改：唯一提交入口、完整分支恢复、回放字段 | 4、5 |
| crates/mcp/src/resource.rs；crates/protocol/src/mcp/request.rs | 新增资源工具/借用分页；修改 cursor/取消公共边界 | 6 |
| crates/kernel/src/runtime/tools/search.rs | 新增索引、借用查询、ToolSearchTool；不创建检索插件系统 | 7 |
| crates/kernel/src/runtime/{mcp,prompt}.rs、extension/context.rs | 修改：目录/选择投影和动态搜索入口 | 5、7、8 |
| crates/acp/src/{mapping,extension}.rs | 修改：结果字段、资源取消和服务端快照 | 4、6、8 |
| web/src/domain/model.ts、workspace/{updateDecoder,updateRouter,controller}.ts | 修改前后端字段、权威快照消费和回放 | 1、8 |
| web/src/features/mcp/{McpPanel,McpServerCard,McpCatalog}.tsx、features/conversation/ToolCallCard.tsx | 修改：实际协议、元数据、加载状态、输出诊断 | 1、8 |
| Cargo.toml、crates/protocol/Cargo.toml、必要的子 crate Cargo.toml | workspace 继承与已有依赖复用；确有解析变化才更新 Cargo.lock | 2、3、6 |
| claw.toml、README.md、README_zh.md | 配置迁移与最终使用说明，保留用户有效凭据引用 | 1、8 |

修改 protocol/tool/message 公共字段时，在当前树搜索所有构造、解构与转换位置，一并修复调用者和测试，不能只修改上表中的主入口。每阶段后运行的 workspace check 用于捕获遗漏消费者。

### 2.2 docparse 风格落点

| 对象 | 持有/借用的上下文 | 方法与 trait 选择 |
|---|---|---|
| NegotiationTransport<T> | 拥有真实 transport 与共享协商状态 | 实现现有 Transport trait；只验证握手，不扩展成万能 RPC 中间件 |
| CatalogMapper<'a> | 借用 RuntimeMcpServer | map/tool 关联方法；wire 和 protocol 都是外部类型时不用非法 From |
| ToolContentText<'a> | 借用一个 ContentBlock | TryFrom 拒绝原生媒体/ToolCall 的错误文本化；write_to 流式投影 |
| ToolResultProjection<'a> | 借用可变结果、策略、取消 token、输出目录 | apply 完成一次投影；I/O 不藏进转换 trait |
| ToolSelectionUpdate<'a> | 借用状态、Transcript、Clock、IdGenerator | replace/load 提交选择；不拿整个 Kernel，也不重复获取 Run gate |
| ResourcePage<'a> | 借用不可变 MCP 快照与分页参数 | Serialize 与 result；只处理当前页，不 clone 整个目录 |
| ToolSearchIndex | 拥有有界词项及统计数据，随目录版本发布 | FromIterator 构建；缓存采用 Arc，无自引用字段 |
| ToolSearch<'a> | 借用索引、快照、当前查询 | ranked 方法检索；没有持久化或远端调用 |
| ToolSearchTool | Weak<Session> 和必要的长生命周期提交依赖 | AgentTool；升级 Weak 后构造短期查询/提交对象 |
| McpResourceTool | Arc<McpSession>、操作 enum、缓存的元数据 | 一个 AgentTool 实现三个固定操作，不增加第二条连接 |

选择标准 trait 是为表达真实转换，不要求所有逻辑都变成 trait。每个新增对象必须有明确所有权或一组相关行为；禁止仅为减少一行参数而创建空 Service/Manager。

## 3. 阶段依赖与验证约定

~~~text
1 协议协商
  → 2 名称与元数据
  → 3 结果预算组件
  → 4 Kernel 结算/Store/ACP
  → 5 工具选择与分支恢复
  → 6 模型资源工具
  → 7 tool_search
  → 8 ACP/WebUI/说明文档
  → 9 完整验收
~~~

命令默认在仓库根目录执行；阶段内明确给出其他 cwd 时例外。以下是未来执行命令，本轮不运行。目标测试命令必须核对实际执行了列出的测试，零测试不能视为通过；显式忽略的真实 transport 测试在阶段 9 单独运行。

下文 Rust 片段是测试断言要点，不是完整测试程序。输入条件在同一步描述，测试初始化沿用指定 tests/support；不为计划中的断言预先创建生产代码或测试文件。

### 阶段 1：四版本协商、普通请求与 Modern 退役

**改动文件：** 第 2.1 节的协议/配置/客户端/参考服务器文件；同步 crates/protocol/src/mcp/{mod,task}.rs、crates/protocol/src/lib.rs、crates/mcp/src/client/invocation.rs、crates/kernel/tests/mcp_host.rs、crates/app 的配置投影，以及 WebUI 最小协议字段显示。只删除不再执行的 Modern 路径；仍用于旧消息解码的类型保留。

**测试文件：** 修改 crates/config/tests/mcp.rs、crates/protocol/tests/mcp.rs、crates/mcp/tests/{config,host,oauth,tool,dynamic_catalog,session_lifecycle}.rs 及各 tests/support。新增 crates/mcp/tests/negotiation.rs、lifecycle.rs、requests.rs；合并旧 lifecycle_legacy/lifecycle_modern。将 interop_legacy/interop_modern 改为 interop_stdio/interop_http；将 mrtr/tasks 中普通进度、取消和内容用例迁入 requests/host 后移除专属用例。

**接口产出：**

- McpProtocolVersion::supported() -> &'static [McpProtocolVersion]；McpProtocolPolicy::{Auto, Preferred(version)} 实现 Default、FromStr；requested_version(&self) -> McpProtocolVersion。
- RuntimeMcpServer::try_from(McpServerConfig) -> Result<Self, McpConfigError>，产出 protocol_policy、request_timeout、request_total_timeout，不再有 McpMrtrPolicy。
- McpServerStatus 的 configured_policy: McpProtocolPolicy、negotiated_protocol: Option<McpProtocolVersion>；Serde 为 configuredPolicy/negotiatedProtocol。
- NegotiationTransport<T>::new(inner: T, policy: McpProtocolPolicy) -> (Self, Arc<Mutex<NegotiationOutcome>>)。NegotiationOutcome 在 negotiation.rs 定义为 Pending、Accepted(version)、Rejected(NegotiationFailure)；本地 NegotiationFailure 表达 UnsupportedVersion、UnexpectedResponseId、InvalidInitializeResult，共享状态用于保留 receive 关闭连接前的真实错误。
- InitializeClient 实现现有 McpClient。ToolRequest<'a> 借用 peer/handler/进度与调用关联上下文，call(&self, request: McpToolRequest) -> Result<McpToolResult, McpError>，取代 MRTR driver 中普通调用部分。

- [x] **1.1 建立失败测试。** negotiation.rs 中增加 defaults_to_latest_supported_version、accepts_each_supported_server_version、rejects_version_before_initialized、rejects_mismatched_response_id、expired_http_session_does_not_reinitialize。四版本响应、显式 Preferred、2026-07-28、未知版本、畸形结果分别采集完整消息顺序；请求失败不得产生 initialized/tools/list，也不得重新发 initialize。

~~~rust
assert_eq!(McpProtocolPolicy::default().requested_version(), McpProtocolVersion::V2025_11_25);
assert!("2026-07-28".parse::<McpProtocolPolicy>().is_err());
assert_eq!(initialize_count, 1);
assert!(!initialized_sent);
~~~

- [x] **1.2 运行失败验证。** 执行 rtk cargo test -p mcp --test negotiation 和 rtk cargo test -p config --test mcp。确认失败源于未提供接口或精确协议旧行为，而非依赖/网络故障。
- [x] **1.3 原子完成协议/配置迁移。** 在一个可编译变更组中更新四版本类型、Auto/Preferred、状态投影和所有旧枚举消费者。旧 mrtr 两字段显式报迁移错误；新总超时默认 300 秒，继续保留非零值校验。更新 claw.toml 的 MCP 示例字段和 README 协议示例，不更改有效 provider/凭据内容。
- [x] **1.4 实现握手保护。** NegotiationTransport 只拦截对应 initialize 的响应、校验 ID/版本并保存结果；普通消息交给原 transport。HTTP 显式 reinit_on_expired_session(false)，后续 header 使用实际版本。错误关闭连接，不能让 SDK 提前 initialized；不调用 rmcp Discover/Auto。
- [x] **1.5 合并客户端并保留正常生命周期。** 移出普通请求的进度、活动/总时限、取消、关联信息，再删除 Modern subscription/MRTR/Task driver。按实际协商版本限制 Host 能力；旧结构化结果照常读取。重连前清除该连接的实际版本，重新协商后再发布。
- [x] **1.6 更新真实 transport 参考入口。** 统一 ReferenceServer::new(version: McpProtocolVersion)；reference_stdio 接收一个可选版本参数，reference_http 接收地址和可选版本参数，默认 2025-11-25。测试配置允许选择四个版本；HTTP 用 initialize/session 模式，stdout 保持纯协议。
- [x] **1.7 通过本阶段验证。** 运行以下命令，预期目标测试均通过、workspace check 无错误。审查生产调用链没有 Modern 连接入口；不能要求拒绝测试和历史文档中的 2026-07-28 字样消失。

~~~sh
rtk cargo test -p protocol --test mcp
rtk cargo test -p config --test mcp
rtk cargo test -p mcp --test negotiation --test lifecycle --test requests --test config --test host --test oauth --test tool --test dynamic_catalog --test session_lifecycle
rtk cargo test -p kernel --test mcp_host
rtk cargo check --workspace --all-targets
~~~

**自审门槛：** F1 有记录级断言；拒绝发生在 initialized 之前；404 不绕过保护；取消/关闭没有因删除 Modern 代码而丢失。

### 阶段 2：稳定名称、宿主元数据与 exposure 配置

**改动文件：** Cargo.toml、crates/protocol/Cargo.toml、crates/protocol/src/{tool,lib}.rs、mcp/{identity,catalog}.rs、extension/events/tool.rs、crates/tools/src/contract.rs、crates/config/src/mcp.rs、crates/mcp/src/{config,tool,session}.rs、client/catalog.rs、crates/kernel/src/runtime/tools/batch.rs。

**测试文件：** crates/protocol/tests/{mcp,tools,extensions}.rs、crates/config/tests/mcp.rs、crates/mcp/tests/{tool,dynamic_catalog}.rs、crates/tools/tests/{contract,registry}.rs、crates/kernel/tests/extensions_host.rs。

**接口产出：**

- McpToolRef::public_name(&self) -> String，算法严格使用全局表。CatalogMapper<'a> 借用 RuntimeMcpServer，map(...) -> Result<McpCatalog, McpError>，在发布前检查重复身份和名称碰撞。
- ToolExposure::{Direct, Deferred, Hidden}；ToolOutputPolicy::{Preserve, Bounded}；ToolAnnotations 四字段为 Option<bool>。
- ToolMetadata 拥有 exposure、annotations、output_schema、output_policy；可增加 mcp_ref: Option<McpToolRef> 表达目录来源，供搜索读取原始身份，不能解析 public name 代替它。
- AgentTool::metadata(&self) -> &ToolMetadata，默认实现用 OnceLock 缓存一份经 ToolMetadata::builder().build() 构造的默认值。这里将 spec 未钉死的返回方式具体化为借用，避免每次查询复制 output schema；MCP adapter 缓存自己的元数据，不用违反 builder 要求的多字段 const 字面量。
- RuntimeMcpServer::tool_exposure(&self, remote_name: &str) -> ToolExposure；tool_call 事件取得本轮工具元数据，未知工具为 None。事件需要拥有值时才转换，不在每个搜索候选上深拷贝 schema。

- [x] **2.1 增加失败测试。** names_survive_catalog_reordering 覆盖非法 Unicode、长度 64/65、长 server_id 和插入/移除无关项。rejects_generated_and_literal_name_collision 先生成一个摘要名，再构造与其相同的合法基础名；无需伪造 SHA 算法或加入生产测试钩子。另测同一远端身份重复出现。

增加 stable_name_digest_input_is_versioned：server_id=github、remote_name=list.issues 的固定预期为 mcp__github__list_issues_8b50abf9f3d3f1b5。该值在编写计划时用独立 Python hashlib 和 spec 的长度编码计算；测试不能调用生产命名函数生成预期值。

~~~rust
assert!(generated.len() <= 64);
assert!(generated.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'));
assert_eq!(before_reorder, after_reorder);
assert!(catalog_result.is_err());
~~~

- [x] **2.2 增加元数据与配置断言。** annotations_none_is_not_false、output_schema_reaches_tool_call_hook、exact_remote_name_override_wins 检查 None/Some(false)、原始 schema、默认 Direct 和逐工具精确覆盖；远端提示不能改变 exposure 或批准结果。
- [x] **2.3 运行目标测试确认旧行为失败。** 使用本阶段末尾相同命令，确认实际执行上述用例。
- [x] **2.4 实现名称与映射。** 从根 workspace 显式声明已有 sha2，protocol 继承。无上下文名称生成归属 McpToolRef；需要 server 配置的 wire 映射归属 CatalogMapper<'a>。metadata 字段从原始输入移入 adapter，避免保存重复的大 schema 副本。
- [x] **2.5 贯通默认接口与 hook。** 普通工具继续借用默认元数据，不批量添加重复实现；本阶段只建立策略表达，完整可见集合约束在阶段 5 接入，禁止把这一中间状态作为可发布 Hidden 功能。
- [x] **2.6 运行验证并检查唯一性。** 初始目录冲突形成该服务器失败；动态刷新冲突保留既有最后成功目录，不能因后来的名字覆盖另一工具。

~~~sh
rtk cargo test -p protocol --test mcp --test tools --test extensions
rtk cargo test -p config --test mcp
rtk cargo test -p tools --test contract --test registry
rtk cargo test -p mcp --test tool --test dynamic_catalog
rtk cargo test -p kernel --test extensions_host
rtk cargo check --workspace --all-targets
~~~

**自审门槛：** F2 已覆盖；所有跨外部 crate 的转换合法；普通工具不承担 MCP 元数据填充；名称与路由身份始终分离。

### 阶段 3：借用式输出投影与完整文件保存

**改动文件：** 新增 crates/tools/src/output.rs、crates/tools/tests/output.rs；修改 crates/tools/src/lib.rs、必要的 Cargo 继承，以及 crates/protocol/src/tool.rs、crates/protocol/tests/tools.rs。复用 crates/tools/src/builtin/output.rs 的文件生命周期经验，不重构已有 Bash 累加器。

**接口产出：**

- ToolOutputInfo：original_text_bytes、visible_text_bytes、omitted_media、full_output_path: Option<String>、save_failure: Option<ToolOutputSaveFailure>；使用 typed-builder。
- ToolOutputSaveFailure 首版使用 Io、TooLarge、Cancelled 三种类型化原因；不得把完整原始结果放入诊断。
- 本阶段先为 ToolResult 增加 output: Option<ToolOutputInfo>，默认 None；阶段 4 再扩展历史 MessageContent，不让本阶段投影器依赖尚未定义的结果字段。
- ToolContentText<'a> 借用 ContentBlock，TryFrom<&'a ContentBlock> 拒绝应走原生映射的 Image/ToolCall；write_to<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> 为预算与 Provider 复用的唯一文本投影。
- ToolResultProjection<'a> 借用 &mut ToolResult、ToolOutputPolicy、&CancellationToken、&Path 输出目录，apply(self) -> Result<(), ToolError>。文件错误写入 output 而非伪造远端调用失败；Preserve 不改变现有结果。

- [x] **3.1 编写失败用例。** aggregate_text_budget_includes_structured_and_links、utf8_preview_keeps_valid_boundaries、media_budget_preserves_whole_blocks、small_result_creates_no_file、oversized_spool_is_not_published。使用大小恰好等于上限和超过一字节的输入，并验证完整文件是投影前规范化结果。

~~~rust
assert!(visible_text_bytes <= 20 * 1024);
assert!(retained_media_count <= 4);
assert!(retained_encoded_media_bytes <= 8 * 1024 * 1024);
assert_eq!(small_result.output, None);
assert!(oversized_output.full_output_path.is_none());
~~~

- [x] **3.2 运行 rtk cargo test -p tools --test output，确认缺失预算行为导致失败。** 记录 UTF-8、结构化内容和真实字节边界的失败，不能只比较内部计数器。
- [x] **3.3 实现共同文本视图和有界 writer。** 按 spec 的模型文本形状逐块计数，超限保留首尾；完整序列化直接写受限文件，不建立第二个完整 JSON String。小 Structured 保留类型，大 Structured 返回明确文本预览；不切 base64。
- [x] **3.4 实现文件发布与失败处理。** tempfile 私有文件名由库生成；写完、关闭后才 keep/发布。测试使用不存在的输出目录或受限 writer 模拟失败，不依赖 sudo、磁盘写满或生产测试字段。超过 64 MiB 的文件不发布，部分文件清理。
- [x] **3.5 增加错误保持测试并通过验证。** spool_io_failure_preserves_remote_status 同时覆盖原 is_error=true/false；取消清理文件，已成功的远端调用不会被改写成应重试的执行失败。

~~~sh
rtk cargo test -p tools --test output --test truncation --test bash
rtk cargo test -p protocol --test tools
rtk cargo check --workspace --all-targets
~~~

**自审门槛：** 投影结构真实借用输入；上限含提示和路径；没有第二份完整结果；文本、媒体和文件上限没有混为一个计量单位。

### 阶段 4：在 Kernel 结算处应用预算并贯通历史

**改动文件：** crates/protocol/src/message.rs；crates/kernel/src/runtime/tools/batch.rs、runtime/session/replay.rs、provider.rs、provider/content.rs；crates/mcp/src/tool.rs 的 progress 投影；crates/acp/src/mapping.rs。同步所有 ToolResult/MessageContent::ToolResult 构造与解构位置。

**测试文件：** 新增 crates/kernel/tests/tool_output.rs；修改 crates/store/tests/mcp_content.rs、crates/acp/tests/mcp_content.rs、crates/protocol/tests/messages.rs、crates/mcp/tests/tool.rs。

**消费接口：** 阶段 2 的借用元数据和阶段 3 的 ToolResultProjection/ToolContentText。

**产出边界：** ToolResult.output 与 MessageContent::ToolResult.output 使用相同 Option<ToolOutputInfo>，缺省兼容旧 JSONL；正常完成结果在 tool_result hook 后投影一次，然后才发布结束事件和持久化。阶段 6 再添加分页结果的专用合法性策略。

- [x] **4.1 新增失败测试。** bounds_hook_expanded_result 让扩展把小结果改为 30 KiB 文本加 Structured；同时检查存储消息、结束事件和下一模型请求的实际文本。spool_failure_does_not_repeat_remote_call 使用带计数的 MCP 测试服务，断言远端计数始终为 1。Kernel 使用系统临时目录时，在 tests/ 内启动当前测试可执行文件的隔离子进程，将其 TMPDIR/TMP/TEMP 指向不存在的目录制造落盘错误；不修改并行测试所在父进程环境，不增加生产测试钩子。

~~~rust
assert_eq!(remote_call_count, 1);
assert_eq!(final_result.is_error, original_remote_is_error);
assert!(next_request_tool_text_bytes <= 20 * 1024);
assert_eq!(stored_output, emitted_output);
~~~

- [x] **4.2 执行目标测试确认旧行为失败。** rtk cargo test -p kernel --test tool_output；现有 batch 未做最终投影，测试必须捕获历史和 Provider 仍过大的结果。
- [x] **4.3 接入最后结算边界。** 查本轮冻结的真实工具元数据，而非按名称前缀判断。Blocked、未知工具、未执行错误保留原语义；不让 output 取代 details。无预算策略直接返回现有结果。
- [x] **4.4 安排 CPU/文件工作。** 需要完整序列化/落盘时，把拥有的结果移动到 blocking 工作中，再在内部构造借用投影；不强转 'static，不复制大结果。取消后等待工作停止并完成临时文件清理，不能只丢 JoinHandle。
- [x] **4.5 复用 Provider 文本实现并传播字段。** 将原 kernel/provider/content.rs 的必要纯文本逻辑接到 ToolContentText，保持原生媒体分支。Store 和 ACP 的正常结束、历史回放两条路径均保留 output。新增 progress_message_is_bounded_in_text_and_structured_views，确保 progress 的显示文本与 Structured.message 合计满足 1 KiB，不能只截显示文本却保留完整结构化 message；保持数值进度和关联 ID。
- [x] **4.6 运行结果与回放回归。** 新旧 JSONL、错误结果、Edit/Blocked 诊断、纯媒体、小结果和文件失效均需覆盖。

~~~sh
rtk cargo test -p kernel --test tool_output --test turn --test provider_runtime
rtk cargo test -p store --test mcp_content
rtk cargo test -p acp --test mcp_content --test mapping
rtk cargo test -p protocol --test messages
rtk cargo test -p mcp --test tool
rtk cargo check --workspace --all-targets
~~~

**自审门槛：** F3 通过真实调用次数和请求字节验证；预算发生在 hook 之后；最终事件和历史一致；没有把文件故障提升为可重跑的远端错误。

### 阶段 5：工具选择状态、唯一提交入口和分支恢复

**改动文件：** crates/kernel/src/runtime/tools/{state,mod}.rs；新增 selection.rs；修改 runtime/{session,tree,transcript}.rs、extension/{host,context}.rs、runtime/mcp.rs；公共选择类型放 crates/protocol/src/tool.rs，沿用 store::EntryKind::ActiveToolsChange。

**测试文件：** state.rs 内精确的 tests 模块；新增 crates/kernel/tests/tool_selection.rs，扩展 crates/kernel/tests/session_extension_state.rs、session_capabilities.rs、compaction.rs。

**接口产出：**

- ToolSelection：loaded/disabled 两个互斥 BTreeSet<String>；版本化持久化外壳包含 version=1，未知版本报错。
- SessionToolSnapshot::active_registry(&self) -> Result<ToolRegistry, ToolError> 保持调用接口，内部按 spec 公式派生，不再维护可独立改写的 active 真相。
- ToolSelectionUpdate<'a>::replace(&self, observed: &SessionToolSnapshot, names: Vec<String>) -> Result<(), KernelError>；load(&self, names: &[String]) -> Result<ToolLoadReport, KernelError>。
- ToolLoadReport 明确 loaded、already_loaded 和 skipped（名称及类型化原因），供阶段 7 展示；load 在最终候选校验时执行 32 KiB 本次新增定义预算。
- ToolSelectionUpdate::restore(entries: &[store::SessionEntry], catalog: &SessionToolSnapshot) -> Result<ToolSelection, KernelError> 是 Kernel 选择重放对象的关联方法，不让 protocol 中的 ToolSelection 依赖 store。

- [x] **5.1 写状态失败测试。** deferred_is_not_activated_by_catalog_refresh、explicit_disable_survives_reconnect、hidden_cannot_be_selected、replace_and_load_have_different_semantics。验证不可用偏好保留，完整选择不影响观察快照之后才出现的工具。

~~~rust
assert!(!declared_names.contains(&deferred_name));
assert!(selection.disabled.contains(&disabled_name));
assert!(selection.loaded.is_disjoint(&selection.disabled));
assert_eq!(loaded_after_two_updates, expected_union);
~~~

- [x] **5.2 写并发与持久化失败测试。** concurrent_loads_merge、failed_selection_sync_does_not_publish、catalog_refresh_does_not_overwrite_selection。通过 tests 中实现既有 SessionStore trait 的失败存储和屏障安排交错；不得添加生产专用注入开关。
- [x] **5.3 跑失败用例。** rtk cargo test -p kernel --test tool_selection；rtk cargo test -p kernel --lib runtime::tools::state::tests。确认当前“非 disabled 自动激活”及多入口写状态的问题被测试捕获。
- [x] **5.4 实现统一更新。** 选择写入串行化；验证当前候选、计算集合、追加并同步 Store、发布新选择构成一个不含 await/网络调用的临界操作。冻结本次 catalog 版本直到提交结束，目录更新随后重新派生；锁序统一为选择更新锁 → 工具状态锁 → Transcript 锁，审查不存在反向嵌套。不得获取 Run gate。
- [x] **5.5 迁移恢复格式。** 保存 selection 与兼容 activeTools；旧 entry 无 selection 时按 spec 规则解释。用阶段 2 的目录旧名/新名唯一映射迁移偏好，未知名称保留，不解析工具名拼路由，不重写历史。
- [x] **5.6 接入所有恢复入口。** 测试 resume_restores_selection、fork_uses_target_branch_selection、tree_navigation_restores_selection_before_event、empty_root_restores_defaults、compaction_keeps_selection。导航提交后、事件发布和下一请求前应用目标分支选择；摘要不承担配置存储。
- [x] **5.7 通过本阶段回归。** 选择写入失败不返回成功报告；两个并发 load 不互相丢失，目标分支外的 loaded 不泄漏。

~~~sh
rtk cargo test -p kernel --test tool_selection --test session_extension_state --test session_capabilities --test compaction
rtk cargo test -p kernel --lib runtime::tools::state::tests
rtk cargo check --workspace --all-targets
~~~

**自审门槛：** F4 的存储和并发部分已覆盖；没有新工具状态数据库；目录、选择和连接可用性是不同概念；锁顺序和事件发布顺序可解释。

### 阶段 6：模型资源工具、分页与取消

**改动文件：** 新增 crates/mcp/src/resource.rs、crates/mcp/tests/resource_tools.rs；修改 crates/protocol/src/mcp/request.rs、mod.rs、crates/mcp/src/{lib,session}.rs、client/{mod,core}.rs、server/mod.rs、crates/kernel/src/runtime.rs、crates/acp/src/extension.rs。为分页保护补充 tools/output.rs 与公共输出策略。

**消费接口：** 阶段 1 的普通请求控制、阶段 3/4 的预算、阶段 5 的有效选择。

**接口产出：**

- McpResourceOperation::{ListResources, ListTemplates, Read}；McpResourceTool 实现 AgentTool，三个固定名称由 enum 关联方法返回。
- ResourceListArguments/ResourceReadArguments 实现 TryFrom<&ToolCall>。McpResourceCursor 包含 version、session_id、catalog_revision、kind、server、next_index，支持校验后的编码/解码。
- ResourcePage<'a> 借用快照和过滤/分页参数，实现 Serialize；result(&self, call_id: ToolCallId) -> Result<ToolResult, ToolError> 返回完整、预算内的分页 JSON。
- McpSession::tool_registry(self: &Arc<Self>) -> Result<ToolRegistry, McpError> 可构造持有该 Session 的资源工具；Session 不反向持有 registry。
- McpSession/McpServerRuntime/McpClient 的 read_resource 路径增加 &CancellationToken 参数；ClientCore 使用实际可取消 request handle。Kernel/ACP 调用者也传自己的操作取消上下文。
- ToolOutputPolicy 增加 ResourcePage(kind)，用于 hook 后分页封包验证；失败返回有界错误，不能截断成功页或保留 nextCursor。

- [x] **6.1 新增失败测试。** lists_snapshot_with_count_and_byte_limits、rejects_cursor_context_mismatch、rejects_stale_catalog_cursor、oversized_single_resource_is_an_error、reads_unlisted_template_uri_through_server。分别覆盖默认 50、接受 1/200、拒绝 0/201/小数、20 KiB 体积和不会停滞的游标。

~~~rust
assert!(page_text.len() <= 20 * 1024);
assert!(serde_json::from_str::<serde_json::Value>(&page_text).is_ok());
assert!(foreign_cursor_result.is_err());
assert_eq!(server_received_uri, requested_uri);
~~~

- [x] **6.2 跑 rtk cargo test -p mcp --test resource_tools 确认资源工具缺失。** 同时增加 Kernel hook 拒绝资源调用、隐藏服务器不可读和取消实际到达请求的用例。
- [x] **6.3 实现操作对象与分页。** ResourcePage 只借用完整快照，最多复制/序列化当前页；资源按 server_id/URI、模板按 server_id/uri_template 排序。cursor 绑定当前上下文，URI 不裁剪；无可容纳单条记录时返回明确错误。
- [x] **6.4 接通真实读取与取消。** URI 交给指定服务器，不按 file/http scheme 在宿主执行；不要求 URI 在列表里。链接提示携带 server 和 read_mcp_resource 用法，且只为允许访问的服务器生成。
- [x] **6.5 检查策略闭环。** 三个工具默认 Direct，但尊重显式禁用；服务器 Hidden 不因开放个别远端工具而开放资源。资源结果走普通 hook 和预算；分页经 hook 修改后超量/非法时返回错误，不返回损坏 JSON。
- [x] **6.6 执行目标与调用者回归。** 资源取消的 API 变动必须修复全部实现和 ACP 调用者；快照分页不能伪装为远端 cursor 请求。

~~~sh
rtk cargo test -p mcp --test resource_tools --test host --test lifecycle --test session_lifecycle
rtk cargo test -p kernel --test mcp_host --test tool_output --test tool_selection
rtk cargo test -p acp --test extension --test mcp_content
rtk cargo test -p tools --test output
rtk cargo check --workspace --all-targets
~~~

**自审门槛：** F5 完整覆盖；只有一个资源工具实现和一条 Session 连接；新读取参数没有仅停留在最外层 select；模板/链接读取不被错误白名单拦截。

### 阶段 7：有界索引、tool_search 与下一请求加载

**改动文件：** 新增 crates/kernel/src/runtime/tools/search.rs、crates/kernel/tests/tool_search.rs；修改 runtime/tools/{mod,state}.rs、runtime/{session,prompt,mcp}.rs。纯排序测试写在 search.rs 的精确 tests 模块，集成行为通过正式 Kernel 接口观察。

**消费接口：** metadata()、阶段 5 的 ToolSelectionUpdate::load/ToolLoadReport、本轮不可变 registry。

**接口产出：**

- ToolSearchDocument 在 search.rs 定义，包含有界检索文本、public name、可选原始来源和预计算词项；借用 mapper 生成文档时从 metadata.mcp_ref 获取来源，不切 public name。
- impl FromIterator<ToolSearchDocument> for ToolSearchIndex；索引拥有有界数据和语料统计，避免持有指向自身 Vec 的引用。
- ToolSearchArguments 实现 TryFrom<&ToolCall>；ToolSearch<'a>::ranked(&self) -> Vec<ToolSearchMatch> 只检索，ToolSearchMatch 在同模块定义为名称与排序信息。
- ToolSearchTool 实现 AgentTool，使用 Weak<Session>；execute 先 ranked，再统一 load，并按实际 ToolLoadReport 生成简短结果。ToolPromptContribution 描述下一次请求才可用。

- [x] **7.1 写纯检索失败测试。** exact_name_beats_description、cjk_bigram_query_matches、ties_use_public_name、schema_depth_and_document_bytes_are_bounded、query_and_limit_are_validated。验证 CJK 二元词项、单字、camelCase、Unicode 小写、256/257 字符和 limit 1/20/21 边界。
- [x] **7.2 写模型请求捕获测试。** deferred_is_declared_only_after_search、same_batch_search_does_not_authorize_sibling_call、search_never_calls_remote_tool、search_revalidates_changed_catalog、selection_only_change_reuses_index。多轮测试用 tests/support 中的可控 Model，直接记录每次 ModelRequest.tools。

~~~rust
assert!(!first_request_names.contains(&target_name));
assert!(second_request_names.contains(&target_name));
assert_eq!(remote_calls_during_search, 0);
assert!(Arc::ptr_eq(&index_before_selection, &index_after_selection));
~~~

- [x] **7.3 跑失败验证。** rtk cargo test -p kernel --test tool_search；rtk cargo test -p kernel --lib runtime::tools::search::tests。确认接口缺失与首次请求暴露全部 MCP 工具被用例捕获。
- [x] **7.4 实现一次构建、多次借用查询。** 用固定 BM25 参数和确定性 tie-break，不添加可替换 ranker trait。索引限制文本/深度，完整 schema 保留在工具目录；只有 catalog 变化才重建索引。
- [x] **7.5 注册并接入提交。** 存在允许搜索的 Deferred 工具时默认声明 tool_search，自身不 deferred；显式禁用仍有效。将查询结果交给阶段 5 的 load，在提交时重新验证目录和 32 KiB 新增定义预算；绝不通过完整 set_active_tools 间接禁用其他候选。返回短说明，不返回 schema，输出策略复用 Bounded，避免服务器长描述扩大搜索结果。
- [x] **7.6 通过下一请求与恢复验收。** 已加载匹配可返回 alreadyLoaded；Hidden 不进入结果；超大 schema 不截断。复用阶段 5 测试加入“真实 tool_search 后”的 resume/fork/navigation/compaction 场景，保持当前批次冻结语义。

~~~sh
rtk cargo test -p kernel --test tool_search --test tool_selection --test session_extension_state --test compaction --test turn
rtk cargo test -p kernel --lib runtime::tools::search::tests
rtk cargo check --workspace --all-targets
~~~

**自审门槛：** F4 的搜索与目录变化部分通过；没有远端检索请求、schema 深拷贝热点或 Session 引用环；零结果/全部已加载不制造无意义的重复选择 entry。

### 阶段 8：ACP/WebUI 权威投影与使用说明

**改动文件：** crates/protocol/src/mcp/status.rs；crates/kernel/src/runtime/mcp.rs、extension/context.rs；crates/acp/src/{mapping,extension}.rs；web/src/domain/model.ts、workspace/{updateDecoder,updateRouter,controller}.ts、features/mcp/{McpPanel,McpServerCard,McpCatalog}.tsx、features/conversation/ToolCallCard.tsx；README.md、README_zh.md、claw.toml。

**测试文件：** 修改 crates/acp/tests/{mcp_content,extension,mapping}.rs、crates/app/tests/web.rs，阶段 5/7 的集成测试增加快照变化断言。前端不新增单元测试要求。

**接口产出：** 已有 MCP Session 快照扩展为包含配置策略、实际版本、有效 exposure 与 loaded 状态的服务端投影；既有 MCP update/status 通路同时响应目录和选择变化。后端以同一版快照生成 UI 数据，前端不重新派生选择规则。

- [x] **8.1 先补后端契约测试。** snapshot_changes_after_tool_load、acp_output_diagnostics_survive_replay、unknown_negotiated_protocol_is_not_displayed_as_connected 检查加载/重连/历史回放和 Option 字段。保证 MCP catalog 的未变化内容不因选择通知复制成另一份状态真相。
- [x] **8.2 更新快照与解码。** 消费 configuredPolicy/negotiatedProtocol、title、元数据、exposure、loaded 和 output；历史缺字段使用明确缺省，不把 null 当有效版本。readOnly/destructive 信息标注为服务器声明。
- [x] **8.3 更新界面。** 展示实际协商值、握手中、错误和重连；工具卡展示元数据及加载状态；消息展示截断、完整路径或保存失败。禁止把任意远端路径做成执行入口，不新增下载服务。
- [x] **8.4 更新说明。** 写明 Auto/Preferred、四版本、2026/MRTR 字段迁移、Direct 默认、精确名称覆盖、搜索下一轮生效和资源工具；明确临时文件、快照分页及单次搜索预算的限制。英文 README 用英文，中文 README 用中文；历史 spec 不批量修改。
- [x] **8.5 运行已有前端入口与契约验证。** 当前 web/package.json 只有 check/build，没有 npm test 或 Playwright 配置；不安装测试框架来完成本阶段。

~~~sh
rtk cargo test -p acp --test mcp_content --test extension --test mapping
rtk cargo test -p app --test web
rtk cargo test -p kernel --test tool_search --test tool_selection
rtk npm --prefix web run check
rtk npm --prefix web run build
~~~

**自审门槛：** 前端没有第二套协议/搜索逻辑；加载和重连状态以服务端更新为准；新旧回放显示一致；所有旧 Modern 标签已从当前运行界面移除。

### 阶段 9：完整静态检查、transport 互操作和真实 WebUI 验收

**改动范围：** 仅为发现的回归修正所属阶段文件；按需扩展 crates/mcp/examples/reference_server/mod.rs 的可运行演示工具/资源，使其能返回大文本、Structured 和模板 URI，不把测试专用字段放入生产 crate。验收记录保存到 docs/superpowers/reports/2026-10-27-mcp-tool-discovery-verification.md，该文件只在真实执行后创建。

- [x] **9.1 检查实现范围和组织。** 逐阶段对照 spec 及第 4 节覆盖矩阵；确认无新 crate、万能 Context、无意义转发 helper、非法 From、无必要的大 clone、跨 await 的 std 锁。检查所有新增函数英文注释、builder、Arc::clone、依赖继承与日志。
- [x] **9.2 完整检查一次。** 以下命令均退出 0 才通过；失败修复后重跑相关检查，全部通过后不无理由重复全量测试。

~~~sh
rtk cargo fmt --all -- --check
rtk cargo check --workspace --all-targets
rtk cargo clippy --workspace --all-targets -- -D warnings
rtk cargo test --workspace --all-targets
rtk npm --prefix web run check
rtk npm --prefix web run build
~~~

- [x] **9.3 显式运行真实 transport 测试。** 使用阶段 1 的 interop_stdio/interop_http；两个目标保留 ignore，以免把启动真实监听器隐藏进普通测试。每个真实用例内部参数化四版本，并覆盖普通工具、Prompt、Resource、Completion。

~~~sh
rtk cargo test -p mcp --test interop_stdio -- --ignored --nocapture
rtk cargo test -p mcp --test interop_http -- --ignored --nocapture
~~~

- [x] **9.4 准备独立验收配置。** 在 /tmp/clawcode-mcp-e2e.toml 中引用实际 provider/model 和现有凭据环境变量，加入 stdio/HTTP 参考服务器、一个 Deferred 工具组和一个 Hidden 工具。不得改用户默认配置，不在记录中输出密钥。构建可运行参考例子和 app：

~~~sh
rtk cargo build -p mcp --examples
rtk cargo build -p app --bin clawcode
~~~

- [x] **9.5 启动生产 app 和参考服务。** HTTP 参考服务器使用 127.0.0.1:39128、版本 2025-06-18；stdio 子进程由 MCP 配置启动 reference_stdio 并传 2024-11-05。端口被占用时选择空闲 loopback 端口并同步临时配置，不终止无关进程。分别保留并最终关闭自己启动的运行会话。

~~~sh
rtk cargo run -p mcp --example reference_http -- 127.0.0.1:39128 2025-06-18
rtk proxy env CLAW_CONFIG=/tmp/clawcode-mcp-e2e.toml rtk cargo run -p app --bin clawcode -- serve --web-root web/dist
~~~

- [ ] **9.6 浏览器验收正式页面。** 用可用浏览器工具打开正式 app，验证：①实际协商为各服务器所选旧版本；②模型搜索并在下一轮调用命中工具；③大文本/Structured 截断并显示文件信息；④列资源、分页、读模板 URI；⑤Hidden 不出现在模型发现结果；⑥加载状态在 reconnect、resume、fork、tree 和空根导航后正确；⑦已有 OAuth/Elicitation 页面和取消流程未回归。用后端事件/请求捕获佐证“下一轮”，不能只凭模型自然语言说已加载。
- [x] **9.7 完成错误和清理验收。** 停止自己启动的 app/MCP 子进程，检查无 watcher/进程残留；保留应保留的输出样本，不以关闭 Session 为由删除完整文件。若真实凭据或交互授权不可用，将对应项标记未验证，不使用 fixture app 或模型代替通过。
- [x] **9.8 写验证记录。** 记录版本基线、通过/失败/跳过的命令、实际协商值、关键 UI 证据、未验证范围和四项既有限制；敏感值脱敏。最终 git status 只应包含本任务的预期改动，不 stage、不 commit、不 push。

**完成门槛：** 所有自动检查通过；两种真实 transport 四版本互操作完成；生产 app/真实 provider 的核心流程通过。任何未完成项必须列明，不能把模拟模型集成测试当作真实 WebUI 验收。

## 4. Spec 到阶段的覆盖矩阵

| Spec 部分 | 承担阶段 |
|---|---|
| §1–2 目标、范围、旧设计替代 | 全局约束、阶段 1、8、9 |
| §3.1–3.4 版本、Auto/Preferred、握手保护、HTTP | 阶段 1；F1；阶段 9 真实 transport |
| §3.5 Modern/MRTR/Task 退役、Host 能力、配置迁移 | 阶段 1、8 |
| §4–5 docparse 风格、trait/借用、crate 职责 | 对象表与所有阶段自审，阶段 9 总审 |
| §6 名称算法、碰撞、旧偏好迁移 | 阶段 2；F2；阶段 5 |
| §7 元数据与有效策略 | 阶段 2、5、8 |
| §8 预算、媒体、文件、错误、进度与历史 | 阶段 3、4；F3；阶段 8 |
| §9 模型资源、快照分页、URI、取消 | 阶段 6；F5；阶段 9 |
| §10 选择公式、完整替换与增量加载 | 阶段 5 |
| §11 索引、排序、边界、下一请求 | 阶段 7；F4 |
| §12 持久化、并发、全部分支恢复 | 阶段 5、7 |
| §13 ACP/WebUI、历史兼容 | 阶段 1 的最小字段、阶段 4、8 |
| §14 文件边界 | 第 2 节文件表及各阶段文件清单 |
| §15 验收与日志 | 各阶段测试与阶段 9 |
| §16 已知限制 | 阶段 8 的说明与阶段 9 的验证记录 |

## 5. 规划交付时的历史状态

最初交付只编写并自审本计划，检查引用路径、命令归属、数值和 spec 覆盖；当时尚未实施，所有步骤保持未勾选。原 spec 此后仍未修改。

文档检查通过：9 个阶段、56 个未完成步骤、48 条未来 shell 命令；测试目标均对应已有文件或本计划明确要求新增的目标，前端脚本与当前 package.json 一致。原 spec 的 SHA-256 未变化。上述检查仅验证计划本身，不代表任何实现测试已经通过。

以上是规划交付时的记录。收到后续实施授权后，已按本计划执行；当前状态以复选框、执行记录和验证记录为准。

## 6. 实施结果与验收边界

- [执行记录](../reports/2026-10-27-mcp-tool-discovery-progress.md)记录了接口适配、测试复用和自审修复。
- [验证记录](../reports/2026-10-27-mcp-tool-discovery-verification.md)包含最终命令结果、正式 app/真实 provider 的证据、截图与清理结果。
- 9.6 的协商、搜索、结果预算、资源、恢复与分支流程已验证；真实第三方人工 OAuth/URL elicitation 以及 WebUI 停止按钮未专测，按 9.7 如实记录，未将模拟测试冒充通过。
- 未 stage、commit、push；没有使用 SubAgent 或 worktree。
