# Pi 编码工具设计

## 目标

在保留现有 `provider`、`config` 和模块化 Kernel 架构的前提下，实现与当前 pi 编码工具一致的 `read`、`write`、`edit`、`bash`。工具运行在 Agent 服务端，不调用 ACP 客户端文件系统或终端能力。

## 行为边界

- `read`：接受 `path`、可选的 1-based `offset` 和 `limit`；文本按 2000 行或 50KB 先到者做头部截断，并返回可继续读取的 offset；按文件魔数识别 JPEG、PNG、GIF、WebP、BMP，返回文本说明和图片块。
- `write`：创建父目录后创建或完整覆盖文件；同一真实文件上的写操作串行执行。
- `edit`：公开 `path + edits[]` Schema，同时兼容 pi 接受的顶层 `oldText/newText` 和字符串化 `edits`；所有替换基于同一份原始内容，要求唯一且互不重叠；保留 UTF-8 BOM、原始 CRLF/LF，并返回展示 diff、标准 patch 和首个变更行。
- `bash`：在 Session cwd 中使用 bash 执行命令；stdout/stderr 按到达顺序合并；支持秒级 timeout 和 Turn cancellation；取消或超时时终止进程组；输出按 2000 行或 50KB 做尾部截断，截断时把完整输出保存到临时文件；最多每 100ms 发布一次局部更新。
- 路径解析与 pi 一致：支持相对路径、绝对路径、`~`、`@` 前缀及 Unicode 空格规范化，不额外增加工作区沙箱限制。
- 只注册 pi 已有的编码工具，不保留 `echo`、`date` 等过渡工具；`BuiltinToolFactory` 根据 config 的 filesystem/shell 开关注册工具。

## 类型与模块

- `protocol` 定义图片内容、结构化工具详情、截断详情和工具执行更新事件；时间戳、TurnId 继续由事件元数据强制携带。
- `tools` 以 `AgentTool`、`ToolFactory` 为公共接口。`ToolExecutionContext` 持有 cwd、取消令牌和局部更新出口。
- `tools::builtin` 按职责拆分为 factory、路径、截断、文件变更队列和四个编码工具。单参数且参数为项目类型的逻辑优先实现为关联方法或 trait，不堆积短 helper。
- `kernel` 为每次调用注入同一个 Turn cancellation，并把局部更新转换为有序 AgentEvent。
- `acp` 将文本、图片和 edit diff 映射为 ACP v2 原生 ToolCallContent；没有原生形状的工具详情保存在 raw output 和命名空间 `_meta` 中。

## 错误与并发

- 参数解析失败返回 `ToolError::InvalidArguments`，执行失败转换为 `is_error=true` 的 ToolResult，确保错误仍进入 transcript。
- 同一个模型 Turn 的兄弟工具继续并行；只有解析到同一真实文件的 `write/edit` 互斥，不同文件不互相阻塞。
- cancellation 传到 read/write/edit/bash。文件写操作在底层写入结束前不释放文件队列，防止取消后晚到写入覆盖后续操作。
- bash 完成后忽略迟到输出；临时文件在最终 ToolResult 发布前 flush/close。

## 验证

- Rust 测试覆盖 Schema、路径、截断、读取分页与图片、写入建目录、并发写入、精确/模糊编辑、BOM/CRLF、bash 成功/失败/超时/取消/截断/局部更新。
- 运行 workspace fmt、clippy、tests 和 Web check/build。
- 使用真实 fixture WebUI 驱动模型依次调用 read/write/edit/bash，检查 ACP 更新、最终 transcript、错误和取消，并确认刷新后持久化结果可回放。
