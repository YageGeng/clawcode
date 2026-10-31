# rig-core 迁移诊断与 usage 复审修复验收

日期：2026-10-31。

## 范围与结果

修复本轮 review 的第 1、2、3、5、6、7、8 项。第 4 项保持既定语义：上游明确报告的 Some(0) 是有效压缩锚点，不等同于 None。本轮没有创建 commit，也没有修改此前的 session 轮询修复。

| 项目 | 结果 |
| --- | --- |
| 1：错误诊断 | ProviderConnectError 的 Display 按类型输出安全分类；ProviderModel 在认证、构造、请求准备、流读取、最终校验和失败终态记录带模型身份的 WARN。HTTP 错误保留 kind/status。ModelFailure 按上游失败判定生成拒答、限制、异常终止等安全摘要，保留失败 usage 和工具未执行提示。 |
| 2：可选 usage | MessageView、ContextLens、AgentUsage 共用 ModelUsages.formatTokens，未知显示“未知”，零显示“0”，整数格式化不经 Number。 |
| 3：Context 口径 | ACP 和 WebUI 与上游 context_tokens 一致：total 优先，否则 input + output（缺 output 时按 0，u64 饱和）；缺 input 且缺 total 时仍未知。推导值不写回 reported total，也不补造标准 ACP Usage。 |
| 5：CLI | 英文统计使用 unknown，恢复无尾随换行。 |
| 6：mapper | 实时投影、命令快照、历史重放各复用同一个借用 mapper；无需重复构造 metadata。发布前释放大型原生事件。保留静态 map 便捷入口，避免无关测试/API 改动。 |
| 7：LegacyUsage | 注释解释 serde-only 构造、builder derive 的规则来源，以及旧 input 字段只验证而不直接采信的原因。 |
| 8：未来 FinishReason | 兜底改为 Debug 表示，保留在既有 raw_stop_reason 字段，不输出到普通日志。 |

认证原始错误、HTTP body、供应商拒答 explanation 都可能带任意敏感文本，因此没有直接输出 AuthError 或 turn_failure 的字符串。原始认证 source 仍可供类型检查，常规日志只使用安全 Display。代码归属沿用 ProviderModel、ModelFailure、ProviderConnectError 和 AcpEventMapper，没有新建工具型 struct 或依赖。

## 验证

先验证新增用例在修复前失败：认证摘要包含合成敏感文本；Kernel 认证错误缺分类和模型身份；401 日志缺状态码；拒答只剩通用摘要；缺 total 的 Context 发出未知扩展更新。实现修复后这些用例通过。

新增或加强的回归：

- provider/upstream_auth：认证 Message、I/O、JSON、HTTP 401、Store 类型的安全诊断与 source 保留。
- kernel/provider_runtime：本地 HTTP 401 日志保留状态码及模型身份；损坏的临时 ChatGPT token record 本地失败，摘要与日志保留 JSON 类别；合成正文、prompt、凭据标记不进入日志。
- kernel/provider_history：Anthropic refusal 的原始 explanation 标记不进入摘要，content_filter 分类及 input=10/output=2/total=12 保留。
- protocol/provider_data：成功、部分 length 答案、无答案 length、拒答、显式错误、未知 finish reason、失败工具调用和 aborted 工具调用；检查失败分类、工具未执行说明、非重试语义和 usage 保留。
- acp/provider_projection：input/output 回退、input-only、u64 饱和、total=0 优先、input=0；metadata 不补造 total。既有未知 usage 与标准 Usage 必填字段测试保持通过。

| 检查 | 结果 |
| --- | --- |
| `cargo test --workspace --tests` | 102 个测试套件，787 通过，0 失败，2 忽略 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo fmt --all -- --check` | 通过 |
| `git diff --check` | 通过 |
| `npm run check`（web） | TypeScript 与 ESLint 通过 |
| `npm run build`（web） | 通过 |
| 实际 React 组件 SSR / decoder 契约检查 | 23 项通过：三个组件的未知/零/大整数，Context 回退与饱和，input 缺失，非法字段拒绝；没有改写生产代码的测试入口 |

本机过程证据位于 `/tmp/rig-review-round2-*.log`；组件检查脚本为 `/tmp/rig-review-round2-ui.cjs`，仅作为本次临时验证工具，不属于产品运行依赖。

## 验证边界

本轮没有再次运行真实供应商 WebUI E2E 或真实 OAuth 登录/刷新；本地 HTTP 集成测试、临时凭据和组件 SSR 不冒充这些验收。未对未来尚不存在的 FinishReason 变体做运行验证。mapper 性能改进由消除重复构造确认，未声明新的延迟或分配量基准数字。已报告零值的压缩策略保持不变。
