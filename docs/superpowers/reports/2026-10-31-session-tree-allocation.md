# 会话树投影重复分配修复验证

日期：2026-10-31。

## 修改范围

- `crates/acp/src/mapping.rs`：SessionTreeOf 拥有并消费 Kernel 返回的独立快照；在序列化前移除 assistant.model_message、compaction.retainedTail。usage 通过取出槽位转换，避免额外克隆；未知和已报告零的语义不变。
- `crates/acp/src/extension.rs`：Tree、Navigate、Branch 将已拥有的快照直接移交投影，不再借用后完整复制。Fork 返回会话 ID，不需要修改。
- 加强既有投影回归，检查大整数、已报告零、缺失计数、保留用户 JSON 中的同名字段，以及源快照保持不变。未引入依赖、生产测试接口、unsafe 代码或额外上下文类型。

## 分配量

沿用复审的 System 全局分配器探针，原样提取生产 SessionTreeOf 和 UsageOf.metadata；确认修复后的方法与当前源码一致。先构造两条记录：assistant 包含隐藏原生内容，compaction 的 retainedTail 包含同样大小的隐藏内容。计数在快照构造完成后开始，覆盖投影到 serde_json::Value；不包含探针打印、输出比较或输入构造。

统计的是累计申请字节，不是峰值内存或存活字节。隐藏内容一栏为两条记录中的合计大小。

| 隐藏内容 | 修复前分配（字节） | 修复后分配（字节） | 最终 JSON（字节） |
| --- | ---: | ---: | ---: |
| 0 | 4,755 | 3,153 | 326 |
| 8 MiB | 8,393,363 | 3,153 | 326 |
| 32 MiB | 33,559,187 | 3,153 | 326 |

三组输出修复前后逐字节相同。限制隐藏内容增加后的额外分配不超过 64 KiB 的断言，在旧实现上失败、在新实现上通过；新实现的测量值不随隐藏内容增长。

本机复核命令：

```sh
cargo run --offline --quiet --manifest-path /tmp/rig-migration-review/Cargo.toml --bin review_tree_allocation_before
cargo run --offline --quiet --manifest-path /tmp/rig-migration-review/Cargo.toml --bin review_tree_allocation
```

临时证据保存在 `/tmp/clawcode-tree-allocation/`：`red.log` 为修复前失败断言，`before.log`/`after.log` 为可比计数，`before-*.json`/`after-*.json` 为对应输出。探针只用于本次测量，不是产品运行依赖。

## 检查与边界

- `cargo test -p acp`：165 通过，0 失败，0 忽略；包括映射、扩展入口、导航、重放与传输相关回归。
- `cargo clippy -p acp --all-targets -- -D warnings`：通过。
- `cargo fmt --all -- --check`、`git diff --check`：通过。
- 分配结果只证明投影阶段的重复复制已移除；Kernel/Store 构建快照本身的既有分配不在本次改动和测量范围。
- 未重跑真实供应商 WebUI E2E、OAuth 或端到端延迟基准。未创建 commit。
