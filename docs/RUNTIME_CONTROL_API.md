# WillDeep Runtime Control API

> 状态：实施中  
> 协议版本：1.0  
> 当前实现版本：v0.21.0-rc8

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

服务端不能提前声明尚未实现的 Unix Socket 或 Windows Named Pipe；完成后才加入 `transports`。

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
diff.snapshot
diff.content
diff.review
diff.revert
```

操作名一旦发布不得在同一协议主版本中改变语义。新增操作向后兼容；删除或改变字段含义需要提升协议主版本。

## 5. 响应信封

成功响应：

```json
{
  "status": "ok",
  "data": {},
  "meta": {
    "protocol_version": "1.0",
    "server_version": "0.21.0-rc8",
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
    "server_version": "0.21.0-rc8"
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
- 文件路径按调用方权限裁剪；公开能力响应不得包含任何用户路径；
- `fields` 错误上下文只允许稳定字段名和非敏感值；
- Workspace 权限由服务端注册表覆盖，客户端字段不能扩大权限；
- 破坏性或可改变工作区状态的调用必须携带精确快照/请求 ID，并经过现有审批策略。

## 8. 迁移顺序

1. [x] 协议 crate、版本、对象类别、操作名、能力、错误码和响应信封；
2. [x] 受 Token 的能力协商端点；
3. [x] `willdeep api` JSON/NDJSON 统一入口；
4. [x] Workspace、Session、Agent、Turn、Task、Approval、Question、Event 与 Diff Review 共享 DTO；
5. [-] Rust Client Library；已实现统一调用、能力协商、NDJSON 解码和 Workspace/会话/Agent/任务/交互/Diff DTO，剩余 Tool、Artifact 与本地传输继续进行；
6. [-] TUI/Web 从手写 HTTP 调用迁移到 Client；TUI 事件、Agent、Task、Inbox、Workspace、Diff Center、Session 搜索、Turn 提交/停止与控制，以及 Web Session 管理和 Turn 提交/停止已迁移；带私有配置引用的 Session 收养和部分 CLI 兼容命令继续迁移；Web/TUI 事件补读已迁移；
7. [ ] Unix Socket 与 Windows Named Pipe；
8. [ ] Swift FFI、移动端和自动化兼容验证。
