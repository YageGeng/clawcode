# MCP 工具发现、结果预算与 Pi 协议协商设计

日期：2026-10-27。

状态：设计草案，供用户审阅；本文件不代表实现已经完成或验证通过。

## 1. 目标与依据

本设计完善现有 MCP 的工具名称、结果预算、元数据、模型资源工具和 deferred/tool_search，并按照用户最新要求，把协议版本支持范围及自动协商行为与本地 Pi 对齐。

代码组织参考 docparse：让有明确职责的 struct 持有或借用所需上下文，通过关联方法组织操作，优先用 From、TryFrom、FromIterator 等标准 trait 表达转换。遵守本仓库的注释、typed-builder、依赖、日志和测试规则。

本次核对基线：

| 仓库 | 本地提交 | 用途 |
|---|---|---|
| clawcode | d14d189 | 现有 Kernel、MCP、Store、ACP 和 WebUI |
| pi | db14038bd | pi-mcp 版本集合、initialize 协商、工具发现参考 |
| docparse | fd41de7 | 借用上下文、关联方法和标准 trait 的组织方式 |

主要源码依据：

- [Pi 支持的协议版本](../../../../pi/packages/mcp/src/protocol/types.ts)、[Pi 初始化实现](../../../../pi/packages/mcp/src/client.ts)、[Pi 协商测试](../../../../pi/packages/mcp/test/client.test.ts)。
- [Pi HTTP transport](../../../../pi/packages/mcp/src/transports/streamable-http.ts)、[工具适配](../../../../pi/packages/coding-agent/src/extensions/mcp/tools.ts)、[工具搜索](../../../../pi/packages/coding-agent/src/extensions/tool-search/tool.ts)。
- [docparse OcrMerger](../../../../docparse/crates/core/src/ocr/merge.rs)：借用配置与页面事实，在对象上完成合并。
- [docparse TextIndex](../../../../docparse/crates/core/src/ocr/index.rs)：FromIterator 构建借用索引，关联方法查询。
- [docparse JSON 视图](../../../../docparse/crates/core/src/render/json.rs)：借用原始数据，通过 Serialize 投影，避免复制完整对象。
- [docparse ResultIndex](../../../../docparse/crates/server/src/service/result_files.rs)：TryFrom 完成有失败可能的解析与一致性校验。

这些引用用于说明已经核对的代码模式，不要求 MCP 复制 docparse 的领域类型。

## 2. 范围与旧设计的替代关系

本次包含六组能力：

1. 与 Pi 相同的协议版本集合及 initialize 自动协商。
2. 稳定、可路由、符合模型工具名称约束的 public name。
3. 保留 MCP 元数据并提供宿主侧工具策略。
4. 在结果进入历史前执行统一预算，保留完整输出的独立文件。
5. 模型可调用的三个 MCP 资源工具。
6. direct/deferred/hidden、tool_search、选择状态持久化及分支恢复。

本设计替代以下历史约束：

- [2026-08-16 MCP Host 设计](2026-08-16-mcp-host-design.md)中的两个精确协议、必填 protocol、禁止协商、Modern discovery/MRTR/Tasks 路径，以及对应 UI 标签和验收标准。
- [2026-08-17 生命周期可靠性设计](2026-08-17-mcp-lifecycle-reliability-design.md)中的 Modern subscription 专属部分及“新工具一律自动激活”的选择规则。

其他已实现行为继续保留：Session 级连接所有权、故障隔离、完整目录快照、显式重连、取消与关闭、OAuth 凭据边界、现有 Prompt/Resource/Completion 和支持版本内的 Host 回调。

本次不引入 codemode、subagent、动态插件安装、配置热更新、远端目录懒加载、向量检索、通用附件服务或后台任务运行时。模型资源分页基于已有快照，不改变当前启动时的完整目录发现策略。

## 3. 协议支持范围与自动协商

### 3.1 已核实的 Pi 行为

Pi 的默认请求版本为 2025-11-25，接受集合严格为：

| 版本 | 默认请求 | 可接受的服务端选择 |
|---|---|---|
| 2025-11-25 | 是 | 是 |
| 2025-06-18 | 否 | 是 |
| 2025-03-26 | 否 | 是 |
| 2024-11-05 | 否 | 是 |

Pi 发送一次 initialize，检查响应版本是否属于上述集合，然后更新 transport 的版本并发送 notifications/initialized。Pi 不依次尝试四个版本，不先调用 server/discover，也不因为版本不支持而反复初始化。它的测试明确覆盖接受 2024-11-05、拒绝未知版本并关闭连接。

Pi 接受部分较新内容形状，例如非对象 structuredContent，不代表它支持以 2026-07-28 建立协议会话。必须区分内容解析宽容度与协议版本集合。

本项目采用相同的单次 initialize 版本协商；这一流程也符合 [MCP 初始化与版本协商规范](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle)。

### 3.2 类型与配置

McpProtocolVersion 只表达上述四个可协商版本，TryFrom/FromStr 拒绝其他值。默认请求版本和支持集合只能定义一次，由关联方法返回，配置、握手和测试不得维护各自的副本。

配置策略为 McpProtocolPolicy：

- Auto：省略 protocol 或配置为 auto；请求 2025-11-25。
- Preferred(version)：显式指定上述四个版本之一；以它作为 initialize 请求版本，仍接受集合中的其他服务端选择。

不增加 Strict/Exact 模式。显式旧版本是请求偏好，不是禁止协商的钉死版本；这与 Pi SDK 的 protocolVersion 选项及集合校验语义一致。

旧的 protocol = 2025-11-25 可继续解析，但语义由“必须相等”变为“请求偏好”。protocol = 2026-07-28 必须在配置阶段报错，错误明确说明删除该字段或改为 auto；禁止静默降级后连接。

状态中分开表达 configuredPolicy 和 negotiatedProtocol。后者在首次握手前为空，成功校验后记录实际版本；连接失败不能展示一个伪造的已协商版本。

### 3.3 握手顺序与不变量

~~~text
建立配置指定的 transport
  → initialize(requestedVersion, clientInfo, implementedCapabilities)
  → 验证响应 ID、响应形状和 selectedVersion
  → 将实际版本设置到连接/HTTP transport
  → notifications/initialized
  → 按能力发现目录
  → 发布 Ready 与完整快照
~~~

约束：

- 未知版本、2026-07-28、畸形响应或超时均关闭该连接，不发布目录。
- 版本未通过校验时不能发送 initialized，不能发送 tools/list 等业务请求。
- HTTP 后续请求使用实际协商版本的 MCP-Protocol-Version header。
- 协商过程不改变配置指定的 transport。继续仅支持 stdio 和 Streamable HTTP，不因选择 2024-11-05 而自动切到旧 HTTP+SSE。
- 旧协议服务器如果只提供旧 HTTP+SSE，仍然无法通过本项目连接；版本兼容不等于新增 transport 兼容。
- 每次显式 reconnect、resume 或 fork 的新连接重新协商，不能复用旧连接的 negotiatedProtocol。
- 每个服务器独立协商和失败；其他服务器继续运行。
- initialize 超时/取消关闭启动过程，不向 initialize 发送 notifications/cancelled。

### 3.4 rmcp 接入约束

继续使用现有 rmcp 3，不自行实现完整 JSON-RPC 客户端，不调用其 Discover/Auto 生命周期。rmcp 的 Auto 是 discovery 与 initialize 的另一套选择策略，不等于 Pi 此处的 initialize 版本协商。

本次检查到本机 rmcp 3.1.4 的两个具体行为：

1. service/client.rs 的 run_initialize 在保存服务端信息后直接发送 initialized；当前项目 ClientCore::from_running 的版本检查发生在此之后。
2. Streamable HTTP 默认允许在 session-expired 404 后内部重新 initialize，且新响应不再交给外层 handler。

因此，只删除 ClientCore 中的版本相等判断不满足要求。

实现采用一个窄职责 NegotiationTransport<T>，包装现有 Transport<RoleClient>：记录 initialize 请求 ID，在匹配响应交给 rmcp 前校验版本，并持有类型化的协商结果或失败原因。无关消息原样转发；错误路径关闭底层 transport，并把真实协商错误带回连接构造层。它不得重写普通 RPC、认证、超时或业务请求逻辑。

HTTP transport 显式关闭 reinit_on_expired_session，防止内部重新握手绕过上述校验。保持现有宿主显式重连路径；HTTP 404 的自动重连与请求重试不属于本轮版本协商要求，不能将二者混为一谈。实现验收必须检查实际 SDK 版本中的行为，不能只依据 API 名称。

### 3.5 Modern 路径及能力迁移

2026-07-28 不再是可连接版本。合并当前 Legacy/Modern wrapper 为一个基于 initialize 的客户端实现，删除仅为 Modern 存在的 discovery、unified subscription、MRTR input-required 和当前 Modern Task driver 生产路径，保留普通请求的进度、超时、取消与正常结果转换。

2025-11-25 本身存在 Tasks 扩展，不能在文档中写成“旧协议没有 Tasks”。本项目当前 Modern Task API 不能冒充该扩展；本轮不另外实现 2025-11-25 Tasks，实际客户端不声明或启用它，与 Pi 当前集成的能力边界相符。

Roots、Sampling、Prompt、Resource、Completion 和已有 Elicitation 在所协商版本及双方能力允许时继续使用。Elicitation 表单适用于 2025-06-18 及之后，URL 模式适用于 2025-11-25；旧版本或未声明的模式返回明确的不支持错误。握手声明以请求版本可表达的能力为准，运行期按实际协商版本再次限制，不能用一次新版本能力声明绕过降级后的约束。URL/Form 的区别以 [Elicitation 规范](https://modelcontextprotocol.io/specification/2025-11-25/client/elicitation)为准。

配置去掉 mrtr_max_rounds。mrtr_total_timeout_sec 迁移为 request_total_timeout_sec，继续给普通工具调用设置不能被 progress 延长的总时限，默认 300 秒；request_timeout_sec 保留为活动超时。旧字段必须产生定向迁移错误，不得被反序列化静默忽略。

历史工具消息及结构化 Task 展示数据保持可读取，不重写历史 JSONL。仅为读取历史必需的旧事件解码可以保留，但不能重新开放 Modern 连接入口。

## 4. 代码组织与 clean code 约束

### 4.1 上下文对象

每个操作对象只携带自己需要的数据，不传入整个 Application/Kernel 充当服务定位器，也不为省参数把所有运行时依赖收进一个万能 Context。

短操作优先使用带生命周期的借用视图，例如 CatalogMapper<'a>、ToolResultProjection<'a>、ResourcePage<'a>、ToolSearch<'a>。它们借用配置、快照和输入，通过关联方法完成一次连贯操作；不复制完整目录、schema 或结果，只为解决借用问题而增加 Arc/Mutex 是不允许的。

跨异步任务、注册工具和后台 watcher 才持有必要的 Arc。所有克隆使用 Arc::clone；禁止为满足 'static 人为延长借用生命周期或泄漏内存。需要 spawn_blocking 的工作显式转移必要的拥有值。

### 4.2 标准 trait

| 场景 | 表达方式 |
|---|---|
| 无失败、且转换一侧属于当前 crate 的类型映射 | From |
| 原始配置到 RuntimeMcpServer/预算策略 | TryFrom |
| 工具参数、cursor、版本字符串的校验 | TryFrom 或 FromStr |
| 已发布目录条目构建检索索引 | FromIterator，或在构造确实可能失败时使用具名关联构造方法 |
| 借用列表/结果的序列化投影 | Serialize |
| 有 I/O、取消、状态变化的流程 | 关联 async 方法，不伪装成 From/TryFrom |

转换必须保留所需上下文。远端 Tool 的命名需要 server_id 时，使用带 server 上下文的 CatalogMapper 方法或明确的输入类型，不能依赖隐式全局状态。

必须遵守 Rust orphan rule：在 mcp 中，rmcp 的 wire 类型和 protocol 的领域类型都不是本 crate 定义的类型，不能直接为这两者实现标准 From/TryFrom。此类映射放在已有 CatalogMapper 等本地上下文对象的关联方法上；不要为了强行使用 trait 让 protocol 依赖 rmcp，也不要增加没有独立意义的包装类型。

### 4.3 类型与模块的规模

- 保留现有 crate 边界，不增加搜索、预算或上下文专用 crate。
- 同一公共概念只在 protocol 定义一次；其他 crate 使用它，不创建同义 DTO。
- 一个 enum 可表达三种资源操作，无需三个复制的工具类。
- 不为每个步骤增加 Service、Manager、Factory 或只有一个转发方法的 struct；上下文 struct 必须确实拥有一组相关行为或借用投影。
- 新增函数必须有英文函数级注释；修改旧逻辑用英文解释变动原因。文档与后续 plan 使用中文。
- 超过三个字段的 struct 使用 typed-builder，Option 字段默认值与 strip_option 遵守 AGENTS.md。
- 新依赖只在根 workspace.dependencies 声明，子 crate 继承，表格保持语义分组与排序。
- 不创建 commit，不使用 SubAgent，不创建 worktree，除非用户之后明确修改对应要求。

## 5. 职责与类型归属

| 层 | 职责 |
|---|---|
| protocol | 版本与配置策略的公共类型、工具注解/元数据、结果预算诊断、资源分页和选择状态的公共投影 |
| config | 原始配置解析、旧字段迁移错误、exposure 和预算数值验证 |
| mcp | 协商、原始身份到 public name、能力目录映射、MCP AgentTool 和资源工具、连接取消 |
| tools | AgentTool 默认元数据接口、公共结果投影和完整文件输出，不依赖 MCP transport |
| kernel | 每轮可见工具快照、检索索引、tool_search、选择状态提交/恢复、结果最终投影 |
| store | 复用 JSONL entry 与现有追加/同步语义，不增加独立工具状态数据库 |
| acp/web | 投影实际协议、工具策略、加载状态和结果诊断，不在前端重做搜索或 MCP 连接 |

保留 ToolDefinition 的 name、description、parameters 三个 Provider 字段。AgentTool 增加具有默认实现的 metadata()，返回 ToolMetadata；普通工具无需重复实现默认值。

ToolMetadata 至少包含 exposure、annotations、output_schema、output_policy。默认值为 Direct、未声明注解、无 output schema、保留现有输出策略。服务器注解不能设置宿主的 exposure 或 output_policy。

## 6. 稳定工具名称

远端身份继续使用 McpToolRef { server_id, remote_name }。public name 仅是模型可调用的名称，不用于字符串拆分路由。

规则：

1. 基础名为 mcp__<server_id>__<remote_name>。
2. 字符只允许 ASCII 字母、数字、下划线和连字符，长度最多 64 字节。
3. 基础名合法且未超长时保留原值。
4. 需要归一化或截断时，将基础名中每个非法 Unicode 标量替换为一个下划线，保留前 47 个 ASCII 字节，再加下划线和 SHA-256 的前 16 个小写十六进制字符；不合并连续下划线。
5. 摘要输入依次为 ASCII 前缀 clawcode-mcp-tool-v1 和一个零字节、server_id UTF-8 字节长度的 8 字节大端整数、server_id 字节、remote_name 字节长度的 8 字节大端整数、remote_name 字节。不对截断后的文本求摘要，不使用随机数、目录位置或 DefaultHasher。
6. 目录发布前检查最终名称与原始身份的一一映射。碰撞、重复远端身份形成类型化目录错误，不静默覆盖，也不以添加排序编号修补。

同名冲突检查在每次完整 catalog 发布前执行；刷新失败沿用既有最后成功快照策略。生成函数只依赖该工具身份，其他工具出现、消失和重排不能修改它的名称。

优先保留 McpToolRef 的关联生成方法，避免单独堆积命名 helper。复用 lockfile 中已有 sha2 实现，并在 workspace 显式声明直接依赖，不自写摘要算法。

搜索索引同时包含原始名称和 public name；UI 主展示可读名称及服务器，必要时展示完整 public name。

旧会话选择恢复时，通过当前目录逐项构造旧基础名到新名的映射；只能使用唯一匹配，不解析显示字符串猜测身份。不能解析的旧项保留为暂不可用偏好。只使用新名声明未来工具；历史消息保持原文。

## 7. 元数据与策略

McpToolInfo 增加 title、output_schema 和保留四个可选布尔字段的 ToolAnnotations：read_only、destructive、idempotent、open_world。字段缺失必须保持 None，不能变成 false。

CatalogMapper 保留这些信息，McpAgentTool::metadata 返回公共元数据。tool_call 扩展事件同时获得本次所选工具的元数据，未知工具为 None；扩展不必反向解析名字获取注解。UI 与 ACP 接收同一公共投影。

output_schema 首版用于保存、检查和展示，不新增严格输出校验、自动修复或外部 $ref 下载。annotations 仅为来源明确的提示，不自动批准调用，不将 destructive=false 解释为安全保证。

本地 exposure 只允许 Direct、Deferred、Hidden。配置默认 Direct；逐工具覆盖只支持原始 remote_name 的精确键，不支持 glob、正则或隐式优先级。

~~~toml
[[mcp_servers]]
name = "github"
protocol = "auto"
url = "https://example.com/mcp"
request_timeout_sec = 120
request_total_timeout_sec = 300
exposure = "deferred"

[mcp_servers.tool_exposure]
get_me = "direct"
delete_repository = "hidden"
~~~

规则由 RuntimeMcpServer 的关联方法解析，命中精确键时覆盖服务器默认值。服务器 Hidden 但显式开放个别工具时，只开放这些工具；不会因此开放该服务器的全部资源。

## 8. 统一结果预算

### 8.1 执行位置

~~~text
AgentTool 执行
  → tool_result 扩展处理
  → ToolResultProjection 按冻结的 output_policy 投影
  → ToolExecutionEnd、Message 持久化
  → ACP/WebUI 与后续模型请求
~~~

MCP 调用及资源读取返回正常领域内容，Kernel 在扩展处理之后调用公共投影。被策略阻止的结果保留其 Blocked 诊断；未知工具和未执行调用不凭名字套用远端结果策略。

ToolResult 增加可选 output 诊断字段，MessageContent::ToolResult 使用相同类型并带 serde 默认值。它独立于现有 details，不能用新的截断信息覆盖 Edit、Blocked 或扩展诊断。旧消息缺少 output 时正常读取。

### 8.2 首版固定上限

本轮使用内部集中常量，不增加一组用户可配置参数：

| 项目 | 上限/规则 |
|---|---|
| 模型可见文本 | 每个结果合计 20 KiB，包含说明、URI、文件位置与结构化文本投影 |
| 内联媒体数量 | 每个结果最多 4 个完整媒体块 |
| 内联媒体编码数据 | 每个结果合计最多 8 MiB，按实际传递的编码字节计算 |
| 完整输出文件 | 每个结果最多 64 MiB 序列化字节；超限不发布冒充完整的文件 |
| progress 消息文本 | 每个快照最多 1 KiB，不为 progress 创建完整输出文件 |

这些是确定的初始产品策略，不是 token 或图像计费保证。调整数值需要同步对应验收，不能让前后端各有一套值。

### 8.3 内容投影

- 文本、EmbeddedResource 文本、ResourceLink 描述和 Structured 的模型文本共享一次总预算。
- 计数与 Provider 文本投影复用同一实现；不能先按紧凑 JSON 计数，后在 Provider 里展开成超预算的 pretty JSON。将必要的纯投影逻辑移入 tools 的借用视图，不重复实现。
- 大文本保留首尾，标记中间省略的字节量；所有切割必须是 UTF-8 边界。
- 小 Structured 保持合法 JSON；过大时改为明确标识的文本预览和文件引用，不返回被截坏的 Structured。
- 文本预算、媒体预算分别计算；媒体只能完整保留或整体转为省略说明，不能切 base64。
- 完整输出保存投影前的规范化结果，包括原内容顺序和 structured content，不宣称保存了所有原始 wire 元数据。
- 使用借用 Serialize 视图和有界 writer 计数/写文件，避免先构造第二个完整 JSON 字符串。不要为了投影克隆大结果或整份 schema。
- 小结果只走快速路径，不生成文件。

### 8.4 完整输出与失败语义

复用现有保留临时输出文件的模式，用 tempfile 创建私有、唯一文件；名称不能直接由远端 URI、工具名或调用参数决定。仅成功写完并关闭后发布路径；超限、取消和 I/O 错误删除未完成临时文件。

output 记录文本前后大小、省略媒体数量、可选完整路径和类型化保存失败原因；不保存另一份完整正文。保存失败或文件超限仍返回有限预览，保留远端 is_error，不能重跑远端工具或把本地落盘失败描述成远端操作失败。

完整文件沿用当前临时输出保留语义，不承诺永久可用，不因 Session close 就删除。fork 继续引用原路径；过期或外部清理必须显示为文件不可用，不能自动重新执行原调用。首版不增加文件 GC、附件索引或模型专用文件读取接口。

当 read/shell 被禁用时，不绕过该选择读取完整文件，模型只能使用预览，用户仍可获知文件位置。现有 read 的超长单行限制继续适用；不得在提示里无条件保证 offset/limit 可以读取任意 JSON 文件。

此预算限制的是最终模型输入和保留的文件体积，不是网络传输、JSON 解码或已分配原始结果的内存上限。不得以此宣称端到端背压已实现。

## 9. 模型资源工具

增加三个 AgentTool 名称：

| 名称 | 参数 | 行为 |
|---|---|---|
| list_mcp_resources | server?、cursor?、limit? | 列出允许服务器的资源快照页 |
| list_mcp_resource_templates | server?、cursor?、limit? | 列出资源模板快照页 |
| read_mcp_resource | server、uri | 经当前 Session 的指定服务器读取 URI |

McpResourceTool 持有 Arc<McpSession> 和操作枚举，共用生命周期和结果策略。McpSession 本身不拥有返回给 Kernel 的工具 registry，因此不得引入回指 registry 的强引用；如果所有权以后改变，需重新检查引用环。

三种操作分别用 TryFrom<&ToolCall> 校验参数，不提供可执行任意 RPC 的 method 参数。使用既有 MCP Session、Server 和 ClientCore，不创建第二条连接。

资源工具只在至少一个允许访问的服务器声明资源能力时出现，默认 Direct，同时尊重显式工具禁用。服务器级 Hidden 阻止该服务器通过资源工具暴露；单个远端工具 Hidden 不隐式改变资源策略。资源读取仍经过正常 tool_call hook，不依赖 readOnly 注解跳过策略。

### 9.1 快照分页

- 默认 50 项，limit 范围 1..=200；页面按条目数和最终 20 KiB 文本预算同时截取。
- 用 ResourcePage<'a> 借用一份不可变快照完成排序、过滤和 Serialize，不为每页克隆整份目录。
- 资源按 server_id、URI 排序；模板按 server_id、uri_template 排序。
- cursor 包含格式版本、Session 标识、catalog revision、列表种类、服务器过滤条件和下一位置。它是经过校验的游标，不是授权凭据。
- cursor 与请求上下文不符、目录 revision 已变化、位置越界时返回明确错误，调用方重新开始。
- nextCursor 必须位于合法完整 JSON 中并计入预算；不能对完成的分页 JSON 直接截字符串导致游标损坏。
- 分页结果由共用预算计数器预先选页；若 tool_result 扩展替换后的分页封包非法或超预算，返回有界的分页结果错误，不把破损 JSON 当作成功页返回，也不提交下一页游标。
- 单条记录无法容纳时，返回该位置的可诊断超大条目错误，不能返回不前进的空页。不得截断实际 URI。

首版仍以启动时已经完整发现的 catalog 为来源；不把这些本地 cursor 传给远端服务器，不宣称已经实现远端按需分页。

### 9.2 读取、取消与内容

server 必须解析为当前 Session 中允许访问且可调用的服务器。URI 原样交给服务器，不在宿主依据 scheme 自行访问 HTTP 或文件。URI 不要求出现在 catalog 中，以支持 ResourceLink 和模板生成的地址。

取消从 ToolExecutionContext 贯穿 McpSession、McpServerRuntime 和 ClientCore，使用 rmcp 可取消请求控制，不能只丢弃等待 future。现有 ACP 读取调用也使用同一读取路径并提供其操作取消上下文。

读取返回的文本、嵌入媒体与二进制内容统一使用第 8 节预算。ResourceLink 的文本投影标明 server、原 URI 和 read_mcp_resource 的用法，只有允许访问的服务器才提供该读取指引。

## 10. 工具可见性与选择状态

保留 builtins、extensions、mcp 三个 registry 分区和每轮不可变快照。策略与选择是不同维度：

| 概念 | 含义 |
|---|---|
| available | 当前已发布目录中存在的工具 |
| exposure | Direct、Deferred 或 Hidden，本地有效策略 |
| loaded | 用户/扩展或搜索明确加载的工具偏好 |
| disabled | 用户/扩展明确禁用的工具偏好 |

模型可见集合按以下规则派生，不再独立维护一个可被任意改写的 active 真相：

~~~text
declared = available ∩ not_hidden ∩ not_disabled ∩ (direct ∪ loaded)
~~~

loaded 和 disabled 互斥；缺席目录的偏好仍保留。连接实际不可用时，执行由既有 Server 生命周期拒绝或取消，不能用偏好冒充连接可用性。

目录刷新只替换目录并重新派生可见集合，不覆盖选择：新 Direct 默认声明，新 Deferred 仅进入可搜索目录，Hidden 不声明、不搜索、不执行。暂时消失再出现的工具恢复此前偏好。

set_active_tools 继续表达显式完整选择：对它观察到的可见候选，选中项载入并移除禁用，未选中项禁用；并发后来出现的项保留其默认或已有策略。不能选择 Hidden。

搜索使用独立的“增量加载”操作，只合并命中项，不调用完整 set_active_tools 来间接禁用其他 Deferred 工具。

## 11. tool_search

### 11.1 注册与依赖

ToolSearchTool 在 Kernel 中实现普通 AgentTool，关联当前 Session 的 Weak 引用，防止 Session → registry → ToolSearchTool → Session 引用环。只持有提交选择所必需的长生命周期依赖；短操作通过借用对象取得具体快照、索引、Store 和策略。

存在允许搜索的 Deferred 工具时默认声明 tool_search；显式禁用仍然有效。它本身不是 Deferred。系统提示通过现有 ToolPromptContribution 指导先搜索再调用，不注入全部 Deferred 名称或 schema。

参数为 query 和可选 limit：query trim 后非空且不超过 256 个 Unicode 标量值，limit 默认 8，允许 1..=20，拒绝小数和非法类型。

### 11.2 索引与排序

ToolSearchIndex 保存当前目录的轻量检索数据，不保存第二份执行 registry；每个文档可引用 Arc 元数据，但不得构造自引用借用结构。ToolSearch<'a> 借用索引、有效选择和查询完成本次检索。

索引字段为 public name、原始名称、服务器名、描述和输入参数名/说明。不要索引完整输出 schema、工具结果、认证配置或隐藏服务器的秘密信息。

采用确定性的 BM25，k1=1.2、b=0.75。完整名称或原始名称精确匹配优先于普通词项评分，同分按 public name 排序。分词拆分标点、下划线和 camelCase，使用 Unicode 小写；CJK 连续文本使用相邻二元词项，单字保留，避免 ASCII 分词使中文查询失效。查询和文档共用同一关联方法，不建立搜索插件 trait。

每个工具检索文档最多保留 16 KiB 文本，参数 schema 遍历深度最多 8；原始 schema 仍完整保存在目录中。目录更改时构建新索引并随快照发布；只变更 loaded/disabled 时复用 Arc 索引，避免每次查询重新分词整个目录。

候选只包含 Deferred 且未显式禁用的可用工具。已 loaded 的匹配可返回 alreadyLoaded，Hidden 永不泄露名称或说明。检索不执行任何远端工具。

### 11.3 加载与模型调用

按排名选择 limit 内的候选，并限制本次新增模型定义的序列化体积合计不超过 32 KiB。单个定义超过该值的工具不自动加载，返回简短的定义过大原因；不截坏 schema。返回加载名称、服务器、短说明及是否已加载，不在工具结果中重复发送完整 schema。

这是一轮新增定义预算，不是长会话累计声明的硬上限；首版不自动 LRU 淘汰工具。显式工具选择、回到更早分支或新 Session 可改变加载集合。

加载经统一选择提交路径完成，结果以实际提交的项目为准。提交失败返回错误，不能声称已加载。查询期间目录变化时，提交阶段重新核对命中项；已撤下或变成 Hidden/disabled 的项不新增加载，并说明未加载原因。

当前模型请求及同批工具调用继续使用既有冻结 registry。新加载工具只出现在下一次模型请求中；即使同批同时包含搜索和未声明工具调用，也不能因执行完成顺序不同而偶然执行成功。

## 12. 持久化、并发与分支

### 12.1 统一提交

提取 Session 级选择提交行为，扩展 set_active_tools 和 ToolSearchTool 使用同一路径。借用的 ToolSelectionUpdate<'a> 只引用工具状态、Transcript、时钟和 ID 生成器等必要依赖，通过关联方法完成验证、持久化和发布。

用一个选择变更串行化边界保护读取当前选择、计算变更、追加并同步 Store、发布新选择。不得再次获取正在执行的 Session Run gate，避免在工具调用内自锁；不得持有 std 锁跨 await 或网络操作。

目录刷新独立更新 catalog；发布选择时根据最新目录派生可见集合，不能覆盖并发目录。两个并发搜索提交增量集合并集，不能各自拿旧 active 列表覆盖对方。

同步失败不能发布成功状态或成功搜索结果；已产生的 Store I/O 失败按现有运行时错误路径处理，不能假装完成了一次可继续写入的事务。

### 12.2 存储格式

继续使用 ActiveToolsChange，保留 activeTools 兼容投影，并增加版本化 selection：

~~~json
{
  "activeTools": ["read", "tool_search", "mcp__github__get_issue"],
  "selection": {
    "version": 1,
    "loaded": ["mcp__github__get_issue"],
    "disabled": []
  }
}
~~~

新读取器以 selection 为真相；activeTools 只是该次提交时的可见名称投影。缺少 selection 的旧 entry 按旧完整选择语义解释：已知目录中的未选项作为显式禁用，旧 activeTools 中暂不可用项保留为 loaded；无选择 entry 的新 Session 使用当前默认 exposure。

### 12.3 恢复范围

- resume：沿当前分支重放最近选择，结合当前配置和目录派生可见工具。
- fork：只继承 fork 点所在分支的选择，不复制来源 Session 在其他分支的运行时状态。
- tree navigate，包括导航到空根：提交 lane 移动后，在发出 SessionTree 事件和创建下一模型请求前应用目标分支选择。
- compaction：不把选择藏在摘要文本中，继续沿完整分支读取 ActiveToolsChange；生成摘要不重置加载集合。
- reconnect：目录暂时撤下不清除 loaded/disabled，重新连接后按实际目录和本地策略恢复。

当前 tree.rs 只重建历史的路径必须接入工具选择恢复，这是本次需求的一部分。只测试 close/resume 不足以证明分支语义正确。

## 13. ACP、WebUI 与兼容边界

协议状态使用配置策略与实际版本，展示例如“自动协商 · 2025-06-18 · stdio”；握手前显示协商中。删除根据“不是 2025-11-25 就是 Modern”的前端分支，以及“不自动回退”的旧文案。

MCP 面板展示 title/public name、有效 exposure、是否已加载、annotations 和 output schema；来源是服务端快照，不在浏览器推导第二套选择状态。目录与选择变化均需要发布足以更新面板的快照投影。

消息渲染识别 output 诊断，展示截断、完整文件位置或保存失败原因。完整文件不是远端可点击任意路径执行入口；本轮不新增文件下载服务器。

现有 OAuth、Prompt、Resource、Completion 与 reconnect 页面继续使用现有 ACP 通路。新增模型资源工具也必须经过相同底层能力，不在前端实现 MCP RPC。

所有时间、TurnId、ToolCallId 和 trace 关联继续复用已有公共类型；不增加独立消息索引。JSONL 新增字段使用默认反序列化，历史正常回放。

## 14. 预期文件边界

以下是实现归属，不是逐文件执行计划。可以在相邻现有模块合并相关类型，但不得把不同生命周期的行为集中到一个大文件。

| 位置 | 预期变化 |
|---|---|
| crates/protocol/src/mcp/identity.rs、status.rs、catalog.rs | 四版本、配置策略/实际版本、稳定名称、元数据 |
| crates/protocol/src/tool.rs、message.rs、extension/events/tool.rs | 公共工具元数据、输出诊断、扩展事件信息 |
| crates/config/src/mcp.rs；crates/mcp/src/config.rs | 自动协商与 exposure、旧字段迁移、总超时 |
| crates/mcp/src/client/negotiation.rs | 窄职责握手验证 transport 与协议策略应用 |
| crates/mcp/src/client、transport.rs | 合并 initialize 客户端，删除 Modern 专属路径，HTTP 头及取消 |
| crates/mcp/src/client/catalog.rs、tool.rs、session.rs | 上下文映射、工具元数据、资源工具注册 |
| crates/mcp/src/resource.rs | 操作枚举、参数转换、ResourcePage 借用视图及读取 |
| crates/tools/src/contract.rs、output.rs | 默认 metadata 接口、结果投影与受限完整输出文件 |
| crates/kernel/src/runtime/tools/state.rs、search.rs、batch.rs | 可见集合、索引/搜索、统一最终结果预算 |
| crates/kernel/src/runtime/session.rs、tree.rs、extension/host.rs | 选择持久化与完整分支恢复 |
| crates/acp/src；web/src/domain、features/mcp、conversation | 状态和结果投影、协议文案 |
| claw.toml、README.md、README_zh.md、相关示例/测试 | 新配置和四版本协商示例；移除 Modern 示例 |

本 spec 作为新的设计依据；历史 spec 不批量重写。实现时清理旧类型、注释和测试名称，但不能把仍用于历史消息解码的公共类型误删。

## 15. 验收标准

### 15.1 协议与生命周期

- stdio 和 Streamable HTTP 分别覆盖四种服务端版本响应；默认请求都是 2025-11-25，显式偏好覆盖另测。
- 接受支持集合内不同于请求值的响应；拒绝 2026-07-28、未知版本、畸形结果和错误响应 ID。
- 非法协商不发送 initialized、不发现目录；每次新连接仅发送一次 initialize，不进行逐版本探测。
- HTTP initialized、后续 POST/GET 使用实际版本；禁止内部 404 重新握手绕过校验。
- 握手超时、取消、认证失败、重连和 shutdown 无连接或子进程泄漏，单 Server 失败隔离。
- Roots/Sampling/Elicitation 的版本能力限制、Prompt/Resource/Completion 和旧 JSONL 回放回归通过。
- 配置缺省为 Auto；旧 Modern/MRTR 配置产生明确迁移错误；WebUI 不再错误标记 Modern。

### 15.2 工具名与元数据

- 合法旧名保持；特殊字符、超长名、同前缀名称、极长 server_id 正确处理。
- 目录排序、增加/撤下其他工具、重连不改变既有名称；摘要碰撞和重复身份不会静默覆盖。
- 调用按原始 McpToolRef 路由；旧选择可唯一迁移，歧义不猜测。
- title、annotations 的 None/false、output schema 经 catalog、AgentTool、扩展和 ACP/WebUI 保持。
- 远端 readOnly 注解不能改变宿主批准或 Hidden 策略。

### 15.3 结果预算

- 多文本块、Structured、EmbeddedResource 与 ResourceLink 合计预算正确，UTF-8 边界、单行巨大 JSON、零文本/纯媒体均覆盖。
- 小结构化内容保留类型；超大内容不生成非法 JSON 或损坏媒体；Provider 实际文本与计数一致。
- tool_result hook 扩大结果后仍有界，Blocked/Edit 等原诊断不丢失。
- 文件保存成功可读取，保存失败、达到 64 MiB、取消不留下冒充完整的文件。
- 保存失败不改变远端业务结果，不重复远端操作；progress 有界。
- Store/ACP/WebUI 的 output 字段往返正确，旧消息无字段正常读取。

### 15.4 资源工具

- 多服务器过滤、禁用/Hidden、没有资源能力、模板 URI 和 ResourceLink URI 均正确。
- 游标版本变化、跨 Session/种类/过滤条件误用、越界和超大单条记录有明确结果。
- 分页 JSON 完整、游标可继续、输出满足预算；读取取消到达实际请求。
- 不把远端 file/http URI 在宿主直接执行，不通过资源入口暴露 Hidden 服务器。

### 15.5 搜索与持久化

- 首次模型请求不含未加载 Deferred schema；搜索后下一请求才声明命中工具。
- 同批未声明调用不能被并发搜索放行；搜索本身不执行远端工具。
- 精确名称优先、中文查询、同分顺序、limit/查询边界、定义体积上限可复现。
- 两个并发搜索均保留结果；完整 set_active_tools 与增量加载语义不同且可验证。
- 目录刷新不意外激活 Deferred 或重新启用 disabled；撤下、恢复、Hidden 变更正确。
- resume、fork、树导航、空根导航、compaction 分别验证加载状态，不只覆盖重启。
- Store 写入/同步失败不能返回成功加载；仅选择变化不重新构建检索索引。

### 15.6 验证方式与日志

Rust 测试复用现有集成测试目录和可控 MCP 参考服务器；协议测试可使用进程/内存 transport，不修改生产代码加入测试专用字段。前端不要求单元测试或 TDD，WebUI 验收必须使用正式 app 后端与配置 provider。

运行相关 Rust 测试、fmt、clippy 和前端检查；最终真实模型验收至少走通“搜索 → 下一轮调用 → 预算结果”和“列资源 → 读取”两条流程，并展示实际协商版本。使用参考服务器通过测试不能替代真实 WebUI 验收。

日志仅记录协商选择、状态变化、目录发布失败、搜索加载提交失败、输出保存失败和取消等有意义边界；不记录完整查询正文、工具参数/结果、认证信息或每个分页项。按现有要求使用完整 tracing 路径和普通格式化消息。

## 16. 本次文档验证与已知限制

本次已核对三个仓库的实际源码、Pi 的版本协商测试定义，以及本机 rmcp 3.1.4 的初始化和 HTTP 自动重新初始化实现；并查阅上述官方生命周期与 Elicitation 规范。

文档检查已通过：12 个本地引用目标存在，TOML 配置和 JSON 选择记录示例可解析，16 个一级设计章节完整，代码围栏成对且没有占位标记或行尾空白。

Pi checkout 没有 node_modules/vitest，因此本次没有运行 Pi 测试，也没有为写 spec 安装依赖。尚未修改生产代码或执行 provider/MCP 真实联调。

需要在实现验收中明确保留的边界：临时完整输出不是永久附件；结果投影不是传输内存上限；快照分页不是远端懒加载；单次搜索定义预算不是长会话累计声明上限。不得在完成报告中把这些范围扩大为已实现的保证。
