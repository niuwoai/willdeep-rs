# WillDeep Runtime Control API

> 状态：实施中  
> 协议版本：1.0  
> 当前实现版本：v0.21.0-rc42

## 1. 目标

本协议是 TUI、Web、Swift App、移动端、自动化脚本与第三方客户端操作同一个 WillDeep Runtime 的稳定边界。Runtime 持有业务状态机；客户端只发送意图、显示结构化状态并按事件游标恢复。

协议定义由 `willdeep-runtime-protocol` crate 唯一维护。服务端和 Rust Client 必须共同依赖该 crate，不得分别复制字符串、错误码或对象枚举。

## 2. 版本与能力协商

客户端连接后的第一步是请求：

```text
GET /v1/capabilities
X-WillDeep-Token: <runtime-local-token>
X-WillDeep-Request-Id: <optional UUID>
```

响应使用统一信封，并返回：

- `protocol_version`：服务端当前协议版本；
- `min_client_protocol_version`：最低兼容客户端协议版本；
- `server_version`：WillDeep 语义化版本；
- `objects`：可观察对象类别；
- `capabilities`：可用功能；
- `operations`：稳定的命名空间操作名；
- `transports`：当前真实可用的传输方式；
- `limits`：事件页、Prompt、附件和 Worktree Patch 等服务端限制。

服务端只声明当前进程实际启用的传输。Unix 客户端优先连接 Runtime 目录内权限为 `0600` 的 Socket；Windows 客户端优先连接拒绝远程连接的随机 Named Pipe。回环 TCP 作为旧状态兼容和诊断回退继续受随机 Token 保护。

本地生命周期端点 `POST /v1/drain` 自 rc32 起提供，仅供 `willdeep daemon upgrade` 使用。它将健康状态切换为 `draining`，原子阻止新的工作生产请求，同时允许停止、审批、回答和只读观察继续；活跃任务归零后旧进程自行退出。替换进程使用新的本地 Token/传输身份接管，长时间观察客户端只有确认身份已更替后才能重连并沿原事件游标续传。若源 Runtime 早于 rc32，Upgrade 收到 404 时必须保持任务原样并要求首次手动迁移，不能悄悄退化为会取消任务的 Shutdown。普通 `POST /v1/shutdown` 仍会取消活跃任务，不能用于版本交接。

## 3. 稳定对象

协议覆盖以下对象：

1. `runtime`
2. `workspace`
3. `session`
4. `agent`
5. `turn`
6. `tool`
7. `task`
8. `approval`
9. `question`
10. `artifact`
11. `event`

对象 ID 必须稳定且不可由客户端伪造父子关系。Session、Root Agent、Turn、Execution Task 与 Child Agent 的身份语义继续遵循 [`RUNTIME_SESSION_PROTOCOL.md`](RUNTIME_SESSION_PROTOCOL.md)。

## 4. 操作命名

统一入口使用 `<namespace>.<method>`，例如：

```text
runtime.capabilities
session.list
session.search
agent.spawn
agent.prompt
agent.wait
approval.resolve
event.list
event.stream
turn.submit
turn.list
turn.stop
tool.list
tool.get
artifact.list
artifact.get
diff.snapshot
diff.content
diff.review
diff.revert
```

`agent.spawn` 的稳定参数为：

```json
{
  "session_id": "00000000-0000-4000-8000-000000000000",
  "prompt": "Inspect the repository structure",
  "profile": "scout",
  "label": "structure scout"
}
```

成功响应是状态为 `queued` 的 `RuntimeAgent`，其中 `id` 可直接传给 `agent.get` 或 `agent.wait`。当前公开 Spawn 固定后台执行，Profile 仅允许 `scout`、`reader`、`deep`；它不接受 Parent ID、Task ID、Workspace、工具权限、`target_file` 或前台执行开关。

操作名一旦发布不得在同一协议主版本中改变语义。新增操作向后兼容；删除或改变字段含义需要提升协议主版本。

## 5. 响应信封

成功响应：

```json
{
  "status": "ok",
  "data": {},
  "meta": {
    "protocol_version": "1.0",
    "server_version": "0.21.0-rc42",
    "request_id": "00000000-0000-0000-0000-000000000000"
  }
}
```

错误响应：

```json
{
  "status": "error",
  "error": {
    "code": "stale_snapshot",
    "message": "snapshot changed; fetch a new review",
    "retryable": false
  },
  "meta": {
    "protocol_version": "1.0",
    "server_version": "0.21.0-rc42"
  }
}
```

稳定错误码包括：`invalid_request`、`unauthorized`、`forbidden`、`not_found`、`conflict`、`stale_snapshot`、`unsupported_operation`、`unsupported_protocol`、`rate_limited`、`unavailable`、`internal`。

## 6. 事件与流

- 历史事件使用全局单调 `sequence` 和显式 `after` 游标；
- NDJSON 每行必须是完整 UTF-8 JSON 对象；
- SSE `id` 对应事件序号，断线以最后已应用 ID 恢复；
- 客户端必须按序去重，不得从终端文本猜测 Tool、Approval 或 Agent 状态；
- 慢客户端从持久事件日志追赶，不能拖慢 Runtime 执行。

## 7. 安全与脱敏

- 所有控制端点必须显式校验随机 Runtime Token；
- API Key、Provider Secret、Runtime Token 永不进入 DTO、事件或错误字段；
- Prompt、附件正文和工具参数默认仅在明确需要的目标操作中返回；
- `agent.spawn` 只接受活跃 `session_id`、Prompt、标签与只读 Profile；父 Agent、Task、Workspace 和 Child ID 由服务端绑定，客户端路径、写目标、`editor` 与未知 Profile 均拒绝；
- `RuntimeTool` 只公开稳定 ID、Session/Turn/Task/Agent 归属、工具名、状态和起止时间；Tool 索引不保存参数、输出正文、Workspace 路径或内部错误；
- `RuntimeTask.failure_domain` 在失败时可取 `provider`、`policy`、`tool`、`harness` 或 `internal`，成功与旧记录为 `null`；未知未来值由旧客户端解码为 `unknown`，字段不包含内部错误正文；
- Workspace Change `RuntimeArtifact` 由内容指纹确认的 Diff Attribution 生成，只公开归属、来源快照和变更项数量；路径与内容必须另走受授权的精确 Diff API；
- Session 收养只接受稳定 ID、Workspace、Profile 与模型等公开字段；配置文件路径由 Runtime 从 Core Session 私有存储恢复，`CreateSessionParams` 拒绝客户端夹带 `config`；
- 文件路径按调用方权限裁剪；公开能力响应不得包含任何用户路径；
- `fields` 错误上下文只允许稳定字段名和非敏感值；
- Workspace 权限由服务端注册表覆盖，客户端字段不能扩大权限；
- 破坏性或可改变工作区状态的调用必须携带精确快照/请求 ID，并经过现有审批策略。

## 8. 迁移顺序

1. [x] 协议 crate、版本、对象类别、操作名、能力、错误码和响应信封；
2. [x] 受 Token 的能力协商端点；
3. [x] `willdeep api` JSON/NDJSON 统一入口；
4. [x] Runtime、Workspace、Session、Agent、Turn、Tool、Task、Approval、Question、Artifact、Event 与 Diff Review 共享 DTO；
5. [-] Rust Client Library；已覆盖全部公开对象、统一调用、能力协商、NDJSON、本地传输，以及 Workspace、Session、Agent、Turn、Task、Approval、Question、Tool、Artifact、Event、Diff Review 和 Worktree Review/Merge/Audit/Quarantine 类型化方法；
6. [-] TUI/Web/CLI 从手写 HTTP 调用迁移到 Client；TUI bridge 的 Session、Turn、Event、Agent、Task、Inbox、Tool、Artifact、完整 Diff Center 与 Worktree 控制已使用共享 Client，Workspace、Web Session/Turn 与 CLI Session/Turn 管理也已迁移；其他兼容命令继续迁移；
7. [x] Unix Socket 与 Windows Named Pipe；
8. [-] Swift FFI、移动端和自动化兼容验证；已提供覆盖全部 11 类公开对象的固定 JSON decoder 夹具，客户端适配层与端到端双读待完成。

## 9. 跨语言兼容夹具

[`public-api-v1.json`](../crates/willdeep-runtime-protocol/fixtures/public-api-v1.json) 是协议 `1.0` 的固定 decoder contract fixture。它包含统一响应信封，Runtime、Workspace、Session、Agent、Turn、Tool、Task、Approval、Question、Artifact、Event 十一类公开对象，以及 Agent Spawn/Prompt/Wait、审批、提问、事件查询请求和公共控制结果；不包含认证凭据、工具参数、输出正文或本机路径。

Swift、Android 和第三方客户端应在 CI 中逐项解码 `responses`，并至少断言：

1. `protocol_version` 的主版本受支持；
2. 未知的可选能力和操作不会导致整个能力响应解码失败；
3. UUID、可空字段、snake_case 枚举和 64 位时间/序号保持精度；
4. `status=error` 与 `status=ok` 使用显式分支处理；
5. 夹具更新需要与 Rust 协议测试、版本号和 Changelog 同批提交。
