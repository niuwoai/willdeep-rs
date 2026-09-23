# 决策单：手机中继从 TUI 挪进 Runtime Daemon（2026-09-23）

> 状态：已实现（0.82.0-rc1，未发布；见文末「完成记录」） | 基于 `fix/cli-mobile-command-alignment` @ 0.81.0-rc9 | 关联：`docs/MOBILE.md`、`docs/RUNTIME_DAEMON.md`、`docs/RUNTIME_CONTROL_API.md`、Android `docs/MOBILE_GATEWAY_REQUIREMENTS.md`「Desktop Command Matrix」
> 一句话：手机成为 Runtime 的又一个**受限客户端**——看得到整个 Runtime（所有会话、所有待处理审批），能做的写操作只有四件。

## 1. 为什么要改

0.81.0-rc9 之前，手机中继是 TUI 进程里的一条 WebSocket：

| 现象 | 根因 |
|---|---|
| 手机上只看得到开 `/mobile` 的那一条会话 | `mobile_state()` 在开中继那一刻算好快照，`sessions` 只放当前会话，之后再也不更新 |
| 关掉终端中继就断，任务却还在 Daemon 里跑 | 中继的生命周期挂在 TUI 进程上，而任务从 0.2x 起就跑在 Daemon 里 |
| 手机上审批不了 | `tool.decide` 回 `Unsupported`；任务卡在审批上只能等人回到电脑前 |
| 侧栏那两个 Runtime 智能体（跑 4 小时 / 53 小时的）手机看不到 | 只转发 `AgentEvent::AssistantText`，工具、审批、别的会话一概不转 |
| 两个终端都开 `/mobile` 会串台，还可能来回刷屏 | 凭据 `mobile-relay.toml` 全局共用，两个进程进同一个 room；`handle_command` 不区分「命令」和「回复」，A 的 `ack` 到 B 被当未知命令回 `error`，这条 `error` 回到 A 又被回一条 `error`……无限往返 |
| Android 的对话面板每 5 秒被清空一次 | Android 每 5 秒发 `session.list` 当心跳，拿到的快照整体替换对话列表，而 CLI 快照里 `messages` 永远是 `[]` |

结论：中继挂错了层。任务、审批、会话全在 Runtime 里，手机理应直接对着 Runtime。

## 2. 决策

1. **中继归 Runtime Daemon 所有**，TUI 里的进程内中继删除。一台机器一个 Daemon，一个 room，串台问题从结构上消失。
2. **手机是 Runtime 的受限客户端**：读操作看整个 Runtime；写操作只开四件——发提示词、新建会话（只限已登记工作区）、停止当前轮次、批准 / 拒绝审批与回答提问。
3. **所有调用走控制面同一个分发入口**（`control_api::execute`），不另写一套业务逻辑：参数校验、幂等、Drain 闸门、公共投影脱敏全部复用。手机端的每个命令都映射到一个已有的 Runtime 操作，白名单是**字面量表**。
4. **开关是持久状态**：`/mobile` 打开后写入 `mobile-relay.toml` 的 `enabled = true`，Daemon 重启 / `daemon upgrade` 之后自动重连；`/mobile off` 才真正断开。
5. **TUI 的 `/mobile` 变成遥控器**：打开、关闭、出二维码、在侧栏显示状态，本身不再持有任何连接。

## 3. 架构

```text
Android ──wss──▶ j.niuwoai.com/ws/broadcast/<room> ◀──wss── Runtime Daemon
                                                            │
                                    MobileGateway（daemon/mobile_gateway*.rs）
                                     ├─ 命令路由：字面量白名单 → control_api::execute（进程内）
                                     ├─ 事件翻译：EventLog 实时订阅 → message.* / tool.* / session.upsert
                                     └─ 快照：session.list + approval.list + question.list + task.list
                                              + Core Session 历史投影（conversation::project）

TUI /mobile ──本机控制面──▶ mobile.enable / mobile.disable / mobile.status
```

- 连接循环、重连（2 秒）、凭据格式、配对 URL、二维码尺寸约束保持 0.81 的样子（`mobile.rs` 只保留这些共享件）。
- Daemon 不监听新端口，仍然只主动外连中继。

## 4. 生命周期

| 场景 | 行为 |
|---|---|
| `/mobile`（TUI）或 `willdeep daemon mobile enable` | 必要时拉起 Daemon → `mobile.enable`：写 `enabled = true`、启动网关、返回配对 URL；TUI 本地渲染二维码 |
| `/mobile off` 或 `willdeep daemon mobile disable` | `mobile.disable`：写 `enabled = false`、断开 |
| 关掉 TUI | 不影响中继 |
| Daemon 启动 | 读到 `enabled = true` 就自动连上 |
| `daemon upgrade` 的 Drain 期 | 旧进程的网关继续在线：只读和审批照常，`message.send` 被 Drain 闸门拒绝（返回可重试错误）；旧进程退出后新进程接手重连。两个进程不会同时在线 |
| `daemon stop` | 网关随进程退出，`enabled` 保持不变，下次启动自动恢复 |

`mobile.enable` / `mobile.disable` 是设值语义，重复调用结果相同，所以**不进幂等缓存**——幂等缓存会把响应体落进 `idempotency.json`，而 `mobile.enable` 的响应里有配对 URL（含 relay token）。

## 5. 命令白名单

手机端发来的类型只认下表；**不在表里、也不是手机命令的类型（`ack`、`error`、`state.snapshot`、`message.append` 等）一律静默丢弃**——这是修掉回声循环的那一刀。

| 手机命令 | Runtime 操作 | 回给手机 |
|---|---|---|
| `session.list` | `session.list` + `approval.list` + `question.list` + `task.list` + 历史投影 | `state.snapshot` |
| `session.select` | 校验会话存在 | `ack`，随后推一份新快照 |
| `workspace.list` | `workspace.list`（登记表） | `workspace.list` |
| `capabilities.get` | 选中会话的 Profile / 模型（只读展示） | `capabilities.updated` |
| `message.send` | 解析目标会话 →（必要时 `session.create`）→ `turn.submit` | `ack` + 用户消息回显 + 新快照 |
| `session.create` | `session.create`，工作区必须已登记 | `session.upsert` |
| `turn.stop` | `turn.stop`（会话的 `active_turn_id`） | `ack` |
| `tool.decide` | 审批：`approval.resolve`（`allow_once` / `deny`）；提问：`question.answer` | `ack` + `tool.updated` |
| `push.register`、`patch.decide`、`diff.get`、`job.kill`、`file.read`、`queue.update` | — | `error`：`Unsupported mobile command: <type>.`（与 Mac 端逐字一致） |

`message.send` 的目标会话按顺序解析（与 Mac 端语义对齐）：

1. 信封带 `session_id` → 就是它（必须存在且未归档）；
2. 带 `payload.workspace_path` → 若当前选中会话在这个工作区且空闲就复用，否则在该工作区新建一条；
3. 都没带 → 当前选中会话；没有选中会话时在登记表的活跃工作区新建一条。

`turn.submit` 的 `turn_request_id` 取手机信封 `id`（是 UUID 时），手机重发同一条命令命中幂等缓存，不会跑两遍。`origin_client` 记 `mobile:<进程随机 id>`，用量账本已认得这个前缀。

图片：`payload.images` 里的 data URL 解码成 `MessageAttachment::Image`（取宽高、校验 MIME 与大小），走 `turn.submit` 既有的附件上限。

## 6. 推送事件

只在手机「在场」时推送：最近 60 秒内收到过手机命令（Android 每 5 秒一次 `session.list` 心跳）。不在场时不花中继流量；重新在场时先补一份完整快照。

| Runtime 事件 | 推给手机 |
|---|---|
| `task.output` 中的 `assistant_text` | `message.append`（整条）+ `message.done` |
| `task.waiting_approval` / `task.waiting_answer` | `tool.pending` |
| `task.interaction_resolved` / `task.interaction_cancelled` | `tool.updated`（`resolved` / `cancelled`，Android 据此移除卡片） |
| `turn.started` / `turn.completed` / `turn.failed` / `turn.cancelled` / `turn.partial` / `turn.interrupted` | `session.upsert`（刷新 `is_responding`） |
| `session.renamed` / `session.archived` / `session.deleted` / `session.unarchived` | 新快照（2 秒内合并） |

不推 token 级增量（`assistant_text_delta`）：中继是公网往返，一条回复几百帧不划算，整条到达足够。

## 7. 快照内容与上限

| 字段 | 内容 | 上限 |
|---|---|---|
| `sessions` | 未归档会话，按最近更新排序；标题取 Core Session 摘要 | 50 条（选中会话总在其中） |
| `active_session_id` | 手机当前选中的会话（`session.select` / `message.send` / `session.create` 更新） | — |
| `pending_tools` | **整个 Runtime** 的待审批 + 待回答，带 `session_id` | 全部 |
| `messages` | 选中会话的历史投影（`conversation::project`，与 Web 端同一份）+ 进行中轮次的实时尾巴 | 40 条，每条 8000 字符 |

「实时尾巴」：Core Session 文件在轮次结束时才落盘。轮次进行中推过的用户回显和助手消息记在网关内存里，拼在历史投影后面；轮次结束、缓存失效后以文件为准。没有这一段，Android 每 5 秒的快照会把刚推过去的消息冲掉。

历史投影按会话文件的 mtime 缓存，5 秒一次的心跳不会反复解析大文件。

## 8. 安全边界

**影响面变大了**：以前 token 泄露只能碰一条会话，现在能看整个 Runtime、给任何会话发提示词。所以：

1. **白名单是字面量**，不是「`.list` 结尾的都放行」这种命名规则（Xedit 只读客户端踩过的坑）。
2. **手机上做不到的事**：删除 / 归档 / 改名 / 分叉 / 回退会话，登记 / 移除 / 切换工作区，改审批档位，改模型，派生 / 停止 / 重试子 Agent，`AlwaysAllow`（「总是允许」是长期规则，只能在桌面上定）。`message.send` 里的 `approval_mode` / `provider_id` / `model` / `skills` / `experts` / `plugins` 一律忽略。
3. **新建会话只限已登记工作区**：手机不能把 Runtime 的访问范围扩到新目录。
4. **展示内容打码**：审批描述、提问、消息正文过一遍 `judge::redact_credentials`；失败原因沿用事件流的闭集合分类，不下发原文。
5. **中继服务端看得到明文**：这是 0.81 就存在的边界（Mac 端同样如此），这次没有放宽也没有收紧。端到端加密另立项（Xedit `docs/MOBILE_GATEWAY_E2E_ENCRYPTION_DESIGN.md`）。
6. **撤销**：`/mobile off` 断开；删除 `mobile-relay.toml` 后再开，生成新的 room 与 token，旧手机失效。

## 9. 与 TUI 的交互

- **审批被别处先答了**：TUI 在快照里发现某个 gate 消失，就撤回对应的对话框（排队中的直接移除），提示「已在其他端处理」。以前按一下才报 `Runtime interaction is no longer pending`。
- **手机发起的轮次**：审批按「发起端优先」只弹回手机。但人回到电脑前，TUI 打开同一条会话时也应当能答——所以 `mobile:` 发起的 gate 在同会话的 TUI 里也弹。两边都弹是安全的：谁先答算谁的，另一边撤回（TUI 靠上一条，手机靠 `tool.updated`）。
- **侧栏「移动中继」**：显示 Daemon 报的状态——关闭 / 已开启·已连接 / 已开启·重连中、手机在线与否、待处理条数。键盘的「待发队列」挪到「运行状态」里，因为手机消息不再进 TUI 队列，而是直接进 Runtime 的轮次队列（同一会话的轮次本来就严格串行）。
- **`/local` 进程内轮次**：Runtime 不知道它在跑，手机上看不到实时输出；轮次结束落盘后，历史投影里能看到。

## 10. 兼容性

- **Android 无需改动**：信封、事件名、字段名全部沿用 Mac 端口径。
- **Mac 端（Xedit）不受影响**：各自的 room 与 token。
- **旧 CLI 读新凭据文件**：多出的 `enabled` 字段被忽略。
- **新 TUI 对旧 Daemon**：`mobile.*` 回 `unsupported operation`，TUI 提示先 `willdeep daemon upgrade`。
- **协议**：`SUPPORTED_OPERATIONS` 增加 `mobile.status` / `mobile.enable` / `mobile.disable`；新增 `MobileRelayStatus`、`MobileRelayEnabled` 两个公开类型。`public-api-v1.json` fixture 不变。

## 11. 不做（及理由）

| 项 | 理由 |
|---|---|
| `patch.decide` / `diff.get` | rs 没有「补丁提案」这一层，写入走审批，已由 `tool.decide` 覆盖 |
| `file.read` | 手机直接读文件等于绕过工作区边界，不开 |
| `job.kill` / `queue.update` | 后台 Shell 与 TUI 键盘队列都不是 Runtime 对象，没有可复用的操作 |
| `push.register` | 没有推送通道；Android 对它静默降级 |
| token 级流式 | 见第 6 节 |
| 端到端加密 | 见第 8 节第 5 条 |

## 12. 验收

- 单测：命令白名单（每条映射、未知手机命令回 Mac 口径、非命令类型静默丢弃）、`message.send` 目标会话解析三种路径、`tool.decide` 映射（approve/reject/answer，永不 `AlwaysAllow`）、事件翻译、快照上限与实时尾巴去重、`enabled` 持久化与旧凭据兼容、TUI gate 撤回、`mobile:` gate 在同会话 TUI 可见。
- 集成：Daemon 内网关对假中继（本机 WebSocket 服务）跑通「快照 → 发消息 → 审批 → 撤回」。
- `cargo fmt --check`、`cargo clippy --workspace --all-targets -D warnings`、`cargo test --workspace` 全绿。
- 文档：`MOBILE.md`、`RUNTIME_DAEMON.md`、`RUNTIME_CONTROL_API.md`、`TUI_GUIDE.md`、`ARCHITECTURE.md`、`CHANGELOG.md`、`PRODUCT_OVERVIEW.md` 同步；Android 仓库的 Desktop Command Matrix 需要另行更新 CLI 一列。

## 13. 完成记录（2026-09-23，0.82.0-rc1）

已验证：

- `cargo fmt --check`、`cargo clippy --workspace --all-targets -D warnings`、`ruby scripts/check_source_size.rb`、`cargo test --workspace` 全绿（`willdeep` 二进制 540 项 + 集成测试 578 项等）。
- 新增 30 项测试：网关 20 项（含本机假中继端到端：Bearer 鉴权与 room 路径、快照、审批卡推送、手机批准后 `tool.updated`、关中继断开），凭据开关 4 项，TUI 4 项，审批归属与协议 `Debug` 遮 token 各 1 项。
- 真实 daemon 冒烟（临时 `WILLDEEP_HOME`，中继指向本机不可达端口）：`daemon mobile enable` 按需拉起 Runtime 并打开中继；`daemon stop` / `start` 后自动恢复为开启；`disable` 落盘关闭；Runtime 不在跑时 `status` / `disable` 不会拉起它，也不会凭空生成凭据。

未验证：

- 真机 Android 经公网中继 `j.niuwoai.com` 的联调。信封字段对照了 Android `MobileGatewayModels.kt` 的解析与 macOS `AgentMobileGatewayBridge.swift` 的输出，但没有在手机上跑过。
- 实施中改动的点：`judge::redact_credentials` 会把换行压成空格，投影层另写了保留排版的包装（它对每个词恰好产出一个词，按原文空白放回去；词数对不上就退回压扁的版本）。
- 基线 rc9 上 `cargo fmt --check` 本身不过（rc8 引入的一处），随本版一并按 rustfmt 重排。
