# Agent WebUI 设计

## 1. 目标

为 clawcode 提供一个桌面优先的现代 Agent WebUI。WebUI 基于 ACP v2 WebSocket 与现有 Agent 后端通信，不直接调用 Kernel，也不引入第二套业务协议。

第一版定位为完整 Agent 工作台，覆盖以下能力：

- 会话创建、搜索、恢复、关闭和重命名。
- 用户消息、Assistant 流式输出、折叠推理和工具执行展示。
- 运行取消与 follow-up 消息队列。
- 会话树查看、Navigate、Branch、Fork 和自动 Compact。
- Skills 发现结果与显式调用。
- MCP 服务器状态和导出工具的只读查看。
- Turn 事件、TurnId、字符串毫秒时间戳和工具详情查看。
- WebSocket 断线重连、Session resume 和消息 replay。

WebUI 仅供本机单用户使用。服务默认监听 `127.0.0.1`，第一版不实现登录系统、权限系统或公网部署能力。

## 2. 已确认的产品约束

- 采用 React、TypeScript 和 Vite 构建独立 SPA。
- 使用冷白珍珠亮色体系，不提供暗色主题。
- 桌面优先；窄窗口自动收起辅助栏，不单独设计手机端工作流。
- 输入仅支持文本、Markdown 和资源链接，不支持图片或任意文件上传。
- 新建会话默认使用服务启动目录，并允许输入本机绝对路径。
- 推理默认折叠，可按消息展开。
- Agent 运行期间发送的新消息进入 follow-up 队列；停止操作只取消当前运行。
- Skills 和 MCP 面板只读；Skill 可以显式调用。
- 当前模型只读展示；模型和 MCP 配置继续由 TOML 管理，重启后生效。
- Compact 摘要由 Agent 自动生成，不要求用户手动填写。
- 会话标题默认取首次用户消息首行并截断，同时允许手动重命名。
- 前端不采用 TDD，不编写前端单元测试。
- 前端产物位于 `web/dist`，不提交构建产物；Rust 服务运行时读取该目录。
- 不提供 UI extension 接入点。

## 3. 页面布局与视觉规范

### 3.1 桌面布局

页面从左到右分为四个视觉区域：

1. 窄图标主导航：固定宽度，提供 Sessions、Skills、MCP 和 About 入口。
2. 会话栏：展示搜索、新建、按时间分组的会话列表，可独立收起。
3. 主对话区：展示会话标题、连接和运行状态、消息流及输入框。
4. 运行详情栏：提供 Tree、Events 和 Tools 三个页签，可独立收起。

主导航始终保留。窗口宽度不足时先收起运行详情栏，再收起会话栏；收起后的区域通过主导航或主对话区按钮重新打开。

### 3.2 亮色体系

- 页面底色使用低饱和蓝灰色，内容面板使用白色和接近白色的冷灰色。
- 主强调色使用靛蓝，成功和连接状态使用绿色，警告使用琥珀色，错误使用红色。
- 通过边框、背景明度和有限阴影建立层级，不使用大面积高饱和色块。
- 正文、代码、次要说明和禁用状态分别使用不同明度，满足常用文本的可读性对比。
- 设计 token 通过 CSS custom properties 集中定义，不在组件中散布产品颜色常量。

### 3.3 消息展示

- 用户消息右对齐，Assistant 消息左对齐。
- Assistant 正文支持 Markdown、表格、链接和代码块；代码块提供复制按钮。
- 推理显示为与正文分离的折叠区，标题展示状态和耗时，不默认展开内容。
- 工具调用以内联卡片展示名称、状态、耗时和摘要；展开后显示格式化输入、输出与错误。
- 每条消息的诊断区域可查看 TurnId、字符串毫秒时间戳和 MessageId，但这些信息不占据默认阅读层级。
- follow-up 队列显示在输入框上方，支持移除单条消息和清空队列。

## 4. 系统架构

### 4.1 部署拓扑

Rust `app` 进程同时提供：

- `web/dist` 静态文件与 SPA fallback。
- `GET /api/ui/bootstrap` 启动信息。
- `/acp` 上的 ACP POST、SSE 和 WebSocket transport。
- `/health` 健康检查。

生产环境下浏览器与 ACP transport 同源。官方 HTTP transport 会校验 WebSocket `Origin`，因此服务只允许由已验证 loopback 监听地址生成的唯一同源 origin，不开放任意或跨域来源。开发环境由 Vite 提供热更新，并代理 bootstrap 请求和 ACP WebSocket 到本地 Rust 服务。

### 4.2 边界原则

- 所有 Agent 行为都通过 ACP 标准方法或 ACP v2 扩展方法完成。
- bootstrap 端点只提供 UI 启动和显示所需的静态信息，不承载 Agent 操作。
- WebUI 不导入 Rust 领域代码，不直接访问 Store、Kernel、MCP 或 Skill 文件系统。
- WebUI 不提供客户端文件系统 callback 和 terminal callback。
- 产品名称、slug 和默认 ACP 路径由 Rust `ProductIdentity` 及服务端配置生成，前端不硬编码项目名。
- UI 状态与持久化 Session 状态分离；刷新浏览器后以服务端 Session replay 为准。

### 4.3 Bootstrap 响应

`GET /api/ui/bootstrap` 返回一个稳定的类型化 JSON 对象：

```json
{
  "product": {
    "name": "服务端 ProductIdentity 名称",
    "slug": "服务端 ProductIdentity slug"
  },
  "acpPath": "/acp",
  "defaultCwd": "/absolute/service/cwd",
  "activeModel": {
    "id": "provider/model",
    "displayName": "配置中的显示名称"
  }
}
```

字段值由 Rust 类型构造。响应不得在路由实现中重复产品名称字符串。

## 5. 前端模块

前端位于 `web/`，按领域组织，而不是按零散组件类型组织：

```text
web/src/
├── bootstrap/       # 启动配置读取与解析
├── acp/             # WebSocket、JSON-RPC、ACP 方法和扩展类型
├── domain/          # Session、Message、Turn、Tool、Tree、Queue 类型
├── workspace/       # 领域状态、reducer、selectors 和 orchestration
├── features/
│   ├── sessions/    # 会话列表、新建、搜索、重命名
│   ├── conversation/# 消息、推理、工具卡片、Markdown
│   ├── inspector/   # Tree、Events、Tools
│   ├── composer/    # 输入、follow-up、取消
│   ├── skills/      # Skill 列表、详情和显式调用
│   └── mcp/         # MCP 状态与工具列表
├── shell/           # 主导航和响应式面板编排
└── theme/           # CSS token、基础样式和组件外观
```

公共边界使用明确的 TypeScript discriminated union 和不可混用的标识类型。禁止使用 `any`。仅有一个调用点且没有独立领域含义的转换逻辑直接内联，避免大量短小 helper。

## 6. ACP 客户端与状态模型

### 6.1 连接状态

ACP 客户端使用可穷举联合类型表达状态：

- `disconnected`
- `connecting`
- `initializing`
- `ready`
- `reconnecting`
- `failed`

状态转换由一个连接对象负责。业务组件不直接操作原始 WebSocket，也不自行生成 JSON-RPC 请求 ID。

### 6.2 Workspace 状态

Workspace 以实体 ID 为索引保存：

- Session 列表和当前 Session。
- Message 实体及其顺序。
- Turn 实体和运行状态。
- ToolCall 实体。
- SessionTree 与当前节点。
- 带稳定 QueueId 的 follow-up 队列。
- Skills 列表。
- 当前 Session 的 MCP 状态与工具列表。
- 连接错误、协议错误和操作错误。

ACP 通知统一转换为领域 action，再由 reducer 更新状态。消息 delta 根据 MessageId 追加；完整消息、工具和 Turn 根据稳定 ID upsert。replay 或重连产生的重复完整消息不得重复插入列表。

每个 ACP 通知都读取 namespaced metadata 中的 TurnId、字符串毫秒时间戳和 sequence。时间戳在前端保持字符串，仅在格式化显示时安全转换；不得把原值永久转换成 JavaScript `number`。

### 6.3 初始化和会话恢复

启动流程固定为：

1. 读取 bootstrap。
2. 建立 ACP WebSocket。
3. 执行 ACP initialize 并校验 v2 及所需扩展方法。
4. 执行 session/list。
5. 用户选择现有 Session 时执行 session/resume，并从头 replay。
6. replay 完成后读取 SessionTree、follow-up 队列、Skills 和 MCP 状态。

WebSocket 意外断开后使用有上限的退避重连。连接恢复后重新 initialize、resume 和 replay。已收到明确成功响应的操作不得重复发送；结果未知的 prompt 不自动重发，界面要求用户确认。

## 7. ACP 能力调整

现有标准 ACP 方法保持不变。WebUI 继续使用现有 Tree、Navigate、Branch、Fork、PendingMessages、ClearQueue、InvokeSkill 和 ExtensionCommand 扩展。

需要在 `AcpExtensionMethod` 中补充并集中定义以下方法，禁止在前后端散布方法字符串：

- SessionRename：写入 Store 的 Session name fact。
- SkillList：返回已发现 Skill 的名称、描述、来源路径和冲突解析后的有效项。
- McpStatus：返回当前 Session 的 MCP server 状态和工具描述。
- PendingMessageRemove：按稳定 QueueId 删除一条尚未执行的 follow-up。

Compact 保留原扩展方法名称，但参数调整为只接收 SessionId。Kernel 负责调用当前配置模型生成摘要并持久化 compaction entry。项目不保留旧参数兼容层。

前端 ACP 扩展常量从一个模块导出。该模块中的默认命名空间由 bootstrap 或 initialize 结果构造，不在多个 feature 中重复项目名。

## 8. 关键交互

### 8.1 新建和命名 Session

新建弹窗默认填入 bootstrap 提供的绝对工作目录，允许用户修改。服务端仍负责验证目录存在且是目录。

首次用户消息成功持久化后，Kernel 使用首行规范化并截断为默认标题，写入 Store name fact。手动重命名通过 SessionRename 完成；空白标题和超长标题由服务端拒绝。

### 8.2 Prompt、流式输出和 follow-up

普通空闲 Session 使用标准 session/prompt。Prompt 请求收到响应后，UI 等待 Running、消息 delta 和 Idle 更新，不以请求响应代表运行结束。

Session 运行时再次发送内容使用 FollowUp 扩展。服务端为队列项生成稳定 QueueId，并在响应和 PendingMessages 结果中返回；队列项在服务端确认后展示，不使用只有浏览器知道的伪队列。移除单条消息使用 PendingMessageRemove，清空全部消息使用 ClearQueue。停止按钮发送标准 cancel notification，仅影响当前运行；已入队 follow-up 保留并在运行结算后处理。

### 8.3 Tree、Navigate、Branch 和 Fork

- Tree 页签展示主干、分支、消息节点和 compaction 节点。
- 点击节点只更新前端预览选择，不立即改变 Kernel 状态。
- Navigate 需要明确按钮确认，然后刷新 replay 和 Tree。
- Branch 从选中节点创建当前 Session 内的新活动分支。
- Fork 以选中节点为 leaf 创建新 Session，默认继承当前 cwd，完成后切换到新 Session。

### 8.4 自动 Compact

Compact 在 Session 串行锁内作为独立操作执行：

1. 记录 compaction operation 的开始时间和关联 TurnId。
2. 读取活动分支的可压缩上下文。
3. 调用当前配置模型生成结构化摘要。
4. 校验摘要非空并持久化 pi v4 compaction entry。
5. 更新活动上下文并发送包含 TurnId 和字符串毫秒时间戳的扩展事件。

模型失败、取消或持久化失败时不移动活动节点，不替换当前上下文。UI 显示具体失败阶段并允许重新触发。

### 8.5 Skills 和 MCP

Skills 页面只读展示有效 Skill。显式调用分为两个协议步骤：前端先调用 InvokeSkill，由 Kernel 解析名称并返回规范化后的 Skill 上下文；随后前端根据 Session 状态使用标准 session/prompt 或 FollowUp 提交该上下文与用户输入。前端不读取 `SKILL.md` 文件，也不缓存 Skill 全文。InvokeSkill 失败时不得继续提交 prompt。

MCP 页面只读展示配置的服务器、连接状态、错误和工具。由于配置不热更新，页面不提供新增、删除、启停或编辑表单。

## 9. 错误处理

- 连接级错误使用持续可见的顶部状态条，重连成功后自动收起。
- 请求级错误显示在发起操作附近，并保留可复制的技术详情。
- 工具失败保留在对应 ToolCall 卡片中，不转换成普通聊天错误。
- 模型失败、取消和协议解析失败使用不同状态，不合并成通用失败。
- 输入草稿在当前浏览器会话中保留；断线和切换右侧页签不得清空草稿。
- 服务端返回无法识别的 ACP 扩展更新时，Events 页签保留原始 JSON，并记录非阻断诊断。
- Events 页签展示当前浏览器连接收到的完整事件以及 resume replay 产生的消息事件；它不是独立审计日志，刷新后不承诺恢复未持久化的中间 delta。
- `web/dist` 缺失时，Rust 服务返回明确的 WebUI 未构建响应，同时 ACP 和 health endpoint 继续可用。
- bootstrap 失败或 initialize 能力不匹配时停止业务请求，显示可操作的错误说明。

## 10. 构建与依赖

- `web/package.json` 使用精确版本，不使用范围版本。
- 前端依赖安装不执行第三方 lifecycle script，除非依赖经过明确审查并确有需要。
- `npm run dev` 启动 Vite 开发服务并代理 Rust 后端。
- `npm run check` 执行 TypeScript 类型检查和 ESLint。
- `npm run build` 生成 `web/dist`。
- Rust `serve` 增加可覆盖的 Web root 参数，默认读取仓库或安装布局中的 `web/dist`。
- `web/dist` 不纳入版本控制。

## 11. 验证策略

前端不采用 TDD，不增加前端单元测试。前端质量门槛包括：

- TypeScript 严格类型检查。
- ESLint 零错误。
- Vite production build 成功。
- 使用真实 Rust 服务进行浏览器验收。
- 检查键盘导航、焦点可见性、亮色对比度、窄窗口折叠和浏览器控制台错误。

浏览器验收至少覆盖：

- 新建、恢复、关闭和重命名 Session。
- Assistant 流式文本、折叠推理和 ToolCall 状态。
- follow-up 入队、移除、清空和当前运行取消。
- Navigate、Branch、Fork 和自动 Compact。
- 断开 WebSocket 后重连、resume 和 replay 去重。
- Skills 列表及显式调用。
- MCP server 和工具只读状态。
- 服务端错误、工具错误、模型错误和未知扩展事件。

Rust 变更继续使用测试驱动，覆盖 bootstrap 类型与路由、SessionRename、SkillList、McpStatus、自动 Compact 事务语义及 ACP 映射。最终运行 Rust fmt、workspace Clippy 和 workspace 全量测试。

## 12. 非目标

第一版明确不包含：

- UI extension 或第三方前端插件机制。
- 客户端文件系统 callback、terminal callback 或浏览器终端。
- 图片、音频和任意文件上传。
- 模型运行时切换。
- MCP 或 TOML 配置编辑和热更新。
- 登录、多人协作、局域网和公网部署。
- 暗色主题和专用手机端界面。
- 提交 `web/dist` 或要求单文件二进制分发。
