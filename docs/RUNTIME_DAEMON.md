# Runtime Daemon 与工作区

Runtime Daemon 是 WillDeep 的常驻本地控制面。任务在它进程内执行，因此关掉 CLI、TUI 或浏览器不会终止正在跑的活儿。

## 基本操作

Daemon 管理命令不要求先配置 Provider，可以独立启动和检查：

```bash
willdeep daemon start
willdeep daemon status
willdeep daemon capabilities
willdeep daemon logs --lines 100
willdeep daemon stop
```

`daemon stop` 表示**主动取消活跃任务后停止**。想保住任务请用下面的 `upgrade`。

## 安全边界

本地控制端点只监听随机 `127.0.0.1` 端口，认证 Token 保存在 `$WILLDEEP_HOME/runtime/daemon.json`。本机客户端优先走权限为 `0600` 的 Unix Socket，或拒绝远程客户端的 Windows Named Pipe，兼容旧状态时才回退到受 Token 保护的回环 TCP。

Unix 下状态文件与日志权限为 `0600`。**不要复制或提交这个运行时目录。**

Runtime 控制 Token 只保留在 Daemon 与 Harness 内存中，不会作为环境变量传给 Shell 或 MCP。详见 [认证与凭据](AUTHENTICATION.md)。

## 单实例与升级

Daemon 通过短周期心跳续租单实例锁。异常退出后，一次 `daemon start` 会等待旧租约过期并安全接管。

安装新二进制后运行：

```bash
willdeep daemon upgrade
willdeep daemon upgrade --timeout 600
```

旧进程进入 `draining` 状态：拒绝新的 Turn / Spawn / Retry / 补充 Prompt，但已运行或等待人工处理的任务继续。活跃任务归零后释放租约，当前二进制接管，排队 Turn 从持久队列继续。

默认等待 300 秒。同版本之间的测试交接需要显式 `--force`。

> 安全 Drain 协议从 rc32 开始。从 rc31 或更早版本首次迁移时，命令会保持旧任务不动并明确要求等待任务完成后手动 stop/start；之后的版本可以直接无损 Upgrade。

## 工作区

### 注册与管理

任务或 Session 首次使用某目录时会自动注册 Workspace。也可以显式管理：

```bash
willdeep daemon register-workspace . \
  --name "项目" \
  --access workspace-write \
  --provider-profile some-im \
  --skill reader \
  --mcp-server docs

willdeep daemon workspaces
willdeep daemon activate-workspace <workspace-id>
willdeep daemon remove-workspace <workspace-id> --yes
```

每个 Workspace 保存独立的访问策略、默认 Provider、Skill 与 MCP 允许列表。

- `activate-workspace` 只改变**新客户端**使用的默认项，已启动任务继续绑定原规范化根目录；
- `remove-workspace --yes` 只移除注册信息，**不删除文件、Session 或历史**。

### 访问策略

| 策略 | 语义 |
|---|---|
| `workspace-write` | Coding Agent 的默认语义：Workspace 内 `create_file` / `edit_file` 免审；Shell、MCP、网络和越界访问仍走原审批 |
| `smart` | 同上安全边界，另精确放行 `cargo test` 及其只读输出过滤管道 |
| `read-only` | 仅在用户显式选择时启用 |

Workspace 策略由 Runtime 在任务入队时**覆盖客户端输入**，客户端无法自报可写。`read-only` 下 Shell、文件写入、Worktree 创建、MCP 和 `editor` 子 Agent 会在审批前被直接拒绝。

非空的 Skill / MCP 列表作为允许列表生效；空列表保持全局配置的兼容行为。

### 切换工作区

TUI 中：

```text
/workspace list
/workspace switch <WORKSPACE_ID>
```

切换会保存当前 Session 的 Inbox 与事件游标，恢复目标 Workspace 的最近 Session（没有则创建），重启该 Workspace 的事件跟随并清空旧的右栏瞬态。Daemon 中旧 Workspace 的任务不受影响，继续运行。

> 跨 Workspace 切换后 `/local` 会保守禁用，因为进程内 Local Harness 的工具边界在启动时固定。从目标目录重新启动 TUI 后才能再次使用。

Web 端的工作区选择器读取 Runtime 注册表，同时与启动白名单取交集，详见 [Web 端指南](WEB_GUIDE.md)。

## 任务

```bash
willdeep daemon submit --workspace . --profile some-im "检查项目并运行测试"
willdeep daemon tasks
willdeep daemon task <task-id>
willdeep daemon cancel <task-id>
```

`submit` 会在必要时自动启动 Runtime 并立即返回任务 ID。Prompt 保留在 Runtime 内存和受保护的 Session/Turn 存储中，**不会出现在进程参数里**。

Daemon 直接调度进程内 Harness Future，不再为每个 Turn 启动 CLI 子进程。任务执行、模型输出、最终 session_id 和终态都可以在事件流中恢复。

## Session 与 Turn

```bash
willdeep daemon create-session --workspace . --profile some-im --title "长期任务"
willdeep daemon sessions
willdeep daemon session <session-id>
willdeep daemon submit-turn <session-id> --request-id <uuid> "继续处理下一步"
willdeep daemon turns <session-id>
willdeep daemon turn <turn-id>
willdeep daemon stop-turn <turn-id>
```

`create-session` 创建长期 Runtime Session，同时建立同 ID 的 Core Session 和生命周期稳定的 Root Agent。

`submit-turn` 使用客户端 `request_id` 幂等入队。**同一 Session 的 Turn 严格串行，不同 Session 可以并发。** 成功 Turn 写入 Core Session 后会清除队列中的私密 Prompt / 附件副本。

Daemon 重启后排队项继续调度；遗留的 Running / Waiting 活动项明确标记为 `Interrupted`，并向事件日志补写可续传的中断事件。对"无工具活动且历史边界完全匹配"的活跃 Turn 会自动重放；存在副作用证据或歧义历史时完整保留并停止自动恢复。

### 会话管理

```bash
willdeep daemon search-sessions "关键词" --workspace . --status idle --profile some-im
willdeep daemon rename-session <session-id> "新名称"
willdeep daemon fork-session <session-id> --title "分叉名称"
willdeep daemon fork-session <session-id> --through-turn <TURN_ID> --provider-profile <P> --model <M>
willdeep daemon archive-session <session-id>
willdeep daemon unarchive-session <session-id>
willdeep daemon export-session <session-id> --output session.json
willdeep daemon delete-session <session-id> --yes
```

Rename、Fork、Archive 和 Delete **只允许在没有活跃或排队 Turn 时执行**，避免覆盖 Harness 正在写入的历史。

Fork 复制 Core 消息快照并创建新的 Session / Root Agent，不复制旧的 Turn、Task、Interaction、事件游标或 Inbox 已读状态。`--through-turn` 可以精确保留到某个已完成 Turn 并同时切换推理配置。

TUI 对应命令：`/session fork --through <TURN_ID> --profile <P> --model <M> [名称]`、`/history [关键词]` 或 `/session search [关键词] [--workspace/--status/--profile/--model/--after/--before]`（两者打开同一个历史会话面板，选中即进入）、`/session switch <SESSION_ID>`。

Export **不包含**队列私密 Prompt、Runtime Token 或 Provider 凭据。Delete 必须显式 `--yes`。

会话标题由首次 Turn 在本地生成，有长度上界；疑似含凭据的 Prompt 只使用通用标题。用户显式命名、收养的旧会话和 Fork 标题不会被覆盖。

## 事件流

Runtime 事件按递增序号写入私有 NDJSON 日志。

```bash
willdeep attach --after 0
willdeep detach
```

`GET /v1/events/stream?after=<序号>` 使用 SSE 先分页补齐持久事件，再实时推送新事件；慢客户端落后广播窗口时自动从日志追赶并按序号去重。TUI 默认使用该实时通道，连接旧 Daemon 时回退到轮询。

`attach --after <序号>` 按游标补读并持续跟随。按 `Ctrl+C` 只断开当前客户端，Daemon 继续运行。

Drain / Shutdown 时会广播关闭状态给 SSE 与 NDJSON 事件流，观察连接主动结束并由客户端沿持久游标重附着，不阻塞优雅关闭。

## 审批与提问

```bash
willdeep daemon pending
willdeep daemon resolve <interaction-id> allow-once
willdeep daemon resolve <interaction-id> deny
willdeep daemon resolve <interaction-id> always-allow
willdeep daemon answer <interaction-id> "自由输入答案"
```

详见 [审批与自动化](APPROVALS.md)。

## Agent 控制

```bash
willdeep daemon agents
willdeep daemon agent <agent-id>
willdeep daemon stop-agent <agent-id>
willdeep daemon retry-agent <agent-id>
willdeep daemon instruct-agent <agent-id> "补充要求"
```

详见 [子 Agent 与后台任务](SUBAGENTS.md)。

## Diff

Runtime 提供带内容指纹的 Workspace Diff 快照与文件内容 API：

```bash
willdeep daemon diff-snapshot --workspace .
willdeep daemon diff-file <snapshot-id> <path>
willdeep daemon diff-review <snapshot-id> <path> <decision>
willdeep daemon diff-verifications <snapshot-id>
willdeep daemon diff-attributions <snapshot-id>
willdeep daemon diff-commit-preview <snapshot-id>
willdeep daemon diff-revert <snapshot-id> <path>
```

Runtime 在可能写入工作区的工具调用前后采集内容指纹，把**真实变化**的路径绑定到 Session、Turn、Task、Agent 和 Tool。调用窗口外已有的脏文件不会被误算。

TUI 的 `/diff` 打开 Diff Review Center：浏览文件、增删统计、着色 Unified Diff、Unified/Side-by-side 切换、Combined/Staged/Unstaged 范围切换、文件内搜索。支持接受、打回、请求修改和标记已审。

安全撤销要求精确快照并二次确认，未跟踪内容移入可恢复回收区。

常见前后台测试命令完成后，会自动把命令、退出码、结果和有界摘要绑定到当时的 Diff 快照；疑似含凭据的命令拒绝记录。

Commit Preview 是只读的，汇总提交消息、暂存/未暂存文件、分支、脱敏后的 Remote / 推送目标和可选 Tag。敏感文件、疑似凭据、冲突、空暂存区或无效目标会明确阻止确认。

## 统一控制协议

统一控制协议从 `v0.21.0-rc1` 起由独立的 `willdeep-runtime-protocol` crate 定义，覆盖全部 11 类公开对象 DTO、Runtime 状态与删除/移除结果，并为所有公开操作提供类型化 Rust Client 方法。

CLI、TUI 和 Web 的 Workspace、Session、Turn、Task、Agent、审批、问答和 Diff 管理统一使用共享 Client，不再手写操作名、参数 JSON 或旧资源 URL。修改操作携带显式 Request ID 并进入跨重启幂等日志。

外部 Spawn 只接受活跃 Session、Prompt、可选标签和 `scout` / `reader` / `log_inspector` / `git_detective` Worker 档只读 Profile；父 Agent、Task 与 Workspace 由 Runtime 推导，调用方不能提交路径或写目标。`deep` 必须由父 Agent 携带升级票据调用，外部 Spawn 不能绕过准入。

进程内 Harness 的 Task、Interaction、Agent 命令及 Daemon 生命周期使用带内部标记的 `/v1/internal` 私有传输，**不属于公开协议**。

公共 API 兼容夹具覆盖 11 类稳定对象，供 Swift、Android 和第三方客户端做跨语言解码回归。

完整契约见 [Runtime 控制 API](RUNTIME_CONTROL_API.md) 与 [Runtime 会话协议](RUNTIME_SESSION_PROTOCOL.md)。

也可以用通用入口直接调用：

```bash
willdeep api session.list --ndjson
willdeep api event.stream --params-file events.json --ndjson
```

## 相关文档

- [CLI 参考](CLI_REFERENCE.md)
- [审批与自动化](APPROVALS.md)
- [子 Agent 与后台任务](SUBAGENTS.md)
- [Runtime 控制 API](RUNTIME_CONTROL_API.md)
- [进程内 Runtime Harness](IN_PROCESS_RUNTIME_HARNESS.md)
