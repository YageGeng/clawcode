# N4 桌面设置、登录、消息操作与轨迹实施计划

依据[桌面设计规格](../specs/2026-11-25-desktop-nsh-design.md)第 8 节 N4。按轮次的改动已在 N3 完成。用户确定的顺序是：设置分页、Provider 登录、消息操作、轨迹视图。每一块单独设计、单独验收。本文先写第一部分，后面的部分在开始做时再补。

## 第一部分：设置窗口

**目标**：设置改成一个独立的系统窗口，左侧是分页导航，右侧是当前页内容，共五页：通用、外观、Providers、远程控制、关于。

**已验证的前提**（探针验证，探针文件已删除）：
- `nsh.json` 需要授予 `capabilities.windows: true`。
- 根组件一旦出现 `<Window>`，NSH 就不再打开隐式主窗口。设置窗口改为只在打开时渲染（放在 `If` 里），主窗口照常由 NSH 打开。
- 无头测试的选择器能找到第二个窗口里的元素，`app.window(id)` 能读窗口状态、点关闭按钮。

### 任务 1：原生层提供「关于」页信息

- `crates/config` 新增 `pub fn config_path() -> Result<Option<PathBuf>, ConfigError>`，按 `load()` 的查找顺序返回第一个存在的配置文件。
- `ext/backend` 新增 `#[module] AppInfo`，提供 `about() -> AboutInfo { nsh_version, config_file, data_dir }`：
  - `nsh_version` 取 `nsh_core::element::description::VERSION`；
  - `data_dir` 取配置里 `session` 的 `data_root()`；
  - 配置读不出来时，两个路径都返回 `None`。
- 应用版本用 `appInfo().version`，日志文件用 `process.logFile`，两者都是 NSH 自带的接口。

### 任务 2：主机提供登录方式和状态

- 在 `initialize` 的回复里读取 `authMethods`，保存为 `HostStore.authMethods(): readonly AuthMethod[]`，每项含 `id`、`name`、`description?`。只有本机主机会返回这一项。
- 新增 `HostStore.signInStatuses(): Promise<readonly ProviderSignIn[]>`，请求 `_clawcode/auth/status`，返回每项的 `methodId` 和 `status`（`signedIn`、`signInRequired`、`unknown`）。回复格式不对时抛错。
- 测试：`tests/stores/host.test.ts` 用脚本化的对端回放 `initialize` 和 `auth/status` 的回复。

### 任务 3：设置窗口与五个页面

- `src/settings/SettingsWindow.tsx`：`<Window id="settings">`，大小 860×600，最小 640×480，关闭时调用 `onClose`。左侧导航宽约 200，项目有：通用（settings 图标）、外观（sun）、Providers（shield-check）、远程控制（laptop）、关于（info）。右侧可滚动，最大宽 640，顶部是页标题。
- **通用**：界面语言；「在…中打开」的默认目标，选项来自 `ExternalEditors.targets()`，带应用图标。
- **外观**：亮色、暗色或跟随系统；文件图标主题及动态模式的子选项，从 `SettingsDialog` 原样迁移。
- **Providers**：只列本机主机的登录方式，显示名称和状态徽标。本机没有就绪时显示提示，说明远程主机的凭据在远程电脑上管理。登录按钮在第二部分实现。
- **远程控制**：复用 `Sharing`（远程控制开关、添加设备、已信任设备），「添加设备」的二维码和配对码也在本页展开。「撤销」改用 `confirm()` 确认。别人请求配对时的「允许/拒绝」确认，以及从主机切换器「添加远程电脑」，仍以对话框形式留在主窗口。
- **关于**：应用版本、NSH 版本、配置文件、数据目录、日志文件。每个路径后有「在文件管理器中打开」按钮，调用 `ExternalEditors.open(path, "file-manager")`。
- **`Shell`**：侧栏「设置」改为打开设置窗口，已打开时调用 `WindowRef.focus()`。原来跳到远程控制页的入口（设置里的链接、标题栏的设备数）改为打开设置窗口并切到「远程控制」页。
- **删除**：`SettingsDialog`，以及 `RemoteDialogs` 里的 `sharing` 和 `invite` 两种模式。
- **测试**（无头）：
  - 侧栏打开设置窗口后切换各页；
  - 改界面语言后，主窗口的文案跟着变；
  - 远程控制页的开关和添加设备；
  - 关于页显示应用版本；
  - Providers 页列出登录方式和状态；
  - 关闭窗口后可以再次打开。

### 任务 4：文档与检查

- `apps/desktop/DESIGN.md` 记录设置窗口的布局和交互。
- 全量检查：`nsh test`、`nsh check`、`nsh build`、Clippy、pre-commit。
- 在 KDE Wayland 原生目标上截图：设置窗口亮色英文、暗色中文各一张。

### 第一部分实施记录（2026-11-28）

- **任务 1**：`config::config_path()` 和原生 `AppDetails.about()` 已完成，「关于」页的版本和路径读取正常。这两个函数只做转发，没有单独写单元测试，靠「关于」页的界面测试覆盖。
- **任务 2**：`authMethods` 和 `signInStatuses()` 已完成，测试覆盖了缺少名称的登录方式会被过滤，以及未知状态按「未知」显示。
- **任务 3**：
  - 设置窗口和五个页面已完成；`nsh.json` 已授予 `windows` 权限。
  - `SettingsDialog` 已删除，`RemoteDialogs` 只保留配对请求确认和「添加远程电脑」。
  - 原来依赖「Esc 关闭设置对话框」的两个用例，改为在设置窗口里点关闭按钮。
  - 下拉框宽度测试改为挂载新的外观页。
  - 远程控制页由 `tests/App.test.tsx` 中的 `remote_control_settings_page_shares_and_invites_a_device` 覆盖，用真实应用和离线测试配置运行。
    - 无头测试没有应用数据目录，只检查页面内容（设备名称、开关、已信任设备，以及「数据目录不可用」的提示）。
    - 在 `--target linux` 下跑完整流程：开启远程控制、添加设备并显示二维码和配对码、关闭邀请、关闭远程控制。已通过，并截图 `target/settings-remote.png`。
- **任务 4**：`DESIGN.md` 已补设置窗口的布局和交互说明。桌面 NSH 215 项全部通过，`nsh check` 和 `nsh build` 通过。在 KDE Wayland 原生目标上截了两张图：浅色英文、深色中文，都在 Providers 页。

## 第二部分：Provider 登录

**目标**：在 Providers 页登录本机的模型提供方。发消息或切换模型时，如果因为未登录被拒绝，提示里提供一个「登录」入口。

**后端流程**（见[登录设计](../specs/2026-11-05-acp-provider-login-design.md)第 4.3 节）：
1. 客户端发送 `auth/login { methodId }`。
2. 后端通过 `elicitation/create` 发来验证网址和设备码：`mode: "url"`，范围是这次 `auth/login` 的请求 id，`_meta.clawcode` 里带 `userCode` 和 `methodName`。
3. 登录结束后，后端用 `$/cancel_request` 撤销这次 elicitation，然后回复 `auth/login`。
4. 客户端回复 `decline` 或 `cancel` 表示放弃登录。

后端没有退出登录的接口，所以这次不做「退出」。

### 任务 1：可取消的请求

- `AcpConnection.request()` 新增 `signal?: AbortSignal` 选项。信号中止时，向对端发送 `$/cancel_request { requestId }`，并以取消错误结束本地的 Promise。
- 测试：`tests/client/connection.test.ts`，中止后对端收到 `$/cancel_request`，Promise 被拒绝。

### 任务 2：主机的登录流程

- 在 `initialize` 中声明客户端能力 `elicitation: { url: {} }`。
- `HostStore` 新增：
  - `signIn(methodId)`：发 `auth/login`，不设超时。同一时间只允许一次登录。
  - `cancelSignIn()`：对未回答的 elicitation 回复 `decline`，并取消 `auth/login`。
  - `signInFlow(): { methodId; code?: { url; userCode?; message } } | null`，表示当前登录到了哪一步。
- 处理 `elicitation/create`：URL 模式的请求放进 `signInFlow`，直到被取消；后端撤销时清除，其他模式回复 `cancel`。
- `AcpProtocol.authRequiredMethod(error)`：从 `-32000` 错误的 `data.methodId` 中取出登录方式。发消息失败和改会话设置失败时，如果是未登录，提示带上 `signIn: methodId`。
- 测试：`tests/stores/host.test.ts`，覆盖：
  - 能力声明；
  - 收到设备码后写入 `signInFlow`；
  - 登录成功后清空 `signInFlow`；
  - 取消时回复 `decline` 并发出 `$/cancel_request`；
  - 发消息时遇到未登录错误，提示带上登录方式。

### 任务 3：界面

- **Providers 页**：
  - 未登录的项显示「登录」，登录中显示「等待登录…」和「取消」，状态读不出来时也允许登录。
  - 收到设备码后，在该行下方展开卡片：大号设备码（可复制）、「在浏览器中打开」（`openUrl`，`nsh.json` 授予 `openUrl` 的 `https`）、后端给的说明文字。
  - 登录结束后刷新状态，失败时显示原因。
- **提示**：`ToastMessage` 新增 `action`。带登录方式的提示显示「登录」按钮，点击后打开设置窗口的 Providers 页，并开始这一项的登录。
- **测试**：
  - Providers 页：点登录、看到设备码、取消，以及登录成功后显示「已登录」；
  - 发消息遇到未登录错误时，提示里出现「登录」，点击后打开 Providers 页并开始登录。

### 任务 4：文档与检查

- `apps/desktop/DESIGN.md` 记录登录交互。
- 全量检查。
- 在原生目标上截一张设备码卡片的图。

### 第二部分实施记录（2026-11-28）

- **任务 1**：`AcpConnection.request()` 支持 `signal`，中止时发 `$/cancel_request` 并拒绝 Promise，有测试。
- **任务 2**：
  - `initialize` 声明 `elicitation: { url: {} }`；`HostStore` 新增 `signIn`、`cancelSignIn`、`signInFlow`，处理 `elicitation/create`。
  - 未登录错误的登录方式由 `AcpRequestError.authMethod` 读取（计划里写的是 `AcpProtocol.authRequiredMethod`，放在错误类型上更直接）。
  - `tests/stores/host.test.ts` 覆盖了计划列出的五种情况。
- **任务 3**：
  - Providers 页的登录、设备码卡片、取消、成功后刷新状态均已完成。
  - `ToastManager` 会复制提示内容，函数会丢失，所以按钮的回调由 `ToastStore` 按提示 id 另存。超时消失的提示，回调留在表里，量很小，暂不清理。
  - 远程主机的未登录提示不带「登录」按钮，凭据在那台电脑上管理。
- **任务 4**：`DESIGN.md` 已补登录交互。桌面 NSH 224 项全部通过，`nsh check` 和 `nsh build` 通过。在 KDE Wayland 原生目标上截了设备码卡片：`apps/desktop/target/settings-sign-in.png`。

## 第三部分：消息操作

**目标**：在消息上复制原文、编辑之前的提问重新发送（Branch）、从已结束轮次的最后一条回复 Fork 出新会话。范围沿用旧客户端 M4b 设计第 3 节，后端的 `_clawcode/session/tree`、`branch`、`fork` 已就绪，不改后端。

**行为**：
- 用户消息：气泡下方右对齐，悬停或键盘焦点时显示「复制」「编辑」。助手的最终回复：已有的「复制」和时间后面加「Fork」，只在已结束的轮次出现（运行中的轮次本来就不显示操作）。
- 会话运行中时「编辑」和「Fork」禁用。子 agent 会话只有「复制」。
- **编辑**：输入框换成这条消息的原文，上方出现提示条「正在编辑之前的消息」和「取消」。取消或按 Esc 还原原来的草稿。发送时：
  1. 取 `tree`，找 `kind == "message"`、`payload.message_id` 等于这条消息、`payload.content.type == "user"` 的条目；
  2. `branch { sessionId, entryId: 条目的 parentId }`；
  3. 重新回放会话（`openSession`，后端的 `branch_navigated` 也会触发回放，二者共用同一次）；
  4. 按普通发送流程发出新消息。
  失败时提示原因，输入框保留编辑后的文字并停在编辑状态。原消息的图片不带回（会话里只存了图片数量）。
- **Fork**：取 `tree` 找到这条回复的条目，`fork { sessionId, entryId, cwd }`，刷新会话列表并打开新会话。进行中按钮转圈，失败时提示。

### 任务 1：协议与主机
- `AcpProtocol`：`treeEntry(tree, messageId, role)` 从 `tree` 回复中找条目，返回 `{ entryId, parentId }`。
- `HostStore.editPrompt(sessionId, messageId, draft)` 和 `HostStore.fork(sessionId, messageId): Promise<SessionId>`。
- 测试：`tests/client/acp/tree.test.ts`（找到用户消息返回父条目，包括父条目为根；id 不存在；只匹配 `message`）；`tests/stores/host.test.ts`（编辑的请求顺序、Fork 返回新会话 id 并刷新列表）。

### 任务 2：界面
- `TurnSource` 增加消息操作，`TurnRow` 的用户消息和最终回复显示按钮。
- `Composer` 增加编辑状态和提示条；`SessionView` 持有正在编辑的消息。
- 测试：编辑后发送（transcript 换成新分支）、取消还原草稿、Fork 打开新会话、运行中禁用。

## 第四部分：轨迹视图

**目标**：会话标题行有「对话 / 轨迹」切换。轨迹视图逐轮、逐次请求地展示 agent 循环：请求的模型、首 token 延迟、token 用量、工具调用及耗时、提示词和工具清单变化。范围沿用旧客户端 M4a 设计第 4–7 节，后端的 `_clawcode/session/trajectory` 和 `trajectory/content` 已就绪。

### 任务 1：数据与纯逻辑（`src/client/trajectory.ts`）
- 解码 `TrajectoryPage` 和各种记录；格式不对时抛错。
- `TrajectoryLog`：持有已加载的记录和游标。`tail()`、`earlier()`（`before`）、`refresh()`（`after`，超出 `limit` 时接着取；`branchChanged` 时整体替换）。
- 行模型：轮次标题（第 N 轮，耗时和 token 合计）、请求分隔、用户、助手、工具、压缩、分支摘要、设置、扩展；轮次折叠、每次请求下的工具调用合成一行；汇总（轮次、请求、token、缓存命中率）。
- 时间总览的纯函数：横条、工具泳道贪心分配（最多 3 行，放不下的合并）、缩放平移换算、区间筛选、相邻窄条合并。
- 测试：`tests/client/trajectory.test.ts`。

### 任务 2：视图
- `SessionView` 标题行加分段切换，按会话记住，快捷键 `Ctrl+Shift+T`。轨迹视图保留输入框。
- `src/trajectory/TrajectoryView.tsx`：工具栏（时长模式、轮次折叠、调用折叠、时间总览开关、加载全部、汇总、时间区间和清除）、时间总览（`Canvas`）、记录表（`List`，等高行，列：类型、内容、输入、输出、思考、耗时；首行「加载更早」；末行「进行中」）。
- 时间总览交互：单击选中、悬停浮层、拖动选区间、滚轮缩放、Shift+滚轮平移、双击复原、右键或 Esc 清除区间。
- 显示时在消息完成、工具完成、一轮结束后 150ms 合并补取；运行中末尾显示「进行中」行。跟随底部、「回到最新」。
- 状态：首次加载、空会话、历史库不可用或失败（原因和「重试」）、补取失败（表尾一行和「重试」）。

### 任务 3：记录详情
- 右侧面板新增「记录」标签，显示选中的记录：类型、标题、状态、「上一条」「下一条」。
- 分页：概述、计时、用量、系统提示词、工具、选项、参数、结果、原始内容，按记录类型提供。
- 系统提示词和工具清单按哈希取 `trajectory/content`；提示词变化时提供「完整 / 差异」切换，差异按行计算。

### 任务 4：文档与检查
- `DESIGN.md` 记录消息操作和轨迹视图；全量检查；原生截图。

### 第三部分实施记录（2026-11-28）

- **任务 1**：`AcpProtocol.treeEntry` 和 `HostStore.editPrompt`、`HostStore.fork` 已完成。`editPrompt` 在分支切换、回放完成后就返回，新消息照常发送，不等这一轮结束。用独立的后端副本验证过 `tree` 条目格式（`payload.message_id`、`payload.content.type`）和 `fork`、`branch` 的参数。
- **任务 2**：用户消息的「复制」「编辑」悬停显示，最终回复多了「Fork」。编辑状态和提示条在 `Composer` 中，取消还原最初的草稿。
- **测试**：`tests/session/MessageActions.test.tsx`（复制、取消编辑、编辑后发送、Fork）、`tests/client/acp/protocol.test.ts`（`treeEntry`）。脚本化对端按回放内容生成 `tree`，`branch` 截断回放并发出 `branch_navigated`，`fork` 复制回放。原来按「第一个复制按钮」取回复的两个用例，因为用户消息也有了复制按钮，改为取第二个。

### 第四部分实施记录（2026-11-28）

- **任务 1**：`src/client/trajectory.ts` 完成解码、`TrajectoryLog`（尾页、向前分页、`after` 补取、分支替换）、行模型、汇总和时间总览的纯函数。补取从最早一个还没有结果的工具调用之前开始查，并替换那之后的记录，这样工具结果到达后会更新。字段名对照真实后端的回复核对过。
- **任务 2**：视图、工具栏、时间总览、记录表已完成。时间总览没有用主题的图表色，因为它们是灰色，三条泳道分不开，改用 info、success、warning。
- **任务 3**：记录详情已完成。提示词差异直接用 NSH 的 `DiffView`（传新旧两段文本），没有自己算差异。
- **测试**：`tests/client/trajectory.test.ts`（10 项）、`tests/trajectory/TrajectoryView.test.tsx`（4 项：轮次与汇总、折叠、记录详情、没有历史库）。
- **任务 4**：`DESIGN.md` 已补两部分的交互。桌面 NSH 全部测试通过。原生截图：`target/trajectory.png`、`target/trajectory-prompt.png`、`target/message-edit.png`。
