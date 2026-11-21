# Desktop M3：工作区工具 Implementation Plan

> **注意：** 本计划的界面部分（轨道、侧栏的文件和变更模式、文件和 diff Tab、设置 Tab）已被 [M3b 外壳重构](./2026-11-21-desktop-m3b-shell-redesign.md) 改写；后端和数据部分仍然有效。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 desktop 加上工作区工具：
- 文件侧栏（按目录懒加载的文件树）和变更侧栏（Git 变更列表）；
- 只读的文件预览 Tab 和 diff Tab；
- 工作区管理：添加快捷键、排序、找不到文件夹的状态、行尾的 Git 分支。

所有文件系统和 Git 数据都由后端新增的 `_clawcode/workspace/*` 扩展方法提供。

**Architecture:**
- **协议：** 在 protocol crate 新增 4 个只读扩展方法，参数和结果类型放在 `protocol/src/workspace.rs`。desktop 直接反序列化成这些类型。
- **服务端：** 实现放在 `acp/src/workspace.rs` 的 `WorkspaceRoot`：一个校验过、规范化过的工作区根目录，挂着全部读操作。
  - 读目录和文件用 `ignore` crate；
  - Git 用 `git` CLI，通过 tokio 子进程调用；
  - `extension.rs` 只负责解析参数和分发。
- **UI：**
  - 侧栏增加模式（会话 / 文件 / 变更），由轨道按钮和快捷键切换，模式写进 `desktop.json`；
  - 文件树的数据模型是不依赖窗口的 `FileTree`，渲染用 gpui-kit `Tree`；
  - 两种新 Tab 各是一个视图实体，打开时异步加载，不持久化。

**Tech Stack:** 与 M2 相同。
- 后端新增依赖：workspace 已有的 `ignore = "0.4"`，以及 tokio 的 `process` feature。
- UI 新增使用 gpui-kit 的 `Tree` / `TreeState`、`Editor` / `EditorState`（只读、带行号）、`Badge`；`Diff` / `DiffState` 已在 M1b 用过。

**Spec:** [Desktop 客户端设计](../specs/2026-11-19-desktop-gpui-kit-design.md)（4.6、5.3、5.4、5.5、8、11、12、14、15 节），前序计划见 [M2](./2026-11-20-desktop-m2-interactions-and-controls.md)。

## Global Constraints

- M1a、M1b、M2 的 Global Constraints 继续有效：AGENTS.md 规则 1–16、只用主题 token 和 rem、图标统一用 Lucide、clippy `-Dwarnings`、测试只放在 `tests/` 里。
- **不按任务提交。** 整个 M3 完成、并且用户明确要求后再提交，提交时带上本计划。
- **只读。** 不提供暂存、提交、撤销等 Git 写操作，文件预览也不能编辑（spec 8、16）。
- **为远程预留（spec 4.6）：** 文件树、变更、diff、文件预览的数据一律走 `_clawcode/workspace/*`，desktop 不直接读工作区的文件系统。只有两个例外：本机文件夹选择器和「在文件管理器中显示」。
- **路径参数：**
  - `root` 可以是任意存在的绝对目录，不限于已登记的工作区，这和 `session/new` 接受任意 `cwd` 一致，是有意的。进程内没有风险；将来开放远程时，访问范围由 4.6 节的认证和配对约束，不在这几个方法里再做限制。
  - `root` 是工作区的绝对路径；
  - `path` 是相对 `root`、用 `/` 分隔的路径，空字符串表示根目录；
  - 以下情况一律按 `invalid_params`（-32602）拒绝：绝对路径、含 `..` 的路径、规范化后落在 `root` 外面的路径（含指向外部的符号链接）。
- **上限：**

  | 操作 | 上限 | 超出时 |
  | --- | --- | --- |
  | 列目录 | 2000 项 | `truncated: true` |
  | 读文件 | 1 MiB | 返回 `TooLarge { size }` |
  | 判断二进制 | 看前 8 KiB | 其中有 NUL 字节就返回 `Binary` |
  | diff | 2 MiB | 截断，`truncated: true` |
  | Git 状态 | 5000 条变更 | `truncated: true` |
  | 每次 `git` 调用 | 10 秒 | 超时或找不到 git：`git_status` 返回 `GitUnavailable`，`git_diff` 返回 `WorkspaceError::Git` |

  `git` 调用都带环境变量 `GIT_OPTIONAL_LOCKS=0`，不和用户自己的 git 操作抢 index 锁。
- **列目录：**
  - 目录在前、文件在后，同类按名称排序（不区分大小写）；
  - 遵守 `.gitignore`；
  - 以 `.` 开头的条目默认隐藏，`show_hidden` 为真时才显示；
  - `.git` 目录永远不显示。
- **变更行：** 一个文件一行。行首的状态字母和颜色如下：

  | 字母 | 含义 | 颜色 token |
  | --- | --- | --- |
  | M | 修改 | `warning` |
  | A | 新增 | `success` |
  | D | 删除 | `danger` |
  | R | 重命名 | `info` |
  | ? | 未跟踪 | `muted_foreground` |
  | U | 冲突 | `danger` |

  - 同一个文件既有已暂存又有未暂存的改动时，按工作区里的状态（porcelain 的 Y 位）显示字母。Y 位是 `.` 时才用 index 的状态（X 位）。
  - diff 一律是工作区相对 `HEAD` 的改动（`git diff HEAD -- <path>`）。未跟踪的文件，以及还没有任何提交的仓库，用 `git diff --no-index -- /dev/null <path>`。
- **当前工作区（spec 5.4）：** 取当前 Tab 所属的工作区；没有这样的 Tab 时，取侧栏里上次点过的工作区；再没有就取列表里的第一个。
- **快捷键（spec 11）：**
  - 文件模式：Ctrl+Shift+E（macOS ⌘⇧E）；
  - 变更模式：Ctrl+Shift+G（⌘⇧G）；
  - 添加工作区：Ctrl+O（⌘O）。

  都绑在 `WORKSPACE_CONTEXT`，菜单和 tooltip 显示同一个快捷键。
- **轨道（spec 5.3）：** 从上到下依次是会话（`messages-square`）、文件（`folder`）、变更（`git-branch`）。变更按钮在当前工作区有变更时，右上角显示数量徽标（gpui-kit `Badge`，`.max(99)`）。
- **新 Tab 不写进 `desktop.json`。** 和草稿 Tab 一样，重启后不恢复。
- 新文案都写进 `locales/app.yml`，en 和 zh-CN 都要有。新图标加到 `assets.rs` 的 `ExtraIcons`。

## Review Focus

1. **路径越界：** 参数里有 `../`、绝对路径、指向工作区外的符号链接时，请求按 -32602 拒绝，不读任何工作区外的东西。测试在 Task 2（`paths_outside_the_root_are_rejected`）。
2. **大文件和二进制文件：** 打开 50 MiB 的日志或一张 PNG 时，预览 Tab 不卡住、不崩溃，只显示「文件过大」或「二进制文件」的说明。测试在 Task 2（`large_and_binary_files_are_not_returned`）和 Task 6（`binary_file_shows_a_notice`）。
3. **非 Git 目录、没装 git、空仓库：** 变更侧栏分别显示「此工作区不是 Git 仓库」「未找到 git」；还没有提交的仓库照常列出新文件，diff 能打开。测试在 Task 3（`status_outside_a_repository`、`diff_in_a_repository_without_commits`）和 Task 8（`not_a_repository_shows_a_notice`）。
4. **特殊文件名：** 含空格、中文、换行的文件名，以及重命名的文件，在变更列表里路径都正确，点开都能看到 diff（依赖 `-z` 输出）。测试在 Task 3（`porcelain_parses_renames_and_odd_names`）。
5. **工作区目录被删掉：** 文件或变更侧栏开着、或者预览 Tab 开着的时候，工作区目录被删除：刷新后显示错误说明，不 panic；侧栏这一行变成「找不到文件夹」。测试在 Task 7（`deleted_workspace_shows_an_error`）。

---

### Task 1: 协议类型和方法注册

**Files:**
- Create: `crates/protocol/src/workspace.rs`
- Modify: `crates/protocol/src/lib.rs`（导出）、`crates/protocol/src/identity.rs`（4 个新方法）
- Test: `crates/protocol/tests/workspace.rs`

**Interfaces:**
- Produces（全部 `Serialize + Deserialize`，`#[serde(rename_all = "camelCase")]`，枚举用 `#[serde(tag = "type", rename_all = "snake_case")]`）：
  - `AcpExtensionMethod::{WorkspaceListDir, WorkspaceReadFile, WorkspaceGitStatus, WorkspaceGitDiff}`，方法名依次为 `_clawcode/workspace/list_dir`、`_clawcode/workspace/read_file`、`_clawcode/workspace/git_status`、`_clawcode/workspace/git_diff`；`is_query()` 都返回 `true`；`parse` 能识别这 4 个名字。
  - 参数类型：
    - `WorkspaceRootParameters { root: PathBuf }`
    - `WorkspacePathParameters { root: PathBuf, path: String }`
    - `WorkspaceListDirParameters { root: PathBuf, path: String, show_hidden: bool }`（`show_hidden` 带 `#[serde(default)]`）
  - 列目录：
    - `WorkspaceEntryKind { Directory, File, Symlink }`
    - `WorkspaceDirEntry { name: String, path: String, kind: WorkspaceEntryKind }`
    - `WorkspaceDirListing { entries: Vec<WorkspaceDirEntry>, truncated: bool }`
  - 读文件：`WorkspaceFileContent::{Text { text: String }, Binary, TooLarge { size: u64 }}`
  - Git 状态：
    - `WorkspaceChangeKind { Modified, Added, Deleted, Renamed, Untracked, Conflicted }`，带 `pub fn letter(self) -> char`，依次为 `M A D R ? U`
    - `WorkspaceChange { path: String, original_path: Option<String>, kind: WorkspaceChangeKind }`
    - `WorkspaceGitStatus::{Repository { branch: Option<String>, changes: Vec<WorkspaceChange>, truncated: bool }, NotRepository, GitUnavailable}`
    - `impl WorkspaceGitStatus { pub fn from_porcelain_v2(output: &[u8], limit: usize) -> Self }`：解析 `git status --porcelain=v2 -z --branch --untracked-files=all` 的输出，返回 `Repository`
  - diff：`WorkspaceDiff::{Patch { patch: String, truncated: bool }, Binary, Unchanged}`
  - 上限常量：`pub const WORKSPACE_ENTRY_LIMIT: usize = 2000;`、`WORKSPACE_FILE_LIMIT: u64 = 1024 * 1024;`、`WORKSPACE_DIFF_LIMIT: usize = 2 * 1024 * 1024;`、`WORKSPACE_CHANGE_LIMIT: usize = 5000;`

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn workspace_methods_are_registered_queries() {
    for (method, name) in [
        (AcpExtensionMethod::WorkspaceListDir, "_clawcode/workspace/list_dir"),
        (AcpExtensionMethod::WorkspaceReadFile, "_clawcode/workspace/read_file"),
        (AcpExtensionMethod::WorkspaceGitStatus, "_clawcode/workspace/git_status"),
        (AcpExtensionMethod::WorkspaceGitDiff, "_clawcode/workspace/git_diff"),
    ] {
        assert_eq!(method.as_str(), name);
        assert!(method.is_query());
        assert_eq!(AcpExtensionMethod::parse(name), Some(method));
    }
}

#[test]
fn porcelain_parses_branch_and_change_kinds() {
    let output = b"# branch.oid 1234\0# branch.head main\0\
1 .M N... 100644 100644 100644 a a src/lib.rs\0\
1 A. N... 000000 100644 100644 0 a new.rs\0\
1 D. N... 100644 000000 000000 a 0 gone.rs\0\
u UU N... 1 2 3 4 a b c both.rs\0\
? notes.txt\0";
    let WorkspaceGitStatus::Repository { branch, changes, truncated } =
        WorkspaceGitStatus::from_porcelain_v2(output, 10)
    else { panic!("repository") };
    assert_eq!(branch.as_deref(), Some("main"));
    assert!(!truncated);
    let kinds: Vec<(&str, char)> = changes.iter().map(|c| (c.path.as_str(), c.kind.letter())).collect();
    assert_eq!(kinds, [("src/lib.rs", 'M'), ("new.rs", 'A'), ("gone.rs", 'D'), ("both.rs", 'U'), ("notes.txt", '?')]);
}

#[test]
fn porcelain_parses_renames_and_odd_names() {
    // A rename record carries the original path as the next NUL-separated field.
    let output = b"# branch.head (detached)\0\
2 R. N... 100644 100644 100644 a a R100 new name.rs\0old name.rs\0\
? \xe4\xb8\xad\xe6\x96\x87 dir/line\nbreak.txt\0";
    let WorkspaceGitStatus::Repository { branch, changes, .. } =
        WorkspaceGitStatus::from_porcelain_v2(output, 10)
    else { panic!("repository") };
    assert_eq!(branch, None);
    assert_eq!(changes[0].path, "new name.rs");
    assert_eq!(changes[0].original_path.as_deref(), Some("old name.rs"));
    assert_eq!(changes[0].kind, WorkspaceChangeKind::Renamed);
    assert_eq!(changes[1].path, "中文 dir/line\nbreak.txt");
}

#[test]
fn porcelain_stops_at_the_limit() {
    let output = b"? a\0? b\0? c\0";
    let WorkspaceGitStatus::Repository { changes, truncated, .. } =
        WorkspaceGitStatus::from_porcelain_v2(output, 2)
    else { panic!("repository") };
    assert_eq!(changes.len(), 2);
    assert!(truncated);
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p protocol --test workspace`
Expected: 编译失败，找不到 `WorkspaceGitStatus` 等类型。

- [ ] **Step 3: 实现**

- 解析规则：
  - 按 NUL 切分记录；`# branch.head <name>` 给出分支，值为 `(detached)` 时记为 `None`；
  - `1 XY sub mH mI mW hH hI <path>`：第 8 个空格之后的全部内容是路径（路径本身可以含空格）；
  - `2 XY sub mH mI mW hH hI Xscore <path>`：第 9 个空格之后是路径，紧跟着的下一条 NUL 记录是 `original_path`；
  - `u XY sub m1 m2 m3 mW h1 h2 h3 <path>`：第 10 个空格之后是路径，一律是 `Conflicted`；
  - `? <path>` 是 `Untracked`；`! ` 开头的忽略。
- 状态字母取 Y 位，Y 是 `.` 时取 X 位。`M` 和 `T` 记作 Modified，`A`、`D`、`R` 各记作对应类型，`C` 记作 Added。
- 路径中的非 UTF-8 字节按 `String::from_utf8_lossy` 转换。
- 4 个方法同时加进 `parse` 的数组。`is_query` 的匹配是穷尽的，要显式写进 `true` 那一组。

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test -p protocol --test workspace`
Expected: 4 个测试全部通过。

---

### Task 2: 服务端：工作区根目录、列目录、读文件

**Files:**
- Create: `crates/acp/src/workspace.rs`
- Modify: `crates/acp/src/lib.rs`（`pub mod workspace;`）、`crates/acp/Cargo.toml`（`# fs` 组加 `ignore = { workspace = true }`；tokio 的 features 加 `"process"`）
- Test: `crates/acp/tests/workspace.rs`

**Interfaces:**
- Consumes：Task 1 的全部类型。
- Produces：
  - `pub struct WorkspaceRoot(PathBuf)`：规范化后的根目录。
  - `impl TryFrom<&Path> for WorkspaceRoot`：错误类型为 `WorkspaceError`。必须是绝对路径、存在、是目录，再做 `canonicalize`。
  - `pub enum WorkspaceError { Root(AcpWorkingDirectoryError), OutsideRoot, NotFound, Io(String), Git(String) }`，`thiserror`。`Git` 在 Task 3 才用到：diff 时 git 调用超时、找不到 git 或命令失败。
  - `impl WorkspaceRoot`：
    - `pub fn resolve(&self, relative: &str) -> Result<PathBuf, WorkspaceError>`
    - `pub fn list_dir(&self, relative: &str, show_hidden: bool) -> Result<WorkspaceDirListing, WorkspaceError>`
    - `pub fn read_file(&self, relative: &str) -> Result<WorkspaceFileContent, WorkspaceError>`

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn listing_sorts_directories_first_and_hides_dotfiles() {
    let dir = tempfile::tempdir().expect("workspace");
    std::fs::create_dir(dir.path().join("src")).expect("dir");
    std::fs::write(dir.path().join("b.txt"), "b").expect("file");
    std::fs::write(dir.path().join("A.txt"), "a").expect("file");
    std::fs::write(dir.path().join(".env"), "x").expect("file");
    let root = WorkspaceRoot::try_from(dir.path()).expect("root");

    let names = |listing: WorkspaceDirListing| listing.entries.into_iter().map(|e| e.name).collect::<Vec<_>>();
    assert_eq!(names(root.list_dir("", false).expect("list")), ["src", "A.txt", "b.txt"]);
    assert_eq!(names(root.list_dir("", true).expect("list")), ["src", ".env", "A.txt", "b.txt"]);
}

#[test]
fn listing_respects_gitignore_and_never_shows_git() {
    let dir = tempfile::tempdir().expect("workspace");
    std::process::Command::new("git").arg("init").arg("-q").current_dir(dir.path()).status().expect("git init");
    std::fs::write(dir.path().join(".gitignore"), "target/\n").expect("ignore");
    std::fs::create_dir(dir.path().join("target")).expect("dir");
    std::fs::write(dir.path().join("main.rs"), "").expect("file");
    let root = WorkspaceRoot::try_from(dir.path()).expect("root");

    let listing = root.list_dir("", true).expect("list");
    let names: Vec<_> = listing.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, [".gitignore", "main.rs"]);
}

#[test]
fn paths_outside_the_root_are_rejected() {
    let outside = tempfile::tempdir().expect("outside");
    let dir = tempfile::tempdir().expect("workspace");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).expect("symlink");
    let root = WorkspaceRoot::try_from(dir.path()).expect("root");

    for path in ["../x", "/etc/passwd", "a/../../x", "escape"] {
        assert!(matches!(root.list_dir(path, false), Err(WorkspaceError::OutsideRoot)), "{path}");
    }
}

#[test]
fn large_and_binary_files_are_not_returned() {
    let dir = tempfile::tempdir().expect("workspace");
    std::fs::write(dir.path().join("big.log"), vec![b'a'; 1024 * 1024 + 1]).expect("big");
    std::fs::write(dir.path().join("pic.bin"), [0x89, b'P', b'N', b'G', 0, 0]).expect("binary");
    std::fs::write(dir.path().join("ok.rs"), "fn main() {}").expect("text");
    let root = WorkspaceRoot::try_from(dir.path()).expect("root");

    assert_eq!(root.read_file("big.log").expect("big"), WorkspaceFileContent::TooLarge { size: 1024 * 1024 + 1 });
    assert_eq!(root.read_file("pic.bin").expect("bin"), WorkspaceFileContent::Binary);
    assert_eq!(root.read_file("ok.rs").expect("text"), WorkspaceFileContent::Text { text: "fn main() {}".into() });
}

#[test]
fn listing_stops_at_the_entry_limit() {
    let dir = tempfile::tempdir().expect("workspace");
    for n in 0..protocol::WORKSPACE_ENTRY_LIMIT + 1 {
        std::fs::write(dir.path().join(format!("f{n}")), "").expect("file");
    }
    let root = WorkspaceRoot::try_from(dir.path()).expect("root");
    let listing = root.list_dir("", false).expect("list");
    assert_eq!(listing.entries.len(), protocol::WORKSPACE_ENTRY_LIMIT);
    assert!(listing.truncated);
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p acp --test workspace`
Expected: 编译失败，找不到 `WorkspaceRoot`。

- [ ] **Step 3: 实现**

- `resolve` 的规则：
  - 拒绝绝对路径，拒绝 `Component::ParentDir`；
  - 和根拼接后做 `canonicalize`，不存在时返回 `NotFound`；
  - 规范化结果不以根开头时返回 `OutsideRoot`。
  - 指向工作区外的符号链接因此会被拒绝。
- `list_dir` 用 `ignore::WalkBuilder::new(dir).max_depth(Some(1)).hidden(!show_hidden).git_ignore(true).git_exclude(true).parents(true)`，跳过深度 0 的条目本身和名为 `.git` 的条目。条目的 `path` 是相对根、用 `/` 分隔的路径。先收集再排序，再截到上限。
- `read_file` 先用 `metadata().len()` 判断大小，超过上限就不读内容；读出来的内容前 8 KiB 有 NUL 就是 `Binary`；内容不是 UTF-8 也按 `Binary` 处理。

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test -p acp --test workspace`
Expected: 5 个测试全部通过。

---

### Task 3: 服务端：Git 状态和 diff

**Files:**
- Modify: `crates/acp/src/workspace.rs`
- Test: `crates/acp/tests/workspace.rs`

**Interfaces:**
- Consumes：Task 1 的 `WorkspaceGitStatus::from_porcelain_v2`、`WorkspaceDiff`；Task 2 的 `WorkspaceRoot`。
- Produces（`impl WorkspaceRoot`）：
  - `pub async fn git_status(&self) -> WorkspaceGitStatus`
  - `pub async fn git_diff(&self, relative: &str) -> Result<WorkspaceDiff, WorkspaceError>`

- [ ] **Step 1: 写失败的测试**

测试用临时仓库，`git` 调用前设置 `user.name`、`user.email` 和 `commit.gpgsign=false`。

```rust
#[tokio::test]
async fn status_lists_changes_with_the_branch() {
    let repo = git_repo(); // helper: init, commit `a.rs`, branch `main`
    std::fs::write(repo.path().join("a.rs"), "changed").expect("edit");
    std::fs::write(repo.path().join("new file.rs"), "new").expect("new");
    let root = WorkspaceRoot::try_from(repo.path()).expect("root");

    let WorkspaceGitStatus::Repository { branch, changes, .. } = root.git_status().await else { panic!("repository") };
    assert_eq!(branch.as_deref(), Some("main"));
    let rows: Vec<_> = changes.iter().map(|c| (c.path.as_str(), c.kind.letter())).collect();
    assert_eq!(rows, [("a.rs", 'M'), ("new file.rs", '?')]);
}

#[tokio::test]
async fn status_outside_a_repository() {
    let dir = tempfile::tempdir().expect("plain dir");
    let root = WorkspaceRoot::try_from(dir.path()).expect("root");
    assert_eq!(root.git_status().await, WorkspaceGitStatus::NotRepository);
}

#[tokio::test]
async fn diff_covers_tracked_untracked_and_binary_files() {
    let repo = git_repo();
    std::fs::write(repo.path().join("a.rs"), "changed\n").expect("edit");
    std::fs::write(repo.path().join("u.rs"), "untracked\n").expect("new");
    std::fs::write(repo.path().join("b.bin"), [0u8, 1, 2]).expect("binary");
    let root = WorkspaceRoot::try_from(repo.path()).expect("root");

    let WorkspaceDiff::Patch { patch, truncated } = root.git_diff("a.rs").await.expect("diff") else { panic!("patch") };
    assert!(patch.contains("+changed") && !truncated);
    let WorkspaceDiff::Patch { patch, .. } = root.git_diff("u.rs").await.expect("diff") else { panic!("patch") };
    assert!(patch.contains("+untracked"));
    assert_eq!(root.git_diff("b.bin").await.expect("diff"), WorkspaceDiff::Binary);
}

#[tokio::test]
async fn diff_in_a_repository_without_commits() {
    let dir = tempfile::tempdir().expect("repo");
    std::process::Command::new("git").args(["init", "-q"]).current_dir(dir.path()).status().expect("init");
    std::fs::write(dir.path().join("first.rs"), "hello\n").expect("file");
    let root = WorkspaceRoot::try_from(dir.path()).expect("root");

    let WorkspaceDiff::Patch { patch, .. } = root.git_diff("first.rs").await.expect("diff") else { panic!("patch") };
    assert!(patch.contains("+hello"));
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p acp --test workspace`
Expected: 编译失败，找不到 `git_status`。

- [ ] **Step 3: 实现**

- 每次调用 `git` 都通过一个私有方法：`tokio::process::Command::new("git")`，加 `-C <root>`，设置环境变量 `GIT_OPTIONAL_LOCKS=0`，用 `tokio::time::timeout(10s)` 包住。
  - 找不到程序（`ErrorKind::NotFound`）或超时：`git_status` 返回 `GitUnavailable`，`git_diff` 返回 `Err(WorkspaceError::Git(原因))`，两者都打一条 `tracing::warn!`。
- 状态：
  - 先跑 `rev-parse --is-inside-work-tree`，失败时返回 `NotRepository`；
  - 再跑 `status --porcelain=v2 -z --branch --untracked-files=all`，把输出交给 `from_porcelain_v2(.., WORKSPACE_CHANGE_LIMIT)`。
- diff：
  - 先 `resolve` 校验路径（删除的文件 `resolve` 会返回 `NotFound`，这时改为只做词法校验：不是绝对路径、不含 `..`）；
  - 用 `ls-files --error-unmatch -- <path>` 判断文件是否被跟踪，`rev-parse --verify HEAD` 判断有没有提交；
  - 被跟踪并且有 HEAD 时跑 `diff HEAD -- <path>`；否则跑 `diff --no-index -- /dev/null <path>`（这条命令有差异时退出码为 1，不算失败）。
- 输出里含 `Binary files` 并且没有 `@@` 时返回 `Binary`；输出为空返回 `Unchanged`；超过 `WORKSPACE_DIFF_LIMIT` 字节时截断到最后一个完整行，`truncated: true`。

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test -p acp --test workspace`
Expected: 9 个测试全部通过。

---

### Task 4: 扩展方法分发和 desktop 客户端

**Files:**
- Modify: `crates/acp/src/extension.rs`（4 个分支）、`crates/acp/src/server.rs`（`initialize` 广告的方法列表加 4 个）
- Modify: `crates/desktop/src/backend/client.rs`
- Test: `crates/acp/tests/extension.rs`、`crates/desktop/tests/backend.rs`

**Interfaces:**
- Consumes：Task 1–3。
- Produces（`impl AcpClient`）：
  - `pub async fn list_dir(&self, root: &Path, path: &str, show_hidden: bool) -> Result<WorkspaceDirListing, BackendError>`
  - `pub async fn read_file(&self, root: &Path, path: &str) -> Result<WorkspaceFileContent, BackendError>`
  - `pub async fn git_status(&self, root: &Path) -> Result<WorkspaceGitStatus, BackendError>`
  - `pub async fn git_diff(&self, root: &Path, path: &str) -> Result<WorkspaceDiff, BackendError>`

- [ ] **Step 1: 写失败的测试**

`crates/acp/tests/extension.rs`（沿用文件里的 `send_request(kernel, IntegrationRequest)` 和 `integration_kernel(root)`；先把 4 个新方法加进 `IntegrationRequest::matches_method` 的白名单，否则请求根本发不出去）：

```rust
#[tokio::test]
async fn workspace_methods_answer_and_reject_escapes() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("a.txt"), "hi").expect("file");
    let root = workspace.path().to_string_lossy().into_owned();

    let data = tempfile::tempdir().expect("kernel data");

    let listing = send_request(
        integration_kernel(data.path()),
        IntegrationRequest::new(
            AcpExtensionMethod::WorkspaceListDir.as_str(),
            serde_json::json!({ "root": root, "path": "" }),
        ),
    )
    .await
    .expect("list");
    assert_eq!(listing["entries"][0]["name"], "a.txt");

    let error = send_request(
        integration_kernel(data.path()),
        IntegrationRequest::new(
            AcpExtensionMethod::WorkspaceReadFile.as_str(),
            serde_json::json!({ "root": root, "path": "../x" }),
        ),
    )
    .await
    .expect_err("escape");
    assert_eq!(error.code, agent_client_protocol::ErrorCode::INVALID_PARAMS);
}
```

`crates/desktop/tests/backend.rs`（`ScriptedBackend` 的 `workspace` 是临时目录）：

```rust
#[test]
fn workspace_tools_round_trip_through_the_client() {
    let backend = ScriptedBackend::start();
    std::fs::write(backend.workspace.path().join("notes.md"), "# hi").expect("file");
    let root = backend.workspace.path();

    let listing = backend.block_on(backend.client.list_dir(root, "", false)).expect("list");
    assert!(listing.entries.iter().any(|entry| entry.name == "notes.md"));
    assert_eq!(
        backend.block_on(backend.client.read_file(root, "notes.md")).expect("read"),
        WorkspaceFileContent::Text { text: "# hi".into() }
    );
    assert!(matches!(
        backend.block_on(backend.client.git_status(root)).expect("status"),
        WorkspaceGitStatus::NotRepository | WorkspaceGitStatus::Repository { .. }
    ));
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p acp --test extension workspace && cargo test -p desktop --test backend workspace`
Expected: acp 返回 method not found；desktop 编译失败。

- [ ] **Step 3: 实现**

- `extension.rs` 的 4 个分支：
  - 反序列化参数，再 `WorkspaceRoot::try_from(&params.root)`；
  - `list_dir` 和 `read_file` 包进 `tokio::task::spawn_blocking`；
  - `WorkspaceError::{Root, OutsideRoot, NotFound}` 映射成 `invalid_parameters`，`Io` 和 `Git` 映射成 internal error；
  - 结果用 `serde_json::to_value`。
- desktop 端每个方法都用 `self.extension(AcpExtensionMethod::.., serde_json::to_value(params)?)`，再 `serde_json::from_value` 成 protocol 类型；反序列化失败映射成 `BackendError::Request`。

- [ ] **Step 4: 运行，确认通过**

Run: 同 Step 2。
Expected: 通过。

---

### Task 5: 侧栏模式、轨道和当前工作区

**Files:**
- Modify: `crates/desktop/src/state.rs`（`LayoutState` 加 `sidebar_mode`）、`crates/desktop/src/actions.rs`、`crates/desktop/src/ui/shell/rail.rs`、`crates/desktop/src/ui/sidebar.rs`、`crates/desktop/src/ui/mod.rs`、`crates/desktop/src/assets.rs`、`crates/desktop/locales/app.yml`
- Test: `crates/desktop/tests/shell_tools.rs`（新建）

**Interfaces:**
- Produces：
  - `state.rs`：`#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "snake_case")] pub enum SidebarMode { #[default] Sessions, Files, Changes }`，`LayoutState.sidebar_mode: SidebarMode`（`#[serde(default)]`）。
  - `actions.rs`：`ShowFiles`、`ShowChanges`、`AddWorkspace`，分别绑定 `primary!("shift-e")`、`primary!("shift-g")`、`primary!("o")`，context 为 `WORKSPACE_CONTEXT`。
  - `SidebarState.selected_workspace: Option<PathBuf>`：点击工作区行或它下面的会话行时设置。
  - `impl AppView`：
    - `pub fn sidebar_mode(&self) -> SidebarMode`
    - `pub fn set_sidebar_mode(&mut self, mode: SidebarMode, cx: &mut Context<'_, Self>)`：会打开已折叠的侧栏，然后持久化；
    - `pub(in crate::ui) fn current_workspace(&self) -> Option<PathBuf>`：按 Global Constraints 的三级回退。
  - 轨道按钮 id：`rail-sessions`、`rail-files`、`rail-changes`，都加 `.test_support()`。

- [ ] **Step 1: 写失败的测试**

```rust
#[gpui_kit::test]
fn rail_and_keys_switch_the_sidebar_mode(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    live.window(cx, |window, cx| window.click(SharedString::from("rail-files"), cx));
    cx.update(|cx| assert_eq!(live.view.read(cx).sidebar_mode(), SidebarMode::Files));
    live.window(cx, |window, cx| window.press("ctrl-shift-g", cx));
    cx.update(|cx| assert_eq!(live.view.read(cx).sidebar_mode(), SidebarMode::Changes));
    live.window(cx, |window, cx| window.click(SharedString::from("rail-sessions"), cx));
    cx.update(|cx| assert_eq!(live.view.read(cx).sidebar_mode(), SidebarMode::Sessions));
}

#[gpui_kit::test]
fn sidebar_mode_survives_a_restart(cx: &mut TestAppContext) {
    // Switch to Changes, read the saved state back, and open a second shell from it.
    let mut state = DesktopState::default();
    state.layout.sidebar_mode = SidebarMode::Changes;
    let (_handle, view, _dir) = support::shell::open(cx, state);
    cx.update(|cx| assert_eq!(view.read(cx).sidebar_mode(), SidebarMode::Changes));
}

#[test]
fn unknown_sidebar_mode_falls_back_to_sessions() {
    let state: DesktopState = serde_json::from_str(r#"{"version":1,"layout":{"sidebar_mode":"inspector"}}"#)
        .expect("an unknown mode does not fail the file");
    assert_eq!(state.layout.sidebar_mode, SidebarMode::Sessions);
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p desktop --test shell_tools`
Expected: 编译失败，找不到 `SidebarMode`。

- [ ] **Step 3: 实现**

- `render_sidebar` 按模式分派：会话模式沿用现有内容；文件和变更模式在本任务先渲染一个占位标题，具体内容在 Task 7、8 填上。
- 轨道的变更按钮在 Task 8 才挂上徽标。
- `LayoutState` 里的未知模式值：现有的 `desktop.json` 解析失败会整体回退到默认值（M1 行为）。`sidebar_mode` 加 `#[serde(default)]`，未知值要单独回退到 `Sessions`，不能连累整个文件。可以给 `SidebarMode` 写一个 `deserialize_with`，把未知字符串映射成默认值。

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test -p desktop --test shell_tools`
Expected: 3 个测试全部通过。

---

### Task 6: 文件预览 Tab 和 diff Tab

**Files:**
- Create: `crates/desktop/src/ui/viewers/mod.rs`、`crates/desktop/src/ui/viewers/file_view.rs`、`crates/desktop/src/ui/viewers/diff_view.rs`
- Modify: `crates/desktop/src/ui/mod.rs`（`TabEntry` 两个新变体）、`crates/desktop/src/ui/shell/card.rs`（Tab 标题和内容）、`crates/desktop/src/ui/shell/tab_ops.rs`、`crates/desktop/src/ui/shell/drafts.rs`（创建视图）、`locales/app.yml`
- Modify: `crates/desktop/tests/support/shell.rs`：加 `pub fn live_in_git_repo(cx) -> Live`（在后端的临时工作区里 `git init`，设置 `user.name`、`user.email`、`commit.gpgsign=false`，提交内容为 `x` 的 `a.rs` 和内容为 `alpha` 的 `notes.txt`，分支名 `main`）
- Test: `crates/desktop/tests/shell_viewers.rs`（新建）

**Interfaces:**
- Consumes：Task 4 的 `read_file`、`git_diff`。
- Produces：
  - `TabEntry::File { workspace: PathBuf, path: String }`、`TabEntry::Diff { workspace: PathBuf, path: String }`；`key()` 分别为 `file:{workspace}:{path}`、`diff:{workspace}:{path}`。
  - `impl AppView`：
    - `pub fn open_file(&mut self, workspace: PathBuf, path: String, window: &mut Window, cx: &mut Context<'_, Self>)`
    - `pub fn open_diff(&mut self, workspace: PathBuf, path: String, window: &mut Window, cx: &mut Context<'_, Self>)`
    - 同一个 key 已经打开时只激活它，不重复开。
  - `pub struct FileView`、`pub struct DiffView`：各是一个实体，`new(workspace, path, client, window, cx)` 后立即异步加载。都带 `pub fn reload(&mut self, window, cx)`，Tab 被激活时调用一次。
  - `impl FileView { pub fn editor(&self) -> Option<&Entity<EditorState>> }`：渲染也用它，加载完成并且是文本时为 `Some`。
  - `impl DiffView { pub fn state(&self) -> Option<&Entity<DiffState>> }`：同上，是补丁时为 `Some`。
  - `AppView` 提供 `pub fn file_view(&self, workspace: &Path, path: &str) -> Option<Entity<FileView>>` 和同形的 `diff_view`，从已打开的 Tab 视图里查。
  - 测试读这两个实体的状态，不靠绘制快照：M2 已经证实输入框里的文字不进无界面测试的快照，`Editor` 和 `Diff` 的正文也不能指望。
  - 根元素 id 分别是 `file-view`、`diff-view`，都加 `.test_support()`。
  - `TabsState` 不变：新 Tab 不写进 `desktop.json`。

- [ ] **Step 1: 写失败的测试**

```rust
#[gpui_kit::test]
fn file_tab_shows_the_text_read_only(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    std::fs::write(live.backend.workspace.path().join("main.rs"), "fn main() {}\n").expect("file");
    live.window(cx, |window, cx| live.view.update(cx, |view, cx| {
        view.open_file(live.backend.workspace.path().to_path_buf(), "main.rs".into(), window, cx)
    }));
    let root = live.backend.workspace.path().to_path_buf();
    let text = |cx: &App| {
        let view = live.view.read(cx).file_view(&root, "main.rs")?;
        Some(view.read(cx).editor()?.read(cx).value().to_string())
    };
    eventually(cx, "text loaded", |cx| text(cx).as_deref() == Some("fn main() {}\n"));
    // Typing into a read-only preview changes nothing.
    live.window(cx, |window, cx| window.input("x", cx));
    cx.update(|cx| assert_eq!(text(cx).as_deref(), Some("fn main() {}\n")));
}

#[gpui_kit::test]
fn binary_file_shows_a_notice(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    std::fs::write(live.backend.workspace.path().join("pic.png"), [0x89, b'P', 0, 0]).expect("file");
    live.window(cx, |window, cx| live.view.update(cx, |view, cx| {
        view.open_file(live.backend.workspace.path().to_path_buf(), "pic.png".into(), window, cx)
    }));
    live.until(cx, "notice", |window| shows(window, "Binary file"));
}

#[gpui_kit::test]
fn opening_the_same_file_twice_reuses_the_tab(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    std::fs::write(live.backend.workspace.path().join("a.md"), "a").expect("file");
    for _ in 0..2 {
        live.window(cx, |window, cx| live.view.update(cx, |view, cx| {
            view.open_file(live.backend.workspace.path().to_path_buf(), "a.md".into(), window, cx)
        }));
    }
    cx.update(|cx| assert_eq!(live.view.read(cx).tabs().len(), 1));
}

#[gpui_kit::test]
fn diff_tab_renders_the_patch(cx: &mut TestAppContext) {
    let live = live_in_git_repo(cx);
    std::fs::write(live.backend.workspace.path().join("a.rs"), "changed\n").expect("edit");
    live.window(cx, |window, cx| live.view.update(cx, |view, cx| {
        view.open_diff(live.backend.workspace.path().to_path_buf(), "a.rs".into(), window, cx)
    }));
    let root = live.backend.workspace.path().to_path_buf();
    eventually(cx, "patch parsed", |cx| {
        live.view.read(cx).diff_view(&root, "a.rs")
            .and_then(|view| view.read(cx).state().cloned())
            .is_some()
    });
}

#[gpui_kit::test]
fn viewer_tabs_are_not_restored(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    std::fs::write(live.backend.workspace.path().join("a.md"), "a").expect("file");
    live.window(cx, |window, cx| live.view.update(cx, |view, cx| {
        view.open_file(live.backend.workspace.path().to_path_buf(), "a.md".into(), window, cx)
    }));
    cx.update(|cx| assert!(live.view.read(cx).state().tabs.open.is_empty()));
}
```

实现前先用一个最小测试确认两点：`Editor` 在 `readonly(true)` 下收到输入后 `value()` 确实不变；`DiffFile::parse` 能解析 `git diff --no-index` 产生的补丁（它的文件头是 `a//dev/null`）。

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p desktop --test shell_viewers`
Expected: 编译失败，找不到 `open_file`。

- [ ] **Step 3: 实现**

- **FileView：**
  - `Text` 时创建 `EditorState::new(window, cx).language(<扩展名>).line_number(true).default_value(text)`，渲染 `Editor::new(&state).readonly(true)`。语言名直接用文件扩展名，gpui-kit 认不出的扩展名按纯文本处理。
  - `Binary` 显示 `Viewer.binary`（「二进制文件，无法预览」/「Binary file — no preview」）。
  - `TooLarge { size }` 显示 `Viewer.too_large`（大小按 `{n:.1} MiB` 显示）。
  - 出错时显示错误原文和「重试」按钮。
  - 加载中显示 `Skeleton`。
- **DiffView：**
  - `Patch` 时 `DiffFile::parse(&patch)` → `DiffState::new(files, cx)`，渲染 `Diff::new(&state).line_number(true).syntax_highlight(true)`；
  - `truncated` 时在顶部加一条 `Viewer.diff_truncated`；
  - `Binary` 显示 `Viewer.binary_diff`；`Unchanged` 显示 `Viewer.unchanged`（「这个文件已经没有改动」）。
- **Tab：**
  - 标题是文件名，前面加图标（文件用 `file-text`，diff 用 `git-compare`）；tooltip 是相对路径。
  - 运行状态区不显示内容；`⋯` 菜单只有「关闭」。
  - 关闭沿用现有的 Tab 关闭规则（右邻优先）。
- **文案：** `Viewer.loading`、`Viewer.binary`、`Viewer.too_large`、`Viewer.binary_diff`、`Viewer.unchanged`、`Viewer.diff_truncated`、`Viewer.retry`。

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test -p desktop --test shell_viewers`
Expected: 全部通过。

---

### Task 7: 文件侧栏

**Files:**
- Create: `crates/desktop/src/file_tree.rs`（纯数据模型，不依赖 gpui，和 projection、transcript 同层）、`crates/desktop/src/ui/tools/mod.rs`、`crates/desktop/src/ui/tools/files.rs`（把节点转换成 gpui-kit `TreeItem` 并渲染）
- Modify: `crates/desktop/src/lib.rs`、`crates/desktop/src/ui/mod.rs`、`crates/desktop/src/ui/sidebar.rs`、`locales/app.yml`
- Modify: `crates/desktop/tests/support/shell.rs`：加 `impl Live { pub fn add_workspace(&self, cx, path: &Path) }`（登记工作区并设为侧栏当前选中的工作区）
- Test: `crates/desktop/tests/file_tree.rs`、`crates/desktop/tests/shell_tools.rs`

**Interfaces:**
- Consumes：Task 4 的 `AcpClient::list_dir`；Task 5 的 `current_workspace`、`SidebarMode::Files`；Task 6 的 `AppView::open_file`。
- Produces：
  - `pub struct FileTree`（`file_tree.rs`）：
    - `pub fn new(root: PathBuf) -> Self`
    - `pub fn root(&self) -> &Path`
    - `pub fn set_listing(&mut self, dir: &str, listing: WorkspaceDirListing)`
    - `pub fn needs_listing(&self, dir: &str) -> bool`
    - `pub fn set_expanded(&mut self, dir: &str, expanded: bool)`
    - `pub fn expanded_dirs(&self) -> Vec<String>`（含根目录 `""`，按深度排序，刷新时按这个顺序重新列）
    - `pub fn nodes(&self) -> Vec<FileNode>`
    - `pub fn clear(&mut self)`
  - `#[derive(Debug, Clone, PartialEq, Eq)] pub struct FileNode { pub path: String, pub name: String, pub kind: FileNodeKind, pub expanded: bool, pub children: Vec<FileNode> }`，`TypedBuilder`；`pub enum FileNodeKind { Directory, File, Placeholder(PlaceholderKind) }`，`PlaceholderKind { Loading, Empty, Truncated }`。
  - 节点的 `path` 就是相对路径。还没加载过的目录带一个 `Loading` 占位子节点，已加载的空目录带一个 `Empty` 占位子节点。原因是 gpui-kit 只把有子节点的项当作文件夹（`is_folder` 看的是 `children` 是否为空）。占位节点的 `path` 为 `{dir}/\u{0}`，转换成 `TreeItem` 时设为禁用。
  - `ui/tools/files.rs`：`impl From<&FileNode> for TreeItem`（递归转换，`expanded` 原样带过去）。
  - `AppView` 持有 `files: Option<FileTreeView>`。`FileTreeView { tree: FileTree, state: Entity<TreeState>, show_hidden: bool, error: Option<String> }` 有 4 个字段，按规则 5 用 `TypedBuilder` 构造。工作区变化时整体重建。
  - `render_item` 给每一行一个 id `file-node-{path}` 并加 `.test_support()`。文件行在 `render_item` 返回的元素上挂 `on_click` 打开预览；文件夹行的点击交给 `Tree` 自己处理展开和收起；占位行不响应点击。
  - 侧栏标题行有两个按钮：`files-hidden`（切换显示隐藏文件，图标 `eye` / `eye-off`）和 `files-refresh`（`refresh-cw`）。

- [ ] **Step 1: 写失败的测试**

`tests/file_tree.rs`：

```rust
fn listing(names: &[(&str, WorkspaceEntryKind)], dir: &str) -> WorkspaceDirListing { /* builds entries with path = dir/name */ }

#[test]
fn unloaded_directories_render_as_folders_with_a_placeholder() {
    let mut tree = FileTree::new("/w".into());
    tree.set_listing("", listing(&[("src", Directory), ("a.rs", File)], ""));
    let nodes = tree.nodes();
    assert_eq!(nodes[0].path, "src");
    assert_eq!(nodes[0].kind, FileNodeKind::Directory);
    assert!(!nodes[0].expanded);
    assert_eq!(nodes[0].children[0].kind, FileNodeKind::Placeholder(PlaceholderKind::Loading));
    assert_eq!(nodes[1].kind, FileNodeKind::File);
    assert!(nodes[1].children.is_empty());
    assert!(tree.needs_listing("src"));
}

#[test]
fn expanding_a_loaded_directory_shows_its_children() {
    let mut tree = FileTree::new("/w".into());
    tree.set_listing("", listing(&[("src", Directory)], ""));
    tree.set_listing("src", listing(&[("lib.rs", File)], "src"));
    tree.set_expanded("src", true);
    let nodes = tree.nodes();
    assert!(nodes[0].expanded);
    assert_eq!(nodes[0].children[0].path, "src/lib.rs");
    assert_eq!(tree.expanded_dirs(), ["", "src"]);
}

#[test]
fn an_empty_directory_is_still_a_folder() {
    let mut tree = FileTree::new("/w".into());
    tree.set_listing("", listing(&[("empty", Directory)], ""));
    tree.set_listing("empty", listing(&[], "empty"));
    // An empty loaded folder keeps one placeholder child so it stays a folder.
    assert_eq!(
        tree.nodes()[0].children[0].kind,
        FileNodeKind::Placeholder(PlaceholderKind::Empty)
    );
}
```

`tests/shell_tools.rs`：

```rust
#[gpui_kit::test]
fn files_mode_lists_the_workspace_and_opens_a_file(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default()); // workspace = backend temp dir
    std::fs::create_dir(live.backend.workspace.path().join("src")).expect("dir");
    std::fs::write(live.backend.workspace.path().join("src/lib.rs"), "pub fn a() {}").expect("file");
    live.window(cx, |window, cx| window.press("ctrl-shift-e", cx));
    live.until(cx, "file tree", |window| shows(window, "src"));
    live.window(cx, |window, cx| window.click(SharedString::from("file-node-src"), cx));
    live.until(cx, "expanded", |window| window.try_find("file-node-src/lib.rs").is_some());
    live.window(cx, |window, cx| window.click(SharedString::from("file-node-src/lib.rs"), cx));
    live.until(cx, "file tab", |window| window.try_find("file-view").is_some());
}

#[gpui_kit::test]
fn deleted_workspace_shows_an_error(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default());
    let gone = tempfile::tempdir().expect("gone");
    let path = gone.path().to_path_buf();
    live.add_workspace(cx, &path); // helper: registers and selects the workspace
    drop(gone);
    live.window(cx, |window, cx| window.press("ctrl-shift-e", cx));
    live.until(cx, "error", |window| shows(window, "Folder not found"));
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p desktop --test file_tree --test shell_tools`
Expected: 编译失败，找不到 `FileTree`。

- [ ] **Step 3: 实现**

- **加载：** 进入文件模式、工作区变化、点刷新时，按 `expanded_dirs()` 的顺序依次 `list_dir`，全部完成后一次 `set_items`。
- **展开：** 订阅 `TreeEvent::Expanded(id)`：先 `set_expanded`；`needs_listing` 为真时异步加载这一个目录，回来后 `set_items`。`Collapsed` 只做 `set_expanded(false)`。
- **点文件：** 点击非文件夹节点时调用 `open_file(workspace, path)`。点击占位节点不做任何事。
- **错误：** 根目录列不出来时（工作区被删），整个侧栏显示已有的 `Sidebar.missing_folder`（「找不到文件夹」/「Folder not found」），不新建同义的键；下面带重试按钮。
- **截断：** `truncated` 为真时，在那一层末尾加一个禁用的「仅显示前 2000 项」节点。
- **文案：** `Files.title`、`Files.loading`、`Files.empty_dir`、`Files.truncated`、`Files.show_hidden`、`Files.hide_hidden`、`Files.refresh`、`Files.retry`、`Files.no_workspace`。

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test -p desktop --test file_tree --test shell_tools`
Expected: 通过。

---

### Task 8: 变更侧栏、轨道徽标、工作区行的分支

**Files:**
- Create: `crates/desktop/src/ui/tools/changes.rs`
- Modify: `crates/desktop/src/ui/mod.rs`、`crates/desktop/src/ui/sidebar.rs`、`crates/desktop/src/ui/shell/rail.rs`、`locales/app.yml`
- Modify: `crates/desktop/tests/support/shell.rs`：加 `pub fn row_texts(window: &mut Window, id: &str) -> Vec<String>`（只取位于这个元素边界内的绘制文字，按从左到右排序）
- Test: `crates/desktop/tests/shell_tools.rs`

**Interfaces:**
- Consumes：Task 4 的 `git_status`；Task 5 的模式和当前工作区；Task 6 的 `open_diff`。
- Produces：
  - `AppView.git: HashMap<PathBuf, GitSnapshot>`，`pub struct GitSnapshot { pub status: Option<WorkspaceGitStatus>, pub loading: bool }`。
  - `impl AppView`：
    - `pub fn refresh_git(&mut self, workspace: PathBuf, cx: &mut Context<'_, Self>)`：已有请求在跑时不重复发；
    - `pub fn change_count(&self, workspace: &Path) -> usize`（测试也要用，所以是 `pub`）。
  - 变更行 id：`change-row-{n}`；刷新按钮 id：`changes-refresh`；徽标包在 `rail-changes` 按钮上。

- [ ] **Step 1: 写失败的测试**

```rust
#[gpui_kit::test]
fn changes_mode_lists_letters_and_opens_a_diff(cx: &mut TestAppContext) {
    let live = live_in_git_repo(cx); // helper: backend workspace is a repo with one commit of `a.rs`
    std::fs::write(live.backend.workspace.path().join("a.rs"), "changed").expect("edit");
    live.window(cx, |window, cx| window.press("ctrl-shift-g", cx));
    live.until(cx, "change row", |window| window.try_find("change-row-0").is_some());
    let row = live.window(cx, |window, _| row_texts(window, "change-row-0")); // helper: painted texts inside one element
    assert_eq!(row, ["M", "a.rs"]);
    live.window(cx, |window, cx| window.click(SharedString::from("change-row-0"), cx));
    live.until(cx, "diff tab", |window| window.try_find("diff-view").is_some());
}

#[gpui_kit::test]
fn not_a_repository_shows_a_notice(cx: &mut TestAppContext) {
    let live = live(cx, DesktopState::default()); // plain temp dir
    live.window(cx, |window, cx| window.press("ctrl-shift-g", cx));
    live.until(cx, "notice", |window| shows(window, "This workspace is not a Git repository"));
}

#[gpui_kit::test]
fn a_finished_run_refreshes_the_changes(cx: &mut TestAppContext) {
    let live = live_in_git_repo(cx);
    let id = live.open_new_session(cx);
    live.window(cx, |window, cx| window.press("ctrl-shift-g", cx));
    live.until(cx, "clean", |window| shows(window, "No changes"));
    // The scripted `edit` tool rewrites notes.txt in the workspace.
    live.run(cx, &id, vec!["edit".into()]);
    live.until(cx, "refreshed", |window| shows(window, "notes.txt"));
}

#[gpui_kit::test]
fn switching_workspace_loads_its_changes(cx: &mut TestAppContext) {
    let live = live_in_git_repo(cx);
    let other = tempfile::tempdir().expect("other workspace");
    live.add_workspace(cx, other.path());
    live.window(cx, |window, cx| window.press("ctrl-shift-g", cx));
    live.until(cx, "notice", |window| shows(window, "This workspace is not a Git repository"));
}

#[gpui_kit::test]
fn rail_badge_counts_the_changes(cx: &mut TestAppContext) {
    let live = live_in_git_repo(cx);
    std::fs::write(live.backend.workspace.path().join("x.rs"), "x").expect("new");
    std::fs::write(live.backend.workspace.path().join("y.rs"), "y").expect("new");
    live.window(cx, |window, cx| window.press("ctrl-shift-g", cx));
    let root = live.backend.workspace.path().to_path_buf();
    eventually(cx, "two changes", |cx| live.view.read(cx).change_count(&root) == 2);
    live.until(cx, "badge", |window| row_texts(window, "rail-changes").contains(&"2".to_string()));
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p desktop --test shell_tools changes`
Expected: 失败。

- [ ] **Step 3: 实现**

- **刷新时机（spec 8）：**
  - 切到变更模式；
  - 点 `changes-refresh`；
  - 收到 `RegistryEvent::SessionChanged(id)`，并且这个会话的 `cwd` 等于某个工作区：刷新那个工作区；
  - 后端就绪后，对每个存在的工作区刷新一次，给工作区行拿到分支；
  - 变更模式开着时当前工作区变了（比如切到另一个工作区的 Tab），而 `git` 里还没有这个工作区的快照：拉一次。已有快照就直接显示，不重复请求。
- **行的样式：** 状态字母（颜色见 Global Constraints，等宽、`font_semibold`）、文件名、灰色的目录（`muted_foreground`，过长时截断）。重命名行的目录后面再加「← 原名」。
- **各种状态：**
  - `NotRepository` 显示 `Changes.not_repository`；
  - `GitUnavailable` 显示 `Changes.git_missing`；
  - 列表为空显示 `Changes.clean`（「没有改动」/「No changes」）；
  - `truncated` 时在末尾加一行「仅显示前 5000 项」。
- **徽标：** 当前工作区 `change_count > 0` 时，`rail-changes` 外面包 `Badge::new().count(n).max(99)`。
- **工作区行：** 有分支时在行尾显示 `git-branch` 图标加分支名（`text_xs`、`muted_foreground`，最长 12 个字符，超出截断）。
- **文案：** `Changes.title`、`Changes.refresh`、`Changes.clean`、`Changes.not_repository`（「此工作区不是 Git 仓库」/「This workspace is not a Git repository」）、`Changes.git_missing`（「未找到 git」/「git was not found」）、`Changes.truncated`、`Changes.renamed_from`。

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test -p desktop --test shell_tools`
Expected: 通过。

---

### Task 9: 工作区管理

**Files:**
- Modify: `crates/desktop/src/workspace.rs`（`move_entry`）、`crates/desktop/src/ui/shell/dialogs.rs`、`crates/desktop/src/ui/shell/title_bar.rs`（文件菜单）、`crates/desktop/src/ui/sidebar.rs`（右键菜单）、`locales/app.yml`
- Test: `crates/desktop/tests/state.rs`（或现有的 workspace 注册表测试文件）、`crates/desktop/tests/shell_actions.rs`

**Interfaces:**
- Produces：
  - `impl WorkspaceRegistry { pub fn move_entry(&mut self, path: &Path, offset: isize) -> bool }`：按偏移移动，越界时不动并返回 `false`。
  - 工作区行的右键菜单在现有三项之外加「上移」「下移」，第一个和最后一个分别禁用。
  - `AddWorkspace` 动作（Ctrl+O）打开现有的添加工作区对话框；文件菜单加同名菜单项。
  - `impl AppView { pub fn add_dropped_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<'_, Self>) -> usize }`：spec 8 的「拖入文件夹」。只登记其中的目录（规范化后），文件忽略，返回新增的个数。侧栏根元素加 `.on_drop::<ExternalPaths>(..)` 调用它，拖入时用 `drag_over` 显示和 composer 一样的高亮。

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn workspaces_move_up_and_down_within_bounds() {
    let mut registry = WorkspaceRegistry::default();
    for path in ["/a", "/b", "/c"] { registry.add(Path::new(path)); }
    assert!(registry.move_entry(Path::new("/c"), -1));
    assert!(!registry.move_entry(Path::new("/a"), -1));
    let order: Vec<_> = registry.entries().iter().map(|e| e.path.clone()).collect();
    assert_eq!(order, [PathBuf::from("/a"), PathBuf::from("/c"), PathBuf::from("/b")]);
}

#[gpui_kit::test]
fn ctrl_o_is_bound_to_add_workspace(cx: &mut TestAppContext) {
    let (handle, _view, _dir) = support::shell::open(cx, DesktopState::default());
    cx.update_window(handle, |_, window, _| {
        let binding = window.highest_precedence_binding_for_action(&AddWorkspace).expect("bound");
        assert_eq!(binding.keystrokes()[0].unparse(), if cfg!(target_os = "macos") { "cmd-o" } else { "ctrl-o" });
    }).expect("window");
}

#[gpui_kit::test]
fn dropped_folders_become_workspaces(cx: &mut TestAppContext) {
    let (handle, view, _dir) = support::shell::open(cx, DesktopState::default());
    let folder = tempfile::tempdir().expect("folder");
    let file = folder.path().join("note.txt");
    std::fs::write(&file, "x").expect("file");
    let added = cx.update_window(handle, |_, _, cx| view.update(cx, |view, cx| {
        view.add_dropped_paths(vec![folder.path().to_path_buf(), file.clone()], cx)
    })).expect("window");
    assert_eq!(added, 1);
    cx.update(|cx| assert!(view.read(cx).state().workspaces.contains(folder.path())));
}
```

（路径选择器本身在无界面测试里没有可用的探针，所以 Ctrl+O 只测绑定；对话框是 M1 已有的代码路径。）

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p desktop workspaces_move_up_and_down_within_bounds ctrl_o_is_bound dropped_folders`
Expected: 编译失败。

- [ ] **Step 3: 实现**

- **排序：** spec 写的是拖拽排序，但 gpui-kit 的 `SidebarMenuItem` 没有拖放回调，所以第一版用右键菜单的「上移 / 下移」，见文末的「需要确认的偏离」。移动后立即持久化。
- **找不到文件夹：** M1 已经有 muted 样式和「找不到文件夹」。本任务确认工作区目录在运行中被删掉后，下一次刷新 catalog 或 Git 时，这一行会变成这个状态；右键菜单的「移除」可用，「新会话」禁用。为这一条补一个 UI 测试 `deleted_workspace_row_turns_missing`。

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test -p desktop`
Expected: 全部通过。

---

### Task 10: 验收

**Files:** 无代码改动；截图和记录放在 `.superpowers/sdd/<本计划>/` 下（已被 git 忽略）。

- [ ] **Step 1: 全量检查**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -Dwarnings && cargo test --workspace --no-fail-fast && pre-commit run --all-files`
Expected: 全部通过。

- [ ] **Step 2: 生产后端实测（规则 9）**

在嵌套 KWin 里启动 debug 构建，`XDG_CONFIG_HOME` 和 `XDG_DATA_HOME` 都指向隔离目录，工作区用本仓库的一个临时克隆。逐项检查：
1. 文件侧栏：展开多层目录、切换隐藏文件、刷新、打开 Rust 和 Markdown 文件，确认语法高亮和只读；
2. 打开一个大于 1 MiB 的文件和一张图片，看到对应说明；
3. 改两个文件、加一个新文件、重命名一个文件，变更侧栏显示 M、?、R，轨道徽标为 3；
4. 点开每一行的 diff；
5. 让 agent 改一个文件，Run 结束后变更列表自动刷新；
6. 添加一个非 Git 目录作为工作区，看到「此工作区不是 Git 仓库」；
7. Ctrl+O、Ctrl+Shift+E、Ctrl+Shift+G 都生效；工作区上移、下移后重启，顺序保持。

- [ ] **Step 3: 截图**

浅色和深色主题、中英文两种语言、最小窗口（960 × 640）、两档界面字号，各截一张文件侧栏加预览 Tab、变更侧栏加 diff Tab 的图，对照 Design Guides 的审查清单逐项检查。

---

### Task 11: 文件图标主题（用户在执行中追加）

用户要求：文件图标可以在设置里选，预置「默认」和 Catppuccin 的四种配色（Latte、Frappé、Macchiato、Mocha）。像界面主题一样分「浅色文件图标」和「深色文件图标」两个选项，跟随界面当前的浅色或深色模式；两个下拉框都列出全部选项（同 Zed）。

**Files:**
- Create: `scripts/vendor-catppuccin-icons.py`：从 Open VSX 的 `Catppuccin.catppuccin-vsc-icons` 解出的扩展目录生成图标包。只读输入，输出一个文件；不在构建时运行。
- Create: `crates/desktop/icons/catppuccin-icons.json`：由上面的脚本生成。内容是 Mocha 版里被映射表引用到的 SVG（`icons`），加上 `file`、`folder`、`folderExpanded`、`fileExtensions`、`fileNames`、`folderNames`、`folderNamesExpanded` 这几张表，以及来源版本号。
- Create: `crates/desktop/src/file/icons.rs`（不依赖 gpui）
- Modify: `crates/desktop/src/state.rs`（`AppearancePreference.light_file_icons` / `dark_file_icons`）、`crates/desktop/src/ui/settings.rs`（外观分页加两个下拉框）、`crates/desktop/src/ui/tools/files.rs`（文件树的图标）、`crates/desktop/src/ui/shell/card.rs`（预览 Tab 的图标）、`locales/app.yml`、`THIRD_PARTY_NOTICES.md`（Catppuccin vscode-icons 的 MIT 声明）
- Test: `crates/desktop/tests/file_icons.rs`、`crates/desktop/tests/state.rs`

**Interfaces:**
- `state.rs`：`#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "snake_case")] pub enum FileIconTheme { #[default] Default, Catppuccin }`；`AppearancePreference.file_icons: FileIconTheme`（`#[builder(default)]`）。未知值和 `SidebarMode` 一样回退到默认值，不能让整个文件解析失败。
- `file_icons.rs`：
  - `pub enum Flavor { Mocha, Latte }`，`impl Flavor { pub fn for_dark(dark: bool) -> Self; pub fn recolor(self, svg: &str) -> String }`。`Latte.recolor` 把 26 个 Mocha 调色板颜色逐一换成 Latte 的对应颜色；`Mocha.recolor` 原样返回。
  - `pub struct FileIconCatalog`（解析后的图标包），`impl FileIconCatalog { pub fn builtin() -> &'static Self; pub fn file(&self, name: &str) -> &str; pub fn folder(&self, name: &str, expanded: bool) -> &str; pub fn svg(&self, icon: &str) -> Option<&str> }`。
  - 文件按小写文件名查 `fileNames`，再从最长的复合扩展名开始查 `fileExtensions`（`index.d.ts` 先查 `d.ts` 再查 `ts`），都没有时用 `file`。文件夹按小写名字查 `folderNames` / `folderNamesExpanded`，否则用 `folder` / `folderExpanded`。
- UI：
  - 「默认」沿用现有的 Lucide 图标。
  - 「Catppuccin」用 `img()` 渲染彩色 SVG（gpui 的 `svg()` 是单色蒙版，画不了彩色图标），配色取所选主题的 flavor；Latte、Frappé、Macchiato 由 Mocha 图标换色得到。
  - `Arc<Image>` 按（图标名，配色）缓存在 `AppView` 上，主题或配色改变时清空。
  - 生效范围：文件树的文件和文件夹、文件预览 Tab。diff Tab 保留 `git-compare`，好和预览 Tab 区分开。

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn files_resolve_by_name_then_longest_extension() {
    let catalog = FileIconCatalog::builtin();
    assert_eq!(catalog.file("Cargo.toml"), "cargo");
    assert_eq!(catalog.file("lib.rs"), "rust");
    assert_eq!(catalog.file("README.md"), catalog.file("readme.md"));
    assert_eq!(catalog.file("no-such.zzz"), "_file");
}

#[test]
fn folders_resolve_open_and_closed() {
    let catalog = FileIconCatalog::builtin();
    assert_eq!(catalog.folder("src", false), "folder_src");
    assert_eq!(catalog.folder("src", true), "folder_src_open");
    assert_eq!(catalog.folder("whatever", false), "_folder");
    assert_eq!(catalog.folder("whatever", true), "_folder_open");
}

#[test]
fn every_mapped_icon_is_bundled() { /* each value of the four tables and the three defaults has an svg */ }

#[test]
fn latte_recolors_the_mocha_palette() {
    let svg = r##"<path stroke="#fab387" fill="#1e1e2e"/>"##;
    assert_eq!(Flavor::Latte.recolor(svg), r##"<path stroke="#fe640b" fill="#eff1f5"/>"##);
    assert_eq!(Flavor::Mocha.recolor(svg), svg);
}
```

`tests/state.rs` 加：`light_file_icons` 和 `dark_file_icons` 默认是 `Default`，`"catppuccin_latte"`、`"catppuccin_macchiato"` 能读回来，未知值回退到 `Default` 而且同一文件里的其他字段照常读出。

- [ ] **Step 2–4:** 运行测试，确认失败；实现；运行 `cargo test -p desktop --test file_icons --test state`，确认通过。UI 部分在验收时截图检查：深色和浅色各截一张 Catppuccin 文件树。

---

## 需要确认的偏离

1. **工作区排序用「上移 / 下移」代替拖拽。** gpui-kit 0.10 的 `SidebarMenuItem` 没有拖放回调。要做拖拽就得放弃 `SidebarMenu`、自己画工作区行，这超出 M3 的范围。
2. **新增第 4 个扩展方法 `_clawcode/workspace/read_file`。** spec 第 8 节只列了 `list_dir`、`git_status`、`git_diff`，但文件预览 Tab 需要文件内容。按 4.6 节的远程约束，desktop 不能自己读工作区文件，所以补上这个方法。
3. **变更列表里每个文件只占一行，diff 一律对比 `HEAD`。** 已暂存和未暂存的改动合在一起显示。因为第一版没有暂存操作，分开显示没有用处。
4. **文件预览 Tab 和 diff Tab 不写进 `desktop.json`。** 和草稿 Tab 一样，重启后不恢复。
5. **冲突文件显示 `U`。** spec 只列了 M / A / D / R / ?，但 porcelain 的 `u` 记录需要一个字母，否则冲突文件会被漏掉。
