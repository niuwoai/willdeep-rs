# Web / CLI 会话展示回归

> 2026-09-08 | v0.72.0-rc59 | 未发布、未安装

## 自动化验证

| 命令 | 结果 |
| --- | --- |
| `cargo test -p willdeep-core --lib conversation:: --offline` | 8 项通过 |
| `cargo test -p willdeep-core --lib session:: --offline` | 27 项通过 |
| `cargo test -p willdeep --bin willdeep tui:: --offline` | 161 项通过 |
| `cargo test -p willdeep --bin willdeep web::tests --offline` | 29 项通过 |
| `cargo test -p willdeep-core --lib provider:: --offline` | 50 项通过 |
| `yarn --cwd web lint` | 通过 |
| `yarn --cwd web build` | 通过 |

本地二进制构建通过，执行 `target/debug/willdeep --version` 返回 `willdeep 0.72.0-rc59`。

## 渲染验证

- 使用最终构建产物和本地合成 API 数据，未使用真实用户会话或真实模型调用。
- 真实浏览器中显示系统活动卡、单张六步骤计划卡（五项完成、一项跳过）。
- 宿主指令与计划原文可展开，主动粘贴相同英文的用户消息仍保留用户气泡。
- 刷新后恢复同一份计划状态；TUI TestBackend 验证 24 列中文、状态符号和原文折叠。

## 验证边界

- 旧消息缺少来源元数据时不依靠提示词文字猜测来源。
- 只投影完整 plan/progress 围栏；不完整、未知状态、无法匹配的更新保留为正文。
- 计划展示不改变执行调度、目标完成判断或验收权限。
- 最初 core 全套运行：451 通过、16 项因回环监听权限失败、4 项忽略；上述 Provider 重跑全部通过。
