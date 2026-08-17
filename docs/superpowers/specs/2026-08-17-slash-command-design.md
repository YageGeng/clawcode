# Slash Command 统一机制设计

## 目标

在 Clawcode 中建立由 Kernel 统一发现、解析和路由的 Slash Command 机制，并保持 Pi 的命令优先级与输入 Hook 顺序。所有命令通过 ACP v2 向客户端发布，通过标准 `session/prompt` 执行；WebUI 只负责展示命令、提交原始输入和渲染服务端结果。

首版统一现有 Extension Command、Skill 和 Prompt Template，并增加 `/compact`、`/name`、`/session` 三个具备完整后端能力的 Builtin Command。需要客户端导航或交互选择器的 `/new`、`/resume`、`/tree`、`/fork` 等命令不在首版范围内。

## 非目标

- 不新增独立 `command` crate。
- 不在 WebUI 中维护命令注册表或执行命令业务逻辑。
- 不为通用参数解析增加引号、转义、变量或递归展开语法。
- 不改变 Pi 兼容的 Extension input Hook、Skill 和 Prompt Template 执行顺序。
- 不增加客户端文件系统、Terminal callback 或其他客户端回调能力。

## 架构

公共命令类型统一下沉到 `protocol::slash_command`。Kernel 的 `runtime::command` 是命令快照、解析、冲突处理和执行编排的唯一所有者。Builtin、Extension、Skill 和 Prompt Template 继续由各自模块及 Factory 构建，Kernel 使用类型化适配器组合这些来源，不让来源模块互相依赖。

Kernel 内部使用 `ResolvedSlashCommand` 枚举表达解析结果，分别携带 Builtin 枚举、已注册 Extension Command、Skill 描述或 Prompt Template 描述。不同来源的执行语义在枚举分支中保持显式，不使用一个弱类型动态回调统一所有行为，也不复制各模块已有的描述类型。

现有 `AvailableAgentCommand` 演进为统一 Slash Command 定义，避免同时保留两套含义重复的结构体。核心公共类型包括：

- `SlashCommandDefinition`：名称、描述、参数提示、来源和别名信息。
- `SlashCommandSource`：`Builtin`、`Extension`、`Skill`、`PromptTemplate`。
- `SlashCommandInvocation`：原始输入、命令名和未经分词的参数字符串。
- `SlashCommandOutcome`：`Handled`、`ExpandedPrompt`、`NotFound`、`Rejected`。
- `SlashCommandError`：`InvalidArguments`、`Busy`、`ExecutionFailed`、`Ambiguous`。
- `SlashCommandOutput`：命令名、执行状态和有序内容块。

`Handled` 的精确定义是命令不进入常规 Agent 工具循环。命令自身仍可调用 Kernel 服务或 Provider；`/compact` 就是模型驱动的 Kernel 操作。

## 命令发现与命名

Kernel 为每个 Session 生成确定性的完整命令快照。命令来源按以下顺序排列，同一来源内按最终命令名排序：

1. Builtin
2. Extension
3. Skill
4. Prompt Template

命名规则如下：

- Builtin 使用 `compact`、`name`、`session`，这些名称为保留名称。
- Extension 始终提供 `extension-id:command` 限定名称。
- Extension 短名称仅在全局唯一且不与更高优先级名称冲突时发布。
- 多个 Extension 使用同一短名称时不发布该短名称，执行歧义短名称时返回限定名称提示。
- Skill 使用 `skill:skill-name`，不与普通模板争抢名称。
- Prompt Template 使用模板自身名称，优先级最低。

命令名称保持 Pi 当前的大小写行为，首版不增加大小写归一化或别名配置。Builtin 名称不能被其他来源覆盖。冲突的低优先级命令不出现在 ACP 快照中，避免客户端展示实际上不可执行的命令。

Extension、Skill 或 Prompt Catalog 的有效内容变化后，Kernel 重建完整快照并发出一次 `AvailableCommandsChanged`。ACP 客户端以最后一次快照整体替换本地状态，不合并增量。

## 输入解析

只有输入第一个字符为 `/` 时才尝试解析 Slash Command。命令名在第一个 ASCII 空格处结束；分隔空格之后的内容作为原始参数字符串传递，不由通用解析器分词。引号、转义、默认值和模板变量由具体命令或 Prompt Template 自己解释。

包含图片、音频、Resource 等非文本内容的 Prompt 不作为 Slash Command 执行，完整进入普通 Agent 流程。未识别的 Slash Command 返回 `NotFound`，原始文本不变并继续作为普通用户输入交给 Agent，这与 Pi 的行为一致。

Skill 或 Prompt Template 展开只执行一次。展开结果即使以 `/` 开头也不会重新进入 Builtin 或 Extension 路由，避免递归循环和意外执行。

## 执行顺序

完整输入流程保持为：

```text
ACP session/prompt
  -> Builtin Command
  -> Extension Command
  -> Extension input hook
  -> Skill expansion
  -> Prompt Template expansion
  -> before_agent_start hook
  -> Agent / Provider
```

Builtin 和 Extension 是可直接处理输入的命令，因此位于 Extension input Hook 之前。input Hook 仍可转换普通输入；转换后的文本继续经过 Skill 和 Prompt Template，但不会回到 Builtin 或 Extension 阶段。Skill 优先于 Prompt Template。未命中任何命令的输入最终进入 Agent。

Kernel 在命令路由前建立带 `RunId`、`TurnId`、`trace_id`、时间源和 Event Sink 的类型化执行上下文。Extension Command Context 也从该上下文获得关联标识，不再以缺少 Run 和 Turn 的上下文执行。命令不创建第二个无关联 Turn；需要 Provider 的 `/compact` 同样沿用当前 trace。

需要修改 Session 状态的命令通过 Session operation gate 串行化。已注册但当前状态不允许执行的命令返回 `Busy`，而不是静默排队或进入 Agent。

## 消息与持久化

所有命令相关消息继续满足项目的统一约束：必须包含字符串形式的 `TurnId`，并持久化消息创建时间、开始时间和结束时间三个字符串形式的毫秒时间戳。即时命令消息的开始和结束时间相同；流式压缩输出按首 Token 和末 Token 记录。

直接处理的 Builtin 或 Extension Command 持久化原始命令输入。可展示结果使用 `SlashCommandOutput` 持久化，不进入后续模型上下文。协议消息类型应显式区分命令输入、命令输出和模型 Assistant 消息，不能伪造 Provider metadata，也不能在 Kernel、ACP 和 WebUI 重复定义同构结构体。

Skill 或 Prompt Template 展开后仍只形成一条用户消息。该消息同时保存：

- 用户实际提交的原始内容，用于 ACP 实时展示和回放。
- 展开后的模型输入，用于本次调用及恢复 Session 后重建模型上下文。
- 命令来源和名称等展开来源信息，用于诊断。

这样既不会在界面重复展示两条用户消息，也不会在恢复 Session 后依赖可能已经变化或删除的 Skill、Template 文件重新展开。

命令执行事件与消息复用输入的 TurnId。`/compact` 的压缩事件和压缩结果也归属该 Turn，不再由压缩实现无条件生成一个与 Slash 输入无关的 TurnId。其他 ACP 扩展入口调用手动压缩时，由入口建立等价的执行上下文。

## Builtin Command

### `/compact [custom instructions]`

无参数时使用默认压缩指令；有参数时将完整参数字符串作为自定义压缩指令。命令调用现有 Pi v4 兼容压缩流程，保留 `session_before_compact`、压缩流式事件、Store 记录和 `session_compact` 行为。它不进入普通 Agent 工具循环，但可能调用 Provider。

压缩 API 接受类型化执行上下文和可选自定义指令，不再在内部无条件创建独立 Turn。默认指令仍由 Kernel 维护，不能由 WebUI 复制。

### `/name [name]`

有参数时规范化并持久化 Session 名称，随后发出 `SessionTitleChanged`。无参数且已有名称时输出当前名称；无参数且没有名称时输出 `Usage: /name <name>`。成功修改继续映射为 ACP `session_info_update`，使所有客户端同步更新。

### `/session`

命令不接受参数。它从 Session 和 Store 读取 Session ID、名称、消息数量、用户与 Assistant 消息数量、工具调用与结果数量、Token 使用量和费用等当前可计算统计，不调用 Provider。未知或当前无法计算的值应明确省略或标记不可用，不伪造零值。

## 命令输出与错误

命令输出作为非模型上下文消息持久化，并映射为 ACP 原生 `agent_message`。这样普通 ACP v2 客户端可以直接显示文本内容；产品命名空间下的 ACP `meta` 额外携带命令名称、来源和执行状态，使 WebUI 能使用命令卡片样式。命名空间和产品名称必须来自公共项目常量，不能散落字符串字面量。

已识别命令的参数错误和执行错误属于异步命令结果。由于 ACP `session/prompt` 已经响应，它们不再作为 JSON-RPC 传输错误返回，而是产生可展示的错误命令消息，并以 Idle 状态结束。内部错误链写入具有关联 trace 的日志，客户端只接收稳定、可读且不泄漏敏感信息的错误。

`NotFound` 不产生命令错误。`Ambiguous` 必须列出可执行的 Extension 限定名称。`InvalidArguments` 应包含该命令的用法。`ExecutionFailed` 保留具体错误源供日志诊断。

## ACP 映射

所有传输复用同一 ACP 映射：

- 命令发现使用 `available_commands_update`。
- 命令执行使用标准 `session/prompt`。
- 命令文本输出使用 `agent_message`。
- Session 重命名使用 `session_info_update`。
- 命令运行和结束使用 `state_update`。
- 来源、别名和命令状态等扩展信息使用 ACP v2 `meta`。

`available_commands_update` 的原生字段承载名称、描述和参数提示；来源类型、限定名称、是否为短名称别名等信息位于产品命名空间 `meta`。WebSocket、HTTP 和 stdio 不实现各自的命令分支。

实时消息和 Session 回放必须走同一个消息到 ACP 的映射。ACP 批量回放仍按持久化事件顺序及 sequence 发送，命令输入、输出、Session 信息变化和压缩事件不能在 WebUI 中重新排序。

## WebUI

WebUI 将最后一次 `available_commands_update` 作为唯一命令目录。命令面板不硬编码 Builtin、Extension、Skill 或 Prompt Template。选择命令后只将 `/command ` 写入 Composer，用户提交后走标准 `session/prompt`。

WebUI 的 Slash Command 路径不展开命令，不把命令改写为私有 Compact、Rename 或 Skill 扩展方法，也不根据命令名称执行本地逻辑。侧边栏中面向任意 Session 的独立管理操作不属于 Slash Command 路径，可继续使用对应 ACP Session 能力。带命令 metadata 的 `agent_message` 使用区别于模型回复的命令卡片样式；缺少扩展 metadata 时仍按普通 ACP Agent 文本显示，保证兼容其他客户端。

实时和回放事件继续使用同一 reducer 和渲染组件，并以服务端 sequence 为唯一顺序依据。Session 标题只响应 ACP `session_info_update`，不根据 `/name` 输入做乐观猜测。

## 测试与验收

所有 Rust 测试位于对应 crate 的 `tests/` 集成测试目录，不在正文中增加测试支撑代码。

`protocol` 集成测试覆盖 Slash Command 起始位置、ASCII 空格、原始参数、非命令输入和附件输入。`kernel` 集成测试覆盖来源优先级、保留名称、Extension 限定名称与歧义、稳定快照、未知命令回落、单次展开、input Hook 顺序、三个 Builtin、持久化投影以及 Run、Turn、时间和 trace 关联。压缩测试同时覆盖默认指令和自定义指令。

`acp` 集成测试覆盖完整命令快照、标准 `session/prompt` 执行、命令输出 metadata、`session_info_update`、Idle 结束状态、实时与回放一致性，以及批量回放顺序。

前端不新增单元测试。实现完成后运行 TypeScript 检查和 WebUI 生产构建，并使用正式 App 后端、真实配置和真实 Provider 运行端到端测试，不使用 fixture：

1. 通过 WebSocket ACP 创建 Session，确认命令面板包含 Builtin、Extension、Skill 和 Prompt Template。
2. 执行 `/session`，确认命令卡片和统计内容。
3. 执行 `/name Slash E2E`，确认侧边栏通过服务端更新同步名称。
4. 先发送一条真实模型消息，再执行带自定义指令的 `/compact`，确认真实 Provider 调用、流式事件和最终状态。
5. 刷新并恢复 Session，确认命令输入、命令输出和压缩事件按原顺序回放。

最终验收还包括格式化、lint、完整 Rust 工作区测试和项目 pre-commit。任何命令处理路径都不能绕过公共项目常量、关联日志、消息时间或 Store 持久化约束。
