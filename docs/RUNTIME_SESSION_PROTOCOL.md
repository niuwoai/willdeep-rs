# Runtime Session 与 Root Harness 协议

> 状态：实施中
> 首个目标版本：v0.17.0
> 最后更新：2026-08-11（v0.21.0-rc41 Agent 显式详情与活动摘要）

## 1. 目标

Runtime Session 是 TUI、Web、移动端、Swift App 和自动化客户端共同操作的持久会话。客户端断开不终止执行；重新连接后按事件游标恢复消息、Agent、工具、审批和任务状态。

Core Session 的可选 `goal` 字段保存用户明确设置的持续目标。旧会话缺少该字段时按未设置读取；TUI 启动或切换 Session/Workspace 时必须从目标 Session 恢复，禁止沿用上一个会话的进程内 Goal。Goal 只在构造后续 Prompt 时注入，不新增伪造的用户或系统聊天消息。

Provider Profile、模型和私有配置引用随 Session 持久化；客户端切换后必须以目标 Session 为准，启动参数只用于新会话默认值。Skills 与 MCP 属于可撤销的 Workspace 权限：每个 Task 执行前从当前持久 Workspace 注册表重新解析，不允许历史 Session 快照恢复已撤销能力。

Agent Store 的 input/output/total Token 是 Agent 身份级累计值，而不是最后一次响应快照。Root Agent 在同一 Session 的后续 Task 中延续累计值；Child Agent 同身份重试也延续。累计使用饱和加法并持久化，Daemon 重启不得清零。

Root 与 Child Agent 都持久记录实际执行模型。Root 从 Runtime Task/Session 恢复；Child 从解析后的 Subagent Profile 生命周期事件恢复。公开 Agent DTO 的 `model` 为向后兼容的可选字段，TUI、Web、移动端和 Swift 观察客户端不得从 Profile 名称猜测模型。

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
- `title_source`（私有元数据）：`auto_pending | auto | user | legacy`

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
GET  /v1/sessions/search?q={query}
GET  /v1/sessions/{session_id}
POST /v1/sessions/{session_id}/rename
POST /v1/sessions/{session_id}/fork
POST /v1/sessions/{session_id}/archive
POST /v1/sessions/{session_id}/unarchive
GET  /v1/sessions/{session_id}/export
DELETE /v1/sessions/{session_id}
POST /v1/sessions/{session_id}/turns
GET  /v1/sessions/{session_id}/turns
GET  /v1/turns/{turn_id}
POST /v1/turns/{turn_id}/stop
POST /v1/turns/{turn_id}/retry
```

创建 Turn 必须提供客户端生成的 `request_id`。同一 Session 内重复提交相同 `request_id` 返回原 Turn，不再次运行模型或工具。

### 3.1 会话管理语义

- 标题和消息正文仍以 Core `SessionStore` 为唯一来源；Runtime 元数据不得复制标题或消息。
- 新建且未显式命名的 Session 以 `auto_pending` 开始，首个真实 Turn 入队前在本地从 Prompt 生成最多 80 字符的标题；不得为标题调用 Provider。疑似密码、Token、API Key、私钥、常见凭据前缀或高熵字段必须回退到通用标题。显式 Rename、旧会话收养与 Fork 不得被自动覆盖。
- Rename 只更新 Core 标题，并同步推进 Runtime `updated_at`；标题去除首尾空白后必须为 1–200 个字符。
- Fork 创建新的 Session ID 与 Root Agent ID，复制当前 Core 消息、Workspace、Profile、模型覆盖和配置引用，但不复制 Turn、Task、Interaction、事件游标和 Inbox 已读状态。请求可携带 `through_turn_id`，仅复制到该已完成 Turn 的持久 `message_end` 边界；缺失边界的旧 Turn 明确拒绝精确 Fork。`provider_profile` 和 `model` 可覆盖源 Session，并用于后续所有 Turn 的原生 Harness 构建。
- Archive 禁止新 Turn，但不删除历史；Unarchive 恢复为 Idle。活跃或仍有 Queued Turn 的 Session 不允许归档。
- Delete 永久删除 Runtime Session、所属 Turn 元数据和本地 Core Session 文件；API 请求体必须携带与路径一致的 Session ID 确认。活跃或仍有 Queued Turn 的 Session 不允许删除。
- Export 返回带 schema/version 的 JSON 快照，包含 Runtime 元数据、Core 消息和 Turn 审计元数据；不得包含已排队的私密 Prompt、附件副本、Runtime Token 或 Provider 凭据。
- Search 在本地受 Token 控制面执行，可组合标题/持久化 Core 消息、Workspace、状态、Provider Profile、模型与更新时间上下界；查询、结果数量和摘要长度必须有上限。
- Rename、Fork、Archive 与 Delete 在读取或改写 Core 快照前必须确认 Session 没有活跃 Turn，也没有 Queued Turn，防止与 Harness 写入竞争。

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

- Daemon 启动时只自动重放边界可证明安全的遗留活跃 Turn；旧 Task 标记为 `interrupted`，重放 Turn 进入 `queued`，不得假装旧执行已经完成。
- 已持久化但未开始的 `queued` Turn 保持队列顺序并自动恢复调度。
- 活跃 Turn 只有在旧 Task 没有任何持久 Tool 活动，且 Core 历史仍等于 `message_start`，或只多出与私有队列完全一致的一条用户消息时自动重新排队；已存在的用户消息由 Harness 复用，不删除也不重复写入。
- 任何已请求、完成、失败或中断的 Tool 记录都视为可能存在副作用，必须阻断自动重放。
- Core 历史含有额外消息、附件不一致或边界缺失时维持 `interrupted` 并保留全部内容，禁止猜测式截断。
- 已完成的 Core Session 消息不能因 Task 状态文件损坏而回滚。
- Provider/工具副作用是否可安全重放无法证明时，只允许显式重试。
- 旧 Pending Approval 与 `ask_user` 随失去执行协程而安全取消；自动重放再次抵达人类门时创建新交互并重新等待用户决定。

### 6.1 数据迁移

- Runtime Session 元数据当前 schema 为 2；schema 1 在内存完成确定性字段补全后升级。
- 改写源文件前必须在同一私有 Runtime 目录创建原始字节备份，备份名包含源 schema、时间和随机 ID，且不得覆盖既有备份。
- 新 schema 使用原子替换持久化；写入失败时原文件与备份仍可恢复。完成迁移后再次启动不得重复创建备份。
- 高于当前实现的未来 schema 必须拒绝读取，不能忽略未知语义后降级写回。

## 7. 安全约束

- API Key 只由 Harness 按 Provider Profile 解析，不进入 Session、Turn、Task 或 Event。
- Prompt、附件、工具参数和路径默认不写入公共事件摘要。
- Workspace 在创建 Session 时 canonicalize；每次恢复执行都重新建立当前路径边界。
- Profile、Skills、MCP 和审批策略在每个 Turn 开始时从当前配置重新加载，但历史事件记录使用的配置指纹。
- 客户端不能通过伪造 Session ID、Task ID 或 Parent ID 控制其他对象。

## 8. 迁移顺序

1. [x] 持久 RuntimeSession/RuntimeTurn 与受保护读写 API、CLI。
2. [x] Turn 幂等提交、串行队列、取消、恢复和 Task 关联。
3. [x] TUI `/runtime` 改为 Session Turn，并支持多轮与现有 Core Session 幂等收养。
4. [x] 普通 TUI Prompt 默认走 Runtime，保留 `/local <任务>` 显式单轮本地兼容模式。
5. [ ] Web、移动端和 Swift App 接入同一协议。
6. [x] Runtime 内原生持有 Harness，移除每 Turn 启动独立 CLI 进程的过渡实现。
7. [x] 完成 Rename、完整快照 Fork、Archive、Delete、Export、Search 的 Runtime API、CLI 与 TUI/Web 客户端入口。
8. [x] 已完成指定 Turn Fork、Fork Provider/模型覆盖、TUI 同 Workspace 原地切换、聊天事件 Session 隔离和组合搜索。

## 9. 验收条件

- 关闭并重开 TUI 后，继续同一 Session 的下一轮不会丢失上下文。
- 重复 `request_id` 不会产生重复模型请求或工具副作用。
- 同一 Session 的 Turn 严格串行，不同 Session 可受控并发。
- Cancel、失败、审批等待、Daemon 重启和 Turn 重试均能恢复到可解释状态。
- TUI、Web、移动端和 Swift App 观察到一致的 Session、Root Agent、Turn、Task 与 Child Agent 树。
