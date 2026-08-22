# Spatial OS WebUI 现代化重设计

## 1. 背景

Clawcode WebUI 已具备 Session 管理、ACP v2 WebSocket 通信、断线重连与 replay、流式 Thinking 和 Agent 消息、Tool Call、follow-up 队列、Skills、MCP、Tree、Events、Tools Inspector 等完整工作台能力。

当前界面已经采用亮色布局，但整体更接近传统后台：多个灰色面板边界较重，消息、工具、状态和 Inspector 之间的视觉关系不够统一，长会话中的流式内容与协议事件容易形成视觉噪声。本次重设计不改变 ACP 业务协议，而是在现有功能基础上建立一套现代、明亮、具有未来操作系统气质的 UI/UX 系统。

本设计补充并更新 `2026-08-14-agent-webui-design.md` 中的视觉、布局、消息展示、Composer 和响应式规范。原文中的系统架构、ACP 方法、领域状态、部署拓扑和安全边界继续有效；如两份文档在 UI 表达上存在冲突，以本文为准。

## 2. 目标

- 建立统一的 Spatial OS 亮色视觉语言，形成 Clawcode 的产品识别度。
- 保持高信息密度，同时让主对话成为视觉中心，减少面板割裂感。
- 让 Thinking、Agent Markdown、Tool Call start/end、重连恢复和 Inspector 使用同一套状态语言。
- 保持 ACP sequence、cursor 和 toolCallId 的可追溯性，但不让协议细节干扰日常阅读。
- 优化流式消息和 replay 后的前端渲染体验，避免按 frame 创建大量独立视觉节点。
- 在桌面宽屏、普通笔记本、平板窄窗和手机宽度下提供可理解、可操作的自适应布局。
- 满足键盘操作、屏幕阅读器、文本对比度、减少动态和减少透明度等可访问性要求。

## 3. 已确认的设计决策

### 3.1 视觉方向

采用 **Spatial OS Workbench**：

- 使用珍珠白和冷淡蓝作为稳定内容基底。
- 使用蓝紫作为主交互色，薄荷青作为连接、流式运行和空间状态光。
- 透明、模糊和光谱效果只用于主导航、Session 框架、状态胶囊和 Inspector 等外围层。
- Markdown 正文、代码块、Tool 输入输出和 Composer 使用高不透明度或完全不透明表面，确保长期阅读与语法高亮可读性。
- 科幻感来自空间层级、精密细线、状态环和克制光谱，不采用黑底霓虹、CRT 扫描线、频繁闪烁或装饰性 HUD。

### 3.2 桌面布局

采用 **Docked Lens** 四区布局：

1. 主导航轨道：Sessions、Skills、MCP、About 等一级入口。
2. Session 栏：搜索、新建、分组列表、运行状态和当前 Session。
3. 主对话区：Session 标题、连接状态、消息流和 Composer。
4. Context Lens：Tree、Events、Tools 及当前选中消息或工具的上下文。

Session 栏在桌面宽屏中常驻。Inspector 默认可见但允许独立折叠。主对话区保持稳定的最小阅读宽度，不因 Inspector 内容变化发生跳动。

### 3.3 消息组织

采用 **Narrative Stream**：

- 用户消息右对齐，使用低饱和强调背景。
- Agent 正文左对齐并直接参与文档流，不套大面积聊天气泡或 Turn 卡片。
- Thinking 是正文前的可折叠 Reasoning Trace，支持 Markdown 和语法高亮。
- 同一语义文本块内的连续 Thinking 或 Agent 流式 frame 在视觉层合并更新，不为每个 frame 创建独立消息卡片。
- Tool Call start 和 Tool Call end 保持为两个协议事件，通过稳定 `toolCallId` 在同一视觉工具模块内建立生命周期关联。
- sequence 默认不逐条显示；只在恢复分界线、Events Inspector 或诊断详情中展示单值或范围。
- replay 完成后插入一条轻量恢复分界线，例如“已恢复 128 条事件 · cursor 1714”，不重复强调每条历史消息的恢复来源。

### 3.4 Composer 键盘语义

- `Enter` 发送当前输入。
- `Shift + Enter` 插入换行。
- 输入法处于 composing 状态时，`Enter` 只确认候选词，不发送消息。
- 空白输入不发送。
- Agent 运行期间发送的新消息继续进入 follow-up 队列，不打断当前运行。

## 4. 设计系统

### 4.1 颜色 token

第一版使用以下语义色作为实现基线；实施时必须对真实前景与背景组合执行对比度检查，必要时只调整明度，不改变整体色相关系。

| 语义 | 建议值 | 用途 |
| --- | --- | --- |
| Canvas | `#EAF1FF` | 页面外部空间和空间光背景 |
| Content | `#FBFDFF` | 对话正文、Markdown 和稳定内容表面 |
| Glass | `rgba(255, 255, 255, 0.72)` | 导航和外围框架 |
| Panel | `rgba(242, 247, 255, 0.72)` | Session、Inspector 和次级区域 |
| Raised | `#FFFFFF` | Composer、Tool Call、弹窗和悬浮元素 |
| Text Primary | `#17253B` | 标题和正文 |
| Text Secondary | `#687893` | 描述、元数据和占位内容 |
| Border | `rgba(84, 112, 163, 0.18)` | 默认细边框和分隔线 |
| Accent | `#536FF1` | 当前项、主操作和键盘焦点 |
| Spatial Cyan | `#53D8CF` | 连接、运行、流式状态和空间光 |
| Success | `#13805D` | 完成和成功 |
| Warning | `#946719` | 等待、重试和需要注意的状态 |
| Destructive | `#C53F57` | 删除、失败和不可恢复操作 |

状态不能只依赖颜色表达。运行、成功、警告和失败必须同时提供文字、图标或形状差异。

### 4.2 材质和层级

- 层级 0：Canvas，只承载低对比度静态径向光，不承载正文。
- 层级 1：Glass 框架，用于主导航和外围面板；模糊范围控制在 16–24px。
- 层级 2：Content 稳定表面，用于消息和主要操作区域。
- 层级 3：Raised 表面，用于 Composer、Tool Call、菜单、弹窗和 Context Lens 内的选中详情。
- 阴影只表达可交互层级，不为每条 Agent 正文增加阴影。
- 不使用覆盖大面积内容的动态渐变；静态空间光不应降低文本对比度。

### 4.3 圆角、边框和间距

- 工作台外框：18–20px。
- 浮动面板和弹窗：14–18px。
- Composer 和 Tool Call：10–12px。
- 小型按钮、状态胶囊和列表项：6–9px。
- 默认边框 1px；键盘焦点使用 2px 可见焦点环，不通过阴影模拟不可辨认的焦点。
- 使用 4px 基础间距栅格。主要区域间距优先使用 8、12、16、20 和 24px。

### 4.4 字体

- 产品正文使用现代系统无衬线字体栈，优先选择本地可用的 Inter-compatible 字体，不依赖运行时远程字体服务。
- 代码、JSON、sequence 和 cursor 使用系统等宽字体栈。
- 正文默认不少于 14px，辅助元数据不少于 12px。
- 不使用全大写承载长标题；全大写仅用于短小分区标签，并增加字间距。

### 4.5 图标

- 使用现有一致的 SVG 图标系统，不使用 Emoji 作为功能图标。
- 仅图标按钮必须有可访问名称和 Tooltip。
- 图标尺寸、线宽和状态变体在主导航、Composer 和 Inspector 中保持一致。

## 5. 信息架构和页面布局

### 5.1 主导航轨道

- 当前一级入口使用低饱和 Accent 表面、左侧位置指示和可见图标状态。
- 产品标志展示连接状态点，但状态点不代替顶部连接文字。
- 底部放置低频入口，不与高频 Sessions、Skills 和 MCP 混排。
- Hover、Focus 和 Selected 是三种独立状态。

### 5.2 Session 栏

- 顶部固定显示标题、新建按钮和搜索框。
- Session 按 Today、Yesterday、Earlier 分组，保持现有排序语义。
- 当前 Session 使用左侧 Accent 指示和浅色背景，不使用大面积高饱和填充。
- 运行中的 Session 同时显示薄荷青状态点和可访问文本。
- 重命名、关闭和删除继续在当前 Session 上下文中完成；破坏性操作必须二次确认。
- Session URL 继续采用 `/sessions/:sessionId`。点击 Session、创建 Session、浏览器刷新和前进后退必须保持选中状态一致。
- URL 中的 Session 不存在或已删除时，显示明确空状态，并回退到可用 Session 或 Session 列表，不保留失效选中态。

### 5.3 主对话头部

- 第一层显示 Session 标题。
- 第二层以较弱层级显示 cwd 或必要上下文，不与标题竞争。
- 右侧状态胶囊显示连接、当前模型和必要运行状态。
- Context 使用小型进度环或胶囊展示；环形图必须同时提供可访问文本。
- 连接错误和重连使用持续可见但不遮挡正文的状态条；恢复后自动收起。

### 5.4 Context Lens

- Context Lens 是覆盖在主对话区上的单实例、非模态悬浮窗，不占用 Inspector 顶部空间，也不改变主对话滚动位置。
- Inspector 保留 Tree、Events、Tools 三个页签。用户检查消息或 Tool Call 时，Inspector 必须打开并切换到 Tree。
- Message 通过自身持久化 entry 定位；Tool Call 不作为独立 Tree 节点，优先定位同一 Turn 内最近的 assistant message，无法可靠映射时不得选择错误节点。
- Tree 自动选中目标 entry、滚动到可视区域中央并提供短暂定位高亮；关闭 Context Lens 后继续保留 Tree 的选中状态。
- Context Lens 中的 Message 详情展示角色、MessageId、TurnId、时间、sequence range、模型、usage 和 Markdown 正文；Tool 详情复用现有 Tool Call 输入、输出、状态和 start/end 展示。
- 悬浮窗提供关闭按钮并响应 Escape；关闭后焦点恢复到触发检查的消息或工具按钮。悬浮窗使用 `role="dialog"`、`aria-modal="false"`，不设置焦点陷阱。
- 手机宽度下悬浮窗接近视口全宽并位于 Inspector 之上，保持独立滚动；reduced-motion 和 reduced-transparency 设置必须分别关闭入场动画和玻璃模糊。

### 5.5 Skills、MCP 和 About

- 这些一级页面复用 Spatial OS 外框、标题、搜索、空状态、列表和详情表面。
- Skills 使用“列表 + 详情”的工作台模式，显式调用入口使用 Accent 主操作。
- MCP 使用服务器生命周期和工具数量建立层级；连接、重试和错误状态使用统一状态语言。
- MCP Elicitation 弹窗使用 Raised 不透明表面，阻塞时明确说明关联 Session 和工具。
- About 保持低信息密度，不复制复杂工作台布局。

## 6. 消息和事件映射

### 6.1 用户和 Agent Markdown

- 用户输入支持普通文本、Markdown、图片和资源引用的现有能力。
- Agent 正文和 Thinking 统一使用共享 Markdown 渲染器与语法高亮规则。
- 标题、段落、列表、引用、表格、链接和代码块使用文档式间距，而不是聊天气泡内部的紧凑堆叠。
- 长链接、长单词、JSON 字符串和代码行必须自动换行或提供局部横向滚动，不允许撑破消息区。
- 外部链接具有明确外链提示，并使用安全的打开策略。

### 6.2 Reasoning Trace

- 默认折叠或维持用户上次操作状态；正在流式生成时可显示摘要和运行状态。
- 展开内容按 Markdown 渲染。
- 连续 Thinking frame 更新同一个语义块，避免为每个 frame 创建新 DOM 容器。
- 完成后停止动态状态，不保留无意义脉冲。
- 屏幕阅读器不逐 frame 播报 Thinking 内容。

### 6.3 Tool Call

- Tool Call start 和 end 在领域状态中保持独立事件。
- 视觉层按 `toolCallId` 关联为一个生命周期模块，依次显示调用名称、运行状态、输入、结果和耗时。
- start 已到达但 end 尚未到达时显示 Running；重连后若只恢复到 start，继续等待对应 end，不伪造完成状态。
- 输入按格式化 JSON 代码块渲染，支持自动换行和复制。
- 输出按 Markdown 渲染；若输出明确为结构化 JSON，可提供 JSON 视图，但默认不得重复展示两份相同内容。
- 失败、取消和超时是不同状态。错误详情保留可复制的技术信息，但不记录或展示已被服务端脱敏的凭据。
- start/end 的 sequence 在 Inspector 和展开诊断中展示，默认正文只显示生命周期结果。

### 6.4 Command、Compaction 和 Extension

- Command 与普通 Tool Call 使用相同的运行、输出和错误语义，但保留命令类型标识。
- Compaction 使用轻量系统分界块，说明上下文已压缩、时间和必要摘要，不伪装成 Agent 正文。
- 未识别 Extension 使用中性诊断卡，不打断后续消息渲染；原始数据只在 Events 或展开详情中展示。

### 6.5 replay 和 sequence range

- 前端领域状态继续以原始 ACP sequence 进行去重和恢复确认。
- 视觉合并的文本块必须保留其覆盖的 `sequenceStart` 和 `sequenceEnd`，不能只保留最后一个 sequence。
- Tool Call start/end 各自保留原始 sequence，不因视觉关联而合并协议身份。
- replay 批次到达时优先批量更新领域状态和视图，避免逐事件触发完整对话重渲染。
- 历史批次完成后只增加一条恢复分界线；live 消息继续追加到当前 Narrative Stream。

## 7. Composer 和命令交互

### 7.1 输入行为

- `Enter` 发送，`Shift + Enter` 换行。
- 处理 `KeyboardEvent.isComposing` 和输入法 composition 生命周期，避免中文候选确认误发送。
- 发送后仅在请求已进入本地处理流程时清空输入；同步校验失败时保留原文。
- Session 切换、Inspector 折叠和短暂断线不得清空草稿。
- 空白输入禁用发送；仅包含有效附件或资源时遵循现有协议允许情况。

### 7.2 运行状态和 follow-up

- Idle 时显示发送按钮。
- Running 时显示停止按钮，同时 Composer 保持可输入。
- Running 期间提交的消息进入服务端 follow-up 队列，并在 Composer 上方显示队列数量。
- 队列项支持查看、编辑、移除和清空；所有操作以服务端确认为准。
- 停止只取消当前运行，不自动删除已排队 follow-up。

### 7.3 Slash Command、资源和附件

- Slash Command 面板锚定 Composer，不遮挡当前输入和队列。
- 搜索结果支持键盘上下移动、Enter 确认和 Escape 关闭。
- 图片、资源和附件以紧凑 chip 展示，包含类型、名称、移除按钮和错误状态。
- 资源表单、附件错误和发送错误显示在 Composer 邻近位置，不使用全局通用错误覆盖具体原因。

## 8. 状态和错误反馈

### 8.1 连接与重连

- 状态包含 Disconnected、Connecting、Initializing、Ready、Reconnecting 和 Failed。
- Reconnecting 显示当前重试次数和恢复起点，例如“第 2/5 次重连 · 从 seq 1842 恢复”。
- 重连期间保留已渲染内容和草稿，不显示空白页面或全屏加载。
- 恢复成功后显示一次性恢复分界线，并将状态恢复为 Connected。
- 无法恢复时提供可操作说明和重试入口，不自动重复发送结果未知的 prompt。

### 8.2 Tool 和请求错误

- Tool 错误保留在对应生命周期模块中。
- Session 操作错误显示在 Session 栏或对应弹窗附近。
- Composer 发送错误显示在输入区附近并保留草稿。
- MCP、Skill 和 Inspector 错误显示在对应页面或页签中。
- 全局错误仅用于无法继续初始化、协议能力不匹配或工作台整体不可用的情况。

### 8.3 空状态

- 无 Session 时主区域引导创建 Session，并显示默认 cwd。
- Session 无消息时显示简洁起始提示，不展示虚构示例消息。
- Skills、MCP 和 Inspector 空状态说明原因及可执行动作；只读页面不展示无效编辑 CTA。

## 9. 响应式规范

### 9.1 `>= 1280px`

- 使用完整 Docked Lens 四区布局。
- Session 栏和 Inspector 常驻，均允许折叠。
- 主对话区维持适合 Markdown 和代码阅读的最小宽度。

### 9.2 `1024–1279px`

- 主导航、Session 栏和主对话常驻。
- Inspector 改为右侧覆盖抽屉，打开时不永久压缩正文。
- 抽屉关闭后将焦点恢复到触发按钮。

### 9.3 `768–1023px`

- 主导航轨道和主对话常驻。
- Session 与 Inspector 均使用按需覆盖面板，同一时间只打开一个辅助面板。
- Composer 始终可见，覆盖面板不得遮挡正在输入的内容。

### 9.4 `< 768px`

- Session 列表与对话采用页面级导航，不将四区布局压缩为四个窄栏。
- 进入 Session 后显示明确返回 Session 列表入口。
- Composer 固定在可视区域底部，并适配安全区域、虚拟键盘和横竖屏变化。
- Tree、Events、Tools 使用全高抽屉或独立页面。
- 触摸目标最小 44×44px；桌面紧凑控件不得直接缩小后用于触屏。

## 10. 可访问性和键盘

- 正常正文对比度至少达到 WCAG 4.5:1；大号文本至少达到 3:1。
- 所有可操作元素支持键盘访问，Tab 顺序与视觉顺序一致。
- 使用 `:focus-visible` 提供清晰焦点，不移除浏览器焦点且不给出替代样式。
- Dialog、Elicitation、Command Palette 和移动抽屉正确管理焦点范围；关闭后恢复到触发元素。
- Escape 关闭当前最上层的非破坏性浮层；存在未提交输入时不得静默丢失内容。
- 仅图标按钮、状态环、进度指示和折叠控件具有明确的可访问名称和展开状态。
- 连接变化、Tool 完成和错误使用克制的 live region 播报。
- 不逐 token 或逐 frame 播报流式正文，避免屏幕阅读器持续打断。
- 状态不只依赖颜色；图标、文字或形状至少提供一种冗余表达。

## 11. 动态和材质降级

- Hover、Focus、面板展开和状态切换控制在 140–200ms。
- 动画只使用 opacity 和 transform 等低成本属性，不对消息高度、宽度或长列表位置执行连续布局动画。
- 流式文本不使用逐 token 淡入、位移或打字机动画。
- 运行状态允许低频、低幅度状态光，但完成后立即停止。
- `prefers-reduced-motion: reduce` 下关闭位移、缩放、脉冲和循环光效，只保留即时状态变化。
- `prefers-reduced-transparency: reduce` 或浏览器不支持 `backdrop-filter` 时，将 Glass 与 Panel 替换为接近不透明的浅色表面。
- 降级后仍保持完整层级和对比度，不能依赖模糊本身区分前后层。

## 12. 性能约束

- Thinking 和 Agent 连续流式 frame 更新当前语义块，不为每个 frame 创建新的 React 列表项或 Markdown 根节点。
- Markdown 解析与语法高亮不得因 Inspector、Session 状态或连接状态变化而无条件重复执行。
- replay 和 multi-message 批次优先以批量 action 更新领域状态，减少 React commit 次数。
- 长会话需要保持可扩展性；实施阶段应基于性能测量决定窗口化或分段挂载阈值，不在未测量前引入破坏滚动锚点的虚拟化。
- Tool 输入和输出默认折叠超长内容，展开只影响当前 Tool 模块。
- Inspector 只订阅当前页签和当前选中对象所需状态，避免 Events 更新导致整个工作台重渲染。
- 透明和模糊效果限制在固定数量的外围面板，不能为每条消息应用 `backdrop-filter`。
- 所有性能优化必须维持 cursor、sequence range、滚动锚点和重连去重语义。

## 13. 实现边界

### 13.1 主要前端影响范围

- `web/src/theme/`：重构语义 token、基础排版、Markdown、代码、状态和响应式规则。
- `web/src/shell/`：实现 Docked Lens、自适应辅助面板和焦点恢复。
- `web/src/features/sessions/`：更新 Session 列表、状态、URL 选中和窄屏页面导航。
- `web/src/features/conversation/`：实现 Narrative Stream、Reasoning Trace、Tool 生命周期和恢复分界线。
- `web/src/features/composer/`：实现 Enter/Shift+Enter/composition 语义、队列和运行态外观。
- `web/src/features/inspector/`：实现 Context Lens 联动和响应式抽屉。
- `web/src/features/skills/`、`web/src/features/mcp/`：统一到 Spatial OS 组件语言。

应优先复用现有领域组件和状态，不为纯视觉差异复制第二套消息、工具或 Session 组件。

### 13.2 协议边界

- 本次设计不新增 ACP 方法，也不改变后端 Session、Tool Call 或 replay 的协议身份。
- 如前端当前无法表示合并文本块的 sequence range，可在前端领域模型中增加派生范围，但不得覆盖或丢弃原始事件 sequence。
- Tool start/end 的视觉关联使用现有稳定 ID；不得通过相邻位置或名称猜测关联。
- Session URL 继续由前端路由和现有 Session ID 驱动，不改变 Kernel 创建 Session 的职责。

## 14. 验证与验收

前端继续豁免 TDD 和单元测试，但必须通过以下质量门槛：

- TypeScript 严格类型检查通过。
- ESLint 通过。
- production build 成功。
- 浏览器控制台无新增错误和未处理 Promise rejection。
- 在 375、768、1024、1280 和 1440px 宽度检查布局。
- 使用键盘完成 Session 选择、Composer 输入发送、Tool 展开、Inspector 切换、弹窗和抽屉关闭。
- 验证 Enter 发送、Shift+Enter 换行和中文输入法候选确认不误发送。
- 验证正文、次要文本、状态和焦点对比度。
- 验证 reduced-motion 和 reduced-transparency 降级。

WebUI 端到端验收必须运行真实 production `app` 后端及其配置 provider，不引入 fixture backend。至少覆盖：

1. 启动真实 `app` 和 production WebUI。
2. 创建 Session 并发送会产生 Thinking、Agent Markdown 和 Tool Call 的真实 prompt。
3. 验证流式文本在同一语义块内更新，Markdown 和语法高亮正确。
4. 验证 Tool start/end 保持可追溯并在视觉模块中正确关联。
5. 在 Agent 运行期间发送 follow-up，确认队列和停止语义。
6. 刷新 `/sessions/:sessionId`，确认自动选中并恢复同一 Session。
7. 在消息流运行或完成后主动断开 WebSocket，再恢复连接。
8. 验证 initialize、resume、cursor replay、批量恢复、去重、恢复分界线和继续 live stream。
9. 验证 Session、Inspector、Skills 和 MCP 的宽屏与窄屏交互。

性能验收需要记录长 replay 前后的浏览器 Performance 或 React Profiler 数据，至少比较：

- 恢复消息数量。
- React commit 次数和主要 commit 耗时。
- Markdown 重算次数。
- 恢复期间主线程长任务。
- 恢复完成到界面可交互的时间。

不以主观“看起来更快”替代性能证据。

## 15. 非目标

- 不实现暗色主题。
- 不改变 ACP transport、后端存储或 Kernel Session 创建流程。
- 不实现多用户、登录或 user_id 鉴权。
- 不引入传统黑底赛博朋克、CRT、扫描线或持续闪烁 HUD。
- 不为视觉效果增加 Canvas、WebGL 或重型动画运行时。
- 不重新设计或替换已经确认的 Markdown 与语法高亮能力。
- 不使用 fixture backend 代替真实 WebUI 端到端测试。
- 不在本次设计阶段创建 commit。

## 16. 完成定义

满足以下条件时，Spatial OS WebUI 重设计视为完成：

- 已确认的 Spatial OS、Docked Lens 和 Narrative Stream 在所有主要页面一致落地。
- Composer 严格遵循 Enter 发送、Shift+Enter 换行和 composing 保护。
- Tool Call start/end、sequence range、cursor replay 和 Session URL 语义未回归。
- 真实后端端到端测试覆盖创建 Session、真实流式响应和断线重连。
- 响应式、键盘、对比度、reduced-motion 和 reduced-transparency 验收通过。
- production build、前端检查和相关 Rust 检查通过。
- 性能测量证明 replay 和流式渲染没有因重设计产生回退，并对长会话达到可接受的交互时间。
