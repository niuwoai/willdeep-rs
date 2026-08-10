# Changelog

## [0.21.0-rc14] - 2026-08-11

### Added

- Rust Runtime Client 新增 Tool 与 Artifact 的列表、单项类型化查询方法，调用方不再手写操作名和返回 DTO。
- 新增 Unix Socket 真实往返测试，验证类型化 Tool 查询携带 Runtime Token、稳定过滤参数并正确解码公共 DTO。

## [0.21.0-rc13] - 2026-08-11

### Added

- Web 新增 `/api/runtime/activity`，仅允许查询启动时白名单中的 Workspace，并返回统一 Runtime Tool/Artifact DTO。
- React 侧栏每两秒刷新工具总数、运行数、产物数和最近工具状态，新增完整中英日文案。

### Security

- Web Activity 的 Workspace 选择通过 Runtime 注册表与 Web 启动白名单双重校验；浏览器不能借查询参数枚举其他本机 Workspace。

## [0.21.0-rc12] - 2026-08-11

### Added

- 协议 crate 新增稳定 `RuntimeArtifact`、`ArtifactKind`、`ListArtifactsParams` DTO，以及 `artifact.list`、`artifact.get` 操作。
- Runtime 将工具窗口内由内容指纹确认的 Diff Attribution 映射为 Workspace Change Artifact，绑定 Session、Turn、Task、Agent、来源快照与变更项数量。

### Changed

- TUI Runtime 快照直接查询结构化 Tool/Artifact，右栏展示当前工作区的工具总数、运行数、产物数和最近工具状态，不再只依赖聊天事件聚合。

### Security

- Artifact 元数据不暴露 Workspace 路径、变化文件名或内容；调用方必须使用受 Workspace 授权和精确快照校验的 Diff API 获取具体内容。

## [0.21.0-rc11] - 2026-08-11

### Added

- 协议 crate 新增稳定 `RuntimeTool`、`ToolStatus`、`ListToolsParams` DTO，以及 `tool.list`、`tool.get` 操作。
- Runtime 新增有界持久 Tool Activity 索引，记录主/子 Agent 工具的 Session、Turn、Task、Agent 归属和毫秒级生命周期；重启时运行中记录收敛为 Interrupted。

### Security

- Tool Activity 不持久化也不返回工具参数、输出正文、Workspace 路径或内部错误；工具名有长度上限，列表数量受服务端限制。

## [0.21.0-rc10] - 2026-08-11

### Added

- Core Session 以向后兼容字段持久化私有配置引用，供 Runtime 在收养现有 Session 时自行恢复。
- 协议 crate 新增严格拒绝未知字段的通用 `IdParams`，供 Session 与 Turn 查询/控制操作复用。

### Changed

- `daemon sessions/session/search/rename/fork/archive/export/delete` 与 Turn 提交、列表、查询、停止全部改走共享 Runtime Client，自动使用 Unix Socket 或 Windows Named Pipe。
- TUI 与 Web 收养 Session 改用统一 `session.create`，客户端不再重复发送配置文件路径。

### Security

- `CreateSessionParams` 明确拒绝 `config` 等未知字段；收养操作只接收稳定 Session ID，并以 Runtime 已持有的 Core Session 配置为准，不信任客户端提供的路径。

## [0.21.0-rc9] - 2026-08-11

### Added

- Runtime Daemon 在 Unix 平台监听权限为 `0600` 的本地 Socket，在 Windows 平台监听拒绝远程客户端的随机 Named Pipe；共享 Runtime Client 支持两种本地传输。

### Changed

- CLI、TUI 与 Web 的 Runtime 控制客户端优先读取 daemon 状态中的本地端点，旧版状态仍自动回退到受 Token 保护的回环 TCP。
- 能力协商只报告当前进程实际启用的本地传输；关闭 Daemon 时安全清理自有 Unix Socket。

### Fixed

- `daemon stop` 正确接受关闭端点返回的 HTTP 202 空响应，不再在 Daemon 已退出后误报 JSON 解码失败。

### Security

- Unix Socket 启动时拒绝覆盖同名普通文件，仅在确认目标仍为 Socket 后清理；Windows Named Pipe 禁止远程客户端连接，所有传输继续要求随机 Runtime Token。

## [0.21.0-rc8] - 2026-08-11

### Added

- 协议 crate 新增 Session 搜索结果、文本/图片附件、Turn 提交与列表参数 DTO，以及 `session.search`、`turn.list` 稳定操作。
- 统一 API 实现 Session 组合搜索和 Turn 提交、列表、查询与停止。

### Changed

- TUI Session 搜索和 TUI/Web Turn 提交、停止改走共享 Runtime Client；Turn 提交同时使用外层 Request ID 与会话内 Turn Request ID 去重。

### Security

- 统一 Turn 提交在 Runtime 边界限制 1 MiB Prompt、12 个附件、文本字符数、图片 MIME/尺寸和 10 MiB 总载荷；参数严格拒绝 Workspace、权限或其他未知控制字段。

## [0.21.0-rc7] - 2026-08-11

### Added

- 统一 API 实现 `session.create/rename/fork/archive/delete/export`；协议 crate 新增创建、重命名、Fork、归档和精确确认删除参数 DTO。

### Changed

- Web Session 列表、重命名、指定 Turn Fork、归档/恢复、删除和导出改走共享 Runtime Client；Session 修改操作纳入持久 Request ID 幂等。

### Security

- Web 在调用统一 Session 管理前仍校验启动时 Workspace 白名单；公开 Session 响应不包含配置文件路径、排队 Prompt、附件或内部错误，删除同时要求目标 ID 与 confirmation 完全一致。

## [0.21.0-rc6] - 2026-08-11

### Added

- 协议 crate 新增 Diff Snapshot、File Content、Review、Verification、Attribution、Commit Preview 与 Revert 稳定 DTO，并公开完整的 `diff.*` 操作集合。

### Changed

- TUI Diff Review Center 的快照、内容、审查、验证、归因、提交预览和撤销全部改走共享 Runtime Client，不再手写旧 Diff HTTP 请求。
- `diff.review`、`diff.verification.record` 和 `diff.revert` 纳入持久 Request ID 幂等，陈旧 Snapshot 使用稳定 `stale_snapshot` 错误码。

### Security

- 统一 Diff API 复用 Runtime Workspace 授权、精确 Snapshot 校验、审查备注/验证摘要上限、敏感验证命令拒绝与 Recovery 保守撤销；客户端不能通过新增参数强制覆盖冲突。

## [0.21.0-rc5] - 2026-08-11

### Added

- 协议 crate 新增稳定 Runtime Workspace、Workspace Access 和注册参数 DTO；统一 API 补齐 `workspace.register/ensure/activate/remove`。

### Changed

- `workspace.list` 返回公开 DTO；配置注册、自动确保、激活和移除均进入统一响应信封与 Request ID 持久幂等。
- TUI `/workspace` 与 Web 启动/Workspace 列表使用的 bridge 改走共享 Runtime Client，不再直接调用旧 Workspace HTTP 端点。

### Security

- Workspace 根目录继续由 Runtime 规范化并验证为真实目录；访问模式、Provider、Skills 与 MCP 允许列表只由服务端注册表持久化，任务提交时客户端字段不能扩大权限。
- 公共 Workspace 事件只记录稳定 Workspace ID，不再把根路径写入新事件；公开 DTO 的路径字段保持可选，为后续远程权限裁剪保留语义。

## [0.21.0-rc4] - 2026-08-11

### Added

- 协议 crate 新增稳定 Runtime Task、Pending Approval 和 Pending Question DTO，以及 `task.list/get/cancel`、`question.list` 统一操作。
- 修改类统一请求新增私有持久幂等日志：执行前记录 Pending，完成后记录响应；Runtime 重启后相同 Request ID 返回原响应，崩溃窗口中的不确定请求拒绝自动重放。

### Changed

- TUI Attention Inbox 的 Task、审批和问题列表改走共享 Runtime Client；取消后台 Task 改用幂等 `task.cancel`。
- Web/TUI 的事件补读改用统一 `event.list`，不再读取旧原始事件端点；实时 TUI 继续使用共享 NDJSON Client。
- 审批和问题解决响应只返回 ID、Task ID、Resolved 状态与时间，不回显审批描述、回答正文或内部 Interaction 存储字段。

### Security

- 公开 Task DTO 不包含 PID 和内部错误；Pending DTO 不包含 Resolution、回答正文或解决时间。
- 幂等文件仅保存参数指纹和脱敏响应，不保存原始 Prompt、Agent 指令或用户回答；Pending 日志落盘失败时不会执行修改操作。

## [0.21.0-rc3] - 2026-08-11

### Added

- 协议 crate 新增稳定 `RuntimeSession`、`RuntimeTurn`、`RuntimeAgent`、`RuntimeEvent` 及对应状态枚举；公开 DTO 不包含配置文件、队列 Prompt、附件、消息边界或内部错误字段。
- Runtime Client 新增跨 Chunk NDJSON UTF-8/多信封解码测试，覆盖流式网络切包边界。

### Changed

- 统一 API 的 Session/Turn/Agent 查询与 Agent Wait 改为公开 DTO；Agent 列表不返回报告，只有显式 `agent.get`/`agent.wait` 返回有界报告详情。
- TUI 实时事件改用共享 Runtime Client 的 NDJSON 流；Agent 列表、补充 Prompt、停止、重试、审批和问题回答也改走统一 API，旧 SSE 仅保留服务端兼容端点。
- `agent.stop` 与 `agent.retry` 加入统一调度和 Request ID 幂等范围。

### Security

- 新写入的 Tool 事件不再持久化工具参数和输出，子 Agent 事件不再持久化报告及 Workspace 路径。
- 统一 `event.list` 和 NDJSON 流会在读取边界净化旧日志中的 Tool 参数/输出、Agent 报告、Workspace 路径及内部错误，保留工具名称、状态和 AI 正式回复。

## [0.21.0-rc2] - 2026-08-11

### Added

- 新增受 Token 保护的 `POST /v1/api` 统一 JSON 控制入口，以及 `willdeep api <operation> --params-file <JSON|->`；Session、Agent、Turn、Approval、Question 与 Event 的首批操作使用协议 crate 定义的请求/响应信封。
- 新增可续传 `GET /v1/events/stream.ndjson`，每行均为完整统一事件信封；CLI `willdeep api event.stream --ndjson` 与共享 Rust Client 可按 `after` 游标消费长连接。
- 新增 `willdeep-runtime-client` crate，集中处理回环端点校验、Runtime Token、能力协商、统一调用及有界 NDJSON 解码。
- TUI Prompt 新增 `/webapp` 与 `/webapp status`；前者在当前 Workspace 启动内嵌 Web App，候选列表同步提供命令说明。

### Changed

- CLI 的统一 API、能力查询与 NDJSON 事件消费改用共享 Rust Client，不再各自拼装 HTTP 请求。
- 修改类统一请求按 Request ID 做进程内有界幂等去重；同一 ID 携带不同参数会返回冲突。
- Web App 启动不再强制进入 Provider 首次设置；无 Provider 时仍可查看本地 Runtime 状态，真正提交模型任务时再返回配置错误。

### Security

- 幂等缓存只保留请求参数指纹，不额外保存 Prompt 或回答正文；内部错误对客户端统一脱敏，详细上下文只进入 Runtime 本地日志。
- `/webapp` 只允许回环监听地址；远程访问仍须由反向代理、VPN 或 SSH Tunnel 显式提供认证与暴露边界。

## [0.21.0-rc1] - 2026-08-11

### Added

- 新增独立 `willdeep-runtime-protocol` crate，定义协议版本、11 类控制对象、稳定 namespaced operation、能力、传输类型、协议限制和统一成功/错误信封。
- Runtime 新增受 Token 保护的 `GET /v1/capabilities`，CLI 新增 `daemon capabilities`；响应包含服务端/最低客户端协议版本、服务版本和可选请求 ID。

### Changed

- 阶段 7 从共享协议 crate 开始演进；现有 `/v1/*` 原始响应暂时保持兼容，后续客户端迁移不需要复制协议常量和错误语义。

### Security

- 能力文档与响应只描述公开协议元数据，不包含 Runtime Token、Provider Key、Prompt、工具参数、文件正文或用户路径；端点显式执行本地 Runtime Token 校验。

## [0.20.0-rc6] - 2026-08-11

### Added

- 新增受 Token 保护的 Worktree 审计 API/CLI `worktrees-audit`，区分 Active、Reviewable、Merged、Clean、Quarantined、Missing 和无 Agent 记录的 Unknown 状态。
- 新增 `quarantine-agent-worktree --snapshot <ID> --yes`：把符合条件的完整 Git Worktree 移入 Runtime Recovery，并持久记录新路径和隔离时间。

### Changed

- 保守清理不调用 `git worktree remove`、不删除文件或分支，改用 `git worktree move` 保留完整目录与 Git 关联；状态持久化失败时尝试移回原路径。

### Security

- Worktree Review、合并、审计与隔离 Handler 均显式校验 Runtime Token；隔离还要求终态 Agent、WillDeep 托管直系目录、精确 Child 快照、显式 `--yes`，且仅允许干净或已精确合并且无冲突/未跟踪内容的 Worktree。

## [0.20.0-rc5] - 2026-08-11

### Added

- 子 Agent 成功报告附带最多 16 KiB 的 Worktree 路径、分支和 `git status --short` 状态，可靠回流主 Harness。
- 新增受 Token 保护的 Agent Worktree Review/合并 API、CLI `agent-worktree-review` 与 `merge-agent-worktree --review <ID> --yes`，以及 TUI Agent 详情 `W` 审查、`M` 批准合并。

### Changed

- Review ID 精确绑定 Agent、Child Diff 快照、Root Diff 快照和二进制补丁；任一侧变化后旧 Review 自动失效，合并只应用工作区补丁，不自动 Commit、Push 或清理 Worktree。

### Security

- 合并前执行 `git apply --check`，同文件并发修改、Child 未解决冲突、未跟踪文件、不可表示变更和超过 2 MiB 的补丁全部阻断；CLI 必须同时提供精确 Review ID 和 `--yes`，TUI 必须先打开 Review 再显式按 `M`。

## [0.20.0-rc4] - 2026-08-11

### Added

- 内置 Editor 子 Agent 默认创建 `willdeep/agent-<id>` 专属 Git Worktree；TOML Profile 可用 `worktree = "shared" | "dedicated"` 覆盖策略。
- 子 Agent 生命周期与 Runtime Agent Store 新增实际 Workspace、根 Workspace、Worktree 分支和隔离标记。

### Changed

- Editor 已审批目标按根 Workspace 相对路径映射到专属 Worktree；Child 写工具的 Diff Attribution 在自己的 Worktree 内采集，不再误归属父工作区。

### Security

- 专属 Worktree 仅能从包含当前 Workspace 的 Git 仓库创建，使用不可预测 Agent UUID 路径；任务结束时保留分支与目录供审查，不自动删除或合并用户改动。

## [0.20.0-rc3] - 2026-08-11

### Added

- Web Workspace API/选择器返回 Runtime 注册表中的稳定 ID、名称、active 状态和 `read-only/smart/workspace-write` 模式；Composer Skills 使用 Workspace 允许列表。

### Changed

- Web 可见 Workspace 是 Runtime 注册项与 `--workspace/--web-workspace` 启动白名单的交集；新 Web Session 优先使用 Workspace 默认 Provider。
- Coding Workspace 自动注册默认 `workspace-write`，即目录内文件写入免审；Shell、MCP、网络与越界访问继续使用现有审批状态机，`read-only` 仅显式启用。

### Security

- 新增原子 Workspace `ensure` API：只在目录尚未注册时创建默认项，绝不覆盖已有访问策略、Provider、Skills 或 MCP 设置；浏览器请求不能扩大服务启动时的路径上界。

## [0.20.0-rc2] - 2026-08-11

### Added

- TUI 新增 `/workspace list` 与 `/workspace switch <id>`，并加入 `/` 命令候选；可从 Runtime 注册表查看和切换工作区。

### Changed

- TUI 切换 Workspace 时保存当前 Session 游标与 Inbox 状态，恢复或创建目标 Session，重启 Runtime 事件跟随并刷新状态、Skills 与移动会话；旧 Workspace 后台任务继续运行。

### Security

- 跨 Workspace 切换后禁用仍绑定启动目录的进程内 `/local` Harness，避免旧工具边界被用于新路径；Runtime 模式继续以服务端注册表重建边界。

## [0.20.0-rc1] - 2026-08-11

### Added

- Runtime 新增持久 Workspace 注册表与受 Token 保护的注册、更新、列表、激活、移除 API；CLI 增加 `register-workspace`、`workspaces`、`activate-workspace` 和 `remove-workspace`。
- 每个 Workspace 独立保存规范化根目录、访问策略、默认 Provider Profile、Skill 与 MCP 允许列表；任务和 Session 首次使用目录时自动注册。

### Changed

- 激活 Workspace 只改变默认项，已入队 Task 和既有 Session 继续绑定原根目录；移除注册不会删除工作区文件、Session 或历史。

### Security

- Workspace 策略字段不接受客户端反序列化，并由 Runtime 在任务入队时强制覆盖；只读策略会在审批前拒绝 Shell、文件写入、Worktree 创建、MCP 调用和 Editor 子 Agent。
- Workspace 注册表使用 Runtime 私有原子写入机制，不存储 Provider Key、Prompt 或文件正文。

## [0.19.0-rc8] - 2026-08-10

### Fixed

- TUI Inbox 不再长期显示已完成或已取消的 Runtime 任务，完成超过 5 分钟后自动移出“最近完成”。
- Runtime Gate 增加 Task ID 关联；点击等待审批/回答的任务或在详情中按 Enter，会直接打开实际审批/回答控件，而非停留在不可操作的任务详情。

## [0.19.0-rc7] - 2026-08-10

### Added

- Runtime 在主 Agent 与子 Agent 的潜在写工具调用前后采集工作区内容指纹，持久绑定 Session、Turn、Task、Agent、Tool 和真实变化路径。
- 新增受 Token 保护的 Diff Attribution API、CLI `daemon diff-attributions`，TUI Diff Review 显示每个文件最近的责任 Agent 与工具，并沿连续快照链保留多次工具变更。

### Fixed

- 归属计算只比较工具窗口前后发生变化的路径，不再把调用开始前已经存在且未变化的脏文件误算给当前 Agent。

### Security

- Diff Attribution 仅持久化内容指纹、结构化 ID、工具名和相对路径，不持久化文件正文、工具参数或凭据。

## [0.19.0-rc6] - 2026-08-10

### Added

- Runtime 新增精确快照 Commit Preview API；CLI `daemon diff-commit-preview` 与 TUI Diff Review 的 `P` 面板展示提交消息、分支、暂存/未暂存文件、Remote、推送目标和可选 Tag。
- Commit Preview 在敏感文件或疑似凭据、冲突、空暂存区、Detached HEAD、缺失 Remote 和无效 Tag 时给出结构化阻断原因，并且不执行任何 Git 写操作。

### Fixed

- 增加 Runtime 聊天纯净度回归测试，锁定对话区只展示用户输入和 AI 最终回复；轮次、Task/Agent ID 和工具活动仅保留在状态层。

### Security

- Remote URL 在展示前移除用户信息、查询参数和片段；敏感检查只返回文件路径与规则代码，不回传或记录匹配到的凭据内容。

## [0.19.0-rc5] - 2026-08-10

### Added

- Core `run_command` 新增结构化验证报告器，自动识别常见前后台测试命令并上报命令、退出码、Passed/Failed/TimedOut/LaunchFailed 与失败摘要。
- Runtime 新增精确 Diff Snapshot Verification 持久化 API；CLI `daemon diff-verifications` 和 TUI Diff Review 展示最近验证结果。

### Security

- 验证摘要限制为 8 KiB、40 行且保持 UTF-8 边界；普通 Shell 不记录，命令出现 API Key、Token、Secret、Password 或 Authorization 标记时拒绝持久化。

## [0.19.0-rc4] - 2026-08-10

### Fixed

- 修复 TUI/Web 隐式启动 Runtime Daemon 时，启动状态直接写入终端 stdout 并穿透 Ratatui、显示在 Prompt 区域的问题；只有显式 `daemon start` 保留控制台提示。
- TUI Runtime 提交确认改为底部短暂状态，不再向聊天区写入 Turn/Agent ID；AI 完成消息移除 Runtime ID 前缀，只展示模型真实返回内容。

## [0.19.0-rc3] - 2026-08-10

### Added

- Runtime 新增持久 Diff Review 记录，支持 Accepted、Rejected、Changes Requested、Reviewed；CLI 新增 `daemon diff-review`，TUI 使用 A/D/C/M 保存并显示文件审查状态。
- 新增 `POST /v1/diffs/{id}/revert`、CLI `daemon diff-revert` 与 TUI `R` 二次确认，可按 Combined/Staged/Unstaged 安全撤销单文件。

### Security

- 审查和撤销必须匹配当前 Workspace 的精确内容快照；文件在打开后发生任何变化都会以冲突拒绝操作。
- 未跟踪文件及 HEAD 中不存在的新增内容不会直接删除，而会原子移动到 `runtime/recovery` 可恢复目录；冲突文件拒绝自动撤销。

## [0.19.0-rc2] - 2026-08-10

### Added

- TUI Diff Review 新增 Unified/Side-by-side 双视图；并排模式将相邻删除与新增行配对，按 Unicode 显示宽度截断和补齐双栏。
- 新增 Combined/Staged/Unstaged 范围循环切换，以及当前文件增量搜索、匹配高亮和前后跳转。

### Changed

- 将 Diff Review 状态/渲染与聊天 Markdown 渲染拆分为独立 TUI 模块，主文件降至 3000 行以内。

## [0.19.0-rc1] - 2026-08-10

### Added

- 新增 Workspace Diff 快照模型，记录 HEAD、文件新增/修改/删除/重命名/冲突/未跟踪状态、暂存与未暂存范围、二进制标记、增删统计及内容指纹。
- Runtime 新增受 Token 保护的 `GET /v1/diffs` 和 `GET /v1/diffs/{id}/content`；CLI 提供 `daemon diff-snapshot`、`daemon diff-file`，TUI `/diff` 提供文件导航、滚动和 Unified Diff 着色。

### Security

- Diff API 只允许访问已注册 Session/Task 的规范化 Workspace；文件内容请求必须匹配当前快照 ID，变更后返回冲突，且拒绝绝对路径、父目录穿越与符号链接逃逸。
- 单文件 Diff 输出限制为 512 KiB，并在 UTF-8 边界安全截断；Runtime API 不暴露 Provider 凭据或未请求的文件正文。

## [0.18.0-rc3] - 2026-08-10

### Added

- 子 Agent 完成事件携带最多 64 KiB 的成功报告或失败原因，Runtime Agent Store 持久化结果；CLI 单 Agent 详情和 TUI Enter 详情层可查看状态、策略、实时用量与报告。
- 新增 `willdeep daemon instruct-agent <AGENT_ID> <INSTRUCTION>`、受 Token 保护的 `POST /v1/agents/{id}/instructions`，以及 TUI `/agent instruct <AGENT_ID> <INSTRUCTION>`。
- Core 新增子 Agent 指令收件箱，在下一次模型请求前注入父级补充指令；补充指令与模型结束发生竞态时继续下一轮，不丢指令。

### Changed

- CLI Agent 列表显示当前/最大轮次、已用/预算 Token 与执行时限；单 Agent 查询额外打印策略和最终报告。

### Security

- 追加指令限制为 1–16384 字节且只允许发送给当前 Harness 内运行中的后台子 Agent；命令应用、拒绝或 Runtime 重启后立即清除正文，事件审计不记录正文。
- Agent 报告采用 UTF-8 边界安全截断，并只通过本地受 Token Runtime API 暴露。

## [0.18.0-rc2] - 2026-08-10

### Added

- 子 Agent Profile 新增 `token_budget`、`timeout_seconds` 和 `max_consecutive_failures` TOML 策略；示例配置给出保守默认值和环境变量凭据用法。
- 核心 Agent Loop 累计 Provider 用量，在达到 Token 总预算时以结构化错误终止；子 Agent 执行支持硬超时。
- Runtime Agent 持久化启动时的最大轮次、Token 预算和执行时限，TUI 右栏同时展示实时用量与策略上限。

### Changed

- 同一子 Agent Profile 连续失败达到阈值后开启进程内熔断，拒绝继续启动；任一成功执行会自动复位该 Profile 的失败计数。

### Security

- 预算、时限与熔断均在 Core/Runtime 执行，不依赖客户端自觉；策略事件不包含 Prompt、工具参数、Provider 凭据或文件内容。

## [0.18.0-rc1] - 2026-08-10

### Added

- 新增 `docs/HERDR_RESEARCH_AND_INTEGRATION.md`，基于 Herdr 官方资料记录状态权威、Snapshot+事件、状态聚合、Socket 控制面、许可证边界和分阶段集成方案。
- 新增 `willdeep integrations herdr status [--json]`，检查 Herdr CLI、Pane 环境、Socket 配置和生命周期上报就绪状态，不输出 Socket 路径。
- Runtime 在 Herdr Pane 环境内将全部 Task 聚合为 `working/blocked/idle`，通过公开 `herdr pane report-agent` CLI 非阻塞上报并去重相同状态。

### Security

- Herdr 仅为可选进程级适配器，不成为 Runtime 状态来源；未安装、版本不兼容或上报失败均不影响 Harness。
- 上报命令使用结构化进程参数，不经过 Shell，不传输 Prompt、API Key、工具参数、文件内容或 Socket 路径。
- 研究文档明确 Herdr 当前 AGPL/商业双许可证边界；WillDeep 不复制或链接 Herdr 源码。

## [0.17.0-rc8] - 2026-08-10

### Added

- Runtime Session 持久化可选模型覆盖；创建 Session、独立 Runtime 任务及 Session Turn 均把模型传入原生 Harness。
- CLI `fork-session` 新增 `--provider-profile`、`--model`，TUI `/session fork` 支持 `--profile`、`--model` 和 `--through`，Web Fork API 支持相同结构化字段。
- Session Search 支持组合 Workspace、状态、Provider Profile、模型及更新时间上下界；CLI 与 TUI 均可组合文本和结构化过滤条件。

### Changed

- CLI Session 列表与搜索结果显示实际 Provider Profile 和模型；Fork 的 Provider 覆盖同步写入 Core Session Profile，Runtime 仍是模型覆盖的事实来源。

### Security

- Provider Profile 和模型覆盖统一去除首尾空白、拒绝空值并限制长度；搜索没有文本时必须至少提供一个结构化过滤条件。

## [0.17.0-rc7] - 2026-08-10

### Added

- Runtime Turn 持久记录 Core 消息起止边界；CLI `fork-session --through-turn`、Runtime/Web Fork 请求和 TUI `/session fork-turn` 支持精确保留到指定已完成 Turn。
- TUI 新增 `/session switch <SESSION_ID>`，可在同一 Workspace 内不退出进程切换并恢复聊天会话。

### Changed

- TUI Runtime 事件跟随器将可见任务关联到 Session，聊天区只渲染当前 Session 的模型、工具和子 Agent 输出。

### Security

- 指定 Turn Fork 不根据消息角色或文本猜测边界；旧 Turn 缺少持久边界、Turn 未完成或不属于源 Session 时一律拒绝。
- TUI 暂不允许跨 Workspace 原地切换，避免复用旧 Workspace 的 Agent、Skills 和安全边界。

## [0.17.0-rc6] - 2026-08-10

### Added

- Runtime Session 新增 Rename、完整消息快照 Fork、Archive/Unarchive、Delete、JSON Export 和标题/消息 Search 的受 Token API 与 CLI。
- TUI 新增 `/session rename|fork|archive|unarchive|search|export|delete`，命令面板可发现；Web 会话侧栏新增本地标题筛选以及重命名、分叉、归档、导出和删除操作。
- Session 生命周期操作发布 `session.renamed/forked/archived/unarchived/deleted` 持久事件。

### Changed

- 将 TUI Session 命令和命令候选拆分为独立模块，使主 TUI 文件重新低于 3000 行上限。

### Security

- Rename、Fork、Archive 和 Delete 拒绝活跃或仍有排队 Turn 的 Session，避免与 Harness 消息写入竞争；Delete 要求精确 ID 二次确认，TUI 禁止删除当前打开会话。
- Fork 不复制 Turn、Task、Interaction、事件游标或 Inbox 已读状态；Export 不包含队列私密 Prompt、附件副本、Runtime Token、Provider 凭据和内部游标。
- Web Session 操作只允许作用于配置的 Workspace；未认证 Web 不新增消息摘要全文搜索端点，全文搜索保留在受 Runtime Token 保护的 CLI/TUI。

## [0.17.0-rc5] - 2026-08-10

### Fixed

- 修复 Runtime 在等待审批或 `ask_user` 时重启，只取消 Interaction 却让 Task 永久停留在 Waiting 状态的问题；所有遗留 Running、Cancelling 和 Waiting Task 现在统一收敛为 Interrupted。
- 修复 `/v1/shutdown` 先等待 HTTP Handler 退出、后取消 Harness，导致 Pending Interaction 与优雅停止互相等待的问题；现在先异步取消并收敛 Harness，再关闭 Server。
- 修复 Daemon 异常退出后首次 `start` 等待时间短于陈旧锁期限、必然无法接管的问题；单实例锁现在由活 Daemon 定期续租，启动方会持续等待并安全接管过期租约。
- 清除恢复任务中的遗留 PID，并为恢复的 Task、Turn 和 Pending Interaction 补写持久事件，使 TUI/Web 从旧游标重连后能观察中断与取消状态。

## [0.17.0-rc4] - 2026-08-10

### Added

- 新增 CLI/Runtime 共用 Harness Factory 和非交互执行入口，统一 Provider、视觉降级、审批、Skills、MCP、Tools、子 Agent Profile、会话写入与后台结果回流。
- 新增 Runtime 原生事件 Sink，Agent 事件直接写入持久 EventLog 并同步 AgentStore。

### Changed

- Runtime TaskManager 改为在 Daemon 内调度 Harness Future，不再为每个 Turn 启动 `willdeep --web-input-json` 子进程或转写 stdout/stderr。
- 审批、`ask_user`、取消和后台 Agent 控制继续使用原持久 Runtime 状态机；其他 CLI 客户端解决交互后，同一个 Harness Future 从等待点恢复。

### Fixed

- Runtime 取消现在直接撤销完整 Harness Future，并通过唯一收尾路径提交终态，避免子进程退出与 Turn 状态竞争。

### Security

- Runtime Token 不再通过每 Turn 私有 stdin 发送给子进程，只保留在 Daemon 和 Harness 内存中，且不会传入 Shell 或 MCP 环境。

### Documentation

- 定义并完成 Daemon 内原生 Harness 的共享 Factory、Invocation、Event Sink、审批、取消、生命周期、迁移步骤和可验证验收条件。

## [0.17.0-rc3] - 2026-08-10

### Added

- Web 新增持久 Session 历史读取和真实 Runtime Turn 停止 API；SSE 提交事件返回稳定 Session、Turn 和 Root Agent ID。
- 路线图加入覆盖 Runtime、Web/TUI、审批、多模态、后台任务、Agent Team、Workspace、远程、Computer Use、CLI、Skills、质量和 Swift 替换的完整逐项交付清单。

### Changed

- Web 聊天改为创建或收养 Core Session 并提交统一 Runtime Turn，转发持久 Runtime 事件；浏览器断开只分离观察者，后台任务继续执行。
- Web 历史会话从 Core Session 恢复用户与助手消息，完成后刷新会话列表；API 响应统一暴露 `X-App-Version`。

### Fixed

- Web 停止按钮即使在 Turn ID 返回前被点击，也会等待提交完成后取消真实 Turn，避免只中断浏览器而留下后台任务继续运行。
- 修复 Runtime 在 Turn 已被调度器领取但尚未绑定 Task 时取消，仍可能启动 Harness 并把 Cancelled 覆盖成 Completed 的竞态；Task 绑定现在会原子复核 Turn 与 Session 所有权。

## [0.17.0-rc2] - 2026-08-10

### Added

- TUI 新增 `/local <任务>`，可显式让单轮任务使用原进程内 Harness，作为 Runtime 迁移期兼容入口。

### Changed

- TUI 普通 Prompt 默认进入当前长期 Runtime Session 的持久 Turn；`/runtime` 保留为明确别名，并与普通输入复用同一提交状态机。
- 命令候选、命令面板和帮助文案明确展示默认 Runtime 与 `/local` 行为。

### Fixed

- Runtime 托管 Session 收到中间事件时先重载 Core 历史，仅合并游标与已读状态，防止 TUI 内存旧快照覆盖后台 Harness 刚保存的消息。

## [0.17.0-rc1] - 2026-08-10

### Added

- Runtime 可幂等收养现有 Core Session，保持 Session ID、历史消息、Workspace 和稳定 Root Agent 身份不变。

### Changed

- TUI `/runtime` 改为向当前长期 Runtime Session 提交 Turn，不再为每轮创建彼此孤立的一次性 Runtime Task。
- Runtime 托管会话由 Harness 独占写入消息历史；TUI 立即显示用户输入，并在 Turn 终态从 Core Session 同步完整历史，避免重复消息或覆盖后台上下文。
- TUI 在调度 Turn 前持久化 Runtime 托管标记和事件游标，避免极快 Provider 完成后被客户端旧会话快照覆盖。

## [0.16.0-rc10] - 2026-08-10

### Added

- 定义稳定 Runtime Session / Root Agent / Turn / Execution Task 身份、状态、幂等、恢复与安全协议。
- Runtime 新增持久 Session/Turn 元数据、稳定 Root Agent ID，以及受 Token 保护的 Session/Turn 创建、列表、详情和停止 API。
- CLI 新增 `daemon create-session/sessions/session/submit-turn/turns/turn/stop-turn`，支持从脚本管理长期 Runtime 会话。
- Turn 使用客户端 `request_id` 幂等入队，同一 Session 按持久序号严格串行；成功执行后清除队列中的 Prompt 和附件副本。
- Runtime 为 Turn 发布 queued/started/completed/failed/cancelled/interrupted 事件，并在审批或提问时同步等待状态。

### Changed

- Runtime 启动时将遗留活动 Session/Turn 明确恢复为 Interrupted，自动恢复未开始的排队 Turn，同时保持 Core Session 消息文件为唯一历史来源。
- 排队 Turn 可在 Task 绑定前安全取消并释放 Session，避免领取/启动竞争窗口阻塞后续队列。

## [0.16.0-rc9] - 2026-08-10

### Added

- Runtime 新增受 Token 保护的 `GET /v1/events/stream` SSE 事件流；连接时从指定游标分页补齐持久事件，随后实时推送新事件。
- SSE 客户端落后广播窗口时自动从 NDJSON 日志继续追赶，并使用单调序号消除历史读取与实时广播交界处的重复事件。
- TUI Runtime Bridge 使用可取消的长连接 follower 实时消费事件；连接旧版 Daemon 时自动回退到原轮询接口。

### Changed

- EventLog 与 SSE 实现拆入独立模块，Agent 控制 HTTP/CLI 处理器迁入 `agent_control`，Daemon 主文件恢复到 2000 行以内。

### Security

- SSE 端点沿用 Runtime 随机本地 Token 鉴权；TUI 仍按当前 Workspace 查询任务归属，不向会话展示其他 Workspace 的事件内容。

## [0.16.0-rc8] - 2026-08-10

### Added

- 后台任务保存稳定 Child Agent UUID，并支持按 Agent UUID 精确取消或重试；重试产生新的后台任务 ID，但保持同一 Agent 身份。
- Runtime 新增受 Token 保护的持久 Agent 命令队列和 stop/retry API；原 Harness 轮询执行命令并回写 applied/rejected 结果。
- CLI 新增 `daemon stop-agent` 与 `daemon retry-agent`；TUI Runtime 区支持上下选择 Agent，并用 `K/R` 请求停止或重试。

### Changed

- 后台子 Agent 的 Running、Completed、Failed、Blocked 与 Cancelled 状态统一由后台任务生命周期驱动，重试时清理旧轮次、工具、Token 和错误状态。
- TUI 侧栏交互方法继续拆入独立模块，主文件保持在 3000 行以内。

### Security

- Agent 控制 API、Harness 拉取和结果确认均要求 Runtime 本地随机 Token；命令仅能作用于所属 Runtime Task 的后台 Child Agent。

## [0.16.0-rc7] - 2026-08-10

### Added

- Core 新增带稳定 UUID 的子 Agent 启动、轮次、工具、用量和完成事件；前台及正常完成的后台 `spawn_agent` 均上报 Profile、Label 与运行模式。
- Runtime 根据结构化事件创建 Child Agent，并持久关联 Root `parent_id`；子 Agent 独立记录轮次、当前工具、Token、终态与 Blocked 状态。
- TUI Runtime 区域按父子层级展示 Root/Child Agent，标记后台模式；聊天活动栏同步子 Agent 的轮次和工具进度。
- CLI Agent 输出补充 Profile、Label 和前台/后台模式。

### Changed

- `spawn_agent` 返回值同时包含稳定 Agent ID 与后台任务 ID，便于父 Harness、Runtime 和用户精确关联。
- 子 Agent 的 Blocked 不再映射成 WaitingApproval，避免不存在待处理审批时误导用户。

## [0.16.0-rc6] - 2026-08-10

### Added

- Runtime 新增持久 `RuntimeAgent` 实体；每个托管任务拥有稳定 Root Agent ID，并预留 `parent_id` 支持后续原生子 Agent 树。
- 受 Runtime Token 保护的 `GET /v1/agents`、`GET /v1/agents/{id}` 以及 `daemon agents/agent` 查询结构化 Agent 生命周期。
- Agent 持久记录 Workspace、Profile、状态、当前轮次、当前工具、Token 用量、完成时间和错误；Daemon 重启时将遗留活动 Agent 标记为 Interrupted。
- TUI 右栏 Runtime 区域展示当前 Workspace 的 Agent、状态、Profile、轮次、工具和 Token 摘要。

### Changed

- Harness 的轮次、工具和用量事件直接驱动 Agent 状态，不通过终端文本或画面推断。
- 旧 Runtime Task 在加载时自动补建 Root Agent 并回填 `agent_id`，保持现有任务数据向后兼容。

## [0.16.0-rc5] - 2026-08-10

### Added

- TUI 按持久事件游标补读 Runtime 的模型轮次、工具请求、工具结果、Token 用量和完成事件，并实时映射到聊天区与活动栏。
- Runtime 健康响应暴露最新事件序号；任务保存首个事件序号，使首次从 TUI 提交时既不回放旧日志，也不会遗漏快速完成的任务。
- Session 保存 Runtime 事件游标、`/runtime` 用户消息和正式回复；退出并恢复同一会话后聊天记录保持完整且不重复。

### Changed

- Runtime Inbox、Interaction 与事件补读按当前 Workspace 过滤；不可见事件仍推进全局游标，避免跨工作区泄漏或反复扫描。
- `/runtime` 允许仅附件任务，文本和附件不能同时为空。
- 扩展 CLI、TUI、Computer Use、Herdr 互操作和 Swift Harness 替换的完整产品路线图与执行顺序。

## [0.16.0-rc4] - 2026-08-10

### Added

- TUI 每秒同步 Runtime 任务与 Pending Interaction，并合入右栏 Attention Inbox；远端运行、等待、失败和完成状态与本地条目统一排序。
- 在 Runtime 审批或提问条目上按 Enter，复用现有 Allow/Disallow/Always Allow 和 ask_user 弹窗；解决结果发回原后台 Harness。
- Runtime 任务条目支持 `K` 请求停止；TUI 状态栏显示成功或失败反馈。
- Composer 新增 `/runtime <task>`，把文本、粘贴文本附件和图片附件提交给可分离 Runtime；TUI 退出不终止任务。

### Changed

- `/` 命令候选和帮助加入 `/runtime`，Runtime 提交沿用当前 Workspace、Provider Profile、配置路径和显式 Skills 展开结果。

## [0.16.0-rc3] - 2026-08-10

### Added

- Runtime 托管 Harness 遇到工具审批或 `ask_user` 时进入持久 Pending Interaction，不再因无终端 stdin 而直接拒绝。
- 新增 `daemon pending/resolve/answer`：支持 Allow once、Deny、Always allow，以及候选外自由输入；解决后原 Harness 在同一任务中继续。
- Runtime Task 新增 WaitingApproval 与 WaitingAnswer 状态，创建、解决、取消和最终执行顺序写入可续传事件流。

### Security

- Runtime 控制 Token 仅通过启动时私有 stdin 消息传入 Harness 内存，不再放入子进程环境，Shell 和 MCP 子进程不会继承该凭据。
- Interaction 类型与解决类型严格匹配；不可 Always Allow 的审批不能通过伪造解决请求升级权限。

### Changed

- 将 Daemon 单元测试拆入独立模块，生产实现保持在 2000 行以内。

## [0.16.0-rc2] - 2026-08-10

### Added

- Runtime 新增持久任务模型与 `daemon submit/tasks/task/cancel`，非交互 Harness 由 Daemon 持有，提交客户端退出后继续执行。
- Prompt 通过私有 stdin 传给 Harness 子进程，不出现在进程参数；模型事件、最终结果、session_id、错误和退出状态回流持久事件日志。
- 任务支持 Queued、Running、Cancelling、Completed、Failed、Cancelled、Interrupted 状态，Daemon 异常重启时将未完成记录安全标记为 Interrupted。
- Daemon 新增跨平台单实例租约锁、并发启动协调和失效锁恢复；停止 Runtime 时主动取消其持有的 Harness 进程。

### Changed

- 所有 Runtime 本地 API 响应统一携带 `X-WillDeep-Version`，任务元数据使用私有原子 JSON 文件保存。

## [0.16.0-rc1] - 2026-08-10

### Added

- 新增 `willdeep daemon start/status/stop/logs`，提供跨平台持久 Runtime 的首个可运行控制面。
- Daemon 使用随机回环 TCP 端点和仅本机可读的随机控制 Token，健康响应携带当前服务版本。
- Unix 后台进程使用独立进程组，Windows 使用 detached process flags，启动命令退出后 Runtime 仍持续运行。
- Daemon 状态采用原子写入，Unix 状态文件和日志权限收紧为 `0600`，支持优雅关闭和失效状态恢复。
- 新增持久化 NDJSON Runtime 事件日志与单调序号；`willdeep attach --after <cursor>` 可补齐离线事件，`Ctrl+C` 或 `willdeep detach` 不会停止 Daemon。

## [0.15.0-rc4] - 2026-08-10

### Added

- Git 未合并冲突和待审 Diff 进入 Attention Inbox；条目 ID 包含真实暂存/未暂存差异及受限未跟踪内容签名。
- Inbox 的 Worktree/Diff 条目支持精确详情、标记已读，新内容不会错误继承旧 Diff 的已读状态。
- 子 Agent 因审批拒绝时结构化上报 `blocked`，可在权限条件改变后重试。
- Core 新增 Agent→Session→Workspace 状态树，TUI 运行状态使用相同优先级逐层上卷。

## [0.15.0-rc3] - 2026-08-10

### Added

- Inbox 已读集合随会话 JSON 持久化，旧会话缺少该字段时保持向后兼容。
- 后台 Shell 和子 Agent 保存安全的可重放启动器，失败或取消后可按 `R` 创建新任务并真实重试。
- 后台任务成功、失败或取消时发送终端 BEL 提示，并继续把结果交还主 Harness。

## [0.15.0-rc2] - 2026-08-10

### Added

- Attention Inbox 支持键盘选择、自动跟随滚动、鼠标打开详情、停止运行任务以及标记终态条目已读。
- 右栏快捷键帮助补充 Inbox 的 Enter、K、M 操作。

### Changed

- 将 TUI 侧栏渲染和测试拆分为独立模块，使生产主文件保持在 3000 行以内。

## [0.15.0-rc1] - 2026-08-10

### Added

- Core 新增统一 Runtime 状态、Attention 来源与分组模型，覆盖审批、提问、后台 Shell、子 Agent、Worktree 和 Diff 审查。
- 后台任务状态可映射为统一的工作中、失败、完成和取消状态，并支持按人工介入优先级排序与父级状态聚合。
- TUI 右栏新增 Attention Inbox，按“需要你处理”“正在工作”“最近完成”聚合审批、提问、失败任务、后台 Shell 和子 Agent。

## [0.14.0-rc4] - 2026-08-10

### Fixed
- `ask_user` 长问题自动换行时，按视觉行计算弹窗高度与选项鼠标命中位置。

### Added

- 聊天区加入 TUI 焦点循环；`Ctrl+W` 在 Prompt、聊天和状态栏之间切换，点击或滚轮进入聊天焦点，方向键滚动，Esc 返回 Prompt。
- `/` 命令和 `$` 技能候选支持鼠标点击插入；审批按钮支持鼠标决策，单选 ask_user 支持点击提交，多选支持点击勾选并从底部操作区发送或跳过。
- 聊天搜索框支持鼠标定位编辑光标，后台任务详情支持鼠标滚轮。

### Changed

- Prompt、聊天和状态栏统一使用高亮边框、焦点标题和底部状态提示。

## [0.14.0-rc3] - 2026-08-10

### Added

- TUI 增加 `Ctrl+P` 全局命令面板，统一模糊搜索命令、Skills、当前与最近会话、后台子 Agent/任务和工作区文件。
- 命令面板支持方向键、Tab/Shift+Tab、Enter、Esc 和鼠标选择；命令、技能与文件插入 Prompt，后台任务直接打开详情。

### Security

- 工作区文件候选最多读取 300 个相对路径，跳过重型构建目录与符号链接，不读取文件正文，也不越过当前 Workspace。

## [0.14.0-rc2] - 2026-08-10

### Added

- TUI 增加 `Ctrl+F` 聊天搜索，支持大小写不敏感匹配、高亮、`Enter`/`Shift+Enter` 循环跳转及 Esc 关闭。
- 右侧状态栏标题支持点击折叠或展开，滚轮改为滚动内容；后台任务可点击或从任务分组按 Enter 打开详情，查看元数据与最近输出，运行中的任务可按 `K` 请求停止。

### Changed

- 右栏鼠标命中基于实际可见行计算，手动滚动时不再被当前选中分组强制拉回。

## [0.14.0-rc1] - 2026-08-10

### Added

- 新增 `docs/CLI_TUI_RUNTIME_ROADMAP.md`，记录从 TUI 交互、Attention Inbox、Runtime Daemon 到 Agent Mission Control、Review、统一 API、移动端、Workflow 和 Herdr 互操作的完整实施计划与验收标准。
- TUI 增加 `F1` 全局快捷键帮助；空 Prompt 时也可用 `?` 打开，避免拦截普通文本中的问号。
- Prompt 与状态栏使用明确的焦点边框和标题，底部状态行同步显示当前焦点及帮助入口。

### Changed

- 当前开发阶段进入 v0.14.0，按路线图推进 TUI 交互基础收尾。

## [0.13.0-rc5] - 2026-08-10

### Added

- TUI 右侧状态栏支持鼠标或 `Ctrl+W` 聚焦、方向键选择、Enter/Space 折叠展开、Esc 返回 Prompt，以及 `Ctrl+B` 整体显示或隐藏；窄终端使用覆盖层呈现。

## [0.13.0-rc4] - 2026-08-10

### Added

- TUI Prompt 输入 `/` 或命令前缀时展示带说明的命令候选，支持上下方向键选择、Enter/Tab 插入及 Esc 关闭，选择候选不会立即执行命令。

## [0.13.0-rc3] - 2026-08-10

### Added

- TUI Prompt 输入 `$` 或 `$关键词` 时展示技能名称与描述候选，支持上下方向键选择、Enter/Tab 插入及 Esc 关闭；技能正文仍只在发送后读取。

## [0.13.0-rc2] - 2026-08-10

### Fixed

- TUI 改用 Ratatui 实际排版行数计算聊天区滚动范围，避免按单词换行或 Markdown 样式渲染后最后一行被裁掉。

## [0.13.0-rc1] - 2026-08-10

### Added

- TUI 在聊天区末尾临时展示单行、限长的模型阶段文本，正式回复完成后自动移除并替换为答案。
- Web 增加吸附在 Composer 上方的单行思考/工作状态、逐轮工具轨迹，以及发送/停止图标按钮；停止会断开 SSE 并终止对应 Harness 子进程。
- Web 支持粘贴长文本和图片、发送前预览与删除；附件仅在点击发送后交给当前 Provider，非多模态模型继续使用配置的视觉模型解析。
- Web Composer 支持 `/` 命令候选、`/help`、`/goal`、`/compress`、`/skills`、`/clear`，以及 `$` 技能名称/描述候选。
- Web 技能候选排除可能包含历史 Prompt 的 `auto-*` 录制项，并隐藏带明显凭据标记的描述。
- Web 聊天区使用独立历史滚动容器，用户向上浏览时暂停自动追底；TUI 增加鼠标滚轮浏览历史。
- TUI 渐进渲染 Markdown 标题、粗体、行内代码、引用、列表、代码块和链接。
- TUI 增加 `Ctrl+S` 文本选择模式，释放鼠标给终端原生拖选与复制，再次切换恢复滚轮和点击。

### Changed

- 未配置审批模式时默认使用 `smart` 智能审核。
- 智能审核精确免审 `cargo test` 及其后仅包含 `grep`、`head`、`tail` 的输出过滤管道；重定向、命令连接、命令替换及其他 Cargo 子命令仍需审批。

## [0.12.0-rc1] - 2026-08-10

### Added

- TUI 与 Web 支持简体中文、英语和日语；可通过 `agent.language`、`--language`、`WILLDEEP_LANGUAGE` 或 Web 语言选择器切换。
- Web 语言偏好保存在浏览器本地，并随 SSE 请求传递，使实时 Harness 状态也使用所选语言。

### Changed

- TUI 审批弹窗将 `Y`/`A` 显示为黄色高亮键，将拒绝键 `N` 显示为红色高亮键，动作说明同步本地化。

## [0.11.0-rc1] - 2026-08-10

### Added

- 新增 `--web` 内嵌 Web Server，提供 React + Chakra UI + Vite 纯 CSR 聊天页、健康检查、会话索引和 SSE Chat API。
- Web 模式支持严格 allowlist 内的多工作区切换；应用层保持单用户无鉴权，认证与 HTTPS 由 Nginx/VPN 等上游负责。
- 用户消息发送后立即显示；Harness 轮次、工具与压缩阶段通过 SSE 实时更新，TUI 同步显示最近三条可验证工作进度。
- 新增 Xedit 工具能力对照文档，明确 Rust 内核、Skill/MCP 上层能力及 macOS Computer Use 安全实现路线。

### Changed

- JSON 完成事件增加 `session_id`，Web 客户端可继续同一历史会话；SSE 不发送工具参数、输出或模型私有思维链。

## [0.10.0-rc2] - 2026-08-09

### Fixed

- 空白新会话进入 TUI 时立即显示包含当前工作区名称的本地欢迎语；恢复历史会话不重复显示，也不把欢迎语写入模型上下文。

## [0.10.0-rc1] - 2026-08-09

### Added

- 首次使用交互式 onboarding，支持手动 Provider 配置和 some.im 浏览器登录轮询，并以 `0600` 权限保存 TOML 配置。
- 加载 `~/.willdeep/CLAUDE.md`、项目根 `PRODUCT_OVERVIEW.md`、`AGENTS.md` 与 `CLAUDE.md` 规则。
- 新增 `git_diff`、`list_worktrees`、`create_worktree` 工具；TUI 右栏显示项目、分支、diff 文件数和 worktree 数。
- 在 macOS 上读取 Swift WillDeep 的 Project 列表与历史会话，可用 `--project` 和 `--resume` 继续工作。

### Changed

- 从 Swift 导入的会话续聊会安全保存为 Rust 副本，不覆盖 Swift 原始 JSON；待共享 schema 稳定后再开放双向原地写入。

## [0.9.0-rc1] - 2026-08-09

### Added

- 增加 Core Harness `ask_user` 工具，支持候选单选、多选、跳过以及自由输入其他答案；TUI 与普通终端均可交互。
- 审批决策升级为 Allow once、Disallow、Always allow；TUI 使用 Y/N/A，普通终端提供同等选项。
- Always Allow 规则持久化到 `$WILLDEEP_HOME/always-allow.json`，增加 `--list-approvals` 与 `--clear-approvals`。

### Security

- Shell Always Allow 仅匹配规范化后的完整命令，含管道、重定向、命令连接或换行的命令不提供持久授权；MCP 按精确 server/tool 记忆。
- 文件写入、网络跳转、后台任务取消和 editor 单文件授权不允许持久放行。
- `ask_user` 问题、选项和答案均限制长度，用户自由输入在回传模型前进行标记转义。

## [0.8.0-rc2] - 2026-08-09

### Fixed

- `web_fetch` 不再一律拒绝 HTTP 重定向：同 hostname 最多自动跟随 8 跳，跨 hostname 时重新审批；每一跳重新校验公网地址，并拒绝 HTTPS 降级到 HTTP。

## [0.8.0-rc1] - 2026-08-09

### Added

- `run_command` 支持 `run_in_background`，返回 `job_xxxxxx`；增加 `get_job_output` 与 `kill_job`。
- 后台 Shell 和后台子 Agent 成功、失败、超时或取消后，结果自动注入主 Harness 并触发后续处理。
- 增加 `spawn_agent`，支持前台或后台运行及 `scout`、`reader`、`deep`、`editor` 四种内置工种。
- TOML 增加 `[subagents.<trade>]`，可为各工种绑定不同 Provider Profile、模型、上下文和轮数。
- TUI 宽屏右栏实时显示后台任务/子 Agent 的 ID、状态、耗时和标签。

### Security

- 子 Agent 禁止嵌套派生；只读工种不含写工具和 Shell。
- `editor` 必须单独审批 canonicalize 后的现有目标文件，子 Agent 只能编辑该文件。

## [0.7.0-rc1] - 2026-08-09

### Added

- 增加 `/compress` 本地命令，可立即总结较旧会话、保留最近六条消息并原子保存压缩后的当前会话。

## [0.6.0-rc1] - 2026-08-09

### Added

- some.im 纯文本主模型收到图片时，使用同一 API Base 和 API Key 调用 `qwen3-vl-plus` 生成描述，再把描述交给主模型；支持 `vision_model` 覆盖。
- 增加 `web_search` 和带公网目标校验、拒绝跳转、大小限制的 `web_fetch`，网络操作始终审批。
- TUI 状态栏显示上下文占比、最近输入/输出 Token 和耗时；宽屏增加后台状态侧栏。
- 上下文达到配置窗口约 80% 时生成临时摘要请求视图，保留完整会话存档。

### Changed

- `search_files` 与 `grep_files` 优先调用 `rg`，不可用或执行异常时回退到内置跨平台扫描器。
- Prompt 区最低保持三行，思考与工具阶段默认压缩为单行活动摘要，`Ctrl+O` 可展开工具明细。

## [0.5.0-rc2] - 2026-08-09

### Fixed

- 为无 checkout 的发布 Job 显式设置 `GH_REPO`，修复四平台产物完成后 GitHub Release 创建失败。

## [0.5.0-rc1] - 2026-08-09

### Added

- 增加 `/goal`、`/skills`、`/clear`、`/help` 本地命令和 `$skill-name` 显式技能引用。
- 增加 `/mobile` 二维码界面，通过 `j.niuwoai.com` WebSocket Relay 接入现有 Android Mobile Gateway 协议。
- 手机消息进入当前 CLI 会话，运行中的请求自动排队；助手回复以 `message.append` / `message.done` 回传手机。
- TUI 按用户、助手、系统和错误类型显示不同颜色。
- 增加 Linux AMD64/ARM64 交叉测试、WSL ABI 烟测和四产物 tag Release workflow。

### Changed

- 审批模式对齐为 `strict`、`smart`、`workspace-write`；后两者仅免审当前工作区内的创建与编辑，Shell、MCP 和网络仍需审批。

## [0.4.0-rc1] - 2026-08-09

### Added

- TUI Prompt 升级为可换行编辑器，支持左右/上下、Home/End、鼠标点击定位和内部滚动。
- 支持 `Shift+Enter`、`Alt+Enter` 或 `Ctrl+J` 插入换行，`Enter` 发送。
- 支持 Bracketed Paste；多行或长文本显示为可删除的粘贴附件。
- 支持从系统剪贴板读取图片、编码 PNG、展示附件摘要，并通过 `Ctrl+D` 删除附件。
- 图片附件原生编码到 Chat Completions、Responses 和 Anthropic Messages 三种协议，并随会话持久化。

## [0.3.0-rc2] - 2026-08-09

### Fixed

- TUI 聊天记录支持方向键、翻页键、Home/End 滚动，并在手动查看历史时暂停自动跟随。
- Tool Use 默认聚合为紧凑活动摘要，支持 `Ctrl+O` 展开最近明细，失败计数保持可见。

## [0.3.0-rc1] - 2026-08-09

### Added

- 增加 Ratatui 多轮交互界面，并在界面内处理工具审批和 Agent 事件。
- 增加版本化 JSON 会话存储、原子写入、`--list-sessions` 与 `--resume`。
- 增加多目录 `SKILL.md` 发现、`list_skills`、`read_skill` 和安全资源边界。
- 增加 TOML `mcp_servers` 配置、stdio 生命周期、MCP 初始化、工具发现和命名空间调用。
- MCP 调用接入统一审批，会话在模型请求前先持久化用户输入。

### Changed

- Agent 支持用历史消息继续运行，并始终刷新当前系统提示词。
- 工具定义改为可拥有字符串，以支持运行时 MCP 工具注册。

## [0.2.0-rc1] - 2026-08-09

### Added

- 增加 `~/.willdeep/config.toml` 配置加载与 `--config`/`--profile`。
- 支持在 TOML 中定义多个 Provider、API Dialect、API Base、模型和输出上限。
- 支持 `api_key_env` 安全引用环境变量，也允许受权限保护的明文 `api_key`。
- 增加 Agent 最大轮数与审批模式配置。
- 增加可直接复制的 `config.example.toml`。

## [0.1.0-rc1] - 2026-08-09

### Added

- 建立 Rust workspace、Core crate 与 CLI crate。
- 实现 OpenAI Chat Completions、OpenAI Responses、Anthropic Messages 三种协议适配器。
- 实现 some.im Provider 识别、Bearer 鉴权和会话/工作区上下文请求头。
- 实现模型—工具多轮 Agent Harness。
- 实现与 Swift App 同名的首批工作区工具：搜索、正则搜索、文件读取、目录列表、Git 状态、命令执行、文件创建与精确编辑。
- 实现工作区路径隔离、交互审批、全自动模式和 NDJSON 事件输出。
- 增加三协议完整工具往返契约测试和 macOS/Linux/Windows CI。
