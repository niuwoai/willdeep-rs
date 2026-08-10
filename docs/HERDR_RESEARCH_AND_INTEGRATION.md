# Herdr 研究与 WillDeep 集成方案

> 最后更新：2026-08-10
> 研究对象：[ogulcancelik/herdr](https://github.com/ogulcancelik/herdr)、[Herdr 官方文档](https://herdr.dev/docs/)
> 结论：借鉴状态权威、持久 PTY 与事件控制面；不复制 Herdr，也不让它成为 WillDeep Runtime 的强依赖。

## 1. Herdr 解决的核心问题

Herdr 是面向 Coding Agent 的终端复用器。它把真实 PTY、Workspace、Tab、Pane、后台 Server、断开/重连、鼠标操作和 Agent 状态聚合放入一个 Rust 二进制。Agent 继续运行在真实终端里，客户端断开后由后台 Server 保持进程和 Pane。

它与普通 tmux 的主要差异不是“能开几个 Pane”，而是能够识别并聚合 Agent 的 `idle/working/blocked/done` 状态，让用户直接跳到需要处理的 Pane。官方资料还提供本地 Socket API、事件订阅、Agent wait/prompt、Pane 读取和输入、Workspace/Worktree 管理以及插件入口：

- [Herdr README](https://github.com/ogulcancelik/herdr#readme)
- [Agent 状态与状态权威](https://herdr.dev/docs/agents/)
- [Socket API](https://herdr.dev/docs/socket-api/)
- [持久化与远程访问](https://herdr.dev/docs/persistence-remote/)

## 2. 最值得借鉴的设计

### 2.1 单一状态权威

Herdr 在完整生命周期 Hook 可用时，以 Hook 为唯一状态权威，不再同时使用屏幕规则推断同一个 Agent 的生命周期。没有完整 Hook 时才读取 Pane 底部实时缓冲区并使用 Manifest 规则降级识别。

WillDeep 应坚持更强的版本：

1. WillDeep 原生 Harness 的 Session、Turn、Task、Interaction 和 Agent 状态只由 Runtime 事件决定。
2. TUI、Web、移动端、Swift App 和 Herdr 都是状态投影，不得反向猜测 Runtime 真值。
3. 仅在托管 Claude Code、Codex 等外部 Agent 时，才允许进程或屏幕检测作为降级来源。
4. 同一个 Agent 同一时刻只能有一个生命周期权威，避免 Hook 与画面识别互相打架。

### 2.2 Bootstrap Snapshot + 增量事件

Herdr 的 `session.snapshot` 用于客户端首次建立本地缓存，之后通过事件订阅增量更新；重连或缓存不可信时重新获取 Snapshot。

WillDeep 当前已有持久事件序列号、SSE 游标补读和 Runtime Snapshot。后续统一控制 API 应正式固定：

```text
GET /v1/snapshot?workspace=...
GET /v1/events/stream?after=<cursor>
```

Snapshot 必须带协议版本、服务版本和最新事件游标。客户端先读取 Snapshot，再从该游标订阅；不能先订阅再猜测缺失状态。

### 2.3 状态向上聚合

Herdr 将 Pane 状态聚合到 Tab 和 Workspace，并让未查看的完成状态保持可见。WillDeep 已有 Attention Inbox 和 Agent → Session → Workspace 聚合，应继续使用以下优先级：

```text
waiting_approval / waiting_answer / failed
  > working / queued
  > done_unseen
  > idle / cancelled
  > unknown
```

`done_unseen` 是展示状态，不应污染 Runtime Task 的持久终态。客户端确认查看后只更新 Attention 已读游标。

### 2.4 Agent 可调用的控制面

Herdr 的 CLI 与 Socket API 共用控制面，Agent 可以创建 Pane、发送 Prompt、读取输出、订阅事件和等待状态。WillDeep 的统一控制 API 也应让人、CLI 脚本和 Agent 使用同一协议，并具备：

- 稳定 UUID，而不是临时 Pane 文本；
- 请求幂等 ID；
- 结构化错误码；
- 能力协商；
- 事件游标与断线补读；
- Workspace、审批与工具权限不可绕过。

### 2.5 真实终端与 Runtime 解耦

Herdr 的真实 PTY 对运行外部 CLI Agent、调试器、REPL 和全屏 TUI 很有价值。WillDeep 不应在当前主线重新实现完整终端模拟器。正确分层是：

```text
Herdr / tmux / SSH / 普通终端  = 进程与终端承载层
WillDeep Runtime               = Session、Agent、工具、安全和任务真值
TUI / Web / Mobile / Swift     = WillDeep 原生客户端
```

## 3. WillDeep 已有优势

Herdr 主要观察和编排终端进程；WillDeep 控制的是 Harness 内部语义，因此能提供更精确的信息：

| 能力 | Herdr | WillDeep |
|---|---|---|
| 真实 PTY 与任意 CLI | 核心能力 | 可由外部终端承载 |
| Agent 状态 | Hook 或屏幕 Manifest | Runtime 结构化事件 |
| Provider/模型 | 外部 Agent 自己管理 | Session 持久配置 |
| 工具参数与结果 | 主要读取屏幕 | 原生结构化 Tool Event |
| 审批与 ask_user | Agent 屏幕或 Hook | 持久 Interaction |
| 后台任务结果回流 | 依赖被托管 Agent | 原生回流主 Harness |
| 子 Agent 树 | Pane/集成投影 | 稳定父子 UUID 与 Profile |
| 多模态降级 | 取决于外部 Agent | 原生视觉模型降级 |
| Web/移动/Swift | 非核心 | 同一 Runtime 多客户端 |

因此，WillDeep 不应把路线改成“Herdr 的另一个 Fork”。护城河仍是跨客户端共享的结构化 Agent Runtime。

## 4. 许可与实现边界

Herdr 官方 README 当前声明 AGPL-3.0-or-later 与商业双许可证。WillDeep 采用 Apache-2.0，因此：

1. 不复制 Herdr 源码、内部协议实现或资源文件。
2. 仅依据公开文档实现独立的进程级/Socket 级互操作适配器。
3. 不把 Herdr 源码或修改版静态/动态链接进入 WillDeep 二进制。
4. 若未来打包分发 Herdr、修改 Herdr 或形成更紧密的衍生组合，发布前必须进行许可证审查。

本节是工程边界，不构成法律意见。

## 5. 首批集成设计

### 5.1 生命周期状态上报

当 WillDeep 运行于 Herdr Pane 内，Herdr 会注入 `HERDR_ENV`、`HERDR_SOCKET_PATH` 和 `HERDR_PANE_ID`。WillDeep Runtime 可通过公开 CLI 上报：

```text
herdr pane report-agent <pane-id>
  --source willdeep:runtime
  --agent willdeep
  --state working|blocked|idle
```

映射规则：

| WillDeep 聚合状态 | Herdr 状态 | 摘要 |
|---|---|---|
| Queued/Running/Cancelling | working | 当前任务或工具 |
| WaitingApproval/WaitingAnswer | blocked | approval / question |
| Failed/Interrupted | blocked | failed / interrupted |
| 全部终态且无待处理 | idle | done |

状态必须从 Runtime Task/Interaction 聚合得出，不能从 TUI 文本反推。上报失败只记录脱敏诊断，不能影响 Harness 执行。

### 5.2 安装与状态命令

规划命令：

```text
willdeep integrations herdr status
willdeep integrations herdr install
willdeep integrations herdr uninstall
```

第一版 `status` 检查 Herdr CLI、环境变量、Pane ID 和公开 API 可用性；`install`/`uninstall` 管理 WillDeep 自己的 Herdr Integration 配置，不擅自安装或删除 Herdr 主程序。

### 5.3 Pane 与 Session 关联

Runtime 后续持久化可选的外部承载信息：

```text
external_host.kind = "herdr"
external_host.workspace_id
external_host.tab_id
external_host.pane_id
```

它只用于跳转、状态投影和恢复提示，不成为 Session、Turn 或 Agent 的身份来源。

## 6. 分阶段实施

1. **v0.18：状态上报**——检测 Herdr 环境，聚合 Runtime 状态并非阻塞地上报。
2. **v0.18：诊断命令**——实现 `integrations herdr status` 和脱敏诊断。
3. **v0.19：Session/Pane 关联**——记录承载 Pane，TUI 可跳转到对应 Agent。
4. **v0.20：外部 Agent 托管**——可选通过 Herdr 启动 Codex、Claude Code、OpenCode，并明确其权限边界。
5. **v0.21：事件互操作**——使用公开 Socket API 订阅 Pane/Agent 事件，仍由 WillDeep Runtime 维护原生任务真值。

## 7. 明确不做

- 不在近期重写 VT 终端解析、PTY Multiplexer、SSH、Mosh 或 Tailscale。
- 不把 Herdr 的屏幕检测用于 WillDeep 原生 Harness。
- 不让 Pane ID 替代 WillDeep Session/Turn/Agent UUID。
- 不允许 Herdr 插件绕过 WillDeep Workspace、审批、MCP、网络或 Shell 安全策略。
- 不因 Herdr 未安装、版本不兼容或状态上报失败而阻塞用户任务。

## 8. 验收条件

- Herdr 未安装时 WillDeep 行为完全不变。
- Herdr Pane 内运行时，Working、Blocked、Failed 和 Idle 映射由自动化测试覆盖。
- 多个 Runtime Task 并行时只上报聚合后的最高优先级状态。
- 上报过程不使用 Shell 字符串拼接，不泄露 Prompt、API Key、工具参数或文件内容。
- 集成失败可诊断、可关闭，不影响 Runtime 状态持久化和 Harness 结果。
