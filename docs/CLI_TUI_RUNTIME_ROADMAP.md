# WillDeep CLI、TUI 与 Runtime 路线图

> 最后更新：2026-08-10
> 当前实施版本：v0.16.0-rc9
> 状态图例：`[x]` 已完成、`[-]` 进行中、`[ ]` 待实施

## 1. 产品方向

WillDeep 的目标不是复制 tmux 或 Herdr，而是成为跨平台、可持久运行、可被 TUI、Web、移动端、Swift App 和自动化脚本共同驱动的 Coding Agent Runtime。

Herdr 值得借鉴的是持久 PTY、语义化 Agent 状态、状态聚合、远程重连，以及人和 Agent 共用的自动化控制面。WillDeep 保留自己的核心优势：Provider 协议、结构化工具调用、安全审批、Skills、MCP、多模态降级、后台任务结果回流、子 Agent Profile 和结构化会话。

明确不在近期主线内重新实现完整终端模拟器、SSH/Mosh/Tailscale、tmux 兼容层、多租户账号系统或云端托管控制面。Herdr 作为后期可选集成，不作为 WillDeep Runtime 的强依赖。

## 2. 总体架构目标

```text
Provider / Tools / MCP / Skills
              │
        WillDeep Runtime
     ┌────────┼─────────┐
     │        │         │
    TUI      Web      Mobile
     │        │         │
     └──── Local Control API ──── Swift App / Scripts / Other Agents
```

Runtime 负责会话、主 Agent、子 Agent、后台任务、审批、问题、事件和持久化；客户端只负责展示、输入和控制。所有状态都来自结构化事件，不通过终端文本猜测。

## 3. 实施阶段

### 阶段 0：TUI 交互基础（v0.14.0）

- [x] 全局快捷键帮助浮层，按当前焦点展示操作。
- [x] Prompt、聊天区、右栏和弹窗使用一致的焦点边框与状态提示。
- [x] 右栏标题点击选择、箭头点击折叠、内容滚轮滚动、后台任务详情。
- [x] `Ctrl+F` 聊天搜索、匹配高亮和前后跳转。
- [x] `Ctrl+P` 全局命令面板，搜索命令、会话、Agent、技能和文件。
- [x] 统一弹层的 Esc、方向键、Tab、Enter 和鼠标语义。

验收：主要流程可纯键盘或纯鼠标完成；焦点始终可见；窄终端无越界；快捷键可发现。

### 阶段 1：Attention Inbox（v0.15.0）

- [x] 统一 `idle/working/blocked/waiting_approval/waiting_answer/failed/done/cancelled/unknown` 状态。
- [x] 右栏增加“需要你处理”“正在工作”“最近完成”。
- [x] 审批、ask_user、失败任务、阻塞子 Agent、Worktree 冲突和待审 Diff 统一进入 Inbox。
- [x] 支持允许、拒绝、回答、重试、停止、标记已读和精确跳转。
- [x] Agent → 会话 → Workspace 的状态优先级聚合。

验收：用户无需轮询输出即可处理所有阻塞项，每个条目能跳回准确上下文。

### 阶段 2：持久化 Runtime Daemon（v0.16.0）

- [x] `willdeep daemon start/stop/status/logs`。
- [x] 非交互 Harness 任务由 Runtime 持有，支持提交、列表、详情、取消、终态持久化和事件回流。
- [x] Runtime 托管任务支持持久审批与 ask_user 队列，其他 CLI 客户端可解决后恢复原 Harness。
- [x] Runtime Root Agent 持久实体、受保护查询 API、任务状态同步和 TUI 基础状态摘要。
- [x] `spawn_agent` 稳定 ID、Root→Child 父子关系、Profile/模式、轮次、工具、Token 和正常完成终态进入 Runtime 与 TUI。
- [x] 后台 Child Agent 与后台任务稳定绑定；Runtime 持久 stop/retry 命令并由原 Harness 按 Agent UUID 执行、确认，CLI/TUI 可精确控制。
- [ ] 交互 Harness、子 Agent、后台任务、审批、MCP、Web 和移动网关迁入 Runtime 原生生命周期。
- [x] 持久 Runtime 事件序列号、NDJSON 日志、游标补读以及 `willdeep attach/detach` 基础控制。
- [x] TUI Inbox 接入 Runtime 任务与待处理项；`/runtime` 提交的任务在关闭 TUI 后继续运行，并可重新观察、审批、回答和停止。
- [x] TUI 聊天记录按事件游标完整 attach，重连后恢复逐轮模型与工具时间线，并按 Workspace 过滤和去重。
- [x] 受 Token 保护的 SSE 实时事件推送；按游标分页补读、慢客户端日志追赶、断线重连去重与 TUI 轮询降级。
- [ ] Unix Socket 与 Windows Named Pipe、本地请求幂等键和跨版本能力协商。
- [x] 单实例租约锁、健康检查、私有原子状态保存和优雅停止。
- [ ] 无损优雅升级与版本交接。

验收：客户端断开后任务继续运行，重连能补齐离线事件，异常退出不破坏会话。

### 阶段 3：会话管理与恢复（v0.17.0）

- [ ] 会话选择器以及 `sessions/resume/rename/fork/archive/delete/export`。
- [ ] 恢复 Goal、Provider、模型、Skills、Agent 树、任务、审批、Worktree、Token 和压缩点。
- [ ] 从指定轮次 Fork，并可切换模型或 Provider。
- [ ] 按标题、Workspace、内容、状态、模型和时间搜索。
- [ ] 安全的自动标题与会话数据迁移版本。

验收：任意会话可恢复、Fork、归档和导出，安全策略与系统 Prompt 始终使用当前版本。

### 阶段 4：Agent Mission Control（v0.18.0）

- [ ] 展示主 Agent 与子 Agent 树、父子关系、Profile、模型、状态、工具、耗时、Token 和 Worktree。
- [ ] Agent 详情页包含 Prompt、进度、工具时间线、输出、Diff 和错误。
- [-] 支持新建、补充 Prompt、停止、重试、换模型、查看日志和 Diff；当前已完成后台 Child Agent 的停止与重试。
- [ ] Profile 定义 Provider、模型、工具权限、Skills、预算和递归能力。
- [ ] 并发、深度、轮次、Token、费用与时长限制，连续失败熔断。

验收：每个 Agent 可观察、可控制，结果可靠回流父 Agent，不能无限递归。

### 阶段 5：Diff 与 Review Center（v0.19.0）

- [ ] 按轮次、Agent 和文件追踪新增、修改、删除、重命名与二进制变更。
- [ ] TUI Unified/Side-by-side Diff、语法着色、搜索和文件导航。
- [ ] 接受、打回、请求重改、标记已审和安全撤销单文件。
- [ ] 测试命令、退出码、失败摘要与变更集绑定。
- [ ] Commit Preview、敏感文件检查、Tag 和推送目标确认。

验收：用户可在 TUI 完成主要审查，撤销不会覆盖用户已有修改。

### 阶段 6：Workspace 与 Worktree（v0.20.0）

- [ ] Workspace 注册、删除、切换以及独立安全策略、Provider、Skills 和 MCP。
- [ ] 切换时重新建立路径边界，旧 Workspace 的任务继续运行。
- [ ] 子 Agent 专属 Worktree、Diff 回流、冲突检测和合并审批。
- [ ] 孤儿 Worktree 检测与保守清理。

验收：多 Workspace 不突破隔离，多 Agent 能安全并行修改。

### 阶段 7：统一控制 API（v0.21.0）

- [ ] 稳定定义 Runtime、Workspace、Session、Agent、Turn、Tool、Task、Approval、Question、Artifact 和 Event。
- [ ] 本地 JSON 请求响应与 NDJSON/流式事件协议。
- [ ] `willdeep api session.list/agent.spawn/agent.prompt/agent.wait/approval.resolve/events`。
- [ ] 幂等请求 ID、事件游标、错误码、版本协商和能力列表。
- [ ] Rust Client Library，供 TUI、Web、Swift FFI、移动端和自动化复用。
- [ ] API Key、工具参数、Prompt 和路径按权限脱敏。

验收：所有客户端观察一致状态，TUI 不再直接持有 Harness 业务逻辑。

### 阶段 8：远程访问与移动端（v0.22.0）

- [ ] 手机查看 Workspace、会话、Agent 树和 Attention Inbox。
- [ ] 审批、回答、补充 Prompt、停止、重试和 Diff 摘要。
- [ ] 通知精确定位 Workspace/Session/Agent/Turn，支持多设备去重和静默时间。
- [ ] 图片、长文本附件、发送前删除和视觉模型降级。
- [ ] `j.niuwoai.com` 只做中继，不持有 Provider Key，按最小需求传输会话内容。

验收：用户可只用手机解除阻塞，断线重连不重复操作。

### 阶段 9：Web 完整客户端（v0.22.0）

- [ ] 对接 Runtime API，提供 Workspace、会话、Inbox、Agent、Diff 和任务详情。
- [ ] Composer 保持 `/`、`$`、图片、文本附件、多行、停止和 Prompt 历史。
- [ ] SSE/事件流按游标恢复，刷新不丢运行状态。
- [ ] 桌面三栏、平板双栏、手机单栏；所有文案走 i18n。
- [ ] 继续保持单用户，不在应用内实现认证，依赖 Nginx/VPN/SSH Tunnel。

验收：Web 与 TUI 语义一致，刷新和断线不会造成消息丢失或重复。

### 阶段 10：Workflow 与插件（v0.23.0）

- [ ] TOML Workflow：串行、并行、依赖、条件、重试和人工审批门。
- [ ] Profile 指定模型、Provider、Prompt、Skills、权限、预算与 Worktree 策略。
- [ ] 生命周期 Hook：session、turn、tool、approval、agent、task、review。
- [ ] 插件声明权限，不能读取密钥或绕过 Harness 审批。
- [ ] 支持本地与 Git 仓库插件，暂不建设中心化 Marketplace。

验收：多 Agent 流程可声明式复用，每一步可观察、可停止、可审计。

### 阶段 11：Herdr 互操作（v0.23.0）

- [ ] `willdeep integrations herdr install/status/uninstall`。
- [ ] 向 Herdr 上报准确的 working、blocked、approval、done 和 idle 生命周期。
- [ ] WillDeep 会话与 Herdr Pane 关联并支持精确跳转。
- [ ] 可选通过 Herdr 启动 Claude Code、Codex、OpenCode 等外部 Agent。
- [ ] 外部 Agent 明确标注权限边界，不把画面检测当作强一致结果。

验收：Herdr 用户获得原生状态体验；未安装 Herdr 不影响核心功能。

### 阶段 12：可观测性、诊断与 1.0 加固

- [ ] 结构化日志与 Session/Agent/Turn/Tool Trace ID。
- [ ] `willdeep doctor` 与自动脱敏的诊断包。
- [ ] 首 Token、轮次、工具、重试、Token、费用、压缩和队列指标。
- [ ] Provider 健康状态、事件背压、日志与 Scrollback 上限。
- [ ] 崩溃恢复、协议兼容和跨平台端到端测试。
- [ ] Rust Runtime 满足替换 Swift App Harness 内核的能力与迁移门槛。

验收：长期运行资源有界、问题可诊断、状态可恢复，完成 1.0 发布审计。

### 横向能力 A：CLI 与 TUI 深化

以下能力不单独阻塞某个大版本，可随 Runtime 阶段逐批交付，但必须复用统一事件和状态模型：

- [ ] Headless/CI 模式、稳定退出码、`--json`、`--ndjson` 和安静输出模式。
- [ ] Bash、Zsh、Fish 与 PowerShell 命令补全。
- [ ] 跨会话 Prompt 历史、全文搜索和安全清理。
- [ ] 长会话虚拟滚动，工具输出按活动组聚合并默认显示摘要。
- [ ] 可展开查看工具原始参数、输出、错误和审批依据。
- [ ] 补齐标题、列表、引用、代码块、表格、链接和行内代码等 Markdown 渲染。
- [ ] 文件路径、URL 与诊断位置可点击，并提供不支持鼠标终端的键盘替代操作。
- [ ] 在 Kitty、iTerm2、Sixel 等支持的终端预览图片，否则显示可管理的附件卡片。
- [ ] 主题、紧凑模式、高对比度、可配置键位以及可选 Vim/Emacs 输入模式。

验收：CLI 可稳定用于脚本和 CI；TUI 在超长会话、大输出和不同终端能力下仍保持流畅、可发现和可访问。

### 横向能力 B：Computer Use

- [ ] macOS 先接入 Accessibility、ScreenCaptureKit 与受控输入事件。
- [ ] 截图、窗口识别、点击、键盘输入和滚动均产生结构化、可审计事件。
- [ ] 权限状态明确可见，高风险操作进入统一审批队列。
- [ ] Computer Use 不能绕过 Workspace、工具权限、智能审核或敏感信息规则。
- [ ] 作为 Runtime Tool 提供给主 Agent、子 Agent 和 Workflow，而非成为独立状态机。
- [ ] 后续评估 Windows UI Automation 与 Linux Wayland/X11，并按能力协商降级。

验收：在 macOS 上能够安全完成可复现的桌面操作，用户可查看、停止和审计每一步；其他平台缺少能力时明确降级。

### 1.0 迁移门槛：替换 Swift App Harness 内核

- [ ] 第一阶段：Swift App 使用 Rust Client 观察 Runtime，会话和审批保持双读校验。
- [ ] 第二阶段：Rust Runtime 接管会话、后台任务、子 Agent 和事件持久化。
- [ ] 第三阶段：Rust Runtime 接管完整 Harness、Tools、Skills、MCP 与 Provider 生命周期。
- [ ] 第四阶段：完成数据迁移、故障回退和一致性验证后移除 Swift 旧 Harness。
- [ ] macOS、Windows 和 Linux 完成协议兼容、崩溃恢复、长时间运行和端到端测试。

验收：Swift、TUI、Web、移动端对同一任务观察到一致状态；迁移与回退不丢会话、事件、附件、审批或用户修改。

## 4. 实施原则

1. 每个阶段先定义结构化状态和协议，再实现具体 UI。
2. TUI、Web、移动端不得各自复制业务状态机。
3. 所有破坏性操作必须保守处理用户已有改动。
4. 安全边界、审批和脱敏不能被 Workflow、插件或外部 Agent 绕过。
5. 每个版本同步更新版本号、CHANGELOG、PRODUCT_OVERVIEW 和本文件状态。
6. 每项完成必须有覆盖其验收条件的测试或可重复验证步骤。

## 5. 当前执行批次

v0.16.0-rc9：Runtime 新增受 Token 保护的 SSE 事件流，连接时按游标分页补齐 NDJSON 历史，再切换实时广播；广播积压时回读持久日志，TUI 使用可取消 follower 消费并对旧 Daemon 自动轮询降级。Agent 控制与 EventLog 已拆入模块，Daemon 主文件保持在 2000 行以内。下一批定义持久交互式 Session/Root Harness 协议。

## 6. 建议执行顺序

1. 完成并发布 `v0.16.0-rc9`：Runtime SSE、游标续传、积压追赶和 TUI 实时消费。
2. 定义持久交互式 Session/Root Harness 协议，并将交互式主 Harness 迁入 Runtime 原生生命周期。
3. 将剩余后台 Shell、审批、MCP 和附件迁入统一生命周期。
4. 实现请求幂等、能力协商以及 Unix Socket/Windows Named Pipe 跨平台本地传输。
5. 完成异常退出、守护进程重启和客户端重连的端到端测试。
6. 实现完整会话恢复、Fork、归档、导出与搜索。
7. 实现 Agent Mission Control、预算限制、失败熔断和结果回流。
8. 实现 Diff Review Center、多 Workspace 与安全 Worktree 合并。
9. 稳定统一控制 API 与 Rust Client Library，让 TUI 不再直接持有 Harness 业务逻辑。
10. 让 Web、移动端和 Swift App 逐步迁移到统一 Runtime API。
11. 补齐 Workflow、插件、Herdr 互操作和 Computer Use。
12. 完成可观测性、跨平台测试、安全审计与 Swift Harness 替换，发布 `1.0.0`。

关键路径固定为：`Runtime 持久化 → 会话恢复 → Agent 生命周期 → Diff/Workspace → 统一 API → Web/移动端/Swift 共用内核`。Herdr、Computer Use 和客户端视觉增强可以穿插推进，但不能形成独立于 Runtime 的第二套任务状态机。
