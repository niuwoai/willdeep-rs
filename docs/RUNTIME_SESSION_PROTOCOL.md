# Runtime Session 与 Root Harness 协议

> 状态：实施中
> 首个目标版本：v0.17.0
> 最后更新：2026-08-10

## 1. 目标

Runtime Session 是 TUI、Web、移动端、Swift App 和自动化客户端共同操作的持久会话。客户端断开不终止执行；重新连接后按事件游标恢复消息、Agent、工具、审批和任务状态。

本协议区分四种稳定身份：

```text
Runtime Session（长期存在）
└── Root Agent（会话内稳定）
    ├── Turn（一次用户请求，可排队或重试）
    │   └── Execution Task（一次具体 Harness 执行尝试）
    └── Child Agent（由某个 Turn 产生，可独立控制）
```

- Session ID 不因进程退出、模型切换或 Turn 重试而变化。
- Root Agent ID 在 Session 生命周期内保持不变。
- Turn ID 表示用户意图；幂等重放不能创建第二个 Turn。
- Task ID 表示一次执行尝试；Turn 重试会创建新 Task ID。
- Child Agent ID 在自身重试时保持不变。

## 2. 持久对象

### RuntimeSession

- `id`
- `root_agent_id`
- `workspace`
- `profile`
- `config`
- `status`: `idle | queued | running | waiting_approval | waiting_answer | failed | interrupted | archived`
- `active_turn_id`
- `created_at`
- `updated_at`
- `last_error`

消息正文和附件继续使用 Core `SessionStore` 作为唯一来源；Runtime 元数据不得复制另一份消息历史。

### RuntimeTurn

- `id`
- `session_id`
- `request_id`
- `status`: `queued | running | waiting_approval | waiting_answer | completed | failed | cancelled | interrupted`
- `active_task_id`
- `attempts`
- `created_at`
- `started_at`
- `completed_at`
- `error`

待执行 Prompt 与附件在进入队列时必须私有、原子持久化。执行成功写入 Core Session 后，队列副本应清除正文，只保留审计摘要。

## 3. 本地 API

所有端点必须携带 Runtime 随机 `x-willdeep-token`，并返回 `X-WillDeep-Version`。

```text
POST /v1/sessions
GET  /v1/sessions
GET  /v1/sessions/{session_id}
POST /v1/sessions/{session_id}/turns
GET  /v1/sessions/{session_id}/turns
GET  /v1/turns/{turn_id}
POST /v1/turns/{turn_id}/stop
POST /v1/turns/{turn_id}/retry
```

创建 Turn 必须提供客户端生成的 `request_id`。同一 Session 内重复提交相同 `request_id` 返回原 Turn，不再次运行模型或工具。

## 4. 串行与并发规则

- 同一 Root Agent 默认一次只运行一个 Turn。
- 新 Turn 在 Session 内按创建顺序排队。
- 不同 Session 可以并发，受 Runtime 全局并发上限约束。
- Child Agent 并发受 Profile、深度、Token、费用和时长预算约束。
- `/stop` 只停止目标 Turn 的当前 Task 和子任务，不归档 Session。
- `/retry` 在同一 Turn 下创建新 Task，不能复制用户消息。

## 5. 状态与事件

状态变更必须先原子持久化，再发布 Runtime Event：

```text
session.created
session.status_changed
turn.queued
turn.started
turn.waiting_approval
turn.waiting_answer
turn.completed
turn.failed
turn.cancelled
turn.retry_queued
```

事件必须包含 `session_id`、`agent_id`、`turn_id` 和适用时的 `task_id`。SSE 使用全局单调序号，客户端以最后已应用序号恢复并去重。

## 6. 崩溃恢复

- Daemon 启动时将遗留 `running` Turn 标记为 `interrupted`，不得假装完成。
- 已持久化但未开始的 `queued` Turn 保持队列顺序并自动恢复调度。
- 已完成的 Core Session 消息不能因 Task 状态文件损坏而回滚。
- Provider 请求是否可安全续跑无法证明时，只允许显式重试，避免重复工具副作用。
- Pending Approval 与 `ask_user` 继续关联原 Turn；Turn 终止时自动安全拒绝并关闭等待者。

## 7. 安全约束

- API Key 只由 Harness 按 Provider Profile 解析，不进入 Session、Turn、Task 或 Event。
- Prompt、附件、工具参数和路径默认不写入公共事件摘要。
- Workspace 在创建 Session 时 canonicalize；每次恢复执行都重新建立当前路径边界。
- Profile、Skills、MCP 和审批策略在每个 Turn 开始时从当前配置重新加载，但历史事件记录使用的配置指纹。
- 客户端不能通过伪造 Session ID、Task ID 或 Parent ID 控制其他对象。

## 8. 迁移顺序

1. [x] 持久 RuntimeSession/RuntimeTurn 与受保护读写 API、CLI。
2. [x] Turn 幂等提交、串行队列、取消、恢复和 Task 关联。
3. [ ] TUI `/runtime` 改为 Session Turn，并支持多轮。
4. [ ] 普通 TUI Prompt 默认走 Runtime，保留显式本地兼容模式。
5. [ ] Web、移动端和 Swift App 接入同一协议。
6. [ ] Runtime 内原生持有 Harness，移除每 Turn 启动独立 CLI 进程的过渡实现。

## 9. 验收条件

- 关闭并重开 TUI 后，继续同一 Session 的下一轮不会丢失上下文。
- 重复 `request_id` 不会产生重复模型请求或工具副作用。
- 同一 Session 的 Turn 严格串行，不同 Session 可受控并发。
- Cancel、失败、审批等待、Daemon 重启和 Turn 重试均能恢复到可解释状态。
- TUI、Web、移动端和 Swift App 观察到一致的 Session、Root Agent、Turn、Task 与 Child Agent 树。
