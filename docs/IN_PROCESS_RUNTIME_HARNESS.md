# Daemon 内原生 Harness 设计

> 状态：已于 v0.17.0-rc4 实施
> 目标版本：v0.17.0-rc4
> 上位协议：`RUNTIME_SESSION_PROTOCOL.md`

## 1. 目标

Runtime Daemon 必须直接持有 Harness Future、Agent、Tools、MCP、Skills、后台任务、审批与子 Agent 生命周期，不再为每个 Turn 启动一个 `willdeep --web-input-json` 子进程。

CLI、TUI、Web、移动端和 Swift App 只提交 Turn、消费事件和解决人工交互。Provider 初始化、Prompt、工具权限、会话写入和后台结果回流必须使用同一个 Harness 执行入口。

## 2. 已移除的过渡层及问题

v0.17.0-rc3 及以前的 `TaskManager::submit` 会：

1. 生成 Runtime Task 并绑定 Turn；
2. 启动当前 `willdeep` 可执行文件；
3. 通过私有 stdin JSON 发送 Prompt、附件和 Runtime Token；
4. 把子进程 stdout NDJSON 转写为 Runtime 事件；
5. 通过 HTTP 回环处理审批、Ask User 和子 Agent 命令。

这保证了可分离任务，但仍有以下问题：

- 每个 Turn 重复解析配置、初始化 Provider、Skills 和 MCP；
- 取消依赖杀进程，无法细粒度收尾或复用资源；
- stdout 文本是第二层内部协议，事件需要重复编码和解析；
- Daemon 无法直接管理 Agent、后台任务和子 Agent 对象；
- Swift App 最终无法把它当作可嵌入内核。

## 3. 目标结构

```text
Runtime API
    │
Turn Scheduler
    │
TaskManager ── CancellationToken
    │
HarnessInvocation
    ├── HarnessFactory（配置、Provider、Tools、Skills、MCP、Profiles）
    ├── RuntimeEventSink ── EventLog
    ├── RuntimeApprover ── InteractionStore
    ├── BackgroundTaskRegistry
    └── SessionStore（唯一消息历史）
```

### 3.1 `HarnessInvocation`

每次执行只包含本 Turn 所需且可持久重建的数据：

- `task_id`、`session_id`、`turn_id`；
- canonical Workspace；
- Provider Profile 和配置文件路径；
- Prompt 与附件；
- Runtime 事件、审批和 Agent 控制句柄；
- 取消令牌。

不得在持久 Task 元数据中保存 API Key。凭据只能在执行时从已批准的配置或环境变量解析，并停留在内存中。

### 3.2 `HarnessFactory`

CLI 和 Runtime 共用同一个 Factory，负责：

- Provider 类型、API 方言、模型、上下文窗口和视觉降级；
- Smart/Strict/Workspace Access 审批模式；
- Skills、MCP、Web Tools 和 Always Allow Store；
- 内置及用户覆盖的子 Agent Profile；
- Stable System Prompt。

禁止在 Daemon 中复制一套简化 Provider 或 Tool 初始化逻辑。

### 3.3 `RuntimeEventSink`

AgentEvent 只编码一次为稳定 JSON，再直接写入 EventLog：

```text
task.output task_id=<uuid> {"type":"tool_requested",...}
```

事件 Sink 不得包含 Runtime Token，不得把工具参数或原始输出发给不具备权限的 Web 摘要接口。完整本地事件和面向客户端的脱敏事件是两个层次。

### 3.4 审批和 Ask User

第一阶段继续复用受 Token 保护的 Runtime Interaction API，以保持状态持久化与其他客户端解答能力。后续可把同进程调用优化成内部句柄，但不得绕过 InteractionStore 或产生另一套状态机。

### 3.5 取消

取消必须满足：

- 排队 Turn：不创建 Harness；
- 已领取但未启动：原子复核所有权后停止；
- Provider 请求中：取消 Future 并释放网络请求；
- 工具运行中：可取消工具应终止，不可取消工具完成后不得继续下一轮；
- 等待审批/回答：取消 Interaction，并唤醒 Harness 退出；
- 后台任务/子 Agent：按策略级联停止并记录终态；
- 最终只产生一个 Turn 终态，Cancelled 不能再被 Completed 覆盖。

## 4. 生命周期

```text
Queued
  └─ claim + ownership check
       └─ Running
            ├─ WaitingApproval ── Running
            ├─ WaitingAnswer ──── Running
            ├─ Completed
            ├─ Failed
            ├─ Cancelled
            └─ Interrupted（Daemon 异常退出后恢复）
```

Task、Turn、Root Agent 和 Session 的终态更新必须在同一个完成路径中执行。Harness 不直接修改 Runtime 元数据，只返回结构化结果；TaskManager 负责提交唯一终态并调度下一个 Turn。

## 5. 分步实施

1. [x] 抽出稳定 `agent_event_json`，供 Terminal 与 Runtime Sink 共用。
2. [x] 抽出 `HarnessFactory`，让现有 CLI 行为在无功能变化下通过原测试。
3. [x] 抽出非交互执行入口，统一消息写入、压缩和后台结果回流。
4. [x] 实现 `RuntimeEventSink`，直接写 EventLog 并同步 AgentStore。
5. [x] TaskManager 改为 `tokio::spawn` Harness Future，移除 stdin/stdout/stderr 转写。
6. [x] 用取消通知包裹整个 Harness Future，并保持 Interaction/Agent 命令语义。
7. [x] 删除 `--web-input-json` 的 Runtime 内部用途；隐藏入口仅作为旧客户端兼容层保留。
8. [x] 通过全量测试和真实进程 E2E，验证完成、取消、审批和 Ask User 恢复。

## 6. 验收证据

- 源码中 `TaskManager` 不再调用 `Command::new(current_exe)` 启动 Harness；
- 连续 Turn 仍保持唯一 Root Agent 和完整 Core Session 历史；
- Provider、视觉降级、Skills、MCP、后台任务和子 Agent 行为与 CLI 一致；
- 审批和 Ask User 可由另一个 CLI/Web/移动客户端解决后继续原 Future；
- 浏览器/TUI 断开不取消任务；显式 Stop 能取消 Provider 等待中的 Turn；
- Daemon 退出时活动 Future 收敛为 Interrupted，重启不重复执行已开始 Turn；
- 立即取消、运行中取消、等待审批取消和完成/取消竞争均有回归测试；
- Runtime 事件不经过 stdout 文本转写且不泄露 Token；
- `cargo test --workspace`、Clippy、Web build、release build 和进程 E2E 全部通过。

## 7. 非目标

- 本批不实现跨 Daemon 重启继续同一个内存 Future；异常退出统一恢复为 Interrupted。
- 本批不建立多租户远程执行池。
- 本批不提前实现 Unix Socket/Named Pipe；传输替换不能改变 Harness 生命周期协议。
