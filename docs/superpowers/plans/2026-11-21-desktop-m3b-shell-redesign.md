# Desktop M3b：外壳重构 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 按 T3 的方式重组 desktop 外壳：
- 左侧只有会话卡片（内嵌卡片），去掉轨道；
- 会话详情卡浮在对话区右上角；
- 变更、文件、文件预览、子智能体会话进入每个会话独立的右侧标签面板；
- 设置改为模态对话框，并新增界面和代码两组字体设置。

**Architecture:**
- **后端：** 在 M3 的 `_clawcode/workspace/git_status`、`git_diff` 上加字段和可选参数：基准（merge-base 或 HEAD）、每个文件的增删行数、diff 范围（对比基准 / 未暂存）、不传 `path` 时返回整个工作区的 patch。
- **状态：** `desktop.json` 增加：按归属者保存的右侧面板状态、详情卡显示项、打开方式、子智能体打开位置、六项字体设置；删除 `sidebar_mode`。
- **UI 分成四个新模块：**
  - `ui/panel/`：右侧标签面板及其标签视图（变更、文件、文件预览、子智能体）；
  - `ui/details/`：会话详情卡；
  - `ui/settings.rs`：改为对话框；
  - 外壳布局在 `ui/shell/card.rs` 里改为「会话卡片 | 主卡片 | 右侧面板」三栏。
- M3 的 `ui/tools/`、`ui/viewers/`、`ui/shell/rail.rs` 被拆进新模块或删除。

**Tech Stack:** 与 M3 相同。
- 新用到的 gpui-kit 组件：`Dialog`（`WindowExt::open_dialog`）、`Popover`、`Select::searchable`、`DiffState::scroll_to_file` / 多文件 `Diff`、`DiffFile::{additions, deletions, status}`。
- 系统字体来自 `cx.text_system().all_font_names()`。

**Spec:** [M3b 外壳重构设计](../specs/2026-11-21-desktop-shell-redesign-design.md)，它替换了 [Desktop 客户端设计](../specs/2026-11-19-desktop-gpui-kit-design.md) 的部分章节（M3b 设计第 12 节）。前序计划见 [M3](./2026-11-21-desktop-m3-workspace-tools.md)。

## Global Constraints

- M1a 到 M3 的 Global Constraints 继续有效：
  - AGENTS.md 规则 1–16；
  - 只用主题 token 和 rem，px 只用于持久化的面板宽度和 hairline，并写注释说明原因；
  - 图标统一用 Lucide，缺的加进 `assets.rs` 的 `ExtraIcons`；
  - clippy `-Dwarnings`；
  - 测试只放在 `tests/` 里；
  - 新文案写进 `locales/app.yml`，en 和 zh-CN 都要有。
- **不按任务提交。** M3 和 M3b 合成一个提交（M3b 设计第 2 节第 11 条）。用户明确要求后才提交，提交时带上 M3、M3b 的设计文档和计划。
- **不做的（M3b 设计第 14 节）：**
  - Git 写操作；
  - 环境和 worktree、项目脚本、开发服务器、自动化、侧聊；
  - 远程工作区的外部编辑器；
  - 面板里的终端和浏览器预览；
  - 「某一轮的改动」「某次工具调用的改动」两种 diff 范围。
- **尺寸（M3b 设计第 3 节，默认字号下的 px）：**

  | 区域 | 尺寸 |
  | --- | --- |
  | 会话卡片 | 200 / 248 / 400 |
  | 主卡片 | 最小 480 |
  | 右侧面板 | 最小 320，默认 480，最大为窗口宽度 − 会话卡片 − 480 − 间距 |
  | 详情卡 | 固定 280 |
  | 卡片间距 | 8（spacing `sm`） |
  | transcript 列宽 | 约 680 |

- **详情卡的内嵌条件：** 主卡片正文宽度 ≥ 680 + 280 + 16。
- **抽屉条件：** 右侧面板和主卡片并排时，主卡片宽度会小于 480。
- **字体（M3b 设计 7.2）：**

  | 设置 | 范围 | 步长 | 默认 |
  | --- | --- | --- | --- |
  | 界面字号 | 12–20 | 1 | 16 |
  | 代码字号 | 10–24 | 1 | 14 |
  | 界面字重 / 代码字重 | 100–900 的整数 | −/+ 50 | 400 |

  超出范围的值夹到边界。字体名不在系统字体列表里时，按「默认」显示，但保留保存的值。
- **详情卡降级（M3b 设计 5.3）：**
  - 完整：所有开启的项；
  - 紧凑：去掉「在…中打开」「之前的智能体」「上下文」「用量」；
  - 精简：只剩分支和改动。

  卡片最大高度 = 主卡片正文高度 − 16px。
- **详情卡各项的默认显示：** 在…中打开、分支、改动、会话关系开启；上下文、用量关闭。`desktop.json` 只存和默认值不同的项。
- **外部编辑器（M3b 设计 5.4）：** 按顺序检测 `zed`、`code`、`cursor`、`codium`、`subl`，看它们在不在 PATH 里，启动时缓存检测结果。
- **面板状态：**
  - 归属者键为 `session:{id}` 或 `draft:{workspace path}`；
  - 至多保存 50 个，按 `last_used_ms` 淘汰；
  - 新归属者的面板默认关闭；
  - 面板宽度全局保存。
- **键位（M3b 设计第 10 节）：**

  | 键 | 动作 |
  | --- | --- |
  | Ctrl+B | 开关会话卡片 |
  | Ctrl+J | 开关右侧面板（新增 `TogglePanel`） |
  | Ctrl+Shift+G | 打开「变更」（`ShowChanges`） |
  | Ctrl+Shift+E | 打开「文件」（`ShowFiles`） |
  | Ctrl+, | 打开设置对话框（`OpenSettings`） |
  | Ctrl+O | 添加工作区 |

  都绑在 `WORKSPACE_CONTEXT`，macOS 上用 ⌘。
- **diff 的默认范围：** `git_diff` 的 `scope` 默认为 `base`。M3 还没发布，没有调用方依赖旧的「对比 HEAD」语义。这是对 M3b 设计第 8 节「不改变旧参数语义」的有意例外，执行时在 ledger 里记一条 Ruling。

## Review Focus

1. **巨大的未跟踪文件：** 工作区里有一个没被 ignore 的 500 MiB 日志。「变更」标签的整个工作区 patch 必须在 2 MiB 处停下并显示截断提示，后端不能把整个文件读进内存或跑满 10 秒超时。测试在 Task 1（`whole_workspace_patch_stops_at_the_limit`）。
2. **窗口在并排和抽屉之间来回缩放：** 右侧面板开着时把窗口拖窄，面板变成抽屉；再拖宽，回到并排，标签和活动标签都不丢。测试在 Task 6（`narrow_window_turns_the_panel_into_a_sheet_and_back`）。
3. **草稿切换工作区：** 新会话草稿开着面板，用户在工作区选择条里换了工作区。面板要换成新工作区的状态，旧工作区的状态保留给以后。发送第一条消息后，面板状态归新会话。测试在 Task 6（`draft_panel_follows_its_workspace_and_moves_to_the_session`）。
4. **父会话被删除，而它的子智能体标签还开着：** 会话目录刷新后，相关面板状态被清理，子智能体的会话被 release，不 panic，不留悬空视图。测试在 Task 9（`deleting_the_parent_drops_its_panel_and_subagent_views`）。
5. **Esc 的层级：** 弹出层（详情卡 Popover 或字体下拉）开着时按 Esc，只关弹出层；再按才关抽屉，然后才关设置对话框；对话框关闭后焦点回到打开它的控件。测试在 Task 10（`escape_closes_one_layer_at_a_time`）。

---

### Task 1：后端——基准、增删行数、diff 范围、整个工作区的 patch

**Files:**
- Modify: `crates/protocol/src/workspace.rs`、`crates/protocol/src/lib.rs`
- Modify: `crates/acp/src/workspace.rs`、`crates/acp/src/extension.rs`
- Modify: `crates/desktop/src/backend/client.rs`（`git_diff` 改签名）
- Test: `crates/protocol/tests/workspace.rs`、`crates/acp/tests/workspace.rs`、`crates/acp/tests/extension.rs`

**Interfaces:**
- Produces（protocol）：
  - `WorkspaceChange` 增加 `#[serde(default)] insertions: Option<u64>`、`#[serde(default)] deletions: Option<u64>`；二进制文件为 `None`。
  - `WorkspaceGitStatus::Repository` 增加 `#[serde(default)] base: String`、`#[serde(default)] insertions: u64`、`#[serde(default)] deletions: u64`。
  - `pub enum WorkspaceDiffScope { #[default] Base, Unstaged }`，`serde(rename_all = "snake_case")`。
  - `pub struct WorkspaceDiffParameters { root: PathBuf, #[serde(default)] path: Option<String>, #[serde(default)] scope: WorkspaceDiffScope }`，`camelCase`、`deny_unknown_fields`。
  - `impl WorkspaceGitStatus { pub fn apply_numstat(&mut self, numstat: &[u8]) }`：解析 `git diff --numstat -z` 的输出，写入对应文件的行数，并累加合计。重命名记录的格式是 `ins\tdel\t\0old\0new\0`，二进制文件是 `-\t-\t`。
- Produces（acp）：
  - `WorkspaceRoot::git_diff(&self, path: Option<&str>, scope: WorkspaceDiffScope) -> Result<WorkspaceDiff, WorkspaceError>`。
  - 私有的 `WorkspaceRoot::diff_base(&self) -> DiffBase { rev: String, label: String }`：
    - 有上游时，`rev` 是 `git merge-base HEAD @{upstream}`，`label` 是 `git rev-parse --abbrev-ref @{upstream}` 的输出；
    - 没有上游或分离 HEAD 时，`rev = "HEAD"`、`label = "HEAD"`；
    - 还没有任何提交时，`rev` 是空树 `4b825dc642cb6eb9a060e54bf8d69288fbee4904`，`label = "HEAD"`。
- Produces（desktop）：`AcpClient::git_diff(&self, root: &Path, path: Option<&str>, scope: WorkspaceDiffScope) -> Result<WorkspaceDiff, BackendError>`。

- [ ] **Step 1：写失败的测试**
  - protocol：
    - `numstat_fills_counts_and_totals`：两个普通文件 `3\t1\ta.rs\0`、`-\t-\tb.png\0`，加一条重命名 `2\t0\t\0old.rs\0new.rs\0`；
    - 断言 `a.rs` 是 `(Some(3), Some(1))`，`b.png` 是 `(None, None)`，`new.rs` 是 `(Some(2), Some(0))`，合计 `5 / 1`；
    - 旧格式的 JSON（没有新字段）仍能反序列化。
  - acp：
    - `status_counts_lines_against_the_base`：`git_repo()` 里修改 `a.rs`（`original\n` 改成 `a\nb\n`），并新建未跟踪的 `u.rs`（3 行）。断言 `base == "HEAD"`、`a.rs` 是 `(Some(2), Some(1))`、`u.rs` 是 `(Some(3), Some(0))`、合计 `5 / 1`。
    - `status_base_is_the_upstream_merge_base`：克隆出一个仓库，在克隆里提交一次再修改。断言 `base == "origin/main"`，并且已提交的改动也计入行数。
    - `status_without_commits_counts_every_file_as_added`。
    - `diff_unstaged_scope_ignores_staged_changes`：暂存 `a.rs` 的改动后，再改一行。`Unstaged` 的 patch 只含后改的那一行，`Base` 的 patch 两处改动都有。
    - `whole_workspace_patch_includes_untracked_files`：`path = None`，patch 同时包含 `a.rs` 和未跟踪的 `u.rs`。
    - `whole_workspace_patch_stops_at_the_limit`：写一个 3 MiB 的未跟踪文本文件，断言 `truncated == true`，`patch.len() <= WORKSPACE_DIFF_LIMIT`。
  - extension：在已有的 `workspace_methods_answer_and_reject_escapes` 中，改用 `{"root", "scope": "unstaged"}` 不带 `path` 调用 `git_diff`，能得到结果。
- [ ] **Step 2：运行，确认失败**
  - Run：`cargo test -p protocol --test workspace && cargo test -p acp --test workspace --test extension`
  - Expected：编译失败，报告缺少 `apply_numstat`、`WorkspaceDiffScope` 等。
- [ ] **Step 3：实现**
  - `git_status`：在 porcelain 之后，运行 `git diff --numstat -z <base.rev>`，交给 `apply_numstat`。
    - 未跟踪文件按行计数：用 `BufReader` 分块数 `\n`，最后一行没有换行也算一行；前 8 KiB 里有 NUL 就算二进制，行数为 `None`。
    - 写入 `base.label`。
  - `git_diff`：
    - 有 `path` 时沿用 M3 的单文件逻辑，`Base` 用 `<base.rev>`，`Unstaged` 用 `git diff -- <path>`；
    - `path = None` 时，先跑 `git diff --no-color --no-ext-diff <rev 或省略>`，再依次给 `git_status` 列出的每个未跟踪文件追加 `--no-index` patch。累计超过 `WORKSPACE_DIFF_LIMIT` 就停止追加，并截到最后一个完整行。
    - 单个巨大文件靠子进程输出被截断来控制：读 stdout 时最多读 `WORKSPACE_DIFF_LIMIT + 1` 字节，然后 kill 子进程。不要用 `output()` 收全部输出。
  - `extension.rs`：`WorkspaceGitDiff` 改用 `WorkspaceDiffParameters` 解析参数。
- [ ] **Step 4：运行测试，确认通过**
  - Run：同 Step 2，再加 `cargo test -p desktop --test backend`
  - Expected：全部 PASS。

### Task 2：持久化状态

**Files:**
- Modify: `crates/desktop/src/state.rs`
- Test: `crates/desktop/tests/state.rs`

**Interfaces:**
- 删除 `SidebarMode` 和 `LayoutState::sidebar_mode`。
- `LayoutState` 增加：
  - `panel_width: f32`，带 `MIN_PANEL_WIDTH = 320.0`、`DEFAULT_PANEL_WIDTH = 480.0` 和方法 `panel_width(&self) -> f32`，NaN 时回到默认值并 clamp 下限；上限由布局按窗口宽度算；
  - `details_open: bool`，默认 `true`。
- `AppearancePreference` 增加：
  - `ui_font: Option<String>`；
  - `ui_font_weight: u16`（默认 400）；
  - `code_font: Option<String>`；
  - `code_font_size: f32`（默认 14）；
  - `code_font_weight: u16`（默认 400）。

  常量：`UI_FONT_SIZE_RANGE = (12.0, 20.0)`、`CODE_FONT_SIZE_RANGE = (10.0, 24.0)`、`FONT_WEIGHT_RANGE = (100, 900)`。方法 `clamped(&self) -> Self` 把所有数值夹到各自范围，NaN 回到默认值。
- 新类型：
  - `pub enum PanelTabKey { Changes, Files, File { path: String }, Subagent { session: String }, #[serde(other)] Unknown }`，`serde(tag = "kind", rename_all = "snake_case")`。
  - `pub struct PanelState { open: bool, tabs: Vec<PanelTabKey>, active: Option<usize>, maximized: bool, last_used_ms: u64 }`，用 TypedBuilder，全部 `#[builder(default)]`。
  - `pub enum DetailsItem { OpenIn, Branch, Changes, Relations, Context, Usage }`，带方法 `default_visible(self) -> bool` 和常量 `ALL`。
  - `pub enum OpenIn { #[default] #[serde(other)] FileManager, Editor(String) }`：用 `serde(rename_all = "snake_case")`；编辑器值存成 `{"editor": "zed"}`。
  - `pub enum SubagentOpenTarget { #[default] #[serde(other)] Panel, Main }`。
- `DesktopState` 增加 `panels: BTreeMap<String, PanelState>`、`details_items: BTreeMap<DetailsItem, bool>`、`open_in: OpenIn`、`subagent_open_target: SubagentOpenTarget`，以及以下方法：
  - `details_item_visible(&self, item: DetailsItem) -> bool`；
  - `set_details_item(&mut self, item: DetailsItem, visible: bool)`：等于默认值时删掉这一项；
  - `prune_panels(&mut self, keep: impl Fn(&str) -> bool)`：先去掉 `keep` 返回 false 的键，再按 `last_used_ms` 只保留最新的 50 个；
  - `normalize_panels(&mut self)`：去掉 `Unknown` 标签，修正越界的 `active`。在 `DesktopState::load` 末尾调用。

- [ ] **Step 1：写失败的测试**
  - `old_state_file_with_sidebar_mode_still_loads`：JSON 里带 `"sidebar_mode": "files"` 和 `"tabs": {"open": ["s1"]}`，读取成功，`tabs.open == ["s1"]`。
  - `font_settings_default_clamp_and_round_trip`：默认值是 `None / 400 / None / 14.0 / 400`。`clamped()` 把 `ui_font_weight = 950` 夹成 900，`code_font_size = 8.0` 夹成 10.0，NaN 回到 14.0。写入再读回，值不变。
  - `details_items_store_only_changes`：
    - `set_details_item(Branch, false)` 后存一项；
    - 再 `set_details_item(Branch, true)` 后 map 为空；
    - `set_details_item(Usage, true)` 后存一项；
    - 默认情况下 `details_item_visible(Context) == false`。
  - `panel_state_drops_unknown_tabs_and_prunes_to_fifty`：
    - 一个面板含 `{"kind":"terminal"}` 标签且 `active = 3`，读取后标签被去掉、`active` 被修正；
    - 放 60 个面板，`prune_panels(|_| true)` 后剩 50 个，被淘汰的是 `last_used_ms` 最小的 10 个；
    - `prune_panels(|key| key != "session:gone")` 会删掉 `session:gone`。
  - `open_in_and_subagent_target_fall_back`：`"open_in": "vim-ide"` 读成 `FileManager`，`"subagent_open_target": "split"` 读成 `Panel`，`{"editor": "zed"}` 读成 `Editor("zed")`。
- [ ] **Step 2：运行** `cargo test -p desktop --test state`，Expected：编译失败。
- [ ] **Step 3：按 Interfaces 实现。** `OpenIn` 的回退如果 `#[serde(other)]` 无法和带值的变体共存，就为它手写 `Deserialize`（读成 `serde_json::Value`，再按形状匹配），在 ledger 里记一条 Ruling。
- [ ] **Step 4：运行** `cargo test -p desktop --test state`，Expected：PASS。M3 里依赖 `SidebarMode` 的测试暂时编译不过，由 Task 4 删改。

### Task 3：字体应用

**Files:**
- Modify: `crates/desktop/src/theme.rs`、`crates/desktop/src/ui/mod.rs`（根元素）
- Test: `crates/desktop/tests/theme.rs`

**Interfaces:**
- `AppearancePreference::apply(&self, appearance, cx)` 的流程：
  1. 先套用主题；
  2. 用 `clamped()` 后的值写 `theme.font_size`、`theme.mono_font_size`；
  3. 字体名在 `cx.text_system().all_font_names()` 里时，写 `theme.font_family` / `theme.mono_font_family`，否则保持主题的默认值；
  4. 设置全局 `FontWeights { ui: FontWeight, code: FontWeight }`（`impl Global`），通过 `FontWeights::get(cx) -> FontWeights` 读取，没有设置时都是 400。
- 根元素（`AppView::render` 的最外层 div）加 `.font_weight(FontWeights::get(cx).ui)`。
- `pub fn code_text(cx: &App) -> StyleRefinement`（放在 `theme.rs`）：返回只含 `font_weight = code` 的文字样式，供 Task 7、Task 10 以及 transcript 里的代码块容器使用。

- [ ] **Step 1：写失败的测试**
  - `fonts_apply_to_the_theme_and_survive_a_theme_switch`：
    - 设置 `code_font_size = 18.0`、`ui_font = Some(系统字体列表里的第一个)`；`apply` 后，`theme.mono_font_size == px(18.)`，`theme.font_family` 等于那个字体名；
    - 把 `dark_theme` 换成另一个主题再 `apply`，两者都不变。
  - `missing_font_keeps_the_theme_default_and_the_setting`：`ui_font = Some("No Such Font 123")`，`apply` 后 `theme.font_family` 是主题的默认值，而 `AppearancePreference.ui_font` 不变。
  - `font_weights_are_global`：`ui_font_weight = 600`、`code_font_weight = 350`，`apply` 后 `FontWeights::get(cx)` 是 `(600, 350)`。
- [ ] **Step 2：运行** `cargo test -p desktop --test theme`，Expected：FAIL。
- [ ] **Step 3：实现。**
- [ ] **Step 4：运行** `cargo test -p desktop --test theme`，Expected：PASS。

### Task 4：外壳布局——去掉轨道，三张卡片

**Files:**
- Delete: `crates/desktop/src/ui/shell/rail.rs`
- Delete: `crates/desktop/src/ui/tools/mod.rs` 中侧栏模式相关的部分（`sidebar_mode`、`set_sidebar_mode`、`render_sidebar_panel` 的模式分支、`render_tool_sidebar`）
- Modify: `crates/desktop/src/ui/shell/card.rs`（`render_body` 改成三栏）、`crates/desktop/src/ui/sidebar.rs`（会话卡片的表面和底部设置按钮）、`crates/desktop/src/ui/mod.rs`、`crates/desktop/src/actions.rs`（新增 `TogglePanel`，Ctrl+J）、`crates/desktop/src/ui/shell/title_bar.rs`
- Test: 新建 `crates/desktop/tests/shell_layout.rs`；修改 `tests/shell_tools.rs`（删掉轨道和侧栏模式的测试，它们的行为由 Task 6、7 重新测）

**Interfaces:**
- `AppView::render_body` 的结构：`h_resizable("shell-split")`，依次放会话卡片面板、主卡片面板、右侧面板。
  - 右侧面板此时只渲染一个空卡片，内容由 Task 6 填。
  - 面板关闭或处于抽屉形态时，第三个 `resizable_panel` 用 `.visible(false)`。
- 会话卡片：
  - 外层 div 的 id 是 `"session-card"`；
  - 表面和主卡片一样：`background`、`border`、`radius_lg`，浅色主题加 `shadow_sm`；
  - 内部 `Sidebar` 不再用 `title_bar` 底色；
  - 底部固定 `Button::new("open-settings").ghost()`（medium），图标 `settings-2`，tooltip 带 `OpenSettings` 的快捷键。
- 拖放目标从 `"sidebar-drop"` 改名为 `"session-card"`（同一个元素）。
- `ShowFiles` / `ShowChanges` 先保留动作，处理函数在 Task 6 改写；本任务里让它们什么都不做，并删掉旧的模式切换。
- **没有任何 Tab 时：** 主卡片沿用现在的做法，不画顶栏。此时没有面板归属者，右侧面板和详情卡都不显示，所以 M3b 设计第 3 节「没有活动 Tab 时只显示 [◨]」不需要实现。在 ledger 里记一条 Ruling。
- 主卡片顶栏右侧按顺序放这些按钮，本任务先放占位，点击行为在 Task 6、8 接上：
  - `Button::new("header-changes")`（`git-compare`）；
  - `"header-files"`（`folder-tree`）；
  - `"header-details"`（`info`）；
  - `"header-panel"`（`panel-right`）。

- [ ] **Step 1：写失败的测试**（`shell_layout.rs`）
  - `no_rail_and_three_cards`：窗口里找不到 `"rail"`，能找到 `"session-card"` 和 `"content-card"`。
  - `ctrl_b_hides_the_session_card`：按 Ctrl+B 后 `layout.sidebar_open == false`，再按一次恢复。
  - `settings_button_sits_in_the_session_card`：`"open-settings"` 的 bounds 在 `"session-card"` 的 bounds 之内。
  - `dropping_a_folder_on_the_session_card_adds_it`：沿用 M3 的拖放测试，目标换成新 id。
- [ ] **Step 2：运行** `cargo test -p desktop --test shell_layout`，Expected：FAIL。
- [ ] **Step 3：实现，并删掉旧代码。** `ui/tools/files.rs` 和 `ui/tools/changes.rs` 先保留，Task 6、7 再挪走；只断开它们和侧栏模式的连接。
- [ ] **Step 4：运行** `cargo test -p desktop`，Expected：PASS（`shell_tools.rs` 里已经删掉的测试不算）。

### Task 5：会话卡片内容——「最近」不显示工作区，子智能体高亮祖先

**Files:**
- Modify: `crates/desktop/src/ui/sidebar.rs`、`crates/desktop/src/catalog.rs`
- Test: `crates/desktop/tests/catalog.rs`、`crates/desktop/tests/shell.rs`

**Interfaces:**
- `Catalog::children(&self, parent: &SessionId) -> Vec<&SessionSummary>`：按修改时间倒序。
- `Catalog::listed_ancestor(&self, id: &SessionId) -> Option<SessionId>`：沿 `parent_session_id` 往上找第一个顶层会话。遇到环或父会话缺失时返回 `None`。
- `sidebar_model` 的改动：
  - 「最近」分组用 `Trailing::Age`；`Trailing::Workspace` 只用于搜索结果。
  - `active` 的判断改为：活动 Tab 是会话时，取 `listed_ancestor(id)`，取不到就用它自己的 id。

- [ ] **Step 1：写失败的测试**
  - catalog：`children_and_listed_ancestor`，结构为 `a → b → c`（`c` 的父是 `b`，`b` 的父是 `a`）：
    - `children(a) == [b]`；
    - `listed_ancestor(c) == Some(a)`；
    - 父会话缺失时返回 `None`；
    - `x ↔ y` 互为父子（环）时返回 `None`，并且不会死循环。
  - shell：
    - `recent_rows_show_age_not_workspace`：「最近」分组里每一行的 `trailing` 都不等于工作区名。
    - `an_open_subagent_highlights_its_parent`：打开子智能体会话的 Tab 后，父会话那一行 `active == true`。
- [ ] **Step 2：运行** `cargo test -p desktop --test catalog --test shell`，Expected：FAIL。
- [ ] **Step 3：实现。**
- [ ] **Step 4：运行** 同 Step 2，Expected：PASS。

### Task 6：右侧标签面板——骨架、归属者、持久化、抽屉、文件和预览

**Files:**
- Create: `crates/desktop/src/ui/panel/mod.rs`（面板模型和渲染）、`ui/panel/files.rs`（由 `ui/tools/files.rs` 改造）、`ui/panel/file_view.rs`（由 `ui/viewers/file_view.rs` 改造）
- Delete: `ui/tools/files.rs`、`ui/viewers/file_view.rs`；`TabEntry::File` 和 `TabEntry::Diff` 两个变体；`ui/viewers/mod.rs` 中和 Tab 有关的 `open_file`、`open_diff`、`show_viewer`、`refresh_active_viewer`
- Modify: `ui/mod.rs`、`ui/shell/card.rs`、`ui/shell/drafts.rs`（`adopt_draft`、`select_draft_workspace` 转移面板状态）、`ui/shell/tab_ops.rs`、`actions.rs`
- Test: 新建 `crates/desktop/tests/shell_panel.rs`；删掉或迁移 `tests/shell_viewers.rs` 中针对主卡片 Tab 的断言

**Interfaces:**
- `pub enum PanelOwner { Session(SessionId), Draft(PathBuf) }`：
  - `key(&self) -> String` 返回 `session:{id}` / `draft:{path}`；
  - `AppView::panel_owner(&self) -> Option<PanelOwner>` 取活动 Tab 的归属者。草稿没有工作区时返回 `None`，此时面板不可用。
- 运行时状态：`AppView.panels: HashMap<String, PanelRuntime>`，`PanelRuntime { views: HashMap<PanelTabKey, PanelView> }`。
  - `PanelView` 是枚举：`Changes(Entity<ChangesView>)`、`Files(Entity<FilesView>)`、`File(Entity<FileView>)`、`Subagent(Entity<SessionView>)`。本任务里 `ChangesView` 只是一个显示「加载中」的空视图，由 Task 7 实现；`Subagent` 的创建由 Task 9 实现。
  - 视图在标签第一次被激活时创建（懒加载）。
  - 持久状态仍在 `state.panels` 里。
- `AppView` 的方法：
  - `open_panel_tab(&mut self, tab: PanelTabKey, window, cx)`：打开面板，加入或激活标签，更新 `last_used_ms`，然后 persist。
  - `close_panel_tab(&mut self, index: usize, cx)`。
  - `toggle_panel(&mut self, window, cx)`：Ctrl+J。没有标签时打开 `Changes`；工作区不是 Git 仓库时改为打开 `Files`。
  - `toggle_panel_maximized(&mut self, cx)`。
  - `panel_presentation(&self, window: &Window) -> PanelPresentation`，返回 `Inline`、`Sheet` 或 `Hidden`，按 Global Constraints 里的抽屉条件判断。
- 渲染：
  - 并排形态：第三栏是 `"right-panel"` 卡片，上面是标签条（`"panel-tab:{key}"`，带关闭按钮，中键也能关闭），右侧是 `"panel-maximize"` 和 `"panel-close"`。
  - 抽屉形态：用 gpui-kit `Sheet` 从右侧滑出，有遮罩，Esc 关闭。
  - 最大化：主卡片 `visible(false)`。
- `ShowFiles` 打开 `PanelTabKey::Files`，`ShowChanges` 打开 `PanelTabKey::Changes`。
- 「文件」标签：
  - 搬用 M3 的 `FileTreeView` 逻辑，改成独立实体 `FilesView`，持有 `workspace`、`client`、`FileTree`、`TreeState`；
  - 工具条有 `"files-hidden"` 和 `"files-refresh"`；
  - 点文件调用 `open_panel_tab(PanelTabKey::File { path })`。
- 「文件预览」标签：搬用 M3 的 `FileView`，含 `ViewerSource`；文件没变时，重新激活不重置光标和滚动位置。
- `refresh_catalog` 的末尾调用 `self.state.prune_panels(...)`：保留仍在会话目录里的会话，以及所有 `draft:` 键；同时丢掉被清理归属者的 `PanelRuntime`。
- `adopt_draft`：把 `draft:{path}` 的状态移到 `session:{id}`；`select_draft_workspace`：换成新路径的归属者键，旧状态留在原处。

- [ ] **Step 1：写失败的测试**（`shell_panel.rs`）
  - `ctrl_shift_e_opens_files_and_a_file_preview`：按 Ctrl+Shift+E 后面板打开、活动标签是 `Files`；点 `file-node-src`，再点 `file-node-src/lib.rs`，会多出一个 `File { path: "src/lib.rs" }` 标签，并且主卡片的 Tab 数不变。
  - `panel_state_is_per_session_and_survives_a_restart`：会话 A 打开「文件」，会话 B 不开。切到 B 时面板关着，切回 A 时面板开着。用同一个 `desktop.json` 重开 shell，A 的标签恢复，但内容在激活前不加载。
  - `ctrl_j_toggles_and_defaults_to_changes`：没有标签时按 Ctrl+J，打开 `Changes`；再按，面板关闭，标签保留。在不是 Git 仓库的工作区里，打开的是 `Files`。
  - `narrow_window_turns_the_panel_into_a_sheet_and_back`（Review Focus 2）：把窗口改成 960×640 后，`panel_presentation == Sheet`，并能找到 sheet 元素；改回 1280×820 后回到 `Inline`，标签列表和活动标签不变。
  - `draft_panel_follows_its_workspace_and_moves_to_the_session`（Review Focus 3）：草稿在工作区 W1 打开「文件」，换到 W2 时面板关着，换回 W1 时面板开着；发送第一条消息后，`state.panels` 里有 `session:{id}`，没有 `draft:{W1}`。
  - `deleted_sessions_lose_their_panel_state`：删除会话 A，会话目录刷新后，`panels` 里不再有 `session:A`。
  - `returning_to_an_unchanged_preview_keeps_its_place`：从 `shell_viewers.rs` 迁过来，改为在面板标签之间切换。
  - `maximize_hides_the_main_card`。
- [ ] **Step 2：运行** `cargo test -p desktop --test shell_panel`，Expected：FAIL。
- [ ] **Step 3：实现。** 抽屉用 gpui-kit `Sheet`（`sheet.rs`）。如果它不支持指定从右侧出现，就在 ledger 里记一条 Ruling，改用绝对定位的 div 加遮罩实现。
- [ ] **Step 4：运行** `cargo test -p desktop --test shell_panel --test shell_viewers`，Expected：PASS。

### Task 7：「变更」标签

**Files:**
- Create: `crates/desktop/src/ui/panel/changes.rs`
- Delete: `ui/tools/changes.rs`、`ui/viewers/diff_view.rs`；删掉 `ui/tools/` 和 `ui/viewers/` 两个目录，`ViewerSource` 和 `ViewerState` 移到 `ui/panel/mod.rs`
- Modify: `ui/panel/mod.rs`、`ui/mod.rs`（`GitSnapshot` 继续按工作区缓存，供详情卡和分支显示使用）
- Test: `crates/desktop/tests/shell_panel.rs`

**Interfaces:**
- `ChangesView` 持有：`workspace`、`client`、`scope: WorkspaceDiffScope`，以及每种范围一份缓存 `HashMap<WorkspaceDiffScope, ScopeContent>`。
  - `ScopeContent { files: Vec<ChangedFile>, diff: Option<Entity<DiffState>>, truncated: bool, state: ViewerState }`；
  - `ChangedFile { path, original_path, status: DiffFileStatus, additions, deletions }`，从解析出的 `DiffFile` 得到。
- `ChangesView` 的方法：
  - `set_scope(&mut self, scope, window, cx)`：先显示缓存，再在后台刷新；
  - `refresh(&mut self, window, cx)`：调用 `client.git_diff(root, None, scope)`，用 `DiffParseOptions::truncated_hunks(truncated)` 解析；patch 没变时不重建 `DiffState`；
  - `reveal(&mut self, path: &str, cx)`：调用 `DiffState::scroll_to_file`。
- 渲染：
  - 顶部：范围分段 `"changes-scope-base"`（文字是「对比 {base}」，`base` 来自 `GitSnapshot`）和 `"changes-scope-unstaged"`；合计 `+x −y`；`"changes-refresh"`。
  - 主体：左栏 `uniform_list("changes-files", …)`，行 id 为 `"changed-file-{n}"`，点击调用 `reveal`；右栏是多文件 `Diff`。
  - 截断时，顶部显示 `Viewer.diff_truncated`。
- 刷新时机：
  - 标签被激活时；
  - `RegistryEvent::SessionChanged` 时，对同一工作区所有已创建的 `ChangesView` 调 `refresh`，并刷新 `GitSnapshot`；
  - 刷新按钮。

- [ ] **Step 1：写失败的测试**
  - `changes_tab_lists_files_and_reveals_one`：在 `live_in_git_repo` 里修改 `a.rs`，新建 `new.rs`。打开「变更」后，`changed-file-0` 和 `changed-file-1` 的文字分别含 `a.rs` 和 `new.rs`；点 `changed-file-1` 后，`DiffState` 的可视区包含 `new.rs` 的文件头（用 `scroll_to_file` 之后的 `files()` 加可见范围断言；做不到时退一步，断言调用后 `selected_lines()` 落在 `new.rs` 上，并在 ledger 记 Ruling）。
  - `scope_switch_shows_only_unstaged_lines`：暂存 `a.rs` 的改动后再改一行，切到「未暂存」后，列表只剩 `a.rs`，合计只算后改的那一行。
  - `a_finished_run_refreshes_open_changes_tabs`：从 M3 的 `a_finished_run_refreshes_the_changes` 迁过来。
  - `a_diff_cut_at_the_size_limit_renders_with_a_note`：从 `shell_viewers.rs` 迁过来，改为整个工作区的 patch。
  - `a_long_change_list_builds_only_the_visible_rows`：从 `shell_tools.rs` 迁过来，id 改为 `changed-file-*`。
- [ ] **Step 2：运行** `cargo test -p desktop --test shell_panel`，Expected：FAIL。
- [ ] **Step 3：实现，并删除旧目录。**
- [ ] **Step 4：运行** `cargo test -p desktop`，Expected：PASS。

### Task 8：会话详情卡

**Files:**
- Create: `crates/desktop/src/ui/details/mod.rs`（卡片、显示条件、降级）、`ui/details/open_in.rs`（编辑器检测和启动）
- Modify: `ui/shell/card.rs`（内嵌挂载点和 `"header-details"`）、`ui/session/mod.rs`（`SessionView::last_run_usage(&self) -> Option<RunUsage>`：从 layout 末尾找第一个用量）
- Test: 新建 `crates/desktop/tests/shell_details.rs`、`crates/desktop/tests/open_in.rs`

**Interfaces:**
- `pub struct EditorCatalog { editors: Vec<String> }`：
  - `EditorCatalog::detect(path_var: Option<&OsStr>) -> Self` 按 Global Constraints 的顺序检测，接受 PATH 参数以便测试；
  - `contains(&self, name: &str) -> bool`；
  - 在 `AppView::new` 里用 `std::env::var_os("PATH")` 检测一次。
- `AppView::open_workspace_in(&mut self, target: &OpenIn, path: &Path, cx)`：
  - `FileManager` 调用现有的 `reveal_workspace`；
  - `Editor(name)` 用 `std::process::Command::new(name).arg(path).spawn()`，失败时 `show_notice`，标题为 `Details.open_failed`，内容带命令和错误；
  - 保存的编辑器不在 `EditorCatalog` 里时，按 `FileManager` 处理。
- `pub enum DetailsDensity { Full, Compact, Essential }`，以及纯函数 `DetailsDensity::fit(available: Pixels, heights: [Pixels; 3]) -> (DetailsDensity, bool)`：从完整开始，返回第一个放得下的档；三档都放不下时返回 `(Essential, true)`，`true` 表示需要滚动。各档高度按行数乘行高估算：组标题一行，每个可见行一行。
- `AppView::render_details(&self, presentation: DetailsPresentation, window, cx) -> Option<AnyElement>`，`DetailsPresentation` 为 `Inline` 或 `Popover`。没有活动 Tab 时返回 `None`。
- 各行的元素 id：
  - `"details-open-in"` 和 `"details-open-in-menu"`；
  - `"details-branch"`；
  - `"details-changes"`：点击调用 `open_panel_tab(Changes)` 并把范围设为 `Base`；
  - `"details-parent"`；
  - `"details-agent-{session}"`；
  - `"details-previous"`：「之前的智能体 · N」，可以折叠；
  - `"details-context"`、`"details-usage"`；
  - 卡片为空时显示 `"details-choose"`。
- 运行中与已结束的子智能体：`SessionStatus::Running` 或 `Waiting` 的算「运行中」，其余算「之前的智能体」。
- 右键卡片时打开菜单，每个 `DetailsItem` 一个勾选项，切换时调用 `set_details_item` 并 persist。
- 显示形态：主卡片正文宽度满足内嵌条件、且 `layout.details_open` 时内嵌；不满足时，`"header-details"` 打开 `Popover`。

- [ ] **Step 1：写失败的测试**
  - `open_in.rs`：
    - `detect_finds_editors_on_path_in_order`：在临时目录里放可执行的 `code` 和 `zed` 文件，`detect` 的结果是 `["zed", "code"]`；
    - `missing_saved_editor_falls_back_to_file_manager`：用纯函数 `OpenIn::resolve(&self, catalog: &EditorCatalog) -> OpenIn` 断言。
  - `shell_details.rs`：
    - `density_steps_down_and_scrolls_last`：调用 `DetailsDensity::fit`，分别覆盖三档和需要滚动的情况。
    - `details_card_is_inline_when_wide_and_a_popover_when_narrow`：1280×820 时能找到 `"details-card"`。把会话卡片拉到 400、面板打开，使主卡片变窄后，`"details-card"` 消失；点 `"header-details"` 后，弹出层里有 `"details-branch"`。
    - `changes_row_opens_the_changes_tab`：点 `"details-changes"` 后，面板的活动标签是 `Changes`，范围是 `Base`。
    - `hidden_items_disappear_and_only_changes_are_stored`：通过菜单关掉「分支」后，`"details-branch"` 消失，`state.details_items` 里只有 `Branch: false`。
    - `relations_group_lists_running_and_previous_agents`：父会话有一个 Running 的子会话和一个 Idle 的子会话，能找到 `"details-agent-{running}"`，`"details-previous"` 的文字含 `1`。
    - `a_subagent_shows_its_parent_row`：在子会话的 Tab 上能找到 `"details-parent"`，点击后在主卡片激活父会话。
    - `non_repository_hides_version_control`：`"details-branch"` 和 `"details-changes"` 都不存在，`"header-changes"` 被禁用。
    - `context_and_usage_are_hidden_by_default`。
    - `details_explain_missing_workspaces`：没有登记任何工作区的草稿，卡片显示「添加一个工作区后可查看」和 `"details-add-workspace"` 按钮；工作区目录被删掉后，卡片显示「找不到文件夹」。
- [ ] **Step 2：运行** `cargo test -p desktop --test shell_details --test open_in`，Expected：FAIL。
- [ ] **Step 3：实现。** 浮动卡片用绝对定位挂在主卡片正文上，加 `top`、`right` 偏移，`occlude()` 防止点击穿透到 transcript。
- [ ] **Step 4：运行** 同 Step 2，Expected：PASS。

### Task 9：子智能体标签

**Files:**
- Modify: `ui/panel/mod.rs`、`ui/details/mod.rs`、`ui/shell/tab_ops.rs`（attach / release）、`ui/mod.rs`（会话目录刷新时清理）
- Test: `crates/desktop/tests/shell_details.rs`、`crates/desktop/tests/shell_panel.rs`

**Interfaces:**
- `AppView::open_subagent(&mut self, id: SessionId, alternate: bool, window, cx)`：
  - 目标由 `state.subagent_open_target` 决定，`alternate`（Ctrl+点击）时取另一处；
  - `Panel` 调用 `open_panel_tab(PanelTabKey::Subagent { session })`；
  - `Main` 用现有逻辑打开会话 Tab。
- 面板里的子智能体视图用 `SessionView::new(SessionTarget::Session(handle), …)` 创建，在创建前 `registry.attach(&summary)`。
- 子智能体标签关闭、归属者被清理、或 `PanelRuntime` 被丢弃时，如果主卡片的 Tab 里没有这个会话，就 `registry.release(id)`。
- 面板标签标题：`bot` 图标加会话标题。

- [ ] **Step 1：写失败的测试**
  - `subagent_row_opens_in_the_panel_by_default_and_main_with_ctrl`：
    - 点 `"details-agent-{c}"` 后，面板的活动标签是 `Subagent { session: c }`，主卡片的 Tab 不变；
    - Ctrl+点击后，主卡片多一个 `TabEntry::Session(c)`；
    - 把偏好设成 `Main` 后，普通点击开到主卡片。
  - `deleting_the_parent_drops_its_panel_and_subagent_views`（Review Focus 4）：父会话的面板开着子智能体标签，删除父会话并刷新会话目录后，`state.panels` 里没有它，`PanelRuntime` 被丢弃，`registry.handle(c)` 不再被持有（`attached_count(c) == 0`，若 registry 没有这个计数方法，就新增一个 `pub fn is_attached(&self, id) -> bool`）。
- [ ] **Step 2：运行** `cargo test -p desktop --test shell_details --test shell_panel`，Expected：FAIL。
- [ ] **Step 3：实现。**
- [ ] **Step 4：运行** 同 Step 2，Expected：PASS。

### Task 10：设置对话框与字体设置

**Files:**
- Modify: `crates/desktop/src/ui/settings.rs`（改成对话框；新增「字体」分页和「子智能体默认打开位置」）、`ui/mod.rs`（删除 `TabEntry::Settings`）、`ui/shell/card.rs`、`ui/shell/tabs.rs`、`ui/shell/tab_ops.rs`、`ui/shell/title_bar.rs`（菜单项）、`actions.rs`
- Test: 重写 `crates/desktop/tests/shell_settings.rs`；修改 `tests/shell_locale.rs`、`tests/tabs.rs`（删掉 `TabEntry::Settings`）

**Interfaces:**
- `AppView::open_settings(&mut self, window: &mut Window, cx)`：
  - 已经打开时，只把焦点移到对话框；
  - 否则记下 `window.focused(cx)` 作为 `settings_return_focus`，然后调用 `window.open_dialog(cx, …)`：标题「设置」，宽度 `rems(55.)`，高度 `rems(40.)`，内容是 `Settings::new("settings")`；
  - 关闭时（`on_close`）把焦点还给 `settings_return_focus`。
- 新增 `AppView.settings_open: bool`，供测试读取：`pub fn settings_open(&self) -> bool`。
- 字体分页：
  - 界面字体和代码字体用 `SettingField::render`，里面是可搜索的 `Select`（`searchable(true)`）。选项为「默认」加 `all_font_names()`，按字母排序并去重。`SelectState` 实体在第一次打开对话框时创建，并缓存在 `AppView.font_pickers`。
  - 字号用 `number_input`：界面 12–20，代码 10–24，步长 1。
  - 字重用 `number_input`：100–900，步长 50，写入前先 `round()`，再按 `FONT_WEIGHT_RANGE` 夹住。
  - 任何一项改动都调用 `set_appearance`，后者会 `apply` 并 `remeasure_views`。
- 通用分页新增「子智能体默认打开位置」下拉框，值为 `panel` / `main`。
- `OpenSettings` 的三个入口（Ctrl+,、`"open-settings"` 按钮、标题栏菜单）都调用 `open_settings`。

- [ ] **Step 1：写失败的测试**（`shell_settings.rs`）
  - `ctrl_comma_opens_one_dialog_and_no_tab`：连按两次 Ctrl+, 后 `settings_open()` 为真，只有一个对话框，`view.tabs()` 里没有设置 Tab。
  - `escape_closes_one_layer_at_a_time`（Review Focus 5）：打开对话框，点代码字体的下拉框，按 Esc 后下拉框关闭、对话框仍在；再按 Esc，对话框关闭，焦点回到 `"open-settings"`。
  - `appearance_changes_apply_and_persist`：保留 M3 的这个测试，改为在对话框里操作。
  - `font_fields_write_and_clamp`：通过字段的 setter 把代码字重设为 975，结果是 900；把代码字号设为 18，`theme.mono_font_size == px(18.)`，`desktop.json` 里是 18。
  - `font_size_change_remeasures_rows`：保留 M3 的测试，改为通过对话框修改。
  - `subagent_target_setting_persists`。
- [ ] **Step 2：运行** `cargo test -p desktop --test shell_settings --test shell_locale --test tabs`，Expected：FAIL。
- [ ] **Step 3：实现。**
- [ ] **Step 4：运行** 同 Step 2，Expected：PASS。

### Task 11：文档、文案、全量检查与真实界面验收

**Files:**
- Modify: `docs/superpowers/specs/2026-11-19-desktop-gpui-kit-design.md`：M3b 设计第 12 节表格里的每个章节，在开头加一行 `> 已被 [M3b 外壳重构设计](./2026-11-21-desktop-shell-redesign-design.md) 取代。`
- Modify: `crates/desktop/locales/app.yml`：删除不再使用的 `Rail.*` 等键，补齐 `Panel.*`、`Details.*`、`Settings.fonts*` 等新键；`crates/desktop/src/assets.rs`
- Modify: `docs/superpowers/plans/2026-11-21-desktop-m3-workspace-tools.md`：开头加一行，说明 UI 部分已被 M3b 改写

- [ ] **Step 1：清理没用到的文案**：用脚本列出 `app.yml` 里没有被 `t!("…")` 引用的键，删掉这些键。
- [ ] **Step 2：全量检查**
  - Run：`cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -Dwarnings && cargo test --workspace && pre-commit run --all-files`
  - Expected：全部通过。运行前先备份未跟踪的文件（M3 期间出现过测试文件被清空的事故）。
- [ ] **Step 3：真实界面验收**：production backend 加真实 provider，在隔离的 KWin 加软件渲染环境里，逐项截图确认：
  - 三张卡片，浅色和深色主题；
  - 中文和英文界面；
  - 960×640 下的抽屉形态，以及 1280×820 下的并排形态；
  - 详情卡的三档降级（缩放窗口高度）；
  - 「变更」标签的整个工作区 diff 和定位到文件；
  - 子智能体在面板中打开；
  - 设置对话框的三个入口和 Esc 层级；
  - 字体修改后的视觉效果。

  截图放到 `.superpowers/sdd/<plan>/live/screens`。
