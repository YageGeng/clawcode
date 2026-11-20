# Desktop 客户端（gpui-kit · Inset Workbench）设计

## 1. 背景与目标

clawcode 目前通过 ACP 提供 stdio、HTTP/SSE、WebSocket 三种传输，并自带一个 React WebUI（`web/`）。本设计新增一个原生桌面客户端 `crates/desktop`，用 [gpui-kit](https://github.com/longbridge/gpui-kit) 构建。功能与 WebUI 对齐，并在此基础上补齐工作区能力。

用户给出的硬性要求：

- 必须实现**工作区**：按项目目录组织会话；多会话 Tab 在后台并行运行；提供文件树与 Git 变更面板。
- **界面精致**：icon button 统一，区域分明，会话区是浮在底面上的圆角卡片。
- 支持 **i18n**。
- 支持 gpui-kit 的**全部内置主题**（Catppuccin、Tokyo Night、Ayu、Gruvbox 等）。

成功标准：一个二进制即可完成 WebUI 的全部日常工作流，包括在多个工作区里并行跑多个会话、回答阻塞式请求、查看变更与 diff。中英文、浅色与深色、所有内置主题下都没有布局退化。

可视化稿：[Inset Workbench 视觉稿](./2026-11-19-desktop-gpui-kit-mockup.html)。直接用浏览器打开，内容是同一界面在 Default Light 和 Catppuccin Mocha 下的渲染。

## 2. 已确认的决策

| 主题 | 决策 |
| --- | --- |
| 后端接入 | **进程内 ACP 客户端**：复用 `app::ApplicationFactory`，用 `Client.v2()...connect_with(AcpServerFactory::component(..), ..)` 在进程内连接（库内部走 `Channel::duplex()`） |
| 工作区定义 | 工作区 = 一个已登记的项目目录 = 后端会话的 `cwd`，后端不新增概念 |
| 工作区第一版能力 | 目录 → 会话分组；多会话 Tab 后台运行；文件树和 Git 变更面板。分屏不做 |
| 功能范围 | 与 WebUI 的用户功能全量对齐：核心对话、阻塞式交互、Slash 命令和模型选择、会话树、子 agent、Skills / MCP、Provider 登录。诊断用的 Events 事件日志不在范围内（见第 16 节） |
| 外壳布局 | 图标轨道 + 按工作区组织的侧栏 + 主卡片（Tab）+ 可选的 inspector 卡片 |
| 视觉风格 | **Inset Workbench**：chrome 底面加浮起的圆角内容卡片（即 shadcn Sidebar 的 `inset` 变体）；icon button 只有两档 |
| i18n | `rust-i18n`，与 gpui-kit 组件库共用全局 locale；先做 `en` 和 `zh-CN` |
| 主题 | 从 gpui-kit `v0.10.0` tag vendor `themes/` 下的 21 个主题文件，浅色和深色主题分别选，可以跟随系统 |
| 平台 | 优先保证 Linux（X11 / Wayland），macOS 可以编译运行；Windows 不在范围内 |

## 3. 参考项目调研

| 项目 | 技术 | 布局 | 会话渲染 |
| --- | --- | --- | --- |
| [pi-gpui](https://github.com/bytesbay/pi-gpui) | GPUI 0.2 原生、自建组件，通过 RPC 驱动 pi | 自绘标题栏；左 Dock 是项目 → 会话两级树；中间是会话 Tab 条加聊天；右 Dock 是文件和 Git 变更（可拖宽、可换边） | 每个会话一个 `ChatView` 实体，各自持有一个 pi 进程；transcript 是只追加的 `Vec<Row>`（User / Assistant / Tool / Notice / Summary / Dialog），用 gpui `list()` 底部对齐渲染，只 splice 变更的行；Markdown 按文本 hash 缓存；事件分批合并后刷新 |
| [Wake](https://github.com/iAmCorey/Wake) | GPUI + gpui-component，只读会话库 | 固定三栏：资料库侧栏 224px、会话流 336px、阅读区（正文最大 720px）；⌘K 全文搜索 | `gpui::ListState` 渲染不等高行；用户消息是右对齐气泡，助手正文不加气泡；thinking 和工具簇折叠；压缩显示为居中 pill。i18n 以英文原文为 key，语言包是 JSON |
| [Ghostex](https://github.com/maddada/Ghostex) | GPUI + CEF + libghostty | 标题栏、原生侧栏、工作区列（可分屏、可开 Tab）、底部命令面板 | Chat 和 CLI 双视图；按 Turn 组织，中间工作收进可折叠的 "Active work"，已完成的 Turn 折成 "Worked for 2m 5s"；正文最大宽 768px |

采纳的做法：虚拟列表加尾部跟随（gpui-kit `MessageScroller`）；正文居中限宽；用户消息右对齐，助手正文直接排版；thinking 和工具调用默认折叠；已完成的一次 Run 把工具组折叠起来（来自 Ghostex，Ghostex 的 "turn" 对应本仓库的 Run）；每个会话一个实体，切换会话不中断运行（来自 pi-gpui）；严格的 token 和字阶约束（来自 Wake）。

## 4. 架构

### 4.1 Crate 与依赖

- 新增 workspace 成员 `crates/desktop`，二进制名 `clawcode-desktop`。
- 依赖：`app`、`acp`、`config`、`protocol`、`gpui-kit`、`agent-client-protocol`（client 角色）、`rust-i18n`、`sys-locale`、`serde`、`serde_json`、`tokio`、`futures`、`ignore`、`typed-builder`、`thiserror`、`tracing`。
- 按规则 11 和 12：所有版本都在根 `[workspace.dependencies]` 声明，子 crate 用 `{ workspace = true }` 继承。新增的外部依赖只有 `gpui-kit = "0.10"`、`rust-i18n = "4"`（与 gpui-component 0.10 的版本一致，共享全局 locale）、`sys-locale` 三个。
- **后端唯一改动**：`acp::AcpTransportKind` 增加 `InProcess` 变体，只用于日志和 trace 标记。

### 4.2 线程模型

```
GPUI 主线程（全部 UI 实体）
   ▲ BackendEvent 流（futures mpsc）       │ BackendCommand + oneshot 回复
   │                                      ▼
后台 std::thread：tokio multi-thread runtime
   Application(Kernel) ── AcpServerFactory::component(InProcess)
            ⇅ connect_with（进程内 duplex）
   AcpClient（client 角色连接）
```

- UI 只持有一个 `Backend` 句柄，它是一个 GPUI `Global`，内部包着命令发送端。请求一律是「命令加 oneshot 回复」，UI 侧用 `cx.spawn` 去 await。
- 通知，以及 agent 发给 client 的请求（权限、elicitation、问题），走同一条 `BackendEvent` 流。一个 `cx.spawn` 循环按 `sessionId` 把事件分发给 `SessionRegistry` 里对应的 `SessionHandle` 实体（见 4.5）。会话有没有打开成 Tab、Tab 是否可见，都不影响事件照常写进投影。
- 事件分批应用：循环每次取走通道里已经到达的所有事件，一次性写进投影，每个 `SessionHandle` 每批只 `notify` 一次。流式输出再快，每帧最多重绘一次（pi-gpui 的做法）。
- GPUI 侧不直接依赖 tokio。退出时先关闭 ACP 连接，再调用 `Application::shutdown()`，等 SQLite flush 完成后才结束进程。

### 4.3 模块划分

| 模块 | 职责 | 依赖 UI | 测试方式 |
| --- | --- | --- | --- |
| `backend/` | 启动 runtime、构建 Application、建立 ACP 连接；提供类型化的请求方法（会话的新建、加载、列表、关闭，prompt，cancel，config option，`_clawcode/*` 扩展方法）和 `BackendEvent` 流 | 否 | 集成测试，跑真实 `app` |
| `projection/` | 把 `web/src/workspace/*` 移植成 Rust：ACP update 解码（对应 `updateDecoder.ts`）、单会话投影 reducer（`sessionState.ts`）、全局会话目录（`state.ts`）。`recovery.ts` 处理的是 WebSocket 断线重连后的游标续传，进程内连接不会断线重连（断开就按第 12 节重启应用），所以不移植；需要完整 transcript 时一律用 `session/resume` 带 `replayFrom: start` 全量回放。`sessionStaging.ts` 的按帧写时复制是 React 不可变状态带来的，Rust 里直接原地修改，`apply` 返回本次变化的行 key，不移植。输出 UI 无关的 transcript 行模型 | 否 | TDD 单元测试 |
| `workspace/` | 工作区注册表；文件树（`ignore`，遵守 gitignore，按目录懒加载）；Git 状态和 diff（调 `git` CLI：`status --porcelain=v2 -z`、`diff`） | 否 | 单元测试，用临时 git 仓库 |
| `ui/` | 根视图、标题栏和菜单、轨道、侧栏的各种模式、会话卡片和 Tab、diff 和文件 Tab、inspector、对话框、设置页 | 是 | gpui-kit UI 集成测试 |
| `i18n`、`theme`、`settings` | 语言包和 locale 切换；内置主题注册和外观跟随；desktop 设置文件的读写 | 部分 | 单元测试 |

结构遵循规则 15：操作挂在持有上下文的结构体上，例如 `SessionProjection::apply(update)`、`GitStatus::read(&workspace)`，不把同一份上下文在一串自由函数之间传来传去。超过 3 个字段的结构体用 `typed-builder`；`Arc` 字段的克隆写成 `Arc::clone(&..)`。

### 4.4 状态归属

| 状态 | 拥有者 | 持久化 |
| --- | --- | --- |
| 会话数据、transcript、配置 | 后端（JSONL 和 SQLite），desktop 只读其投影 | 后端负责 |
| 工作区列表、展开状态 | `WorkspaceRegistry`（Global） | `desktop.json` |
| 已打开的 Tab、当前 Tab | `AppView` | `desktop.json` |
| 侧栏和 inspector 的宽度、是否打开、轨道模式 | `AppView` | `desktop.json` |
| 语言、外观、主题 | `DesktopSettings`（Global） | `desktop.json` |
| 已 attach 会话的投影、运行状态、待回答的请求 | `SessionHandle` 实体（`SessionRegistry` 持有） | 不持久化，来源是后端 |
| 单个 Tab 的折叠状态、滚动位置、composer 草稿 | `SessionView` 实体（Tab 的 UI） | 不持久化 |

- `desktop.json` 位于 `<config_dir>/clawcode/desktop.json`，带 `version` 字段。解析失败时备份成 `desktop.json.bak`，再按默认值重建，不影响后端数据。恢复的面板宽度按当前窗口大小 clamp。
- 工作区发现：每次启动调用一次 `session/list`（当前后端不分页、不支持 cursor）。结果里出现的 `cwd`，只要既不在注册表里、也不在 `hidden` 集合里，就以折叠状态追加到列表末尾。这样从 WebUI 或命令行创建的会话也能出现在对应的工作区里。「从列表移除」会把路径记进 `hidden`，不碰磁盘上的任何东西。

### 4.5 会话的 attach 生命周期

- `SessionRegistry` 持有所有**已 attach** 的会话（`SessionHandle`：投影、运行状态、待回答的请求）。`SessionView` 是 Tab 的 UI，只引用一个 handle。关掉 Tab 只销毁 `SessionView`，handle 不受影响。
- **打开**：会话第一次打开成 Tab 时 attach，ACP 请求和 WebUI 相同：先用 `_clawcode/session/runtime` 查运行状态，再发 `session/resume` 带 `replayFrom: start` 全量回放（回放前清空投影）。回放结束由 ACP 服务端在回放的历史之后发出的 `replay_complete` 通知标记，客户端收到它才把会话置为已 attach，不靠 resume 响应或时间窗口推断。启动时恢复的 Tab 只立即 attach 当前那一个，其余在第一次切过去时再 attach，避免启动时一口气回放很多会话。
- **关闭 Tab**：空闲会话立即发 `session/close`，释放后端资源（MCP 连接等），然后移除 handle。正在运行的会话不发 close（close 会中止运行），handle 留着继续接收事件，直到 run 结束后再 close。
- **后台会话的请求**：Tab 已关但还在运行的会话如果发来问题、计划审批或权限请求，侧栏这一行显示 `circle-help`。点击后重新打开 Tab，因为 handle 一直在，请求原样停靠在 composer 上方。

### 4.6 远程能力预留

后续要支持 desktop 之间互相控制：经 relay 或 Tailscale 连到另一台机器上的 clawcode（另一台 desktop 内嵌的后端，或 `clawcode serve`）。第一版不实现远程，但从 M1 起遵守下面的约束，保证以后加远程时不需要重写外壳、投影和会话管理。

1. **UI 不依赖后端的具体形态**：`AppView`、`SessionRegistry`、投影只依赖 `AcpClient` 和 `BackendEvent` 流。进程内只是其中一种传输。远程传输只要实现 `ConnectTo<Client>`（WebSocket、Tailscale 上的 TCP、relay 隧道上的 `ByteStreams`），就能复用 `AcpClient::connect`。`Backend`（线程和 `Application`）只负责本机后端的生命周期。
2. **按连接（host）隔离状态**：每个连接各有一套 `AcpClient`、`SessionRegistry`、`SessionCatalog`、连接状态。M1 只有本机一个 host，`AppView` 里已经把这几样收拢成一个按连接组织的结构。以后 `desktop.json` 的工作区条目和 Tab 再加一个 `host` 字段，serde 默认值为本机，旧文件照常读。
3. **工作区文件系统不假设在本机**：文件树、Git 变更、diff 都改为由后端通过新增的 `_clawcode/*` 扩展方法提供，本机工作区也走同一条路（见第 8 节）。只有两样是本机专属：打开本机文件夹选择器、在文件管理器中显示。连接远程 host 时这两项隐藏，「找不到文件夹」也改为由后端判断。
4. **断线重连**：进程内连接不会断线，远程连接会。重连做法是：重新 `initialize`，然后对每个已 attach 的会话重新执行 attach（投影 `clear` 后用 `replayFrom: start` 全量回放）。全量回放已经能保证结果正确；`recovery.ts` 的游标续传等到远程真正落地时再评估，只作为性能优化。
5. **被控端**：`Backend` 持有的 `AcpServerFactory` 可以同时服务多个连接。以后给被控端加网络监听（复用 `http_router`）时必须带认证（配对 token），而且只在 Tailscale 或 relay 接口上监听。WebUI 现在是只监听 loopback、不认证，这条策略不能沿用到远程。
6. **附件按内容传**：图片和附件以 base64 内容发送，不发送本机路径。

## 5. 信息架构与布局

### 5.1 区域

```
┌ 标题栏：[◧] 文件 编辑 视图 帮助            clawcode              – ▢ ✕ ┐
│┌──┐ 侧栏（chrome）  ┌ 主卡片 ───────────────────────┐ ┌ inspector ─┐│
││💬│ 会话   ☰ ⌕      │ [Tab][Tab][+]   运行中 │ ⋯ ◨ │ │ 会话树      ││
││🗀 │ ✎ 新会话        ├───────────────────────────────┤ │ Agents      ││
││⑂ │ 工作区 ▾  +     │      transcript（限宽居中）      │ │ 上下文      ││
││──│  🗀 runner    › │                               │ │            ││
││✦ │  🗁 clawcode  ⌄ │                               │ │            ││
││⌁ │    会话 A  ⟳    │  ┌ composer ────────────────┐ │ │            ││
││  │    会话 B  ?    │  │                          │ │ │            ││
││◉ │ 最近 ▾          │  └──────────────────────────┘ │ │            ││
││⚙ │                 └───────────────────────────────┘ └────────────┘│
│└──┘ ← 轨道卡片                                                       │
└─────────────────────────────────────────────────────────────────────┘
```
| 区域 | 最小 | 默认 | 最大 | 说明 |
| --- | --- | --- | --- | --- |
| 窗口 | 960 × 640 | 1280 × 820 | — | 尺寸和位置都持久化 |
| 轨道卡片 | 固定 48px | | | 独立的窄内容卡片（6.1 第 2 层），左、下各留 8px，上下边和主卡片对齐 |
| 侧栏 | 200px | 248px | 400px | 可拖宽、可折叠（Ctrl+B），宽度持久化 |
| 主卡片 | 480px | 剩余宽度 | — | `flex_1` + `min_w_0` |
| inspector 卡片 | 260px | 300px | 480px | 默认关闭（Ctrl+J）；窗口太窄时先收起 inspector，再收起侧栏 |

表中的 px 都是默认字号（rem = 16px）下的值，实现时换算成 rem，跟着界面字号一起缩放。可拖拽面板的宽度以 px 持久化，恢复时按当前窗口 clamp。

### 5.2 标题栏

- 用 gpui-kit `TitleBar` 作为标题栏容器。Linux 上从左到右依次是：侧栏开关、gpui-kit `AppMenuBar`（文件 / 编辑 / 视图 / 帮助）、居中的窗口标题。窗口控制按钮**只在实际使用客户端装饰时**才画（Wayland CSD）；X11 或合成器提供服务端装饰时不画，避免出现两套按钮。macOS 使用系统菜单栏和红绿灯，标题栏只保留侧栏开关和拖拽区。
- 标题栏的单击不绑定任何编辑命令，整条保持为拖拽区。

### 5.3 轨道

从上到下依次是：**会话**（`messages-square`）· **文件**（`folder`）· **变更**（`git-branch`，有变更时右上角显示数量徽标）· 分隔线 · **Skills**（`sparkles`）· **MCP**（`plug`）· 弹性空白 · **账户与登录**（`circle-user`）· **设置**（`settings-2`）。

轨道是一张单独的窄卡片，表面规则和主卡片相同（见 6.1 第 2 层）：距窗口左边和下边各 8px（spacing `sm`），上边和下边与主卡片对齐；侧栏仍在 chrome 底面上，夹在轨道卡片和主卡片之间。轨道按钮用 `Button::ghost()`（medium），设置固定在卡片底部。

轨道决定侧栏显示什么内容；当前选中的那一项用选中底色标出。设置会打开单例的「设置」Tab，不切换侧栏。还没交付的里程碑对应的轨道项不显示：文件和变更从 M3 起出现，Skills、MCP、账户与登录从 M4 起出现。不放点了没反应的入口。

### 5.4 侧栏的各种模式

- **会话**：
  - 标题行是「会话」加两个按钮（筛选、搜索 Ctrl+K），下面是「新会话」行（Ctrl+N）。
  - 「工作区」分组带折叠箭头和「+」（添加工作区…）。每个工作区一行，图标是文件夹，展开后是打开的文件夹，行尾显示 Git 分支和展开箭头（`chevron-right` / `chevron-down`）。点击整行（含箭头）切换展开；展开状态只有一个来源，即 `desktop.json` 里工作区的 `expanded`，文件夹图标、箭头方向和下面的会话行都由它决定。不用 gpui-kit 子菜单自带的展开状态，它和持久化的状态会不同步。
  - 搜索按钮是开关：点一下打开搜索框并聚焦，再点一下关闭、清空查询并回到普通列表；打开时按钮显示为选中。Ctrl+K 只负责打开或聚焦，不会关闭；搜索框里按 Esc 也会关闭。
  - 展开的工作区在下面缩进列出它的会话（缩进 32px，和工作区名对齐，不画树形引导线），按修改时间倒序，默认显示 5 条，再给一行「显示全部 N 个」。会话行尾显示状态图标或相对时间。
  - 「最近」分组：跨工作区的最近 10 个会话，行尾显示所属工作区名。
  - 子 agent 会话（带 `parentSessionId`）不出现在工作区列表和「最近」里，从父会话的 Agents 面板或邮件卡片进入，打开后作为普通会话 Tab，标题前加 `bot` 图标。
  - 搜索（Ctrl+K）按标题和工作区路径过滤会话；筛选按钮切换排序（修改时间、创建时间）以及是否只看运行中的会话。
  - 右键菜单：会话行有「重命名…」「删除…」；工作区行有「新会话」「在文件管理器中显示」「从列表移除」。Fork 在后端是从某个条目分叉，所以放在会话树和消息上，不放在会话行上。WebUI 没有会话导出，desktop 也不做。
- **文件**：当前工作区（当前 Tab 所属的工作区，没有 Tab 时取侧栏上次选中的工作区）的文件树，用 gpui-kit `Tree`。点击文件打开预览 Tab。
- **变更**：当前工作区的 Git 变更列表，每行是状态字母（M / A / D / R / ?）、文件名和灰色的目录；右上角有刷新按钮。点击打开 diff Tab。
- **Skills / MCP**：当前会话的 Skills 列表和诊断、MCP 服务器状态（重连、OAuth、prompts / resources）。没有活动会话时，显示「打开一个会话后可查看」。

### 5.5 主卡片和 Tab

- 卡片顶栏（42px）左侧是 Tab 条，可以横向滚动和拖拽排序。Tab 有三类：**会话**、**diff**、**文件预览**，外加单例的**设置**。
- 顶栏右侧依次是：运行状态（转圈图标加「运行中 · 1 分 12 秒」，或「等待回答」，或「空闲」）、竖分隔线、`⋯` 会话菜单（重命名… / 关闭 / 删除…）、inspector 开关。
- 关闭一个正在运行的会话 Tab 不会取消运行，只是从 Tab 条移除；侧栏里的状态图标继续显示（attach 规则见 4.5）。退出应用前如果还有运行中的会话，弹 `AlertDialog`：「有 2 个会话正在运行，退出会停止它们」，按钮为「取消」和「退出」。
- 新会话 Tab 的空状态：卡片中间是「在 **clawcode** 里要做什么？」；composer 上方是工作区选择条（文件夹 chip、路径、分支），可以改成其他工作区。发出第一条消息时才真正创建后端会话。

### 5.6 Inspector 卡片

顶部是分段控件「会话树 · Agents · 上下文」，跟随当前会话 Tab。

- **会话树**：分支导航、跳转（可以选择摘要被离开的分支）、从消息 Fork。点击节点时，在 transcript 中定位到对应的行。
- **Agents**：后台子 agent 的状态和邮件，点击打开子会话 Tab。
- **上下文**：上下文用量、缓存命中、计划和待办、后台终端。

## 6. 视觉规范：Inset Workbench

### 6.1 三层表面

1. **chrome 底面**：标题栏、侧栏，统一用 `title_bar` / `sidebar` token，内部不画分隔线。
2. **内容卡片**：轨道卡片、主卡片和 inspector。`background` 加 1px `border`、`radius_lg`。浅色主题加一层极淡的阴影，深色主题只靠描边。卡片之间、卡片到窗口右边和下边都留 8px（spacing `sm`）。卡片的左边缘和侧栏之间也是同一个间距。轨道卡片到窗口左边同样留 8px，上下边和主卡片对齐。
3. **弹出层**：菜单、`Select`、命令面板、`Popover`、`Dialog`，统一用 `popover` token，阴影最强。

卡片内部的嵌套表面（composer、工具分组框）按 `inner radius ≈ outer radius − gap` 选取最接近的主题圆角档。

### 6.2 Icon button

- 只有两档，都直接用 gpui-kit 的组件尺寸，不自定义框高：**轨道**用 `Button::ghost()`（medium，默认字号下是 32px 框）；**工具栏和行内**用 `Button::ghost().small()`（24px 框）。图标大小、内边距、圆角跟着组件尺寸走，不允许出现第三种尺寸。视觉稿里的 34 / 26px 只是近似。
- 图标统一用 Lucide（gpui-kit `IconName`），描边粗细一致。缺少的图标作为 SVG 资源补进来，必须使用同一套图标集。
- 状态：静止时 `muted_foreground`；hover 时 `accent` 底；选中或弹出层打开时 `sidebar_accent` 底加 `foreground`；禁用时降低不透明度，且没有 hover 反馈；键盘焦点显示 focus ring。
- 所有纯图标按钮都有无障碍名称和 tooltip，有快捷键的在 tooltip 里写出来，例如「新会话 Ctrl+N」。

### 6.3 Token 和尺度

- 颜色只能来自 `cx.theme()` 的语义 token。业务代码里不允许出现 hex 或 rgb 字面量。diff 的增删底色用主题的 success / danger token 加透明度，统一由 `theme` 模块提供。
- 间距和字号使用 rem helper（`gap_2`、`p_4`、`text_sm` 等），确保跟随主题 `font_size` 缩放。只有 hairline、窗口内边距和光栅尺寸可以用 `px`，并且要写注释说明原因。
- 字号层级：卡片标题和侧栏标题一档，正文一档，muted 元信息一档，标签一档，不再引入第五档。代码、路径、快捷键、token 数字用等宽字体，其余用系统 UI 字体。
- 选中状态用行自身的底色表达，不加左侧竖条。

## 7. Transcript 渲染规则

### 7.1 容器

- 使用 gpui-kit `MessageScroller`（尾部跟随，加「回到最新」按钮）。每行的 `ElementId` 取领域 key：`message:{messageId}`、`tool:{toolCallId}`、`compaction:{entryId}` 等，与 WebUI 的 `TranscriptRowKeys` 一致。
- 流式更新时只对 `apply` 返回的变化行调用 `splice` / `remeasure_items`。用户已经往上滚的话，不强制跳回底部。
- 助手正文和 thinking 各自保留一个 `TextViewState`（按 messageId 缓存在 `SessionView` 里，第一次渲染时创建），流式片段用 `push_str` 追加，不重新解析整段 Markdown。
- 正文列用 rem 限宽（默认字号下约 680px）并水平居中；左右留白随卡片宽度变化。

### 7.2 行类型

| 行 | 渲染 |
| --- | --- |
| 用户消息 | 右对齐的 muted 气泡，最大宽 78%；图片显示缩略图，点击放大；steering 和 follow-up 加小标签；slash 命令消息显示为紧凑的命令卡 |
| 助手正文 | `TextView` 加保留的 `TextViewState` 渲染 Markdown，代码块高亮，文字可选中复制；不加气泡；流式输出时末尾显示光标效果 |
| thinking | 一行可折叠的「思考 · 8 秒」（`brain` 图标），默认折叠；流式输出时用 `ShimmerText` 显示「思考中…」 |
| 工具调用 | 连续的工具行（中间没有正文或用户消息）合成**一个分组框**：首行、中间行、末行各自负责自己那段圆角和 hairline，所以虚拟列表里的多行拼起来是一个完整的框。每行依次是 ToolKind 图标（read=`file-text`、edit=`pencil`、delete=`trash-2`、move=`arrow-right-left`、search=`search`、execute=`square-terminal`、fetch=`globe`、think=`brain`、其他=`wrench`）、动词、等宽目标、右侧状态（运行中是转圈加耗时，完成是 ✓ 加摘要，edit 显示 `+86 −41`，失败是 ✗，被拦截有专门的标签） |
| 工具展开 | edit 显示 gpui-kit `Diff`；execute 显示流式终端输出（默认只看尾部 20 行，加一个「完整输出」按钮）；其余类型显示参数和结果摘要 |
| Run 折叠 | 本仓库里，一次 **Run** 是从用户 prompt 到 settle 的完整过程，可能包含多个模型 **Turn**。Run 结束后，它所有的工具组合并折成一行「已工作 1 分 28 秒 · 读取 3 · 编辑 2 · 执行 1」，点击后恢复。正在运行的 Run 始终展开 |
| 用户执行的命令 | 后端的 `user_bash` 消息（WebUI 也只渲染、不提供入口）按 execute 工具行渲染，加「你执行的」标签 |
| 压缩 / 分支摘要 | 居中分隔 pill，例如「上下文已压缩 · 182k → 24k」，可展开查看摘要 markdown |
| 用量 | 一个 run 结束后，在最后一条回复下面加一行 muted 文字：「DeepSeek V4 Pro · 输入 12.4k · 输出 1.8k · 缓存命中 71% · 1 分 28 秒」 |
| 扩展事件 / 错误 / 通知 | 行内 info 或 danger 提示；重试中显示倒计时 |
| 子 agent 邮件 | 紧凑卡片（昵称、摘要），点击打开子会话 Tab |

### 7.3 阻塞式交互

- **问题**（单选、多选、自由输入）、**计划审批**（计划 markdown，按钮为「批准」/「继续规划」）、**权限**（选项按钮）停靠在 composer 上方，在会话卡片内部，不弹全局模态框，所以后台会话可以等你切回来再回答。
- 有未回答的请求时，Tab 和侧栏状态显示 `circle-help` 图标，卡片顶栏的状态显示「等待回答」。
- **MCP elicitation** 也先停靠成一张卡片（「example 需要你填写信息」加「填写…」按钮），点击后才打开表单 `Dialog`。后台会话的请求不会突然在别的会话上方弹出模态框。**Provider 登录**是用户主动发起的，直接用 `Dialog`。Esc 关闭最上层弹层，焦点回到触发它的控件。
- 运行中的计划和待办显示在 composer 上方的可折叠面板里；后台终端显示在 inspector 的「上下文」分段。

### 7.4 Composer

- 浮起的大圆角卡片（嵌套圆角规则见 6.1），内部是 gpui-kit `Textarea`。
  - 输入框上方依次是：附件条（gpui-kit `Attachment`，可删除）、待发送消息 chip（可删除，可在「稍后发送」和「本轮补充」之间切换，和 WebUI 一致）、停靠的交互面板。
  - 底栏左侧是附件和计划模式开关，右侧是上下文环（`ProgressCircle`，点击用 Popover 显示明细）、模型和思考级别（`Select`）、发送按钮。运行中发送按钮变成停止按钮。
- 键位：Enter 发送；Shift+Enter 换行；输入法组字时 Enter 只确认候选；空白内容不发送；运行中按 Enter，按后端 `when_busy` 配置进入「稍后发送」或「本轮补充」队列。Esc 按层级处理：先关掉打开着的命令列表、Popover 或 Dialog，没有弹层时才停止运行。
- `/` 打开命令列表（Popover，带过滤和键盘导航），来源包括 builtin、skill、模板、扩展命令；Tab 或 Enter 补全。
- 支持粘贴和拖入图片，作为附件发送。

### 7.5 状态不只靠颜色

会话状态用图标表达：运行中是 `loader-circle`（转动）；等待回答是 `circle-help`；失败是 `circle-alert`；空闲不显示图标，只显示相对时间。每个状态都带一段无障碍文字。

## 8. 工作区工具

- **注册表**：可以添加工作区（文件夹选择对话框，Linux 上依赖 xdg-desktop-portal；也可以拖入文件夹）、从列表移除、拖拽排序、记住展开状态。路径先规范化（canonicalize）；目录已不存在的工作区显示 muted 样式和「找不到文件夹」，并提供「移除」。
- **数据来源**：文件树和变更都由后端通过新增的 ACP 扩展方法提供（`_clawcode/workspace/list_dir`、`_clawcode/workspace/git_status`、`_clawcode/workspace/git_diff`），读文件、跑 `git` 都在后端那台机器上进行。这样本机和远程工作区走同一套 UI（见 4.6）。这几个扩展是新增方法，不改变已有方法的语义。
- **文件树**：后端用 `ignore::WalkBuilder` 按目录懒加载，遵守 `.gitignore`；隐藏文件默认不显示，可以在侧栏标题的按钮里切换。目录变化不做实时监听，切到文件模式和点刷新时重新读取。
  - ponytail：不监听文件系统变化，需要时再加 `notify`。
- **变更**：后端调用 `git -C <ws> status --porcelain=v2 -z`，解析成变更列表；单个文件的 diff 来自 `git diff [--cached] -- <path>`，未跟踪的文件用 `--no-index` 生成，结果交给 gpui-kit `Diff` 渲染。下面三种时机会自动刷新：切到变更模式、这个工作区里任一会话的 Run 结束（Agent 可能改了文件）、点击刷新按钮。不是 Git 仓库或没装 git 时，显示对应的说明文字。
- **只读**：desktop 第一版不提供暂存、提交、撤销等写操作。文件预览也是只读。

## 9. i18n

- 引擎：`rust-i18n`，声明为 `rust_i18n::i18n!("locales", fallback = "en")`，文案放在 `crates/desktop/locales/app.yml`，格式与 gpui-kit 组件库的 `ui.yml` 相同（v2，按 `Area.key` 分组，每个 key 下列出各语言）。gpui-kit 和我们共用 `rust_i18n` 的全局 locale，一次 `set_locale` 同时切换两边。
- 语言设置：「跟随系统」（默认）/ English / 简体中文。跟随系统时用 `sys-locale` 探测，`zh*` 映射为 `zh-CN`，其余映射为 `en`。
- 运行时切换：调用 `set_locale` 后 `refresh_windows`。已经存进实体的文案（输入框 placeholder、日期分组标签）记录取用时的语言代次，代次变化时重新取一次。
- 文案规则遵循 Design Guides：英文用 sentence case；按钮名称写出动作和对象；会打开对话框的命令加 `…`；中文句子用全角标点，短标签不加句号；不使用「确定要……吗？」这类句式。
- agent 输出、文件内容、会话标题保持原文，不翻译。相对时间和 token 数量按 locale 格式化（`2 天` / `2d`、`12.4k`）。

### 9.1 产品术语表

WebUI 已经在用的中文词，以 WebUI 为准，保证两个客户端说同一种话。

| 概念 | English | 简体中文 |
| --- | --- | --- |
| Workspace | Workspace | 工作区 |
| Session | Session | 会话 |
| New session | New session | 新会话 |
| Recent | Recent | 最近 |
| Run | Run | 运行 |
| Tool call | Tool call | 工具调用 |
| Thinking | Thinking | 思考 |
| Plan mode | Plan mode | 计划模式 |
| Compaction | Context compacted | 上下文已压缩 |
| Branch | Branch | 分支 |
| Fork | Fork | Fork |
| Follow-up | Follow-up | 稍后发送 |
| Steering | Steering | 本轮补充 |
| Pending queue | Pending messages | 待发送消息 |
| Changes | Changes | 变更 |
| Files | Files | 文件 |
| Inspector | Inspector | 详情栏 |
| Session tree | Session tree | 会话树 |
| Agents | Agents | Agents |
| Skills / MCP | Skills / MCP | Skills / MCP |
| Running / Waiting / Idle | Running / Waiting for you / Idle | 运行中 / 等待回答 / 空闲 |

## 10. 主题与设置

- **主题来源**：从 gpui-kit 的 `v0.10.0` tag（和依赖的 crate 版本一致，避免主题 JSON 的 schema 不匹配）把 `themes/` 下的 21 个文件（adventure、alduin、asciinema、aurora、ayu、catppuccin、everforest、fahrenheit、flexoki、gruvbox、harper、hybrid、jellybeans、kibble、macos-classic、mellifluous、molokai、solarized、spaceduck、tokyonight、twilight）vendor 到 `crates/desktop/themes/`，用 `include_str!` 编进二进制；启动时调用 `ThemeRegistry::global_mut(cx).load_themes_from_str(..)` 逐个注册，再加上 crate 自带的 Default Light / Dark。`THIRD_PARTY_NOTICES.md` 补上 gpui-kit 主题的 Apache-2.0 声明。
- 升级 gpui-kit 版本时，主题文件也要从对应的 tag 一起重新 vendor。
- **外观设置**：模式为「跟随系统」/ 浅色 / 深色。浅色主题从所有 `mode = light` 的变体里选（默认 Default Light），深色主题从所有 `mode = dark` 的变体里选（默认 Default Dark）。跟随系统时用 `observe_window_appearance` 实时切换。选择后立即生效并写入 `desktop.json`。
- **设置页**：单例 Tab「设置」（Ctrl+,），用 gpui-kit `Settings` 组件做分页。
  - M1：通用（语言）、外观（模式、浅色主题、深色主题、界面字号）。界面字号通过主题的 `font_size` 改 rem；改动后 transcript 要整体 `remeasure`，否则虚拟列表的行高会和内容对不上。
  - M4：补齐工作区（管理列表）、Provider（登录状态）、关于（版本、配置文件路径、数据目录）。
- 设置页里只放 desktop 自己的偏好，后端配置仍以 `claw.toml` 为准，只提供「打开配置文件」按钮。

## 11. 键位

| 操作 | Linux | macOS |
| --- | --- | --- |
| 新会话 | Ctrl+N | ⌘N |
| 添加工作区… | Ctrl+O | ⌘O |
| 关闭 Tab | Ctrl+W | ⌘W |
| 切换到第 n 个 Tab / 下一个 Tab | Ctrl+1…9 / Ctrl+Tab | ⌘1…9 / ⌃Tab |
| 侧栏 / inspector | Ctrl+B / Ctrl+J | ⌘B / ⌘J |
| 搜索会话 | Ctrl+K | ⌘K |
| 文件 / 变更 | Ctrl+Shift+E / Ctrl+Shift+G | ⌘⇧E / ⌘⇧G |
| 聚焦 composer | Ctrl+L | ⌘L |
| 设置 | Ctrl+, | ⌘, |
| 停止运行 | Esc（焦点在 composer 内且没有打开的弹层时） | Esc |

所有快捷键都通过 `actions!` 和 `bind_keys` 绑定在明确的 `key_context` 上，菜单和 tooltip 里显示同一个快捷键。

## 12. 空状态、加载与错误

| 场景 | 表现 |
| --- | --- |
| 后端启动失败（配置错误、缺少密钥等） | 整窗显示一张错误卡片：说明出了什么问题和错误原文，按钮为「打开配置文件」和「重试」 |
| 没有工作区 | 侧栏显示说明文字和「添加工作区…」按钮；主卡片显示新会话的空状态 |
| 会话加载或回放中 | transcript 显示与行结构对应的 `Skeleton` |
| 后台线程意外退出（ACP 连接断开） | 主卡片顶部显示 danger 横幅「后端已停止」，附「重启应用」按钮（重新执行当前可执行文件后退出）。不在进程内重建 Application：上一个实例的 SQLite lease 和内部状态都不可靠。未发送的草稿会提示先复制 |
| 工作区目录不存在 | 侧栏这一行显示 muted 样式和「找不到文件夹」，右键菜单提供「移除」 |
| 不是 Git 仓库 / 没装 git | 变更侧栏显示「此工作区不是 Git 仓库」或「未找到 git」 |
| 搜索无结果 | 「没有匹配的会话」，并提供清除筛选的按钮 |

## 13. 日志

遵循规则 13 和 14：只在生命周期边界（后端启动和停止、ACP 连接建立和断开、会话打开和关闭）、重试、状态切换和错误路径上打日志，宏一律写全限定路径（`tracing::info!` 等）。不记录 prompt 正文和凭据。进程内 ACP 的参数日志沿用 `acp::trace` 已有的脱敏策略。日志由 `app::LoggingFactory` 按 `claw.toml` 的 `[logging]` 安装，输出到 stderr。配置加载失败时用默认过滤级别安装，保证错误卡片之前的日志不会丢。

## 14. 测试与验收

- **`projection/`**：TDD。在 `crates/desktop/tests/` 里用 `crates/acp/tests` 已有的写法（自定义 `ModelFactory` 脚本化模型输出，构建真实 `Kernel` 和 `AcpServerFactory`）产生**真实的 ACP 帧**，再喂给投影。覆盖流式片段合并、乱序、全量回放、工具生命周期、Run 折叠摘要、子 agent 归属。这里的脚本化模型只用于单元和集成测试，不能当验收后端。
- **`workspace/`**：在临时目录里 `git init`，测 porcelain v2 的解析（重命名、未跟踪文件、含空格的路径）、diff 的生成、文件树的 gitignore 过滤。
- **`backend/`**：起真实的 `Application`（临时数据目录），测会话的新建、列表、加载、关闭，以及优雅关闭时的 flush；这部分不调用模型。
- **UI**：`#[gpui_kit::test]` UI 集成测试，覆盖侧栏的工作区分组和展开、Tab 的打开关闭和后台事件路由、主题切换、语言切换、composer 键位（Enter、Shift+Enter、输入法组字）、Esc 关闭弹层。
- **验收**：和规则 9 对 WebUI 的要求一致，只用生产 `app` 后端加已配置的 provider 真机运行，不使用 fixture 后端。在浅色和深色主题、中英文两种语言、最小窗口尺寸、两档界面字号下各截图一次，对照 Design Guides 的审查清单逐项检查。
- 规则 10：测试代码只能放在 crate 级 `tests/` 目录或 `#[cfg(test)] mod tests` 里。

## 15. 里程碑

每个里程碑都能独立使用，单独写实现计划。M1 的量最大，写计划时预计拆成 M1a（Backend、投影、外壳、主题、i18n）和 M1b（transcript 渲染、composer、设置页）两份。

1. **M1 骨架和对话**：`crates/desktop` 和依赖接入；Backend（进程内 ACP、优雅关闭）；`projection/` 的核心（消息、thinking、工具、运行状态、标题）；`SessionRegistry` 和 attach 生命周期；主题注册和外观；i18n；`desktop.json`；Inset 外壳（标题栏和菜单、轨道、会话侧栏的工作区分组和最近、主卡片和会话 Tab）；transcript 回放和流式渲染（含 Run 折叠，历史消息里的图片可以显示）；新会话空状态和工作区选择；发送、取消、follow-up 队列；退出时有运行中会话的确认；设置页的通用和外观。
2. **M2 交互完整**：问题、计划审批、计划和待办面板、权限、MCP elicitation；Slash 命令列表；模型和配置选择；压缩和分支摘要卡片；上下文环和用量行；图片附件。
3. **M3 工作区工具**：后端新增 `_clawcode/workspace/*` 扩展（列目录、Git 状态、diff）；文件侧栏、变更侧栏、diff Tab、文件预览 Tab；工作区管理（排序、移除、找不到文件夹的状态）。
4. **M4 Inspector 和能力**：会话树（跳转、摘要、Fork、定位行）；Agents 面板和子会话 Tab；Skills 和 MCP 侧栏；Provider 登录；设置页的其余分页。

## 16. 非目标

- 分屏并排查看多个会话。
- Git 写操作（暂存、提交、撤销）和文件编辑。
- 远程控制（relay、Tailscale、被控端的网络监听和配对认证）。第一版不实现，但按 4.6 的约束预留。
- Windows 支持、自动更新、安装包和签名。
- 文件系统实时监听。
- 修改 WebUI 或后端已有 ACP 方法的语义。会新增的只有 `AcpTransportKind::InProcess` 标记（M1）、带回放的 `session/resume` 在历史之后发出标准 v2 `replay_complete` 通知（M1）和 `_clawcode/workspace/*` 扩展方法（M3）。desktop 需要的后端能力可以直接在对应模块里补。
- WebUI 的 Events 诊断面板（ACP 原始事件日志）。
- 用户直接执行 shell 命令的入口。后端有 `user_bash` 扩展，但 WebUI 没有入口；desktop 只渲染已有的这类消息。
- 和同一数据目录上的其他 clawcode 进程（例如同时运行的 `clawcode serve`）之间的状态同步。另一个进程正在运行的会话，desktop 看不到它的实时状态；两边同时向同一个会话发 prompt 的行为不做保证。
