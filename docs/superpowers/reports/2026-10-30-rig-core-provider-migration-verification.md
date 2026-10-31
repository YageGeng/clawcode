# rig-core provider 迁移验收

> 当前发布版及复审修复后的最新实测：生产 app + WebUI + 真实 DeepSeek 基础 12 项及 MCP Sampling 1 项通过，详见 [真实端到端验收](2026-10-30-rig-core-live-e2e.md)。

日期：2026-10-30（America/Los_Angeles）。依据同日中文 [spec](../specs/2026-10-30-rig-core-provider-migration-design.md) 与 [plan](../plans/2026-10-30-rig-core-provider-migration-implementation.md)。用户已明确授权实施；所有修改留在当前工作区，未创建 commit、worktree 或 SubAgent。

> 版本策略已按用户追加要求调整为 crates.io `>=0.46.0-rc.1, <1`。下文首次迁移验收的 Git revision 是历史测试基线，不再是当前依赖约束；发布版切换结果见文末“追加验收”。

## 交付范围

- `rig-core` / `rig-reqwest` 固定到 `fc556f982d501eea9d7167398a19386e4884e776`，版本 `0.46.0-rc.1`。依赖来自官方 Git 源，没有本地 path、patch 或上游修改。
- 删除 provider 下 52 个 vendor 源文件及旧 API 示例/测试。provider 仅保留 `lib.rs`、`factory/mod.rs` 和 `auth.rs`，负责配置、原生模型构建和认证组装。
- 迁移 protocol、config、extension、Kernel、ACP、示例扩展及 WebUI。共享配置、HTTP 池与认证；每个 SessionModelState 的本地 ProviderModel 持有具体模型和 Prepared 缓存。
- 保留原生 assistant 的 Origin/Native；最终响应先校正展示再进入 MessageEnd hook。实际内容编辑使 Native 失效，相邻同类展示块重新分割不造成误判。
- 移除旧 provider hooks、header 白名单和凭证恢复，使用原生 `HttpMiddleware` 工厂按请求绑定上下文。Kernel 是唯一重试预算所有者。
- Usage 保留上游可选计数和成本，使用版本化 JSONL 信封；兼容读取旧记录，不批量改写文件。ACP、tree/navigate/branch 和 compact 直接回复投影为精度安全的十进制字符串；Native 和压缩恢复用 retainedTail 不进入浏览器。
- DeepSeek 使用上游文本能力，图片走既有省略提示。ChatGPT 使用独立绝对目录的上游 `FileTokenStore`；旧 Codex 配置明确报错，未读取或搬移真实 Codex 凭据。

## 工程验证

以下命令均退出 0。使用 `--offline` 复用已经取得的固定上游和 registry 缓存；需要本地监听/缓存写入的 Cargo 测试在允许相应访问的环境执行。

| 命令 | 结果 | 证据 |
|---|---|---|
| `cargo fmt --all -- --check` | 通过 | 最终终端输出 |
| `cargo check --offline --workspace --all-targets --locked` | 通过 | `/tmp/clawcode-rig-migration/check-final.log` |
| `cargo test --offline --workspace --all-targets --locked --no-fail-fast` | **772 passed、0 failed、2 ignored，107 suites** | `/tmp/clawcode-rig-migration/workspace-final.log` |
| `cargo test --offline --workspace --doc --locked` | 通过，13 个 crate 的 doc-test 目标，共 0 个示例测试 | `/tmp/clawcode-rig-migration/doctests.log` |
| `cargo clippy --offline --workspace --all-targets --locked -- -D warnings` | 通过，无警告；覆盖 `.pre-commit-config.yaml` 的 Rust 检查范围 | `/tmp/clawcode-rig-migration/clippy-final.log` |
| `npm --prefix web run check` | TypeScript、ESLint 通过 | `/tmp/clawcode-rig-migration/web-check-final.log` |
| `npm --prefix web run build` | 生产构建通过 | `/tmp/clawcode-rig-migration/web-build.log` |
| `npm --prefix web run test:elicitation` | 1 个测试文件目标通过，0 failed | `/tmp/clawcode-rig-migration/web-test-final.log` |
| `cargo build --offline -p app --bin clawcode --locked` | 生产程序构建通过 | `/tmp/clawcode-rig-migration/app-build.log` |
| `git diff --check` | 通过 | 最终终端输出 |
| `cargo tree --offline --locked -p app --edges normal` | 只有同 revision 的一份 rig-core / rig-reqwest；重复显示为同一依赖的 `(*)` 引用 | `/tmp/clawcode-rig-migration/dependency-tree.log` |

两个 ignored 是原有 MCP HTTP / stdio 外部互操作测试，未计为通过。本轮没有运行 pre-commit 提交流程，也没有提交；运行了其对应且覆盖更广的 fmt/Clippy 命令。工作区 HEAD 仍为 `e7ec2fec847abd234928039b44a6494f814f43a5`。

## A01–A16

| 项目 | 状态 | 证据与边界 |
|---|---|---|
| A01 依赖/删除/复用 | 通过 | 固定 revision；52 个旧源文件删除；无旧 Llm/hooks 生产引用。`prepared_cache.rs` 验证 A→A→B→A 为 **10→1→10→1**；Kernel unit 验证同会话复用、跨会话隔离、关闭释放与凭证变化替换。 |
| A02 路由/配置 | 通过 | 14 种现有 provider/protocol 组合构造；upstream_id、自定义 URL、递归附加参数、目录外显式模型、DeepSeek text-only。未知 Anthropic 模型使用配置输出上限。 |
| A03 流终态 | 分层通过 | 真实 transport、Kernel attempt、MessageEnd hook 和 ACP 回归覆盖 delta→final 校正、完整工具调用、EOF、空 thought、晚到取消 usage；浏览器真实 decoder/reducer/staging 验证清空。未做“真实 provider + 删除 reasoning 扩展”组合 live 测试。 |
| A04 重试/取消 | 通过，OAuth live 除外 | 实际 HTTP + Kernel 的预算矩阵：provider=2 → 3 次、None/agent=5 → 6 次、0 → 1 次、disabled → 1 次；middleware 次数一致。Retry-After 不裁短，首帧/帧间超时和读取取消通过；既有退避取消回归通过。交互登录等待取消未 live 验证。 |
| A05 原生回放 | 通过 | OpenAI Responses encrypted reasoning 保存/重开/fork/下一请求；Anthropic signed thinking 与 DeepSeek reasoning_content 两轮 wire 回放。真实 DeepSeek 工具历史经 fork/重开后继续成功。OpenAI/Anthropic 为合成协议证据。 |
| A06 扩展改写 | 分层通过 | hook 输入为校正后的 Hello；改为 Hi、删除 reasoning 后保存/下一请求不恢复旧 Native。相邻块边界重分割保留 Native。ACP 空 thought 与浏览器清空契约通过；上述删除扩展没有在生产 live 场景启用。 |
| A07 工具边界 | 通过 | 接受空 JSON 的计数工具仍不会在无效参数或结构化失败时执行；实际执行计数为 0。CallId/name/is_error 关联、空结果、图片/MCP 和输出预算回归通过。 |
| A08 middleware | 原生/Kernel 测试通过 | 多值及非 UTF-8 header、Authorization 替换、原始非 JSON body、Exchange、顺序、multipart、失败优先级；完整 Kernel 重试次数验证。真实 DeepSeek 场景未启用自定义 middleware 扩展。 |
| A09 并发/缓存 | 通过 | 两会话上下文与 headers/body 隔离、注册事务回滚；本会话模型切换保留句柄，关闭释放。实际 Prepared 工作量测试和 SessionModelState 身份测试分别覆盖机制与集成。 |
| A10 usage/精度 | 通过 | 新旧 usage、37/10/20/3/40、None/Some(0)、上游累计与 context_tokens；真实 DeepSeek 缓存读取可见，缓存写入未报告保持未知；真实浏览器精确显示 2^53+1。 |
| A11 持久化 | 通过 | 混合旧/新 JSONL，原文件字节不变，未知版本错误包含 entry ID；真实重连 usage 一致、fork/重开继续成功。 |
| A12 OAuth | **部分验证** | 配置校验、旧 codex/auth_file 拒绝、上游 FileTokenStore 合成非过期凭证读取、无网络/不改 sentinel 文件通过。设备码 callback 使用 stderr 经源码核对。真实登录、refresh、同目录跨进程锁与登录等待取消未 live 验证。 |
| A13 日志 | 通过限定范围 | Provider 生命周期日志回归排除 prompt 内容；错误只投影结构化类别/HTTP 状态；配置和 Native 不进入普通日志。未对真实 OAuth 失败路径做运行验证。 |
| A14 工程检查 | 通过 | 上表全部退出 0，ignored 单列。 |
| A15 真实生产 | **12 项通过** | 下节生产 app + production WebUI + 当前 DeepSeek；不是 fixture backend。 |
| A16 组织 | 通过 | 下节职责/生命周期自审；遵守禁止 SubAgent。 |

## 真实生产验收

使用当前仓库配置副本，provider/model/base URL/模型参数保持不变：`deepseek/deepseek-flash`（DeepSeek V4 Flash）。配置副本权限 0600，位置 `/tmp/clawcode-rig-core-e2e/claw.toml`。数据目录和工作目录均隔离于 `/tmp/clawcode-rig-core-e2e`；为让短历史可手动压缩，仅在隔离副本把 `compaction.keep_recent_tokens` 调为 128。原 `claw.toml` 未改动。

生产启动与验收脚本：

```sh
CLAW_CONFIG=/tmp/clawcode-rig-core-e2e/claw.toml target/debug/clawcode serve --bind 127.0.0.1:3001 --web-root web/dist
node crates/app/tests/web/provider_migration.e2e.mjs http://127.0.0.1:3001 /tmp/clawcode-rig-core-e2e/work /tmp/clawcode-rig-core-e2e/evidence
```

独立 headless Chrome profile，未使用个人浏览器资料。脚本通过真实 Composer、实际 ACP 通知及页面状态验收，没有模拟供应商回答。最终脚本退出 0，浏览器未捕获异常。

| 场景 | 实测 |
|---|---|
| production bootstrap | 活动模型为 `deepseek/deepseek-flash` |
| 两次工具调用和流式内容 | 两个真实 `printf` 结果完成；捕获 **18 个可见中间状态样本**；最终回答到达 |
| usage 重连 | 重载前后展示完全相同，没有重复累计 |
| 取消 | **108ms** 返回非运行态 |
| 取消后再次请求 | 新的真实请求成功 |
| follow-up 与 steer | 忙时入队、补充、顺序消费和终态成功 |
| DeepSeek 图片回退 | 实际图片输入后模型收到省略提示并返回 `RIG_IMAGE_FALLBACK`，不声称视觉识别成功 |
| 真实压缩 | 调用真实模型生成摘要；usage input=876、output=958、total=1834、cache read=0，未报告 cache write |
| 压缩后继续 | 真实请求成功 |
| 会话树 | 原生消息和 retainedTail 不出现在返回的浏览器数据中 |
| fork 后继续 | 新会话真实请求成功 |
| close/resume 后继续 | 读取持久历史并再次请求成功 |

证据：

- `/tmp/clawcode-rig-core-e2e/evidence/results.json`：12 项，errors 为空。
- `/tmp/clawcode-rig-core-e2e/evidence/updates.json`、`stream-samples.json`：初始真实工具回合 ACP 更新与页面中间状态。
- `/tmp/clawcode-rig-core-e2e/evidence/tool-loop.png`、`final.png`：实际生产页面。
- `/tmp/clawcode-rig-core-e2e/evidence/tree.json`：压缩后完整 tree 投影。
- `/tmp/clawcode-rig-core-e2e/evidence/browser.log`：最终脚本输出。

首次运行的短历史没有达到默认保留窗口，手动压缩按既有规则返回无可压缩上下文；之后仅调整隔离副本。另一次脚本点到了队列中同名的“本轮补充”按钮，已改为选择 Composer 自己的控件。真正发现的实现缺陷是压缩 retainedTail 内嵌 Native 未被首次 tree 投影覆盖；修复并加入回归后最终全场景成功。

测试服务和本轮启动的 Chrome 均已停止。只保留临时验收证据与隔离数据供复查。

## 浏览器显示契约

另外通过现有 Vite 构建真实 `SessionUpdateDecoder`、`reduceSessionWorkspace`、`SessionWorkspaceStager` 和 `AgentUsage`，在独立 Chrome 加载合成输入；这部分明确属于组件契约检查，不是实际供应商账单。

- thought chunk → final AgentMessage（省略 thought 保留）→ 空 AgentThought；reducer 和 staging 均清空 reasoning。
- metadata `9007199254740993` 覆盖标准字段已经舍入的数字；组件显示 `9,007,199,254,740,993`。
- Some(0) 显示 0；未知显示“未知”；未知 context 更新清除旧已知数；BigInt 累加保持精确。

构建/源码和产物位于 `/tmp/clawcode-rig-core-e2e/contract/`；浏览器结果为 `/tmp/clawcode-rig-core-e2e/evidence/display-contract.html`，截图为 `display-contract.png`。

## A16 自审

- `ConfiguredModel` 只拥有不可变 profile、配置、HTTP、Authenticator 和 timeout。`ProviderModel` 的短 Mutex 临界区只检查当前 auth 与本会话模型；认证 await 位于锁外。请求替换 transport 不写回缓存。
- `ProviderRequest` / `ProviderMessage` 负责请求转换与本段历史的 CallId/name 关联；使用 TryFrom，未复制供应商 codec、Prepared 或 Agent 工具运行时。
- `AssistantProjection<'a>` 借用原生消息，供最终校正和编辑检测共用；没有独立状态机或全局缓存。对等价文本块按前缀比较，避免复制调用方整段历史。
- `ProviderMiddleware` 持有冻结 registry 与本次上下文，原生 factory 按请求实例化，不缓存 body/header 结果。
- `RetryState<'a>` 借用有效策略并持有唯一预算；`UsageOf<'a>`、`SessionTreeOf<'a>` 为短期借用投影，转换行为归对应类型的方法。树投影只处理已知存储类型，不递归改写任意用户 JSON。
- 大型 Native 和供应商枚举按需装箱，完整 Run Future 在共同入口装箱，避免所有调用方承担大栈帧；未通过调大 Clippy 阈值或忽略 lint 掩盖问题。
- 测试仅位于 crate tests/ 或精确的 `#[cfg(test)] mod tests`；没有生产测试专用字段、测试后端或新的浏览器测试框架。

## 未验证项与升级边界

真实 OpenAI Responses / Anthropic 服务调用及 ChatGPT 设备码登录、刷新、同目录跨进程争用未验证；没有借用个人 Codex 凭据补齐这些项。原生协议回放测试不能替代对应 live 证据。

真实模型产生 reasoning 后由 MessageEnd 扩展删除、以及给真实供应商请求附着自定义 middleware 的组合 live 场景未跑；各层行为已有 Kernel/原生 HTTP/ACP/浏览器契约测试。当前生产构建只启用原有 command-guard，没有为验收向生产源文件加入测试专用扩展。

升级后旧应用未必能读取新保存的 usage 信封；旧扩展 provider hooks 与旧 Codex auth 配置需要按 README 迁移。DeepSeek 图片能力回退是用户明确接受的行为。真实返回没有 cache-write 计数时继续显示未知。

## 追加验收：取消 Git revision 固定

用户要求使用最新大版本并持续接收上游更新后，已把根 `rig-core`、`rig-reqwest` 改为 crates.io 范围 **`>=0.46.0-rc.1, <1`**。两者当前均解析为 `0.46.0-rc.1`；Cargo.lock 中全部 Rig 包来自 registry，记录 checksum，没有 Git 源。README、spec、plan 与第三方说明已同步。

查询 crates.io 官方元数据时，最新发布为 `0.46.0-rc.1`、最新稳定版为 `0.45.0`。发布包的 `.cargo_vcs_info.json` 对应 `6eff50a2fd95de3a505000a0e39d2893d83c6f4d`，与首次实现使用的 Git 代码不同，因此重新验证了发布版，未直接沿用原先测试结论。

本次验证结果：

| 检查 | 结果 | 证据 |
|---|---|---|
| `cargo update -p rig-core -p rig-reqwest` | 成功；4 个 Rig 包从 Git 换为 registry | `/tmp/clawcode-rig-migration/registry-update.log` |
| `cargo check --workspace --all-targets --locked` | 通过 | `/tmp/clawcode-rig-migration/registry-check.log` |
| `cargo test --offline --workspace --all-targets --locked --no-fail-fast` | **772 passed、0 failed、2 ignored，107 suites** | `/tmp/clawcode-rig-migration/registry-tests.log` |
| `cargo clippy --offline --workspace --all-targets --locked -- -D warnings` | 通过，无警告 | `/tmp/clawcode-rig-migration/registry-clippy.log` |
| `cargo tree --offline --locked -p app --edges normal` | 单一 registry rig-core / rig-reqwest 版本 | `/tmp/clawcode-rig-migration/registry-tree.log` |
| `cargo fmt --all -- --check`、`git diff --check` | 通过 | 终端输出 |

后续执行 `cargo update -p rig-core -p rig-reqwest` 即可选取范围内的新发布，再运行回归检查。Cargo.lock 保留已验证快照，普通构建不会自动联网升级；这个范围允许后续 0.x 正式版本和 0.46.0 后续预发布，不允许其他发布线的预发布或自动进入 1.x。0.x minor 可能包含 API 破坏性变化，依赖可更新不代表无需适配。依据：[Cargo 版本范围](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html)、[cargo update](https://doc.rust-lang.org/cargo/commands/cargo-update.html)。

此次依赖来源调整没有修改业务实现、增加自动更新任务或创建 commit；没有重跑真实供应商/WebUI 和 OAuth，前述 12 项 live 证据仍只对应首次 Git 基线。


## 复审修复验收

复审提出的五项问题以及两处既有 typed-builder 不符合项已修复，未创建 commit。依赖仍采用 crates.io `>=0.46.0-rc.1, <1`，本轮没有调整版本范围或锁文件。

| 问题 | 修复及证据 |
|---|---|
| MessageEnd 后执行旧调用 | 删除 AssistantAttempt 中重复的工具列表；hook 后从最终消息确定调用与终态。参数从 original 改为 edited 后实际执行 edited；删除调用后执行次数为 0；Error/Refusal/Cancelled 不执行工具；原始失败不能被 hook 复活。`provider_conformance.rs` 的三个新增测试先失败后通过。 |
| MCP 清掉 invalid_arguments | 增量转换保留标记，最终转换复用 AssistantProjection；`sampling_preserves_invalid_tool_arguments` 覆盖两条路径，先失败后通过。 |
| MCP 忽略最终内容 | 最终原生消息替换旧 delta，过滤 reasoning 后返回；`sampling_uses_final_native_content` 覆盖最终修正及没有 delta 的输出，先失败后通过。 |
| 失败终态丢计数 | ModelFailure 用可选装箱字段保留原生 Usage；同一 Anthropic refusal 的上游和项目均为 input=10/output=2/total=12，失败状态、Run 汇总和持久化断言通过。未报告的计数仍为未知。 |
| 原生比较复制完整内容 | 统一可见项筛选，借用比较连续切片、JSON 参数和图片，避免构造临时 owned blocks。语义矩阵通过。独立分配计量的相同 4MiB 文本由 **4,194,944 字节**额外分配降至 **0 字节**。这是比较函数的计量，不代表真实供应商端到端加速比例。 |
| AGENTS.md 第 5 条 | AssistantAttempt 与 ContextUsageEstimate 改为 typed-builder；Option 字段声明默认值。相关生成、压缩和取消测试通过。 |

验证结果：

- `cargo test --offline --workspace --all-targets --locked --no-fail-fast`：**779 passed、0 failed、2 ignored，107 suites**。日志 `/tmp/rig-migration-fixes/workspace.log`；两个 ignored 仍为原有外部 MCP 互操作测试。
- `cargo clippy --offline --workspace --all-targets --locked -- -D warnings`：通过、无警告。日志 `/tmp/rig-migration-fixes/clippy.log`。
- `cargo fmt --all -- --check`、`git diff --check`：通过。
- 最后仅修改测试辅助方法的 unreachable 表达，随后 `cargo test --offline -p kernel --test mcp_host --locked`：11 passed。日志 `/tmp/rig-migration-fixes/mcp-final.log`。
- RED/Green 证据：`/tmp/rig-migration-fixes/{settlement-red,settlement-green,sampling-red,sampling-usage-green,kernel-green}.log`。
- 分配探针：`/tmp/rig-migration-review/tests/projection_allocation.rs`，通过引用真实 source 测量；`/tmp/rig-migration-fixes/allocation-red.log` 与 `allocation-green.log`。仓库没有加入生产测试专用字段、全局分配器或新测试依赖。

本轮没有重跑真实供应商、WebUI、OAuth 登录/刷新；此前 live 证据保留其原始版本与范围，不当作此次修复的新证据。这里也没有把供应商错误正文中可能出现的 token 字段自行解析为计费，只有上游已解码的 Usage 才进入新失败字段。
