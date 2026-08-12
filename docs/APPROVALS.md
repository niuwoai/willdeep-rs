# 审批与自动化

只读工具默认直接执行。会改变世界的操作——写文件、跑 Shell、调 MCP、访问网络——都要经过审批闸门。

## 三档审批模式

与 Swift App 对齐，在 `[agent]` 的 `approval` 中配置：

| 模式 | 语义 |
|---|---|
| `strict` | 创建、编辑、Shell、MCP 都逐次审批 |
| `smart`（默认） | 当前工作区内的创建、编辑免审；另精确放行 `cargo test` 与其只读输出过滤管道；其他 Shell、MCP、网络仍审批 |
| `workspace-write` | 与当前阶段的 `smart` 保持相同安全边界，为后续自动审核器预留独立语义 |

未显式配置时默认采用 `smart`。

`smart` 对 `cargo test` 的放行是精确的：只覆盖 `cargo test` 以及它后面**只由** `grep`、`head`、`tail` 构成的输出过滤管道。`tee`、重定向、`&&`、命令替换和其他 Cargo 子命令都不在此范围内。

## 交互式审批

需要审批时终端显示：

```text
Approval required: edit file: src/main.rs
Allow once? [y/N]
```

三种决定：

- `Y` — Allow once，仅放行当前这一次调用；
- `N` — Disallow，拒绝当前调用；
- `A` — Always allow，**仅在界面明确显示该选项时**可用。

### Always Allow 不是免死金牌

持久放行的粒度被刻意收窄：

- Shell 只记住**规范化后的完整命令**，不是命令前缀，也不是可执行文件名；
- MCP 只记住精确的 `server/tool` 组合。

以下情况一律不提供持久放行：

- 含管道、重定向、命令连接符或换行的 Shell 命令；
- 文件写入；
- 网络重定向；
- 任务取消；
- `editor` 子 Agent 授权。

规则存放于 `$WILLDEEP_HOME/always-allow.json`，Unix 权限为 `0600`。管理命令：

```bash
willdeep --list-approvals
willdeep --clear-approvals
```

## Workspace 策略优先

Runtime 注册表里每个 Workspace 保存独立的访问策略，并在任务入队时**由服务端覆盖客户端输入**——客户端无法自报可写。

`read-only` 策略下，Shell、文件写入、Worktree 创建、MCP 和 `editor` 子 Agent 会在进入审批流程**之前**就被拒绝。

Coding Agent 的默认语义是 `workspace-write`：Workspace 内 `create_file` / `edit_file` 免审，Shell、MCP、网络和越界访问仍走原审批。`read-only` 只在用户显式选择时启用。详见 [Runtime Daemon 与工作区](RUNTIME_DAEMON.md)。

## 网络工具

`web_search` 和 `web_fetch` 在**所有**审批模式下都需要确认。

`web_fetch` 的额外硬约束：

- 拒绝私网、回环和链路本地地址；
- 同 hostname 重定向自动跟随，跨 hostname 重定向重新审批；
- HTTPS 降级到 HTTP 一律拒绝；
- 每次跳转重做公网目标校验；
- 以环路、次数、超时和流式 3 MiB 硬限制约束响应。

## MCP

MCP 调用在所有审批模式下均逐次确认。`smart`、`workspace-write` 和兼容参数 `--full-auto` 只免审当前工作区内的创建、编辑操作，不涉及 MCP。详见 [Skills 与 MCP](SKILLS_AND_MCP.md)。

## 非交互与 CI

在 CI 或已隔离的容器中：

```bash
willdeep --full-auto --json ...
```

非交互输入下，`smart` / `workspace-write` 允许当前工作区内的创建和编辑，`smart` 另允许上述测试管道。**其他 Shell、MCP 和外部操作仍因无法交互审批而拒绝。** Harness 会把拒绝结果作为工具结果返回给模型，不会静默放行，也不会假装成功。

被审批拒绝或被 Workspace 策略拒绝时，`willdeep run` 的退出码为 `4`。

## 后台审批

后台 Harness 需要审批或调用 `ask_user` 时进入 `WaitingApproval` / `WaitingAnswer` 状态，等待任意客户端处理：

```bash
willdeep daemon pending
willdeep daemon resolve <interaction-id> allow-once
willdeep daemon resolve <interaction-id> deny
willdeep daemon resolve <interaction-id> always-allow
willdeep daemon answer <interaction-id> "自由输入答案"
```

处理后原进程内 Future 从等待点继续。TUI 右栏 Inbox、Web Runtime 侧栏同样可以解决这三类审批。

TUI Inbox 中已完成的 Runtime 任务只保留 5 分钟；点击或按 `Enter` 打开等待审批的任务时，直接进入可执行 Allow、Disallow、Always Allow 的审批框。

Runtime 控制 Token 只保留在 Daemon 与 Harness 内存中，不会作为环境变量传给 Shell 或 MCP。

## `ask_user`

模型需要用户做实质选择时可调用 `ask_user`，传入 `question`、可选 `options` 和 `multi_select`。

- TUI 弹层支持方向键选择、空格多选，也可以直接键入未列出的其他答案；
- 普通终端支持输入序号或自由文本；
- Web Runtime 侧栏支持单选、多选和自定义回答。

用户答案经长度限制和标记转义后回到同一工具轮次。

## 相关文档

- [Runtime Daemon 与工作区](RUNTIME_DAEMON.md)
- [子 Agent 与后台任务](SUBAGENTS.md)
- [配置指南](CONFIGURATION.md)
