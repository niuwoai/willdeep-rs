# WillDeep Runtime Control API

> 状态：实施中  
> 协议版本：1.0  
> 当前实现版本：v0.21.0-rc63

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

私有生命周期端点 `POST /v1/internal/drain` 自 rc54 起仅供 `willdeep daemon upgrade` 使用。它将健康状态切换为 `draining`，原子阻止新的工作生产请求，同时允许停止、审批、回答和只读观察继续；活跃任务归零后旧进程自行退出。替换进程使用新的本地 Token/传输身份接管，长时间观察客户端只有确认身份已更替后才能重连并沿原事件游标续传。rc54 客户端仅为 rc53 保留一次 `POST /v1/drain` 兼容桥；更早且不支持安全 Drain 的 Runtime 必须保持任务原样并要求首次手动迁移，不能悄悄退化为会取消任务的 Shutdown。普通 `POST /v1/internal/shutdown` 仍会取消活跃任务，不能用于版本交接。

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
12. `kernel`（事件内核里的入向信号，与 `event` 方向相反：`event` 是 Runtime 向外报告发生了什么，`kernel` 是外界向主 Agent 投递的通知）

对象 ID 必须稳定且不可由客户端伪造父子关系。Session、Root Agent、Turn、Execution Task 与 Child Agent 的身份语义继续遵循 [`RUNTIME_SESSION_PROTOCOL.md`](RUNTIME_SESSION_PROTOCOL.md)。

## 4. 操作命名

统一入口使用 `<namespace>.<method>`，例如：

```text
runtime.capabilities
runtime.status
workspace.register
workspace.ensure
workspace.activate
workspace.remove
session.create
session.list
session.search
session.rename
session.fork
session.archive
session.delete
session.export
agent.spawn
agent.prompt
agent.wait
agent.retry
approval.resolve
event.list
event.stream
kernel.list
kernel.get
kernel.ignore
turn.submit
turn.list
turn.stop
task.diagnostics
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
  "profile": "reader",
  "label": "structure reader"
}
```

`agent.retry` 只接受已经进入终态且可重试的后台 Child Agent。可选 `model` 表示在重试边界重建同一 Provider 的模型实例；运行中的 Agent 不热切模型：

```json
{
  "id": "00000000-0000-4000-8000-000000000003",
  "model": "qwen3-coder-plus"
}
```

成功响应是状态为 `queued` 的 `RuntimeAgent`，其中 `id` 可直接传给 `agent.get` 或 `agent.wait`。当前公开 Spawn 固定后台执行，Profile 只公开无 Shell、无写入的 `reader`、`judge`；旧只读 ID 仅为保存流程兼容继续接受。它不接受 Parent ID、Task ID、Workspace、工具权限、`target_file`、`target_command` 或前台执行开关。命令/写入工种及 `deep` 只允许父 Agent 经安全链发起。

内嵌 Web 的同源适配端点为 `POST /api/runtime/agents/spawn`。浏览器请求额外携带当前选择的 `workspace`，服务端先以启动白名单与 Runtime 注册表交叉验证 Workspace，再确认 `session_id` 属于该 Workspace 且存在活动 Turn；随后只把经过边界校验的 `session_id`、`prompt`、只读 `profile` 和可选 `label` 转交统一 `agent.spawn`。该适配层不接受 Parent ID、Task ID、Child ID、路径、工具权限或写 Profile，成功返回 HTTP `202` 与公开 Agent 摘要。

操作名一旦发布不得在同一协议主版本中改变语义。新增操作向后兼容；删除或改变字段含义需要提升协议主版本。

`kernel.*` 读写事件内核的日志（`~/.willdeep/agent-events/`），**不碰任何一个进程的内存**：跑 Agent 的可能是别的进程，日志是两者之间唯一的共享事实，代价是结果最多落后一次刷盘（秒级）。`kernel.ignore` 只把「还等着人」这个标记摘掉，事件留在日志里，**它不批准任何操作**——审批仍然要在它被提出的地方回答。

`kernel.list` / `kernel.get` 返回 `PublicKernelEvent`：**事件正文不在里面**。body 可能是外部消息、工具输出或 Worker 报告全文，与 Prompt 同级私有；公共 DTO 只带一个按命令审批同规则打码、截断到 120 字符的标题摘要，够回答「这是哪一条」，不够替代读原文。字段是白名单，新增字段不会自动顺流到浏览器。

`turn.submit` 的可选 `origin_client` 记下**是哪一端提交的这一轮**,`RuntimeTask` 原样带出来。审批与提问据此弹回发起端:同一个会话可能同时开在终端和浏览器里,按会话判定的话两处都会弹,谁先答谁算数,另一边只看到问题凭空消失。标识按**界面**分而不是按进程分——终端里用 `/webapp` 起的 Web 与终端同进程,共用一个 ID 就等于没分。旧 Daemon 不产出该字段(读回来是 `null`),此时回落到按会话判定,老任务不至于没人管。

`runtime.status` 返回类型化健康状态、版本、PID、运行时间和事件头；安全升级排空期间状态为 `draining`。`workspace.remove` 与 `session.delete` 返回 `{ id, status }` 结构化结果，其中状态分别为 `removed` 与 `deleted`，客户端无需解析任意 JSON 文本。

## 5. 响应信封

成功响应：

```json
{
  "status": "ok",
  "data": {},
  "meta": {
    "protocol_version": "1.0",
    "server_version": "0.21.0-rc62",
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
    "server_version": "0.21.0-rc62"
  }
}
```

稳定错误码包括：`invalid_request`、`unauthorized`、`forbidden`、`not_found`、`conflict`、`stale_snapshot`、`unsupported_operation`、`unsupported_protocol`、`rate_limited`、`unavailable`、`internal`。

## 6. 事件与流

- 历史事件使用全局单调 `sequence` 和显式 `after` 游标；
- NDJSON 每行必须是完整 UTF-8 JSON 对象；
- SSE `id` 对应事件序号，断线以最后已应用 ID 恢复；
- Web 适配端点 `GET /api/sessions/{session_id}/stream?after=<sequence>&language=<language>` 只接受已在 Web 启动白名单内的 Session；服务端从 Runtime Session 推导活动 Turn、Task、Workspace 与 Task 事件起点，并将客户端游标限制在该起点和当前事件头之间；
- 客户端必须按序去重，不得从终端文本猜测 Tool、Approval 或 Agent 状态；
- 慢客户端从持久事件日志追赶，不能拖慢 Runtime 执行。

## 7. 安全与脱敏

- 所有控制端点必须显式校验随机 Runtime Token；
- API Key、Provider Secret、Runtime Token 永不进入 DTO、事件或错误字段；
- Prompt、附件正文和工具参数默认仅在明确需要的目标操作中返回。唯一例外是 `RuntimeTask.prompt_excerpt`（0.55.0-rc1 起的**有意边界移动**）：提交时按命令审批同一套凭据规则打码、空白压平、截断到 120 字符的提示词前缀，作为任务标识进入 `task.list` / `task.get`——只有 UUID 的任务在 Inbox 和列表里认不出是谁。完整提示词与附件正文仍不进任何公共 DTO 或事件，旧 Daemon 不产出该字段，客户端须容忍 `null`；
- `agent.spawn` 只接受活跃 `session_id`、Prompt、标签与只读 Profile；父 Agent、Task、Workspace 和 Child ID 由服务端绑定，客户端路径、写目标、`editor` 与未知 Profile 均拒绝；
- `RuntimeTool` 只公开稳定 ID、Session/Turn/Task/Agent 归属、工具名、状态和起止时间；Tool 索引不保存参数、输出正文、Workspace 路径或内部错误；
- `task.diagnostics` 是唯一返回失败工具原始参数与输出摘要的操作，专供本机排查「哪条命令、为什么失败」：调用方必须持有本机 Runtime Token（它本来就能导出整段会话转录），Web 桥接与手机中继不转发该操作。公共事件流不受影响——`task.output` 的 `arguments` / `output` 与 `error=` 后缀仍按 `public_event` 剥除，两者是同一份数据的两种口径；
- 失败工具写入持久事件日志前，参数与输出按命令审批同一套凭据规则打码，并截断为有上界的首尾摘要，避免整段 stdout 进日志；
- `RuntimeTask.failure_domain` 在失败时可取 `provider`、`policy`、`tool`、`harness` 或 `internal`，成功与旧记录为 `null`；未知未来值由旧客户端解码为 `unknown`，字段不包含内部错误正文；
- Web Runtime Activity 适配层只投影 Task 的公开关联 ID、状态、Profile、耗时、退出码和失败域；Workspace、Prompt（含 `prompt_excerpt`）、命令、参数、输出、错误正文、报告、路径、模型、配置与 PID 不进入浏览器响应；
- Web SSE 恢复请求不能选择 Workspace、Turn、Task 或 Agent；`after` 只表示客户端最后已应用序号，恢复不会新建 Turn、重复 Provider 请求或更改 Harness 状态；
- Workspace Change `RuntimeArtifact` 由内容指纹确认的 Diff Attribution 生成，只公开归属、来源快照和变更项数量；路径与内容必须另走受授权的精确 Diff API；
- Session 收养只接受稳定 ID、Workspace、Profile 与模型等公开字段；配置文件路径由 Runtime 从 Core Session 私有存储恢复，`CreateSessionParams` 拒绝客户端夹带 `config`；
- 文件路径按调用方权限裁剪；公开能力响应不得包含任何用户路径；
- `fields` 错误上下文只允许稳定字段名和非敏感值；
- Workspace 权限由服务端注册表覆盖，客户端字段不能扩大权限；
- 破坏性或可改变工作区状态的调用必须携带精确快照/请求 ID，并经过现有审批策略。

## 8. 私有 Harness 传输

以下能力不是公开协议，不进入能力协商、跨语言夹具或 `willdeep-runtime-client`：

- 完整内部 Task 提交，包括私有配置引用和服务端覆盖的 Workspace 策略；
- Harness 等待 Approval/Question 的长连接 Interaction；
- 原 Harness 轮询及回执 Agent stop/retry/instruct/spawn 命令；
- 带私有配置引用的 Session 创建；
- Daemon drain 与 shutdown 生命周期。

这些端点统一位于 `/v1/internal`，同时要求 Runtime Token 和 `X-WillDeep-Internal-Transport: 1`。内部标记不是第二个秘密，只用于协议分层和防止误调用；真正的认证仍由随机 Runtime Token、本机绑定与本地传输权限承担。缺少或伪造标记时返回 404，不暴露内部端点语义。

内部 Client 仅存在于 CLI crate，不从共享 Rust Client 导出。rc54 仍可对 rc53 的旧 drain/shutdown 路径执行一次升级兼容；新 Runtime 接管后所有内部调用必须使用新路径。

## 9. 迁移顺序

1. [x] 协议 crate、版本、对象类别、操作名、能力、错误码和响应信封；
2. [x] 受 Token 的能力协商端点；
3. [x] `willdeep api` JSON/NDJSON 统一入口；
4. [x] Runtime、Workspace、Session、Agent、Turn、Tool、Task、Approval、Question、Artifact、Event 与 Diff Review 共享 DTO；
5. [x] Rust Client Library；覆盖全部公开操作、能力协商、NDJSON、本地传输，以及 Runtime、Workspace、Session、Agent、Turn、Task、Approval、Question、Tool、Artifact、Event、Diff Review 和 Worktree Review/Merge/Audit/Quarantine 类型化方法；
6. [x] TUI/Web/CLI 从手写 HTTP 调用迁移到 Client；全部公开 Workspace、Session、Turn、Event、Agent、Task、审批、问答、Tool、Artifact、Diff 与 Worktree 客户端调用已经统一，不再以 404 回退旧资源路由；进程内 Harness 的完整任务提交、Interaction 挂起恢复、Agent 命令队列和 Daemon 生命周期已提取为带内部标记的 `/v1/internal` 私有传输，公开 Runtime Client 不暴露这些操作；
7. [x] Unix Socket 与 Windows Named Pipe；
8. [-] Swift FFI、移动端和自动化兼容验证；已提供覆盖全部 11 类公开对象的固定 JSON decoder 夹具，客户端适配层与端到端双读待完成。

## 10. 跨语言兼容夹具

[`public-api-v1.json`](../crates/willdeep-runtime-protocol/fixtures/public-api-v1.json) 是协议 `1.0` 的固定 decoder contract fixture。它包含统一响应信封，Runtime、Workspace、Session、Agent、Turn、Tool、Task、Approval、Question、Artifact、Event 十一类公开对象，以及 Runtime 状态、对象修改结果、Workspace 注册、Session 删除、Agent Spawn/Prompt/Wait、审批、提问和事件查询契约；不包含认证凭据、工具参数、输出正文或本机路径。

Swift、Android 和第三方客户端应在 CI 中逐项解码 `responses`，并至少断言：

1. `protocol_version` 的主版本受支持；
2. 未知的可选能力和操作不会导致整个能力响应解码失败；
3. UUID、可空字段、snake_case 枚举和 64 位时间/序号保持精度；
4. `status=error` 与 `status=ok` 使用显式分支处理；
5. 夹具更新需要与 Rust 协议测试、版本号和 Changelog 同批提交。
