# WillDeep CLI、TUI 与 Runtime 路线图

> 最后更新：2026-08-10
> 当前实施版本：v0.14.0-rc2
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
- [-] Prompt、聊天区、右栏和弹窗使用一致的焦点边框与状态提示。
- [x] 右栏标题点击选择、箭头点击折叠、内容滚轮滚动、后台任务详情。
- [x] `Ctrl+F` 聊天搜索、匹配高亮和前后跳转。
- [ ] `Ctrl+P` 全局命令面板，搜索命令、会话、Agent、技能和文件。
- [ ] 统一弹层的 Esc、方向键、Tab、Enter 和鼠标语义。

验收：主要流程可纯键盘或纯鼠标完成；焦点始终可见；窄终端无越界；快捷键可发现。

### 阶段 1：Attention Inbox（v0.15.0）

- [ ] 统一 `idle/working/blocked/waiting_approval/waiting_answer/failed/done/cancelled/unknown` 状态。
- [ ] 右栏增加“需要你处理”“正在工作”“最近完成”。
- [ ] 审批、ask_user、失败任务、阻塞子 Agent、Worktree 冲突和待审 Diff 统一进入 Inbox。
- [ ] 支持允许、拒绝、回答、重试、停止、标记已读和精确跳转。
- [ ] Agent → 会话 → Workspace 的状态优先级聚合。

验收：用户无需轮询输出即可处理所有阻塞项，每个条目能跳回准确上下文。

### 阶段 2：持久化 Runtime Daemon（v0.16.0）

- [ ] `willdeep daemon start/stop/status/logs`。
- [ ] Harness、子 Agent、后台任务、审批、MCP、Web 和移动网关迁入 Runtime。
- [ ] `willdeep attach/detach`，关闭 TUI 不终止任务。
- [ ] Unix Socket 与 Windows Named Pipe；事件序列号、断线续传和去重。
- [ ] 单实例锁、健康检查、原子状态保存和优雅升级。

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
- [ ] 支持新建、补充 Prompt、停止、重试、换模型、查看日志和 Diff。
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

## 4. 实施原则

1. 每个阶段先定义结构化状态和协议，再实现具体 UI。
2. TUI、Web、移动端不得各自复制业务状态机。
3. 所有破坏性操作必须保守处理用户已有改动。
4. 安全边界、审批和脱敏不能被 Workflow、插件或外部 Agent 绕过。
5. 每个版本同步更新版本号、CHANGELOG、PRODUCT_OVERVIEW 和本文件状态。
6. 每项完成必须有覆盖其验收条件的测试或可重复验证步骤。

## 5. 当前执行批次

v0.14.0-rc2：完成右栏鼠标折叠、内容滚动、后台任务详情与聊天搜索；随后继续全局命令面板和弹层交互统一。
