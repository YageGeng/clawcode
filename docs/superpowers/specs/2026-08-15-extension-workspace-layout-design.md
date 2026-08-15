# 实验扩展目录迁移设计

## 背景

当前 `crates/extensions` 保存静态编译进程序的实验扩展实现，而 `crates/extension` 提供稳定的扩展协议、注册、运行时和宿主能力。两者处于同一 `crates` 目录会弱化“核心框架”和“实验实现”的边界。

本次仅调整实验扩展 crate 的目录位置，不改变扩展协议、编译选择方式、运行时行为或 crate 名称。

## 目录设计

迁移后的目录结构如下：

```text
clawcode/
├── crates/
│   └── extension/       # 稳定的扩展框架
└── extensions/          # 实验扩展实现 crate
    ├── build/
    ├── examples/
    ├── src/available/
    ├── tests/
    ├── build.rs
    └── Cargo.toml
```

`extensions` 继续作为一个 workspace member，package 名称仍为 `extensions`。现阶段不将每个扩展拆成独立 crate，避免为少量实验实现引入额外依赖图、构建目录和目录发现机制。

## Cargo 依赖关系

根 `Cargo.toml` 中：

- workspace member 从 `crates/extensions` 改为 `extensions`；
- workspace dependency `extensions` 的相对路径同步改为 `extensions`；
- 其他 crate 继续使用 `extensions = { workspace = true }`，不感知物理目录变化。

实验扩展仍依赖核心 `extension` 和 `protocol` crate。核心 crate 不反向依赖实验实现，依赖方向保持单向。

## 构建配置解析

现有 `build.rs` 根据 `CARGO_MANIFEST_DIR` 向上两级寻找 workspace 根目录，这一假设只适用于 `crates/extensions`。

迁移后，默认配置路径从新的 manifest 目录向上一级解析到 workspace 根目录，再拼接公共的配置文件名。显式配置路径仍优先使用 `ProductIdentity::CONFIG_PATH_ENV`，因此固定使用 `claw.toml` 的当前行为和未来可配置能力均保持不变。

如果无法解析父目录，构建脚本返回带目录上下文的错误，不回退到当前工作目录，避免构建结果依赖调用位置。

## 行为与兼容范围

本次迁移不改变以下行为：

- `claw.toml` 中 `[extensions].enabled` 的含义和顺序；
- 编译期扩展目录扫描与生成代码；
- `extensions::compiled_extensions()` 公共入口；
- 扩展 ID、Hook 注册顺序和运行时选择；
- 扩展示例与测试覆盖范围。

不保留旧的 `crates/extensions` 路径兼容层。仓库内部路径引用一次性迁移到新目录。

## 验证

迁移完成后执行：

1. `cargo fmt --all --check`，验证格式；
2. `cargo check --workspace --all-targets`，验证所有 workspace 路径和生成代码；
3. `cargo test --workspace`，验证扩展选择、构建配置与运行时测试；
4. `cargo clippy --workspace --all-targets --all-features -- -D warnings`，验证 Rust 质量门禁；
5. 使用真实配置启动正式后端，确认 `command-guard` 仍按配置编译并启用。

此次目录迁移不修改 WebUI，不需要新增前端测试。
