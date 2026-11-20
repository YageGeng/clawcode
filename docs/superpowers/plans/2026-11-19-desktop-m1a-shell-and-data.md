# Desktop M1a：外壳与数据层 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 新增 `crates/desktop`（gpui-kit 0.10）。做完后应用可以启动；按工作区列出会话；在后台 attach 会话，把 transcript 投影进内存；会话以 Tab 打开和关闭；支持 21 个内置主题和中英文切换；退出时把后端优雅关掉。transcript 的渲染、composer、设置页属于 M1b。

**Architecture:** 后台线程上跑 tokio runtime，构建生产 `app::Application`，再用 `Client.v2()...connect_with(AcpServerFactory::component(InProcess))` 在进程内连上 ACP。UI 拿到一个 `AcpClient`（可 Clone，请求被 spawn 到后台 runtime 执行），以及一条 `BackendEvent` 流。`SessionRegistry` 把事件分批路由到各个 `SessionHandle`，每个 handle 持有一份 `SessionProjection`。`AppView` 渲染 Inset 外壳：chrome 底面（标题栏、轨道、gpui-kit `Sidebar`），加上一张浮起的内容卡片（`TabBar` 和卡片正文）。

**Tech Stack:** Rust 2024 / rustc stable（实测 1.100 可以编译）；gpui-kit 0.10（gpui-pre 0.3.12）；agent-client-protocol 5（`unstable_protocol_v2`）；rust-i18n 4；sys-locale 0.3；tokio；serde；typed-builder。

**Spec:** [Desktop 客户端设计](../specs/2026-11-19-desktop-gpui-kit-design.md)

## Global Constraints

- 新 crate 位于 `crates/desktop`，package 名 `desktop`，二进制名 `clawcode-desktop`。新增的外部依赖只有 `gpui-kit = "0.10"`、`rust-i18n = "4"`、`sys-locale = "0.3"`，放在根 `Cargo.toml` 的新分组 `# gui` 里。`app` 补进根 `[workspace.dependencies]` 的 workspace crates 分组（规则 12）。
- ACP 只用 v2 schema：`agent_client_protocol::v2::schema as wire`，desktop 开启 `unstable_protocol_v2` 和 `unstable_session_set_title` 两个 feature。扩展方法名一律取 `protocol::AcpExtensionMethod::as_str()`，产品 `_meta` 命名空间取 `ProductIdentity::ACP_NAMESPACE`（值为 `"clawcode"`）。
- desktop **不**声明 `sessionUpdateBatch` 能力（服务端因此只发单条帧）；也**不**移植 `recovery.ts`，原因见设计稿 4.3。
- M1a 不注册 agent→client 的请求 handler（权限、elicitation 属于 M2）。生产配置是 `approval = "yolo"`，正常流程不会发出这类请求。
- `desktop.json` 位于 `dirs::config_dir()/ProductIdentity::CONFIG_DIR_NAME/desktop.json`，`version = 1`。文件无法解析时整体改名为 `desktop.json.bak`，然后用默认值。
- 主题文件来自 gpui-kit `v0.10.0` tag 的 `themes/`，共 21 个，注册后连同 Default Light / Dark 一共 38 个主题名。默认主题是 `"Default Light"` / `"Default Dark"`。
- locale tag 只有 `"en"` 和 `"zh-CN"`。跟随系统时，任何以 `zh` 开头的系统 locale 都映射为 `"zh-CN"`，其余映射为 `"en"`。
- 尺寸（默认字号下）：窗口最小 960×640，默认 1280×820；侧栏 200 / 248 / 400px；卡片和窗口边、卡片和侧栏之间的间距 = spacing `sm`；卡片圆角 `radius_lg`。轨道按钮用 `Button::ghost()`，工具栏和行内按钮用 `Button::ghost().small()`，不自定义框高。
- 侧栏：每个工作区默认显示 5 个会话，超出时加一行「显示全部 N 个」；「最近」显示 10 个；带 `parentSessionId` 的子会话不在列表里出现。
- 轨道在 M1a 只显示「会话」这一项。设置页（M1b）、文件和变更（M3）、Skills / MCP / 账户（M4）都还不显示。
- 以下内容留到 M1b，不在 M1a 范围内：
  - 侧栏的筛选按钮（排序、只看运行中）
  - Tab 拖拽排序
  - 卡片顶栏状态里的运行耗时
  - Ctrl+L 和 Ctrl+, 两个快捷键
- 所有颜色取 `cx.theme()` 的 token，间距用 rem helper；不出现 hex 或 rgb 字面量。
- 需要的图标在默认组件图标集之外的，用 `gpui_kit::assets::icon_assets!(ExtraIcons, [MessagesSquare, GitBranch, ListFilter, SquarePen, CircleQuestionMark, X])` 打包，再由应用自己的 `AppAssets: AssetSource` 先查 `ExtraIcons`、查不到再回退到 `gpui_kit::assets::Assets`。
- AGENTS.md 适用：
  - 新函数和非平凡逻辑都要写英文注释。
  - 超过 3 个字段的结构体用 `typed-builder`，`Option` 字段加 `#[builder(default)]`。
  - `Arc` 用 `Arc::clone(&x)` 克隆；不写只接收一个项目类型参数的自由 helper。
  - 测试只放在 `crates/desktop/tests/` 或 `#[cfg(test)] mod tests`。
  - tracing 宏写全路径，用普通格式化参数。
  - 遵循规则 15，用持有上下文的结构体组织操作。
  - Cargo 表按规则 11 分组，并在根表声明。
- clippy 规则：workspace lints 会被 `-Dwarnings` 当作错误，所以非测试代码里不能有 `unwrap`、`panic!`、未检查的切片或下标，一律用 `get` 或 `?`。
- 每个任务负责在 `crates/desktop/src/lib.rs` 里登记自己新增的 `pub mod`（测试需要从 crate 外部访问这些模块）。
- **不按任务提交**。全部改动在用户明确要求后作为一个提交进 `main`，见 Task 12。

## Review Focus

1. **手改坏的 `desktop.json`**：语法错误、未知语言值、未知主题名、宽度为负或 NaN。应用必须照常启动，坏掉的部分回退默认值；整份文件解析失败时，原文件保留为 `.bak`。测试在 Task 2 和 Task 4。
2. **等价路径**：`/a/b`、`/a/b/`、`/a/./b` 必须是同一个工作区；目录已经被删掉的工作区也要照常列出。测试在 Task 2；「找不到文件夹」的标注在 Task 10。
3. **会话元数据缺失或损坏**：没有标题、标题全是空格、时间戳不是数字。这些会话仍然列出来，用「未命名会话」显示，排在有时间戳的会话之后。测试在 Task 6。
4. **事件到达时机不正常**：属于未 attach 会话的事件、Tab 关闭后才到的事件、乱序到达的事件、比消息更早到达的工具更新。都必须正确路由或安全忽略，不能 panic。测试在 Task 7 和 Task 8。
5. **后端启动失败，或启动中途退出**：不能卡住，必须显示错误卡片；关闭流程有超时上限。测试在 Task 5、Task 9、Task 11。

---

### Task 1: Crate 骨架、依赖和 InProcess 传输标记

**Files:**
- Modify: `Cargo.toml`（members 加 `crates/desktop`；`[workspace.dependencies]` 加 `app = { path = "crates/app" }`；新增 `# gui` 分组：`gpui-kit = "0.10"`、`rust-i18n = "4"`、`sys-locale = "0.3"`）
- Modify: `crates/acp/src/trace.rs`（`AcpTransportKind::InProcess`，Display 输出 `"in-process"`）
- Create: `crates/desktop/Cargo.toml`、`crates/desktop/src/lib.rs`、`crates/desktop/src/main.rs`、`crates/desktop/src/app.rs`

**Interfaces:**
- Produces: `desktop::app::run()`（这个任务只打开一个空 Root 窗口，Task 11 再补全）；`acp::AcpTransportKind::InProcess`。

- [ ] **Step 1: 写 `crates/desktop/Cargo.toml`**

`[lints] workspace = true`。`[[bin]] name = "clawcode-desktop", path = "src/main.rs"`。依赖按规则 11 分组：
- workspace crates：`acp`、`app`、`config`、`protocol`
- error：`thiserror`
- tracing & log：`tracing`
- async：`futures`、`tokio`（features `rt-multi-thread`、`macros`、`sync`、`time`）
- serde：`serde`、`serde_json`
- builder：`typed-builder`
- fs：`dirs`
- protocol：`agent-client-protocol`（features `unstable_protocol_v2`、`unstable_session_set_title`）
- gui：`gpui-kit`、`rust-i18n`、`sys-locale`

`[dev-dependencies]`：
- workspace crates：`extension`、`kernel`、`prompt`、`store`、`tools`
- async：`async-trait`、`tokio-util`
- serde：`serde_yaml_ng`
- fs：`tempfile`
- gui：`gpui-kit`（features `test-support`）

- [ ] **Step 2: 加 `InProcess` 变体和 Display 分支**

- [ ] **Step 3: 写最小的 `lib.rs`、`app.rs`、`main.rs`**

`lib.rs` 声明 `pub mod app;`。`app::run()` 调用 `gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(..)`：先 `gpui_kit::init(cx)`，再用 `gpui_kit::open_window(TitleBar::window_options(), ..)` 打开一个只渲染空 `div()` 的视图。`main.rs` 只调用 `desktop::app::run()`。

- [ ] **Step 4: 验证构建**

Run: `cargo build -p desktop && cargo test -p acp --lib`
Expected: 构建成功，acp 单元测试全部通过。

Run: `cargo run -p desktop`
Expected: 打开一个空窗口，关闭窗口后进程退出。

---

### Task 2: `DesktopState` 持久化和 `WorkspaceRegistry`

**Files:**
- Create: `crates/desktop/src/state.rs`、`crates/desktop/src/workspace.rs`
- Test: `crates/desktop/tests/state.rs`、`crates/desktop/tests/workspace.rs`

**Interfaces:**
- Produces（`state.rs`，所有结构体都 `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]` 并加 `#[serde(default)]`）：
  - `pub enum LanguagePreference { #[default] System, #[serde(rename = "en")] En, #[serde(rename = "zh-CN")] ZhCn }`：`System` 序列化为 `"system"`，再加一个 `#[serde(other)]` 变体映射回 `System`，让未知值不会让整份文件解析失败。
  - `pub enum ThemeModePreference { #[default] System, Light, Dark }`：lowercase，同样有 `#[serde(other)]` → `System`。
  - `pub struct AppearancePreference { mode, light_theme: String, dark_theme: String, font_size: f32 }`：用 TypedBuilder；默认值是 `"Default Light"`、`"Default Dark"`、`16.0`。
  - `pub struct LayoutState { sidebar_open: bool, sidebar_width: f32, window_size: Option<(f32, f32)> }`：默认 `true`、`248.0`、`None`。方法 `pub fn sidebar_width(&self) -> f32`，结果 clamp 到 `[200, 400]`，NaN 时返回 248。
  - `pub struct TabsState { open: Vec<String>, active: Option<usize> }`：`open` 里存会话 id。
  - `pub struct DesktopState { version: u32, language, appearance, workspaces: WorkspaceRegistry, layout, tabs }`：TypedBuilder；`pub const VERSION: u32 = 1`；`pub fn default_path() -> Option<PathBuf>`；`pub fn load(path: &Path) -> DesktopState`；`pub fn save(&self, path: &Path) -> std::io::Result<()>`。
- Produces（`workspace.rs`）：
  - `pub struct WorkspaceEntry { pub path: PathBuf, pub expanded: bool }`
  - `pub struct WorkspaceRegistry { entries: Vec<WorkspaceEntry>, hidden: BTreeSet<PathBuf> }`，方法：
    - `pub fn entries(&self) -> &[WorkspaceEntry]`
    - `pub fn add(&mut self, path: &Path) -> bool`
    - `pub fn remove(&mut self, path: &Path)`
    - `pub fn merge_discovered<'a>(&mut self, cwds: impl IntoIterator<Item = &'a Path>)`
    - `pub fn set_expanded(&mut self, path: &Path, expanded: bool)`
    - `pub fn contains(&self, path: &Path) -> bool`
  - 叶子算法 `pub fn normalize(path: &Path) -> PathBuf`：只做词法规整，去掉 `.`、多余和结尾的分隔符，`..` 在词法上弹出前一段；不访问文件系统。

- [ ] **Step 1: 写失败的测试**

```rust
// tests/state.rs
#[test] fn missing_file_loads_defaults()            // load(nonexistent) == DesktopState::default(); language System; appearance.light_theme == "Default Light"
#[test] fn round_trip_preserves_every_field()        // build a non-default state, save, load → equal
#[test] fn corrupt_file_is_backed_up_and_defaults_load() // write "{not json"; load → default; "<dir>/desktop.json.bak" contains "{not json"; desktop.json no longer exists
#[test] fn partial_file_fills_missing_fields()       // {"version":1,"language":"zh-CN"} → language ZhCn, layout default
#[test] fn unknown_enum_values_fall_back_to_system()  // {"language":"fr","appearance":{"mode":"sepia"}} → System / System, no .bak written
#[test] fn sidebar_width_is_clamped()                 // 1000 → 400, 10 → 200, f32::NAN → 248, -5 → 200
#[test] fn save_creates_parent_directory()            // path in a non-existing subdir → saved and loadable
// tests/workspace.rs
#[test] fn add_normalizes_equivalent_paths()          // add "/tmp/w/a", "/tmp/w/a/", "/tmp/w/./a" → entries().len() == 1
#[test] fn remove_hides_from_discovery_until_added_again() // remove(a); merge_discovered([a]) → !contains(a); add(a) → contains(a)
#[test] fn merge_discovered_appends_collapsed_in_first_seen_order() // [b, a, b] into empty → [b, a], both expanded == false
#[test] fn nonexistent_directories_are_kept()        // merge_discovered(["/definitely/missing"]) → contains
#[test] fn add_returns_false_for_existing_workspace()
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test state --test workspace`
Expected: 编译失败，提示 `state` 或 `workspace` 模块不存在。

- [ ] **Step 3: 实现**

- `save`：先写 `desktop.json.tmp`，再 `rename` 覆盖，保证原子替换。
- `load`：读取失败（文件不存在除外）或 JSON 解析失败时，`rename` 成 `.bak`，用 `tracing::warn!` 记下路径和错误，返回默认值。
- `add` 和 `merge_discovered` 都先调用 `normalize`。`add` 会把路径从 `hidden` 里移除，以 `expanded = true` 追加。`merge_discovered` 跳过已存在或在 `hidden` 里的路径，以 `expanded = false` 追加。

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test state --test workspace`
Expected: 全部通过。

---

### Task 3: i18n

**Files:**
- Create: `crates/desktop/locales/app.yml`、`crates/desktop/src/i18n.rs`
- Modify: `crates/desktop/src/lib.rs`（crate 根加 `rust_i18n::i18n!("locales", fallback = "en");`，并声明 `pub mod i18n;`）
- Test: `crates/desktop/tests/i18n.rs`

**Interfaces:**
- Consumes: Task 2 的 `LanguagePreference`。
- Produces：
  - `impl LanguagePreference { pub fn resolve(self, system_locale: Option<&str>) -> &'static str; pub fn apply(self, cx: &mut App) }`
  - `pub struct LocaleGeneration(pub u64)`（实现 `Global`）。`apply` 依次调用 `gpui_kit::component::set_locale(tag)`、让 `LocaleGeneration` 加 1、`cx.refresh_windows()`。
  - 其他代码统一用 `rust_i18n::t!("Area.key")` 取文案。

- [ ] **Step 1: 写 `locales/app.yml`**

格式是 `_version: 2`，与 gpui-kit 的 `ui.yml` 相同。下表列出 M1a 需要的全部 key，`%{x}` 是插值：

| key | en | zh-CN |
| --- | --- | --- |
| `App.title` | Clawcode | Clawcode |
| `Rail.sessions` | Sessions | 会话 |
| `Sidebar.toggle` | Toggle sidebar | 切换侧栏 |
| `Sidebar.new_session` | New session | 新会话 |
| `Sidebar.workspaces` | Workspaces | 工作区 |
| `Sidebar.add_workspace` | Add workspace… | 添加工作区… |
| `Sidebar.recent` | Recent | 最近 |
| `Sidebar.show_all` | Show all %{count} | 显示全部 %{count} 个 |
| `Sidebar.search` | Search sessions | 搜索会话 |
| `Sidebar.no_workspaces` | Add a folder to start sessions in it. | 添加一个文件夹，即可在其中开始会话。 |
| `Sidebar.no_matches` | No matching sessions | 没有匹配的会话 |
| `Sidebar.clear_search` | Clear search | 清除搜索 |
| `Sidebar.untitled` | Untitled session | 未命名会话 |
| `Sidebar.missing_folder` | Folder not found | 找不到文件夹 |
| `Status.running` | Running | 运行中 |
| `Status.waiting` | Waiting for you | 等待回答 |
| `Status.idle` | Idle | 空闲 |
| `Status.failed` | Failed | 失败 |
| `Age.now` | now | 刚刚 |
| `Age.minutes` | %{count}m | %{count} 分钟 |
| `Age.hours` | %{count}h | %{count} 小时 |
| `Age.days` | %{count}d | %{count} 天 |
| `Age.weeks` | %{count}w | %{count} 周 |
| `Menu.file` / `Menu.edit` / `Menu.view` / `Menu.help` | File / Edit / View / Help | 文件 / 编辑 / 视图 / 帮助 |
| `Menu.close_tab` | Close tab | 关闭 Tab |
| `Menu.quit` | Quit | 退出 |
| `Workspace.reveal` | Show in file manager | 在文件管理器中显示 |
| `Workspace.remove` | Remove from list | 从列表移除 |
| `Session.rename` | Rename… | 重命名… |
| `Session.delete` | Delete… | 删除… |
| `Session.close` | Close | 关闭 |
| `Session.more` | More session actions | 更多会话操作 |
| `Dialog.cancel` | Cancel | 取消 |
| `Dialog.rename_title` | Rename session | 重命名会话 |
| `Dialog.rename_action` | Rename | 重命名 |
| `Dialog.delete_title` | Delete “%{title}”? | 删除「%{title}」？ |
| `Dialog.delete_body` | Its history is deleted permanently. | 它的历史记录会被永久删除。 |
| `Dialog.delete_action` | Delete | 删除 |
| `Quit.title_one` | Stop the running session and quit? | 有 1 个会话正在运行，要退出吗？ |
| `Quit.title_other` | Stop %{count} running sessions and quit? | 有 %{count} 个会话正在运行，要退出吗？ |
| `Quit.body` | Quitting stops their runs. | 退出会停止它们的运行。 |
| `Quit.action` | Quit | 退出 |
| `Startup.failed_title` | Couldn’t start the agent backend | 无法启动 Agent 后端 |
| `Startup.open_config` | Open config file | 打开配置文件 |
| `Startup.retry` | Retry | 重试 |
| `Backend.stopped` | The backend stopped. | 后端已停止。 |
| `Backend.restart` | Restart app | 重启应用 |
| `Card.new_session_prompt` | What should we build in %{workspace}? | 在 %{workspace} 里要做什么？ |
| `Card.no_tabs` | Open a session from the sidebar or start a new one. | 从侧栏打开一个会话，或新建一个。 |

- [ ] **Step 2: 写失败的测试**

```rust
#[test] fn resolve_maps_system_locales() {
    use desktop::state::LanguagePreference::*;
    for (pref, sys, want) in [
        (System, Some("zh-CN"), "zh-CN"), (System, Some("zh_TW.UTF-8"), "zh-CN"), (System, Some("zh-Hans"), "zh-CN"),
        (System, Some("en-US"), "en"), (System, Some("fr-FR"), "en"), (System, None, "en"),
        (En, Some("zh-CN"), "en"), (ZhCn, None, "zh-CN"),
    ] { assert_eq!(pref.resolve(sys), want); }
}
#[test] fn every_key_has_both_languages()   // parse locales/app.yml with serde_yaml_ng; every leaf map has non-empty "en" and "zh-CN"
#[gpui_kit::test] fn apply_switches_component_and_app_strings(cx: &mut TestAppContext)
    // cx.update(gpui_kit::init); ZhCn.apply → t!("Sidebar.new_session") == "新会话", gpui_kit::component::locale() == "zh-CN", LocaleGeneration bumped by 1; En.apply → "New session"
```

- [ ] **Step 3: 运行测试，确认失败**

Run: `cargo test -p desktop --test i18n`
Expected: 编译失败，提示 `resolve` / `apply` 不存在。

- [ ] **Step 4: 实现 `resolve` 和 `apply`**

`apply` 读系统 locale 用 `sys_locale::get_locale()`。

- [ ] **Step 5: 运行测试，确认通过**

Run: `cargo test -p desktop --test i18n`
Expected: 全部通过。

---

### Task 4: 内置主题和外观

**Files:**
- Create: `crates/desktop/themes/*.json`（21 个）、`crates/desktop/src/theme.rs`
- Modify: `THIRD_PARTY_NOTICES.md`（加 gpui-kit 主题条目，Apache-2.0，来源 `https://github.com/longbridge/gpui-kit/tree/v0.10.0/themes`）
- Test: `crates/desktop/tests/theme.rs`

**Interfaces:**
- Consumes: Task 2 的 `AppearancePreference`、`ThemeModePreference`。
- Produces：
  - `pub const BUILTIN_THEMES: [&str; 21]`（`include_str!`，按文件名字母序）
  - `pub const DEFAULT_LIGHT_THEME: &str = "Default Light"`、`pub const DEFAULT_DARK_THEME: &str = "Default Dark"`
  - `pub fn register_builtin_themes(cx: &mut App)`：逐个 `ThemeRegistry::global_mut(cx).load_themes_from_str`；某个文件解析失败就 `tracing::error!` 后跳过，不 panic。
  - `impl ThemeModePreference { pub fn resolve(self, appearance: WindowAppearance) -> ThemeMode }`：`System` 时，`Dark` / `VibrantDark` 映射为 `Dark`，其余为 `Light`。
  - `impl AppearancePreference { pub fn apply(&self, appearance: WindowAppearance, cx: &mut App) }`：主题名必须在注册表里存在且 mode 匹配，否则回退到默认主题；`font_size` clamp 到 `[12, 20]`。先用 `Theme::update` 设置 `light_theme`、`dark_theme` 和字号，再 `Theme::change(mode, None, cx)`。

- [ ] **Step 1: vendor 主题文件**

Run:
```bash
git clone --depth 1 --branch v0.10.0 https://github.com/longbridge/gpui-kit /tmp/gpui-kit-v0.10.0
mkdir -p crates/desktop/themes && cp /tmp/gpui-kit-v0.10.0/themes/*.json crates/desktop/themes/
ls crates/desktop/themes | wc -l
```
Expected: `21`

- [ ] **Step 2: 写失败的测试**

```rust
#[gpui_kit::test] fn registers_all_builtin_themes(cx: &mut TestAppContext)
    // init + register → ThemeRegistry::global(cx).themes().len() == 38; contains "Catppuccin Mocha", "Catppuccin Latte", "Tokyo Night", "Gruvbox Light", "macOS Classic Dark"
#[gpui_kit::test] fn dark_preference_applies_selected_dark_theme(cx: &mut TestAppContext)
    // mode Dark, dark_theme "Catppuccin Mocha" → cx.theme().theme_name() == "Catppuccin Mocha" && cx.theme().is_dark()
#[gpui_kit::test] fn system_mode_follows_window_appearance(cx: &mut TestAppContext)
    // mode System, light_theme "Ayu Light": apply(WindowAppearance::Light) → "Ayu Light"; apply(VibrantDark) → "Default Dark"
#[gpui_kit::test] fn unknown_or_wrong_mode_theme_falls_back(cx: &mut TestAppContext)
    // dark_theme "Nope" → "Default Dark"; dark_theme "Catppuccin Latte" (a light theme) → "Default Dark"
```

- [ ] **Step 3: 运行测试，确认失败**

Run: `cargo test -p desktop --test theme`
Expected: 编译失败，提示 `theme` 模块不存在。

- [ ] **Step 4: 实现**

- [ ] **Step 5: 运行测试，确认通过**

Run: `cargo test -p desktop --test theme`
Expected: 全部通过。

---

### Task 5: Backend 和 `AcpClient`

**Files:**
- Create: `crates/desktop/src/backend.rs`（`AcpClient`、`AcpConnection`、`Backend`、`BackendEvent`、`BackendError`、`ExtRequest`）
- Create: `crates/desktop/tests/support/mod.rs`（脚本化 Kernel 测试夹具）
- Test: `crates/desktop/tests/backend.rs`

**Interfaces:**
- Produces：
  - `pub type SessionId = wire::SessionId;`
  - `pub enum BackendEvent { Update(Box<wire::UpdateSessionNotification>), Stopped(Option<String>) }`
  - `#[derive(Debug, thiserror::Error)] pub enum BackendError { Startup(String), Request(String), Stopped }`
  - `#[derive(Clone)] pub struct AcpClient { connection: ConnectionTo<Agent>, runtime: tokio::runtime::Handle }`
    - `pub async fn connect(component: impl ConnectTo<Client> + 'static, events: futures::channel::mpsc::UnboundedSender<BackendEvent>) -> Result<(AcpClient, AcpConnection), BackendError>`：必须在 tokio runtime 里调用。实现上 spawn `Client.v2().on_receive_notification(..).connect_with(component, body)`：notification handler 把每条通知包成 `BackendEvent::Update` 转发出去；body 先发 `InitializeRequest::new(ProtocolVersion::V2, Implementation::new("clawcode-desktop", env!("CARGO_PKG_VERSION")))`，再通过 oneshot 交出 `connection.clone()`，然后等待关闭信号。连接任务结束时发出 `BackendEvent::Stopped(err.map(|e| e.to_string()))`。
    - 请求方法（都是 `async`、返回 `Result<_, BackendError>`；内部做法是 `self.runtime.spawn(conn.send_request(req).block_task())`，然后 await JoinHandle，所以从 GPUI executor 调用也安全）：
      - `list_sessions(&self) -> Vec<wire::SessionInfo>`
      - `new_session(&self, cwd: &Path) -> SessionId`
      - `resume_with_replay(&self, id: &SessionId, cwd: &Path) -> ()`：`replay_from` 设为 `wire::ReplayFrom::Start(..)`
      - `close_session(&self, id: &SessionId) -> ()`
      - `delete_session(&self, id: &SessionId) -> ()`：`wire::DeleteSessionRequest`
      - `set_title(&self, id: &SessionId, title: &str) -> ()`：`wire::SetSessionTitleRequest`
      - `session_running(&self, id: &SessionId) -> bool`：`ExtRequest` 调 `AcpExtensionMethod::SessionRuntime`，参数 `{"sessionId": id}`，响应解成 `protocol::SessionRuntimeSnapshot` 后取 `.running`
      - `prompt(&self, id: &SessionId, text: &str) -> ()`
      - `cancel(&self, id: &SessionId) -> ()`：`wire::CancelSessionNotification`，用 notification 发送
  - `pub struct AcpConnection { shutdown: oneshot::Sender<()>, task: tokio::task::JoinHandle<()> }` 和 `pub async fn close(self)`
  - `ExtRequest { method: &'static str, params: serde_json::Value }`：实现 `JsonRpcMessage` 和 `JsonRpcRequest`，`Response = serde_json::Value`，写法参照 `crates/acp/tests/extension.rs` 里的 `IntegrationRequest`。
  - `pub struct Backend { client: AcpClient, shutdown: Option<oneshot::Sender<()>>, stopped: oneshot::Receiver<()> }`
    - `pub async fn start(config: config::ConfigHandle) -> Result<(Backend, futures::channel::mpsc::UnboundedReceiver<BackendEvent>), BackendError>`：起一个名为 `clawcode-backend` 的 `std::thread`，在里面建 tokio multi-thread runtime，`ApplicationFactory::new(config).build()`。构建失败就把错误文本原样放进 `BackendError::Startup` 返回，线程随即退出。
    - 构建成功后用 `AcpServerFactory::new(Arc::clone(&app.kernel), config.current().app.recovery.max_batch_size)` 和 `AcpClient::connect(server.component(AcpTransportKind::InProcess), ..)` 建立连接，再交回 client，然后 `select!` 等关闭信号或 `kernel.wait_for_shutdown()`。之后依次 `connection.close()`、`app.shutdown()`，最后发 `stopped`。
    - `pub fn client(&self) -> &AcpClient`
    - `pub fn shutdown(self) -> impl Future<Output = ()> + Send`：发关闭信号并等待 `stopped`。超时由调用方负责（见 Task 11）。
  - `tests/support/mod.rs`：`#[derive(TypedBuilder)] pub struct ScriptedBackend { pub kernel: Arc<Kernel>, pub client: AcpClient, pub events: UnboundedReceiver<BackendEvent>, pub runtime: tokio::runtime::Runtime, pub root: TempDir }`（测试代码同样遵守规则 5），加 `pub fn start() -> ScriptedBackend`，内部自建 runtime。Kernel 的构建方式照搬 `crates/acp/tests/plan_mode.rs` 的 `server()`，没有扩展。脚本化模型按最后一条用户消息回答：
    - `"hello"` → 两段 `TextDelta`：`"Hi "`、`"there"`
    - `"think"` → `ReasoningDelta("pondering")`，然后 `TextDelta("done")`
    - `"slow"` → 先睡 400ms，再 `TextDelta("late")`
    - 其他输入 → `TextDelta("ok")`

    另外提供 `pub fn wait_idle(&mut self, id: &SessionId)`，等到该会话的 `StateUpdate::Idle`，上限 5 秒。

- [ ] **Step 1: 写失败的测试**

```rust
#[test] fn new_session_is_listed_with_its_cwd()          // ScriptedBackend::start(); runtime.block_on(client.new_session(root)) ; list_sessions contains id with cwd == root
#[test] fn prompt_streams_updates_until_idle()          // prompt "hello"; collect Update events until Idle; at least one AgentMessageChunk text "Hi "
#[test] fn resume_with_replay_resends_history()        // prompt hello, wait_idle, close_session, drain events, resume_with_replay → events include UserMessage "hello" and AgentMessage "Hi there"
#[test] fn session_running_reports_active_run()         // prompt "slow" → session_running == true; after wait_idle → false
#[test] fn set_title_and_delete_are_reflected_in_list() // set_title "Renamed" → listed title Some("Renamed"); delete → not listed
#[test] fn startup_failure_returns_error_without_hanging() {
    // AppConfig { session_persistence.data_home = tmp, extensions.enabled = [ExtensionId "hook-examples"] }
    // tokio::runtime::Runtime::new().block_on(tokio::time::timeout(10s, Backend::start(ConfigHandle::from_config(config))))
    // → timeout not hit, Err(BackendError::Startup(msg)) and msg contains "hook-examples"
}
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test backend`
Expected: 编译失败，提示 `backend` 模块不存在。

- [ ] **Step 3: 实现 `backend.rs` 和 `tests/support/mod.rs`**

日志按规则 13：启动成功时 `tracing::info!("desktop backend connected")`；启动失败时 `tracing::error!("desktop backend failed to start: {}", error)`；连接结束时 `tracing::info!`，有错误就带上原因。

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test backend`
Expected: 全部通过，每个测试在 5 秒内结束。

---

### Task 6: `SessionCatalog`

**Files:**
- Create: `crates/desktop/src/catalog.rs`
- Test: `crates/desktop/tests/catalog.rs`

**Interfaces:**
- Consumes: Task 2 的 `workspace::normalize`；Task 5 的 `SessionId`。
- Produces：
  - `#[derive(Debug, Clone, PartialEq, TypedBuilder)] pub struct SessionSummary { pub session_id, pub cwd: PathBuf, #[builder(default)] pub title: Option<String>, #[builder(default)] pub created_at_ms: Option<u64>, #[builder(default)] pub modified_at_ms: Option<u64>, #[builder(default)] pub parent_session_id: Option<SessionId> }`
  - `impl From<&wire::SessionInfo> for SessionSummary`：`cwd` 做 normalize；标题去掉首尾空白后为空就记为 `None`。`_meta.clawcode` 里的 `createdAtMs` / `modifiedAtMs` 是十进制字符串，解析失败记为 `None`；`parentSessionId` 读字符串。
  - `#[derive(Default)] pub struct SessionCatalog { sessions: Vec<SessionSummary> }`，实现 `FromIterator<SessionSummary>`。排序规则统一为「`modified_at_ms` 降序，`None` 排在最后，平局按 `session_id` 升序」。方法：
    - `pub fn for_workspace(&self, workspace: &Path) -> Vec<&SessionSummary>`：只返回顶层会话
    - `pub fn recent(&self, limit: usize) -> Vec<&SessionSummary>`：只返回顶层会话
    - `pub fn search(&self, query: &str) -> Vec<&SessionSummary>`：对标题和 cwd 的显示字符串做大小写不敏感的子串匹配；`query` 去空白后为空则返回全部顶层会话
    - `pub fn cwds(&self) -> Vec<&Path>`：去重，按最近顺序
    - `pub fn get(&self, id: &SessionId) -> Option<&SessionSummary>`
    - `pub fn upsert(&mut self, summary: SessionSummary)`
    - `pub fn remove(&mut self, id: &SessionId)`
  - `pub enum RelativeAge { Now, Minutes(u64), Hours(u64), Days(u64), Weeks(u64) }`：`pub fn between(now_ms: u64, then_ms: u64) -> Self`；`pub fn label(self) -> SharedString`，用 `t!("Age.*", count = n)`。

- [ ] **Step 1: 写失败的测试**

```rust
#[test] fn summary_reads_product_meta()   // SessionInfo with meta {"clawcode":{"createdAtMs":"10","modifiedAtMs":"20","parentSessionId":"p"}} → 10, 20, Some("p")
#[test] fn blank_title_and_malformed_meta_fall_back() // title "   " → None; modifiedAtMs "abc" → None
#[test] fn workspace_sessions_exclude_children_and_sort_by_recency()
    // ws /w: s1(mod 5), s2(mod 9), s3(mod None), child c(parent s2) → for_workspace("/w/") == [s2, s1, s3]
#[test] fn recent_spans_workspaces_and_respects_limit() // 12 sessions across 2 workspaces → recent(10).len()==10, ordered
#[test] fn search_matches_title_and_path_case_insensitively() // "KERNEL" matches title "refactor kernel"; "clawcode" matches cwd /x/clawcode
#[test] fn relative_age_buckets() {
    use desktop::catalog::RelativeAge::*;
    let m = 60_000; let h = 60 * m; let d = 24 * h;
    for (delta, want) in [(0, Now), (m - 1, Now), (m, Minutes(1)), (h - 1, Minutes(59)), (h, Hours(1)),
                          (d, Days(1)), (6 * d, Days(6)), (7 * d, Weeks(1))] {
        assert_eq!(RelativeAge::between(1_000_000_000 + delta, 1_000_000_000), want);
    }
    assert_eq!(RelativeAge::between(5, 10), Now); // future timestamps
}
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test catalog`
Expected: 编译失败。

- [ ] **Step 3: 实现**

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test catalog`
Expected: 全部通过。

---

### Task 7: `SessionProjection`

**Files:**
- Create: `crates/desktop/src/projection.rs`
- Test: `crates/desktop/tests/projection.rs`

**Interfaces:**
- Produces：
  - `#[derive(TypedBuilder)] pub struct ProductMeta { #[builder(default)] pub turn_id: Option<String>, #[builder(default)] pub timestamp_ms: Option<u64>, #[builder(default)] pub sequence: Option<u64>, #[builder(default)] pub origin: Option<String>, #[builder(default)] pub blocked: bool }`，实现 `TryFrom<&wire::Meta>`：读取 `meta["clawcode"]`；`blocked` 取自 `disposition.type == "blocked"`。
  - `pub enum Role { User, Assistant }`；`pub enum MessageOrigin { Prompt, UserBash, BranchSummary }`：来自 product meta 的 `origin`，`"user_bash"` 和 `"branch_summary"` 各自对应，其余都是 `Prompt`。
  - `pub struct ImageBlock { pub data: String, pub mime_type: String }`
  - `#[derive(TypedBuilder)] pub struct MessageRow { pub message_id: wire::MessageId, pub role: Role, #[builder(default)] pub turn_id: Option<String>, #[builder(default)] pub text: String, #[builder(default)] pub reasoning: String, #[builder(default)] pub images: Vec<ImageBlock>, #[builder(default)] pub streaming: bool, #[builder(default)] pub timestamp_ms: Option<u64>, #[builder(default = MessageOrigin::Prompt)] pub origin: MessageOrigin }`
  - `pub enum ToolStatus { Pending, Running, Completed, Failed, Cancelled, Blocked }`
  - `#[derive(TypedBuilder)] pub struct ToolRow { pub tool_call_id: wire::ToolCallId, #[builder(default)] pub turn_id: Option<String>, #[builder(default)] pub name: Option<String>, pub title: String, #[builder(default)] pub kind: Option<wire::ToolKind>, pub status: ToolStatus, #[builder(default)] pub raw_input: Option<serde_json::Value>, #[builder(default)] pub output: String, #[builder(default)] pub timestamp_ms: Option<u64> }`
  - `pub enum TranscriptRow { Message(MessageRow), Tool(ToolRow) }`；`impl TranscriptRow { pub fn key(&self) -> RowKey }`
  - `#[derive(Clone, Debug, PartialEq, Eq, Hash)] pub enum RowKey { Message(wire::MessageId), Tool(wire::ToolCallId) }`；`impl Display`，输出 `message:{id}` / `tool:{id}`，M1b 用它做 `ElementId`。
  - `pub enum RunState { Idle, Running, RequiresAction }`
  - `#[derive(Default)] pub struct ProjectionChange { pub inserted: Vec<RowKey>, pub updated: Vec<RowKey>, pub run_state: bool, pub title: bool }`，方法有 `pub fn is_empty(&self) -> bool` 和 `pub fn extend(&mut self, other: ProjectionChange)`。`extend` 合并两份变化：同一个 key 若已在 `inserted` 里，就不再出现在 `updated` 里，两边都去重。之所以用 key 而不用下标，是因为一批事件里先插入的行会让后面的下标失效。M1b 的消费方在一批事件处理完后，按 `index_of` 得到的当前下标升序依次 splice，就能还原插入顺序。
  - `#[derive(Default)] pub struct SessionProjection`（字段私有，包括 rows、按 key 的索引、run_state、title、到达计数、已见到的最大 sequence），方法：
    - `pub fn len(&self) -> usize`、`pub fn row(&self, ix: usize) -> Option<&TranscriptRow>`、`pub fn index_of(&self, key: &RowKey) -> Option<usize>`、`pub fn run_state(&self) -> RunState`、`pub fn title(&self) -> Option<&str>`
    - `pub fn clear(&mut self)`：清空所有行，保留 `run_state` 和 `title`
    - `pub fn apply(&mut self, update: &wire::SessionUpdate) -> ProjectionChange`

**排序规则**（一个具体算法）：新行的排序键是 `(sequence, arrival)`。`sequence` 取 product meta 的值；缺失时用「目前见过的最大 sequence」，这样没有 meta 的行会落在已有行之后。`arrival` 是每次 `apply` 自增的计数。用 `partition_point` 找插入位置，然后重建插入点之后的索引。已有行被更新时位置不动。

**各变体的语义**（照搬 `web/src/workspace/updateDecoder.ts`）：

| update | 处理 |
| --- | --- |
| `UserMessageChunk` / `AgentMessageChunk` | 按 `message_id` 找行，没有就新建（role 对应，`streaming = true`）。`Text` 追加到 `text`，`Image` 追加到 `images` |
| `AgentThoughtChunk` | 同上，但追加到 assistant 行的 `reasoning` |
| `UserMessage` / `AgentMessage` | upsert。`content` 有值时，`text` 和 `images` 整体替换（所有文本块拼接）；`streaming = false`；从 meta 写入 `timestamp_ms`、`turn_id`、`origin` |
| `AgentThought` | `content` 有值时整体替换 `reasoning` |
| `ToolCallUpdate` | upsert，只合并有值的字段。status 映射：`pending` / `paused` → Pending，`in_progress` → Running，`completed` → Completed，`cancelled` → Cancelled，`failed` → 如果 meta `blocked` 或 `raw_output.details.type == "blocked"` 则为 Blocked，否则 Failed。`content` 有值时，`output` = 其中所有文本块拼接 |
| `ToolCallContentChunk` | 文本追加到 `output`；行不存在就先建一行（`title` 用 tool_call_id，status 为 Running） |
| `StateUpdate` | `Running` / `Idle` / `RequiresAction` 更新 `run_state`；变成 Idle 时，所有 `streaming` 行改为 false 并报为 Updated；`Unknown` 忽略 |
| `SessionInfoUpdate` | `title` 为 Value 时，去空白后为空则设为 `None`，否则 `Some`；为 Null 时设为 `None` |
| 其他 | 返回空的 change |

- [ ] **Step 1: 写失败的测试**

测试里用 `serde_json::from_value::<wire::SessionUpdate>(json!({...}))` 构造输入，并写一个 helper：`meta(seq) = {"clawcode":{"turnId":"t","timestampMs":"1","sequence":seq}}`。

```rust
#[test] fn agent_chunks_accumulate_into_one_streaming_row()       // two agent_message_chunk "Hi ","there" same messageId → len 1, text "Hi there", streaming; first change.inserted == [key], second change.updated == [key]
#[test] fn final_message_replaces_streamed_text_and_stops_streaming() // chunks then agent_message content "Hi there!" → text "Hi there!", !streaming
#[test] fn thought_chunks_attach_to_the_assistant_row()          // agent_thought_chunk "a","b" then agent_message_chunk → one row, reasoning "ab"
#[test] fn tool_updates_merge_fields_and_map_status()           // pending(title "Read", kind "read") → in_progress → completed with content text "42 lines" → title kept, status Completed, output "42 lines"
#[test] fn blocked_failure_is_reported_as_blocked()             // status failed + _meta.clawcode.disposition.type "blocked" → Blocked
#[test] fn tool_content_chunks_append_output_even_before_the_tool_row() // tool_call_content_chunk for unknown id → row created, Running, output appended
#[test] fn rows_order_by_kernel_sequence_even_when_out_of_order() // msg seq 2 applied before msg seq 1 → row(0) is seq 1; index_of(late key) == Some(0)
#[test] fn extend_keeps_inserted_keys_out_of_updated()          // a.inserted [k1] extended with b.updated [k1, k2] → inserted [k1], updated [k2]
#[test] fn rows_without_meta_follow_existing_rows()
#[test] fn idle_state_clears_streaming_flags_and_reports_run_state() // running → chunk → idle: run_state Idle, row !streaming, change.run_state
#[test] fn title_update_sets_and_clears_title()                  // "  Plan  " → Some("Plan"); "   " → None; null → None
#[test] fn unrelated_updates_change_nothing()                    // available_commands_update → is_empty()
#[test] fn clear_drops_rows_but_keeps_run_state_and_title()
#[test] fn replayed_scripted_run_rebuilds_the_same_transcript()  // ScriptedBackend: prompt "think", wait_idle; live projection via apply; then close + resume_with_replay into a fresh projection → both have [User "think", Assistant text "done" reasoning "pondering"]
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test projection`
Expected: 编译失败。

- [ ] **Step 3: 实现**

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test projection`
Expected: 全部通过。

---

### Task 8: `SessionRegistry` 和 attach 生命周期

**Files:**
- Create: `crates/desktop/src/session.rs`
- Test: `crates/desktop/tests/session.rs`

**Interfaces:**
- Consumes: `AcpClient`、`BackendEvent`（Task 5）；`SessionSummary`（Task 6）；`SessionProjection`、`ProjectionChange`、`RunState`（Task 7）。
- Produces：
  - `pub enum AttachPhase { Attaching, Attached, Failed(String) }`
  - `pub enum SessionStatus { Idle, Running, Waiting, Failed }`：`Running` ← `RunState::Running`；`Waiting` ← `RunState::RequiresAction`；`Failed` ← `AttachPhase::Failed`；其余为 `Idle`。
  - `pub struct SessionHandle { id, cwd, projection: SessionProjection, phase: AttachPhase, close_when_idle: bool }`（TypedBuilder），提供只读方法 `id()`、`cwd()`、`projection()`、`phase()`、`status()`；`impl EventEmitter<ProjectionChange>`。
  - `pub enum RegistryEvent { BackendStopped(Option<String>), Released(SessionId) }`
  - `pub struct SessionRegistry { client: AcpClient, handles: HashMap<SessionId, Entity<SessionHandle>> }`；`impl EventEmitter<RegistryEvent>`。方法：
    - `pub fn new(client: AcpClient) -> Self`
    - `pub fn handle(&self, id: &SessionId) -> Option<Entity<SessionHandle>>`
    - `pub fn attach(&mut self, summary: &SessionSummary, cx: &mut Context<Self>) -> Entity<SessionHandle>`：已有 handle 就直接返回；否则新建一个（phase 为 Attaching），然后 `cx.spawn` 依次执行 `session_running`（写入 run_state）→ `projection.clear()` → `resume_with_replay`。成功置为 `Attached`，失败置为 `Failed(err)`。
    - `pub fn apply_batch(&mut self, events: Vec<BackendEvent>, cx: &mut Context<Self>)`：按 `session_id` 找 handle，找不到就丢弃这条事件。对每个 handle，把这一批的变化合并成一个 `ProjectionChange`，emit 一次，`notify` 一次。`Stopped` 转成 `RegistryEvent::BackendStopped`。批处理完后，凡是 `close_when_idle` 且已经 Idle 的 handle，发 close 并移除。
    - `pub fn release(&mut self, id: &SessionId, cx: &mut Context<Self>)`：Idle 或 Failed 的会话立即 `close_session`、移除 handle、emit `Released`；Running 或 Waiting 的会话只设 `close_when_idle = true`。
    - `pub fn running_count(&self, cx: &App) -> usize`
    - `pub fn pump(registry: WeakEntity<Self>, events: UnboundedReceiver<BackendEvent>, cx: &mut App) -> Task<()>`：每次取一条事件，再用 `try_next` 把通道里已经到达的全部取走，一起交给 `apply_batch`；registry 被释放后结束。

- [ ] **Step 1: 写失败的测试**

`#[gpui_kit::test]` 用的是 TestAppContext；ScriptedBackend 跑在它自己的 tokio runtime 上。等待真实线程时，先 `cx.executor().allow_parking()`，再按 `references/gpui/test.md` 里「bounded condition」的写法加 5 秒上限。

```rust
#[gpui_kit::test] async fn attach_replays_history_into_projection(cx)   // client.prompt "hello" + wait_idle beforehand; attach → eventually Attached and rows [User "hello", Assistant "Hi there"]
#[gpui_kit::test] async fn live_updates_reach_an_attached_session(cx)  // attach idle session; prompt "hello" → eventually assistant row "Hi there", status back to Idle
#[gpui_kit::test] async fn events_for_unknown_sessions_are_ignored(cx) // apply_batch with an Update for "nope" → no panic, handle("nope") is None
#[gpui_kit::test] async fn releasing_an_idle_session_closes_it(cx)      // release → handle None, Released emitted
#[gpui_kit::test] async fn releasing_a_running_session_waits_for_idle(cx) // prompt "slow", release while Running → handle still Some; after Idle → None
#[gpui_kit::test] async fn backend_stop_is_reported(cx)                 // apply_batch [Stopped(Some("x"))] → RegistryEvent::BackendStopped(Some("x"))
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test session`
Expected: 编译失败。

- [ ] **Step 3: 实现**

attach 失败时 `tracing::warn!`，带会话 id 和错误；不对每一条事件打日志。

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test session`
Expected: 全部通过。

---

### Task 9: `AppView` Inset 外壳

**Files:**
- Create: `crates/desktop/src/actions.rs`、`crates/desktop/src/assets.rs`、`crates/desktop/src/ui/mod.rs`、`ui/title_bar.rs`、`ui/rail.rs`、`ui/sidebar.rs`、`ui/card.rs`、`ui/status.rs`
- Test: `crates/desktop/tests/shell.rs`

**Interfaces:**
- Consumes: Task 2 到 Task 8 的全部产出。
- Produces：
  - `actions.rs`：`actions!(desktop, [NewSession, CloseTab, NextTab, ToggleSidebar, FocusSearch, Quit])`，`#[derive(Clone, PartialEq, Action)] pub struct SelectTab { pub index: usize }`。`pub fn bind_keys(cx: &mut App)` 绑定 `secondary-n` / `secondary-w` / `ctrl-tab` / `secondary-b` / `secondary-k` / `secondary-q` / `secondary-1` 到 `secondary-9`，key context 为 `"Workspace"`。`FocusSearch` 打开侧栏搜索框并让它获得焦点。
  - `assets.rs`：`icon_assets!` 生成的 `ExtraIcons`（列表见 Global Constraints），以及 `pub struct AppAssets`（实现 `AssetSource`，写法参照 gpui-kit-assets 的 `examples/extra_assets.rs`）。
  - `ui/status.rs`：`#[derive(IntoElement)] pub struct StatusIcon { status: SessionStatus }`（RenderOnce）。Running 用 `LoaderCircle` 加旋转动画，Waiting 用 `CircleQuestionMark`，Failed 用 `CircleAlert`，Idle 不渲染；每种状态都带 `t!("Status.*")` 作为 aria label。
  - `pub enum TabEntry { Session(SessionId), Draft { workspace: Option<PathBuf> } }`
  - `pub enum BackendPhase { Starting, Ready, Failed(String), Stopped(Option<String>) }`
  - `pub struct AppView`：私有字段包括 `state: DesktopState`、`catalog: SessionCatalog`、`registry: Option<Entity<SessionRegistry>>`、`client: Option<AcpClient>`、`phase: BackendPhase`、`tabs: Vec<TabEntry>`、`active_tab: Option<usize>`、`search: Entity<InputState>`、`show_all: HashSet<PathBuf>`、`sidebar: Entity<ResizableState>`、`menu_bar: Entity<AppMenuBar>`、`focus_handle`，以及各个 `_subscriptions`。方法：
    - `pub fn new(state: DesktopState, state_path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self`：字段多于 3 个，内部用 TypedBuilder 构造（规则 5）。测试传入临时目录下的 `desktop.json` 路径。
    - `pub fn set_backend(&mut self, result: Result<(AcpClient, UnboundedReceiver<BackendEvent>), BackendError>, cx: &mut Context<Self>)`：Ok 时建 registry、启动 `SessionRegistry::pump`、订阅 `RegistryEvent`，并调用 `refresh_catalog`。Err 时 phase 设为 `Failed(err.to_string())`。
    - `pub fn refresh_catalog(&mut self, cx: &mut Context<Self>)`：`list_sessions`，然后 `replace_catalog`。
    - `pub fn replace_catalog(&mut self, catalog: SessionCatalog, cx: &mut Context<Self>)`：执行 `state.workspaces.merge_discovered(catalog.cwds())`；去掉 `state.tabs.open` 里已经不在 catalog 中的 id；重建 tabs，然后只 attach 当前激活的那个 Tab。
    - `pub fn open_session(&mut self, id: &SessionId, cx: &mut Context<Self>)`：已经打开就激活；否则追加 Tab 并激活。registry 已存在时立即 attach；不存在时只开 Tab，等 `set_backend` 之后由 `replace_catalog` 去 attach 当前 Tab。
    - `pub fn open_draft(&mut self, workspace: Option<PathBuf>, cx: &mut Context<Self>)`
    - `pub fn close_tab(&mut self, index: usize, cx: &mut Context<Self>)`：会话 Tab 要先 `registry.release`；激活的 Tab 被关掉后，优先激活右边的邻居，没有就激活左边的。
    - `pub fn state(&self) -> &DesktopState`、`pub fn tabs(&self) -> &[TabEntry]`、`pub fn active_tab(&self) -> Option<usize>`
    - 每次改动 `state` 都调用 `fn persist(&mut self, cx)`，在 Task 11 实现；这个任务里先只更新内存中的 state。

**渲染结构**（gpui-kit 组件；ElementId 都用领域 id，不用下标）：
- 根节点 `v_flex().key_context("Workspace")`，背景用 `cx.theme().sidebar`。
  1. `TitleBar::new()`：侧栏开关 `Button::new("toggle-sidebar").ghost().small().icon(IconName::PanelLeft)`，tooltip 写成 `t!("Sidebar.toggle")` 加快捷键；非 macOS 平台再放 `menu_bar`；`t!("App.title")` 居中。
  2. 主体 `h_flex`：
     - 轨道：`Button::new("rail-sessions").ghost().icon(MessagesSquare).selected(true)`
     - `h_resizable("sidebar-split").with_state(&self.sidebar)`：
       - 侧栏面板：`size(state.layout.sidebar_width())`、`size_range(200..400)`、`visible(sidebar_open)`，内容见下面的「侧栏」
       - 主面板：`v_flex` 内包卡片，卡片 `pr/pb` 留 `sm`。卡片样式：`bg(theme.background)`、`border_1()`、`border_color(theme.border)`、`rounded(theme.radius_lg)`，非深色主题时加 `shadow_xs()`。
- **侧栏**用 gpui-kit `Sidebar`，结构如下：
  - `SidebarHeader`：标题 `t!("Rail.sessions")`，加搜索按钮 `ghost().small().icon(Search)`，点击后在标题下方显示 `Input::new(&self.search)`。
  - `SidebarMenu`：「新会话」项，图标 `SquarePen`，后缀是快捷键 `Kbd`。
  - `SidebarGroup::new(t!("Sidebar.workspaces")).suffix(添加按钮)`：每个工作区一个 `SidebarMenuItem`，图标展开时为 `FolderOpen`、收起时为 `Folder`，`default_open(expanded)`。`children` 是 `catalog.for_workspace(path)` 的前 5 项；还有更多时加一行 `t!("Sidebar.show_all", count)`，点击后把该路径加进 `show_all`。会话项的 `accessibility_label` 是标题（为空就用 `t!("Sidebar.untitled")`），后缀是 `StatusIcon`；Idle 时显示 `RelativeAge` 的 label；当前激活的会话设 `active(true)`。
  - `SidebarGroup::new(t!("Sidebar.recent"))`：`catalog.recent(10)`，每项后缀是所属工作区的目录名。
  - 搜索框非空时，只显示一个按 `catalog.search(q)` 结果排列的平铺列表；没有匹配时显示 `t!("Sidebar.no_matches")` 和一个「清除搜索」按钮。
  - 没有任何工作区时，显示 `t!("Sidebar.no_workspaces")` 和一个「添加工作区…」按钮（按钮行为在 Task 10 接上）。
  - 用 `cx.observe_global::<LocaleGeneration>` 监听语言切换：重设搜索框的 placeholder，并调 `menu_bar.update(|bar, cx| bar.reload(cx))`。菜单本身的重建（`cx.set_menus`）在 Task 11 接上。
- **卡片顶栏**（高 `h_10`，默认字号下为 40px，是设计稿 42px 的 rem 近似）：
  - 左边 `TabBar::new("tabs").pill().selected_index(active)`。每个 `Tab` 的 prefix 是 `StatusIcon`，label 是会话标题或 `t!("Sidebar.new_session")`，suffix 是关闭按钮 `ghost().xsmall().icon(X)`，id 为 `close-tab:{id}`。
  - 右边是状态文字（`t!("Status.*")`）、一条竖分隔线、`⋯` 按钮（`Ellipsis`，id 为 `session-menu`；菜单在 Task 10 接上）。
- **卡片正文**：
  - `BackendPhase::Failed(msg)` 时显示一张错误卡片，id 为 `startup-error`，内容是 `t!("Startup.failed_title")` 加原始 `msg`，以及两个按钮：`open-config`（`t!("Startup.open_config")`）和 `retry`（`t!("Startup.retry")`），两者在 Task 11 接上。
  - 没有 Tab 时显示 `t!("Card.no_tabs")`。
  - Draft Tab 显示 `t!("Card.new_session_prompt", workspace = 目录名)`。
  - 会话 Tab：phase 为 Attaching 时显示 3 行 `Skeleton`；Attached 时正文留空，等 M1b 渲染；`Failed(e)` 时显示 danger 色的 `e`。

- [ ] **Step 1: 写失败的 UI 集成测试**

用 `gpui_kit::open_window` 打开一个 1280×820 的 headless 窗口。会话数据用 `replace_catalog` 注入，不需要后端。

```rust
#[gpui_kit::test] fn sidebar_groups_sessions_by_workspace(cx)
    // state.workspaces add /w/a, /w/b; catalog: 7 sessions in /w/a, 1 in /w/b, 1 child → find_by_label("Show all 7") exists; titles of 5 newest /w/a sessions present; child title absent
#[gpui_kit::test] fn clicking_a_session_opens_one_tab_and_reuses_it(cx)
    // click the row labelled "refactor kernel" twice → tabs() == [Session(id)], active_tab Some(0)
#[gpui_kit::test] fn close_tab_activates_the_right_neighbor(cx)
    // open s1, s2, s3; activate s2; press "ctrl-w" → tabs [s1, s3], active is s3
#[gpui_kit::test] fn secondary_b_toggles_the_sidebar(cx) // press → state().layout.sidebar_open false; press → true
#[gpui_kit::test] fn new_session_opens_a_draft_for_the_selected_workspace(cx) // press "ctrl-n" → last tab is Draft; card shows prompt containing workspace name
#[gpui_kit::test] fn startup_failure_shows_the_error_card(cx)
    // set_backend(Err(BackendError::Startup("config missing".into()))) → find("startup-error") visible; find_by_label("config missing") exists; find("retry") exists
#[gpui_kit::test] fn search_filters_sessions_and_reports_no_matches(cx)
#[gpui_kit::test] fn language_switch_relabels_the_shell(cx) // LanguagePreference::ZhCn.apply → find_by_label("新会话") exists
#[gpui_kit::test] fn blank_titles_render_as_untitled(cx)     // session with title None → find_by_label("Untitled session") exists
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test shell`
Expected: 编译失败。

- [ ] **Step 3: 实现 `actions.rs`、`assets.rs`、`ui/*`**

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test shell`
Expected: 全部通过。

---

### Task 10: 侧栏操作：工作区的增删和定位、会话重命名和删除

**Files:**
- Modify: `crates/desktop/src/ui/sidebar.rs`、`crates/desktop/src/ui/card.rs`、`crates/desktop/src/ui/mod.rs`
- Create: `crates/desktop/src/ui/dialogs.rs`
- Test: `crates/desktop/tests/shell.rs`（追加）

**Interfaces:**
- Consumes: Task 9 的 `AppView`；Task 5 的 `AcpClient::{set_title, delete_session}`。
- Produces（`AppView` 新增方法）：
  - `pub fn add_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>)`：调用 `cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some(t!("Sidebar.add_workspace").into()) })`，选到目录后 `state.workspaces.add`。
  - `pub fn remove_workspace(&mut self, path: &Path, cx)`：可逆操作，所以不弹确认框。
  - `pub fn reveal_workspace(&self, path: &Path, cx)`：`cx.reveal_path(path)`
  - `pub fn rename_session(&mut self, id: &SessionId, window, cx)`：用 `window.open_dialog` 打开对话框。标题 `t!("Dialog.rename_title")`；`Input` 预填当前标题，Enter 等同于点「重命名」；按钮为 `Dialog.cancel` 和 `Dialog.rename_action`（primary）。提交时调 `client.set_title`，成功后更新 catalog。
  - `pub fn delete_session(&mut self, id: &SessionId, window, cx)`：用 `window.open_alert_dialog` 打开确认框，标题 `t!("Dialog.delete_title", title)`，正文 `t!("Dialog.delete_body")`，按钮为 `Dialog.cancel` 和 `Dialog.delete_action`（danger）。确认后依次：`client.delete_session`、关闭对应 Tab、`registry.release`、`catalog.remove`。
- 右键菜单（`SidebarMenuItem::context_menu`）：
  - 工作区行：`Sidebar.new_session`（调 `open_draft(Some(path))`）、`Workspace.reveal`、`Workspace.remove`
  - 会话行：`Session.rename`、`Session.delete`
- 卡片 `⋯` 按钮用 `DropdownMenu`，菜单项为 `Session.rename`、`Session.close`、`Session.delete`，作用于当前激活的会话 Tab。
- 「找不到文件夹」：在 `replace_catalog` 和 `add_workspace` 的时候算一次 `missing: HashSet<PathBuf>`，用 `!path.is_dir()` 判断，不在 render 里访问文件系统。缺失的工作区行文字用 muted 色，后缀显示 `t!("Sidebar.missing_folder")`。

- [ ] **Step 1: 写失败的测试**

```rust
#[gpui_kit::test] fn removing_a_workspace_survives_rediscovery(cx)
    // right_click workspace row "/w/a" → click menu item "Remove from list" → row gone; replace_catalog with the same sessions → still gone; state().workspaces.contains("/w/a") == false
#[gpui_kit::test] fn missing_workspace_folder_is_marked(cx) // workspace "/definitely/missing" → find_by_label("Folder not found") exists
#[gpui_kit::test] async fn rename_dialog_updates_the_title(cx)
    // ScriptedBackend + set_backend(Ok); session "hello"; open rename dialog, input "Renamed", press "enter" → eventually sidebar row "Renamed" and client.list_sessions title Some("Renamed")
#[gpui_kit::test] async fn delete_confirmation_names_the_session_and_removes_it(cx)
    // open delete for session titled "hello" → find_by_label("Delete “hello”?") exists; click "Delete" → row gone, tab closed, list_sessions lacks the id
#[gpui_kit::test] fn escape_dismisses_the_rename_dialog_and_restores_focus(cx)
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test shell`
Expected: 新增的这 5 个测试失败。

- [ ] **Step 3: 实现**

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test shell`
Expected: 全部通过。

---

### Task 11: 应用生命周期：启动、持久化、外观跟随、退出

**Files:**
- Modify: `crates/desktop/src/app.rs`、`crates/desktop/src/ui/mod.rs`
- Test: `crates/desktop/tests/shell.rs`（追加）

**Interfaces:**
- Consumes: 前面全部任务的产出。
- Produces：
  - `app::run()` 的完整启动顺序：
    1. `config::load()`；成功时用 `LoggingFactory::from_config(&cfg.current().logging)` 安装日志；失败时用 `LoggingFactory::new("info", false)` 安装，并把错误留到后面显示。
    2. `DesktopState::load(&path)`（`path` 来自 `DesktopState::default_path()`，取不到时用当前目录下的 `desktop.json`，并打一条 warn）。
    3. `gpui_kit::application().with_assets(AppAssets).run(..)`，在回调里依次：`gpui_kit::init`、`register_builtin_themes`、`state.language.apply`、`bind_keys`、`cx.set_menus(..)`。菜单：文件（新会话 / 关闭 Tab / 退出）、视图（切换侧栏），都用 `MenuItem::action` 和 `t!` 文案。
    4. `open_window`：在 `TitleBar::window_options()` 的基础上，`window_min_size` 设为 960×640，`window_bounds` 用 `state.layout.window_size`，没有则 1280×820 居中。窗口里先 `state.appearance.apply(window.appearance(), cx)`，再构建 `AppView`。
    5. `cx.spawn` 启动后端：配置加载成功时 `Backend::start(cfg)`，然后 `view.update(set_backend)`，`Backend` 本身存进 `BackendHost`（`Global`，字段 `Option<Backend>`）。配置加载失败时直接 `set_backend(Err(BackendError::Startup(err)))`。
  - `AppView` 新增内容：
    - 订阅 `window.observe_window_appearance`，外观变化时调 `appearance.apply`。
    - 窗口尺寸变化写入 `layout.window_size`；`ResizableState` 的尺寸变化事件写入 `layout.sidebar_width`。
    - `fn persist(&mut self, cx)`：做 500ms 去抖。保存任务存在 `_save_task: Task<()>` 字段里，新的会替换旧的；真正的保存在 `cx.background_spawn` 里执行 `state.save(path)`。Tab 的开关和激活也写进 `state.tabs`，Draft Tab 不保存。
    - `retry` 按钮：重新 `config::load()`，再 `Backend::start`，phase 先回到 `Starting`。
    - `open-config` 按钮：用 `cx.open_with_system(path)` 打开第一个存在的配置候选路径；都不存在时打开 `config_dir/clawcode/` 目录。
    - `RegistryEvent::BackendStopped` 时，卡片顶部显示 danger 横幅 `t!("Backend.stopped")`，附按钮 `restart-app`（`t!("Backend.restart")`）：先 `std::process::Command::new(std::env::current_exe()?).spawn()`，再 `cx.quit()`；spawn 失败时 `tracing::error!`，横幅保持不动。
    - 退出流程：`Quit` action 和 `window.on_window_should_close` 都走同一个 `fn request_quit(&mut self, window, cx)`。如果 `registry.running_count(cx) > 0`，用 `open_alert_dialog` 弹确认：标题按数量选 `Quit.title_one` 或 `Quit.title_other`，正文 `Quit.body`，按钮为 `Dialog.cancel` 和 `Quit.action`（danger）。确认后（或没有运行中的会话时）：同步保存 state（`state.save`，不经过去抖），然后取出 `BackendHost` 里的 backend，执行 `shutdown()`，并和 `cx.background_executor().timer(Duration::from_secs(10))` 赛跑（`futures::future::select`），之后 `cx.quit()`。window should close 回调在 shutdown 完成前始终返回 `false`。

- [ ] **Step 1: 写失败的测试**

```rust
#[gpui_kit::test] async fn quit_with_a_running_session_asks_first(cx)
    // ScriptedBackend; open + prompt "slow"; dispatch Quit → find_by_label("Stop the running session and quit?") exists; click "Cancel" → app still running, dialog gone
#[gpui_kit::test] fn tab_changes_are_persisted_after_debounce(cx)
    // AppView with a temp desktop.json path; open s1; advance clock 600ms (cx.executor().advance_clock) → DesktopState::load(path).tabs.open == ["s1"]
#[gpui_kit::test] fn window_appearance_change_reapplies_theme(cx)
    // appearance mode System, dark theme "Tokyo Night"; the test platform window's simulate_appearance_change(WindowAppearance::Dark)
    // (gpui platform/test/window.rs) → cx.theme().theme_name() == "Tokyo Night"
```


- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p desktop --test shell`
Expected: 新增的测试失败。

- [ ] **Step 3: 实现**

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p desktop --test shell`
Expected: 全部通过。

- [ ] **Step 5: 用生产后端手工验收**

Run: `cargo run -p desktop --bin clawcode-desktop`（使用本机现有的 `claw.toml` 和 provider）

逐项确认：
- 已有会话按工作区出现在侧栏，WebUI 创建的会话也在。
- 点开会话后，Tab 的状态从骨架屏变为已加载。
- 运行状态图标已由 Task 8 / Task 11 的脚本化测试覆盖，手工验收不验它。M1a 没有 composer，无法在 desktop 里发 prompt。
- 在 `desktop.json` 里改语言和主题后重启，界面生效。
- Ctrl+W、Ctrl+B、Ctrl+1 都能用。
- 退出时进程在 10 秒内结束，`desktop.json` 已更新。

用 `run` skill 分别截图：浅色加中文、Catppuccin Mocha 加英文、窗口缩到最小尺寸。

---

### Task 12: 全量验证和提交关口

**Files:** 无新增。

- [ ] **Step 1: 格式化和静态检查**

Run: `cargo fmt --all && cargo clippy --workspace --tests --examples -- -Dwarnings`
Expected: 没有输出 warning 或 error。

- [ ] **Step 2: 测试**

Run: `cargo test -p desktop && cargo test -p acp && cargo test -p app`
Expected: 全部通过。

- [ ] **Step 3: pre-commit**

Run: `pre-commit run --all-files`
Expected: fmt、clippy、rig-boundary 全部 Passed。

- [ ] **Step 4: 对照设计稿第 15 节的 M1a 范围自查**

逐条检查 Global Constraints 和 Review Focus。

- [ ] **Step 5: 等用户明确要求后再提交**

按 `.gitmessage` 的格式，把设计稿、本计划和全部代码作为一个提交放到 `main`：

```bash
git add Cargo.toml Cargo.lock THIRD_PARTY_NOTICES.md crates/acp/src/trace.rs crates/desktop docs/superpowers/specs/2026-11-19-desktop-gpui-kit-design.md docs/superpowers/specs/2026-11-19-desktop-gpui-kit-mockup.html docs/superpowers/plans/2026-11-19-desktop-m1a-shell-and-data.md
git commit -m "feat(desktop): add gpui-kit desktop shell with in-process ACP backend"
```
