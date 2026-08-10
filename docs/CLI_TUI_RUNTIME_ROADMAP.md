# WillDeep CLI、TUI 与 Runtime 路线图

> 最后更新：2026-08-10
> 当前实施版本：v0.21.0-rc19
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

Daemon 内原生 Harness 的拆分边界、取消语义和验收证据见 [`IN_PROCESS_RUNTIME_HARNESS.md`](IN_PROCESS_RUNTIME_HARNESS.md)。

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
- [-] 主 Harness、子 Agent、后台任务、审批、MCP 和 Web 已迁入 Runtime 进程内生命周期；移动网关与跨重启执行资源恢复待完成。
- [x] 持久 Runtime 事件序列号、NDJSON 日志、游标补读以及 `willdeep attach/detach` 基础控制。
- [x] TUI Inbox 接入 Runtime 任务与待处理项；`/runtime` 提交的任务在关闭 TUI 后继续运行，并可重新观察、审批、回答和停止。
- [x] TUI 聊天记录按事件游标完整 attach，重连后恢复逐轮模型与工具时间线，并按 Workspace 过滤和去重。
- [x] 受 Token 保护的 SSE 实时事件推送；按游标分页补读、慢客户端日志追赶、断线重连去重与 TUI 轮询降级。
- [x] Unix Socket 与 Windows Named Pipe、本地请求幂等键和跨版本能力协商。
- [x] 单实例租约锁、健康检查、私有原子状态保存和优雅停止。
- [ ] 无损优雅升级与版本交接。

验收：客户端断开后任务继续运行，重连能补齐离线事件，异常退出不破坏会话。

### 阶段 3：会话管理与恢复（v0.17.0）

- [x] 按 `RUNTIME_SESSION_PROTOCOL.md` 建立稳定 Session / Root Agent / Turn / Execution Task 身份与持久状态机。
- [x] Session/Turn 受保护 API 与 CLI、`request_id` 幂等、同 Session 严格串行、排队/运行取消、终态事件和 Daemon 重启恢复。
- [x] TUI 与 Web 改用统一 Runtime Session/Turn；普通 Prompt 与 `/runtime` 完成现有 Session 幂等收养、多轮提交和终态历史同步，`/local` 提供单轮兼容；Web 提交同一 Runtime Turn、转发持久事件、真实停止并加载历史。
- [x] Web 会话选择器、CLI `sessions/resume` 以及 Runtime/CLI/TUI/Web 的 rename、完整快照 fork、archive、delete、export 已完成；TUI 支持同 Workspace 原地切换并按 Session 隔离聊天事件。
- [ ] 恢复 Goal、Provider、模型、Skills、Agent 树、任务、审批、Worktree、Token 和压缩点。
- [x] 已按持久消息边界支持从指定已完成 Turn 精确 Fork，并可为新 Session 覆盖 Provider Profile 和模型。
- [x] 受 Token CLI/TUI 支持标题、消息内容、Workspace、状态、Provider Profile、模型和更新时间组合搜索；Web 保持当前 Workspace 标题筛选，不向未认证浏览器下发消息摘要。
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

- [x] 按轮次、Agent 和文件追踪新增、修改、删除、重命名与二进制变更；潜在写工具按主/子 Agent 分别采集前后内容指纹，持久绑定 Session、Turn、Task、Agent、Tool 和真实变化路径，并可沿快照链查询。
- [x] TUI Unified/Side-by-side Diff、语法着色、搜索和文件导航；支持 Combined/Staged/Unstaged 范围切换和 Unicode 宽度安全双栏。
- [x] 接受、打回、请求重改、标记已审和安全撤销单文件；撤销绑定精确快照、TUI 二次确认，未跟踪/新增内容进入可恢复回收区。
- [x] 测试命令、退出码、失败摘要与变更集绑定；前后台测试自动绑定精确快照，摘要有界且敏感命令拒绝记录。
- [x] Commit Preview、敏感文件检查、Tag 和推送目标确认；Runtime API、CLI 与 TUI 共用精确快照，只预览不执行 Git 写操作，Remote 凭据只显示脱敏结果。

验收：用户可在 TUI 完成主要审查，撤销不会覆盖用户已有修改。

### 阶段 6：Workspace 与 Worktree（v0.20.0）

- [x] Workspace 注册、删除、切换以及独立安全策略、Provider、Skills 和 MCP；Runtime 注册表、API/CLI、TUI 与 Web 选择器共用同一来源，Web 仍受启动路径白名单上界约束。
- [x] 切换时重新建立路径边界，旧 Workspace 的任务继续运行；TUI 切换恢复目标 Session/游标并重启事件与状态订阅，跨路径后禁用启动时绑定旧根目录的 Local Harness，Runtime 激活不改变既有任务根目录。
- [x] 子 Agent 专属 Worktree、Diff 回流、冲突检测和合并审批；Editor 默认使用专属分支和 Worktree，Runtime 生命周期及 Diff 归因绑定真实 Child 路径，完成报告回流 Worktree 状态；Runtime/CLI/TUI 以 Child/Root 精确快照 Review ID、`git apply --check` 和显式批准执行保守合并。
- [x] 孤儿 Worktree 检测与保守清理；Runtime/CLI 审计已识别活动、待审、已合并、干净、已隔离、路径缺失和未知目录，精确快照与显式确认后仅将安全 Worktree 整体移动到 Recovery，不删除内容或分支。

验收：多 Workspace 不突破隔离，多 Agent 能安全并行修改。

### 阶段 7：统一控制 API（v0.21.0）

- [x] 稳定定义 Runtime、Workspace、Session、Agent、Turn、Tool、Task、Approval、Question、Artifact 和 Event 公共 DTO。
- [x] 本地 JSON 请求响应与 NDJSON/流式事件协议；统一 API 使用版本化请求/响应信封，事件流按全局序号补读并逐行输出完整信封。
- [ ] `willdeep api session.list/agent.spawn/agent.prompt/agent.wait/approval.resolve/events`。
- [-] 幂等请求 ID、事件游标、错误码、版本协商和能力列表；修改类统一请求使用 Pending→Completed 私有日志跨重启去重，不确定崩溃窗口拒绝自动重放；剩余修改操作迁移待完成。
- [-] Rust Client Library，供 TUI、Web、Swift FFI、移动端和自动化复用；已抽出回环连接、Token、能力、统一调用和 NDJSON 解码，TUI 事件、Agent、审批、回答、Workspace 与 Diff Center 已迁移。
- [-] API Key、工具参数、Prompt 和路径按权限脱敏；公开 DTO 排除配置/队列正文/内部错误，公共事件兼容净化 Tool 参数/输出、报告、路径和错误，细粒度远程路径权限待实现。

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

### 阶段 11：Herdr 互操作（v0.18.0 起持续交付）

- [-] 已完成 `willdeep integrations herdr status [--json]`；Integration 配置 install/uninstall 待完成。
- [x] Runtime Task 聚合后向 Herdr 上报准确的 working、blocked 和 idle；Herdr 将完成且未查看的 idle 投影为 done，审批/提问/失败投影为 blocked。
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

v0.21.0-rc19（已完成）：Web Runtime 侧栏支持 Approval 的 Allow Once/Deny/Always Allow，Question 的候选单选、多选提交和自定义回答，以及后台 Agent 的补充 Prompt、停止和重试。服务端每个写端点先重新校验 Runtime 注册 Workspace 与 Web 启动白名单，再确认 Gate/Agent 属于该 Workspace Snapshot；请求体严格拒绝客户端夹带额外作用域。操作完成后立即刷新 Activity，轮询仍负责最终一致性。下一步加入 Web Diff/Artifact 详情和基于 Event Cursor 的 Activity 推送，减少轮询。

v0.21.0-rc18（已完成）：Web Activity API 在既有 Workspace 注册表与启动白名单双重约束内增加 Agent、Approval/Question Gate 和关注项计数；Agent 摘要不下发 Workspace、报告或内部错误。React 将 Runtime 侧栏拆为独立组件，展示 Agent 状态、轮次、待审批/回答详情、工具和产物统计；所有新增文案覆盖中英日。下一步为 Web Gate 增加审批/回答交互，并补 Agent 停止/重试/补充 Prompt。

v0.21.0-rc17（已完成）：Agent Prompt/Wait、Approval Resolve、Question Answer、Event List 与 Workspace Ensure 参数从 daemon 私有结构提升为严格公共 DTO；Agent Command 与 Interaction Result 也改为不含私有 message/error 的稳定返回 DTO。Rust Client 新增 Workspace、Session、Agent、Turn、Task、Approval、Question 和 Event 高频类型化方法，修改操作显式接收幂等 Request ID；TUI bridge 的对应调用以及 Tool/Artifact 查询已迁移。跨语言夹具同步加入控制请求与结果。下一步补齐 Diff/Worktree 类型化 Client，并开始 Web 的 Agent/Inbox 详情交互。

v0.21.0-rc16（已完成）：修正 Rust Runtime Client 的 `tool.get` 与 `artifact.get` 返回契约：成功响应直接包含 `RuntimeTool`/`RuntimeArtifact`，不存在对象由统一 `not_found` 错误信封表达，不再错误声明为 `Option<T>`。新增 Unix Socket 真实 `tool.get` 往返测试，覆盖操作名、ID 参数与直接对象解码。下一步将 Agent、审批、提问和事件的私有参数提升为公共 DTO，并扩展类型化 Client。

v0.21.0-rc15（已完成）：新增固定的 `public-api-v1.json` 跨语言兼容夹具，覆盖 Runtime、Workspace、Session、Agent、Turn、Tool、Task、Approval、Question、Artifact 与 Event 全部 11 类稳定公开对象及响应信封。协议测试逐类反序列化并检查夹具不含 API Key、认证头或 Runtime Token；Object、Capability 和 Transport 的未来新增值统一降级为 `unknown`，避免旧客户端整包解码失败。Swift、Android 与第三方客户端可复用同一文件建立 decoder contract test。下一步扩充修改请求夹具和 Swift 只读观察适配层。

v0.21.0-rc14（已完成）：共享 Rust Runtime Client 新增 `tools/tool/artifacts/artifact` 类型化便捷方法；调用方不再手写 Tool/Artifact 操作名、通用 JSON 参数和返回 DTO。Unix Socket 真实往返测试验证 Token、稳定操作名、过滤参数和响应解码。下一步扩展 Session/Agent/Turn/审批等高频类型化方法，并为 Swift/移动端生成可验证的协议兼容夹具。

v0.21.0-rc13（已完成）：Web 新增受 Runtime 注册表与启动 Workspace 白名单双重约束的 Activity API，React 侧栏每两秒直接消费 Tool/Artifact DTO，展示工具总数、运行数、产物数和最近工具状态；新增文案全部覆盖中英日。下一步继续 Rust Client 类型化便捷方法、Web 详情交互，以及 Swift/移动端兼容验证。

v0.21.0-rc12（已完成）：新增稳定 Artifact DTO 与 `artifact.list/get`。工具窗口内经内容指纹确认的 Diff Attribution 映射为 Workspace Change Artifact，绑定 Session/Turn/Task/Agent、来源快照和变更项数量；公开元数据不泄露 Workspace、文件名或内容，具体读取继续走 Workspace 授权与精确快照保护的 Diff API。11 类 Runtime 公共对象 DTO 至此全部完成。TUI Runtime 快照和右栏已直接消费 Tool/Artifact 查询，显示工作区工具、运行项、产物与最近状态。下一步迁移 Web 展示，并继续 Rust Client 类型化便捷方法。

v0.21.0-rc11（已完成）：新增稳定 `RuntimeTool`、状态和过滤 DTO，以及 `tool.list/get` 统一操作。Runtime 对主/子 Agent Tool Activity 建立有界持久索引，记录 Session/Turn/Task/Agent 归属与毫秒级起止时间，重启时将运行项收敛为 Interrupted。公开记录不保存参数、输出、Workspace 路径或内部错误。下一步稳定 Artifact DTO 与来源绑定，并让 TUI/Web 直接消费 Tool 查询结果。

v0.21.0-rc10（已完成）：`daemon sessions/session/search/rename/fork/archive/export/delete` 与 Turn 提交、列表、查询、停止全部迁移到共享 Runtime Client，因此自动使用 Unix Socket/Windows Named Pipe。Core Session 新增向后兼容的私有配置引用；TUI/Web 收养只通过统一 `session.create` 发送稳定公开字段，Runtime 从 Core Session 恢复配置路径，公共 DTO 明确拒绝 `config`。下一步稳定 Tool、Artifact 公开 DTO 与操作，并继续迁移其他 CLI 兼容命令。

v0.21.0-rc9（已完成）：Runtime Daemon 在 Unix 平台新增权限为 `0600` 的本地 Socket，在 Windows 平台新增拒绝远程连接的随机 Named Pipe；共享 Runtime Client、CLI、TUI 与 Web 优先使用本地传输，旧版 daemon 状态继续回退到受 Token 保护的回环 TCP。能力协商只报告当前实际传输，Unix 清理拒绝覆盖或删除普通文件。macOS 真实 Socket 往返、旧状态兼容和 Windows GNU 目标交叉编译纳入验证。下一步迁移带私有配置引用的 Session 收养和剩余 CLI 兼容命令，并继续稳定 Tool、Artifact DTO。

v0.21.0-rc8（已完成）：协议 crate 新增 Session Search Result、文本/图片附件、Turn Submit/List 参数 DTO，以及 `session.search`、`turn.list` 稳定操作。统一 API 实现组合搜索、Turn 提交/列表/查询/停止；TUI Session 搜索和 TUI/Web Turn bridge 使用共享 Runtime Client。提交入口限制 Prompt、附件数量、文本长度、图片 MIME/尺寸与总载荷，并拒绝客户端夹带 Workspace/权限字段。下一步实现 Unix Socket 与 Windows Named Pipe，并继续迁移带私有配置引用的 Session 收养和 CLI 兼容命令。

v0.21.0-rc7（已完成）：统一 API 实现 Session 创建、重命名、Fork、归档/恢复、删除和导出，修改操作纳入跨重启 Request ID 幂等；协议 crate 新增严格拒绝未知字段的 Session 管理参数 DTO。Web Session 列表、重命名、Fork、归档、删除和导出 bridge 改用共享 Runtime Client，浏览器 Workspace 白名单校验保持在调用前。下一步迁移 Session 创建/Turn 提交与搜索，并实现 Unix Socket 与 Windows Named Pipe。

v0.21.0-rc6（已完成）：协议 crate 新增 Diff Snapshot、File、Content、Review、Verification、Attribution、Commit Preview 与 Revert 稳定 DTO；统一 API 覆盖全部 Diff 读写操作，TUI Diff Center bridge 改用共享 Runtime Client。审查、验证记录和安全撤销纳入跨重启 Request ID 幂等，服务端继续执行 Workspace 授权、精确 Snapshot、敏感命令过滤、内容上限和 Recovery 保守撤销。下一步迁移 Web Session 管理，并实现 Unix Socket 与 Windows Named Pipe。

v0.21.0-rc1（已完成）：新增独立 `willdeep-runtime-protocol` crate，定义协议 `1.0`、11 类控制对象、稳定 namespaced operation、能力/传输/限制、统一成功/错误信封和错误码。Runtime 新增显式 Token 校验的 `GET /v1/capabilities`，支持 `x-willdeep-request-id` 回显；CLI 新增 `daemon capabilities`。旧 `/v1/*` 原始 DTO 保持兼容，下一步实现统一 `willdeep api` 调度并逐步迁移到共享 DTO。

v0.21.0-rc2（已完成）：新增受 Token 保护的 `POST /v1/api`、`willdeep api` 与可续传 `/v1/events/stream.ndjson`；修改类调用以 Request ID 做有界幂等去重且不缓存 Prompt 明文，内部错误对客户端脱敏。新增 `willdeep-runtime-client` crate，CLI 的统一调用、能力协商和 NDJSON 消费已迁移。TUI 新增仅回环监听的 `/webapp [status|127.0.0.1:PORT]`，可从 Prompt 启动当前 Workspace 的内嵌 Web App。下一步稳定共享 DTO、迁移 TUI/Web，并补持久幂等与本地 Unix Socket/Windows Named Pipe。

v0.21.0-rc3（已完成）：协议 crate 新增 Session/Turn/Agent/Event 公开 DTO，排除配置路径、队列 Prompt/附件、内部错误等存储字段；统一 API 与 Agent Wait 返回公开结构。公共事件边界兼容净化新旧 Tool 参数/输出、Agent 报告、Workspace 路径和错误。TUI 的 NDJSON 实时事件、Agent 列表/补充 Prompt/停止/重试、审批和回答迁移到共享 Client。下一步迁移 Approval/Question/Task/Diff DTO、Web Client，并实现持久幂等与 Unix Socket/Windows Named Pipe。

v0.21.0-rc4（已完成）：协议新增 Task、Pending Approval/Question DTO 和统一 list/get/cancel 操作；TUI Inbox、Task 取消及 Web/TUI 事件补读迁移到共享 Client。修改类请求以只保存指纹和脱敏响应的 Pending→Completed 日志跨重启去重，Pending 落盘失败时不执行，不确定崩溃窗口不自动重放。下一步迁移 Workspace/Diff/Web 管理操作，并实现 Unix Socket 与 Windows Named Pipe。

v0.21.0-rc5（已完成）：协议新增 Workspace/Access/注册参数公开 DTO，统一 API 补齐 register/ensure/activate/remove 并纳入持久幂等；TUI `/workspace` 与 Web 启动/列表 bridge 迁移到共享 Client。新 Workspace 事件只记录稳定 ID，路径继续由 Runtime 规范化，公开路径保持可选以支持未来远程裁剪。下一步迁移 Diff/Web Session 管理并实现 Unix Socket 与 Windows Named Pipe。

v0.20.0-rc6（已完成）：新增受 Runtime Token 保护的 `worktrees-audit` 与 `quarantine-agent-worktree --snapshot <ID> --yes` API/CLI。审计区分 Active、Reviewable、Merged、Clean、Quarantined、Missing、Unknown；活动、未合并、冲突、未跟踪、快照变化、路径越界或无 Agent 记录全部拒绝隔离。安全对象通过 `git worktree move` 整体迁入 Recovery，文件、脏状态、Git 关联和分支完整保留，持久化失败时尝试原路回滚。验收测试证明两个专属 Worktree 可同时修改同一文件而互不影响，根工作区保持原内容；阶段 6 完成。

v0.20.0-rc5（已完成）：子 Agent 完成报告附带有界 Worktree 状态；新增受 Token 保护的 Worktree Review/合并 API，以及 `agent-worktree-review`、`merge-agent-worktree --review <ID> --yes` CLI。Review ID 同时绑定 Agent、Child 快照、Root 快照和二进制补丁，TUI Agent 详情以 `W` 审查、`M` 显式批准；Root/Child 任一变化、同文件冲突、未跟踪文件、未解决冲突或超过 2 MiB 的补丁都会阻断，合并不提交、不推送、不清理 Worktree。下一步实现孤儿 Worktree 检测与保守清理。

v0.20.0-rc4（已完成）：内置 Editor 子 Agent 默认创建 `willdeep/agent-<id>` 专属 Git Worktree；已审批目标按根工作区相对路径安全映射，Runtime Child Agent 持久记录实际 Workspace、根 Workspace、分支和隔离模式，写工具 Diff 归因改在 Child Worktree 内采集。`[subagents.<profile>].worktree` 可显式选择 `shared` 或 `dedicated`；任务结束不自动删除 Worktree。下一步实现结构化 Diff 回流、冲突预检和合并审批。

v0.20.0-rc3（已完成）：Web 工作区 API 与选择器改为读取 Runtime 注册表，并与 `--workspace/--web-workspace` 白名单取交集；返回稳定 ID、名称、active 和访问模式，Composer Skills 应用 Workspace 允许列表，新会话优先 Workspace Provider。自动注册采用原子 `ensure`，不会覆盖用户已有策略。按 Coding Agent 语义，允许目录默认 `workspace-write`（文件写入免审），Shell、MCP、网络和越界访问继续审批；`read-only` 仅显式启用。下一步进入子 Agent 专属 Worktree 与 Diff 回流。

v0.20.0-rc2（已完成）：TUI 新增 `/workspace list|switch <id>` 并进入 `/` 候选；切换前保存当前 Session 状态，恢复或创建目标 Workspace Session，重建 Runtime 事件跟随、右栏状态、Skills 与移动会话指向。旧 Workspace 后台任务继续运行；为防止复用启动时绑定旧路径的进程内工具，跨 Workspace 后 `/local` 保守拒绝。下一步让 Web 使用 Runtime Workspace 注册表，而非仅使用启动参数白名单。

v0.20.0-rc1（已完成）：新增持久 Workspace 注册表与受 Token 保护的注册、更新、列表、激活、删除 API/CLI；任务和 Session 自动收养目录，默认 Provider、Skill/MCP 允许列表进入 Harness。访问策略只能由 Runtime 注册表注入，客户端字段不参与反序列化；只读策略在审批前阻止 Shell、文件写入、Worktree、MCP 和 Editor 子 Agent。切换默认 Workspace 不修改既有 Task/Session 根目录，删除注册不删除文件或历史。下一步接入 TUI/Web Workspace 切换器与路径边界重建。

v0.19.0-rc8（已完成）：TUI Inbox 的完成/取消 Runtime 任务仅保留 5 分钟；等待审批/回答的任务详情与实际 Interaction 建立 Task ID 关联，点击任务或按 Enter 会直接打开可操作的审批/回答框。下一步继续阶段 6 的 Workspace 注册表。

v0.19.0-rc7（已完成）：Runtime 在主/子 Agent 的潜在写工具调用窗口前后采集内容指纹，新增持久 Diff Attribution 链和受保护 API/CLI；TUI Diff Review 在文件行展示最近责任 Agent 与工具。预先存在但窗口内未变化的脏文件不会被误归属。阶段 5 已完成，下一步进入多 Workspace/Worktree 注册、切换和权限隔离。

v0.19.0-rc6（已完成）：Runtime/CLI/TUI 加入只读 Commit Preview，基于精确 Diff 快照展示提交消息、分支、暂存状态、脱敏 Remote、推送目标和可选 Tag；敏感文件/凭据、冲突、空暂存区、Detached HEAD 与无效目标会阻断确认。同时增加聊天纯净度回归测试，确保轮次和内部 ID 不进入对话区。下一步完成 Diff 的 Turn/Agent 归属绑定。

v0.19.0-rc5（已完成）：常见前后台测试命令自动把命令、退出码、状态与有界摘要绑定到完成时的精确 Diff 快照；Runtime API、CLI 和 TUI 共用记录，普通或疑似含凭据命令不持久化。

v0.19.0-rc4（已完成）：修复 TUI/Web 隐式启动 Runtime 时启动提示穿透 Ratatui、污染 Prompt 区域；提交确认和 Turn/Agent/Runtime ID 不再进入聊天记录，AI 完成消息只显示真实返回内容。显式 `daemon start` 仍保留控制台反馈。

v0.19.0-rc3（已完成）：Runtime/CLI/TUI 加入持久审查决策和精确快照安全撤销；TUI 支持接受、打回、请求修改、标记已审及撤销二次确认，未跟踪/新增内容移入 Recovery，冲突文件拒绝自动撤销。

v0.19.0-rc2（已完成）：TUI Diff Review 加入 Unified/Side-by-side 双视图、Combined/Staged/Unstaged 范围切换、当前文件搜索、高亮和前后跳转；Diff 与通用渲染逻辑拆分后主 TUI 文件降至 3000 行以内。

v0.19.0-rc1（已完成）：Diff Review Center 首批加入带内容指纹的 Workspace 快照、受 Token 保护的 Runtime/CLI 文件 Diff API，以及 TUI `/diff` 文件导航、滚动和 Unified Diff 着色；路径穿越、符号链接逃逸、陈旧快照和超大输出均保守拒绝或限制。

v0.18.0-rc3（已完成）：Agent Mission Control 第二批加入有界结果报告持久化、CLI/TUI 独立 Agent 详情、运行中追加指令，以及补充指令正文投递后清除的脱敏审计。

v0.18.0-rc2（已完成）：Agent Mission Control 首批加入 Profile Token 总预算、执行超时、连续失败熔断，以及 Runtime/TUI 策略快照展示。

v0.18.0-rc1（已完成）：完成 Herdr 官方资料研究、许可证与架构边界文档、`integrations herdr status` 诊断命令，以及 Runtime Task → Herdr Pane 的去重、非阻塞聚合状态上报。Herdr 未安装或上报失败不影响 Harness。

## 6. 建议执行顺序

1. [x] 发布 `v0.17.0-rc3`：Web 与 TUI 共用 Runtime Session/Turn、持久事件、停止和历史。
2. [x] 发布 `v0.17.0-rc4`：主 Harness 迁入 Runtime 进程内生命周期，移除每 Turn 子进程过渡层。
3. [x] 发布 `v0.17.0-rc5`：完成异常退出、租约接管、孤儿状态收敛、恢复事件续传和 Pending Interaction 优雅停止测试。
4. [x] 已实现完整会话 Rename、快照/指定 Turn Fork、Fork Provider/模型覆盖、TUI 原地切换、归档、删除、导出和组合搜索。
5. [x] 实现请求幂等、能力协商以及 Unix Socket/Windows Named Pipe 跨平台本地传输。
6. [x] 实现 Agent Mission Control、预算限制、失败熔断、独立详情、补充指令和结果回流。
7. [-] 实现 Diff Review Center、多 Workspace 与安全 Worktree 合并；已完成内容指纹快照、统一 API/CLI 与首版 TUI Unified Review。
8. [ ] 稳定统一控制 API 与 Rust Client Library，让 TUI 不再直接持有 Harness 业务逻辑。
9. [ ] 让 Web、移动端和 Swift App 逐步迁移到统一 Runtime API。
10. [-] Herdr 首批状态上报已完成；继续补齐 Pane 关联、跳转、Workflow、插件和 Computer Use。
11. [ ] 完成可观测性、跨平台测试、安全审计与 Swift Harness 替换，发布 `1.0.0`。

关键路径固定为：`Runtime 持久化 → 会话恢复 → Agent 生命周期 → Diff/Workspace → 统一 API → Web/移动端/Swift 共用内核`。Herdr、Computer Use 和客户端视觉增强可以穿插推进，但不能形成独立于 Runtime 的第二套任务状态机。

## 7. 完整产品交付清单

本节是面向最终产品的逐项清单。上面的版本阶段决定实施顺序，本节防止某项体验或安全能力在跨版本推进中被遗漏。

### 7.1 统一 Runtime

- [x] Session、Root Agent、Turn、Execution Task 的稳定身份和持久状态。
- [x] 同一 Session 严格串行、请求幂等、取消、事件持久化和重启恢复。
- [x] TUI 普通输入使用 Runtime Session/Turn。
- [x] Web 使用相同 Session/Turn、历史和事件；浏览器只提交与观察，不再持有独立 Harness 生命周期。
- [ ] Headless CLI 使用相同 Runtime。
- [x] Harness 从每 Turn 子进程过渡为 Daemon 内原生 Future；异常退出明确收敛为 Interrupted。
- [-] 已清理重启后的孤儿 Task/Interaction/PID 并补写恢复事件；Session 并发写保护、能力诊断和更细粒度资源审计待完成。

### 7.2 Web 客户端

- [x] 新建、打开和继续 Runtime Session，用户消息即时显示。
- [x] 提交后暴露 Session、Turn 和 Root Agent ID，SSE 按游标续传持久事件。
- [x] 浏览器断开后任务继续，刷新后可重新附着，停止按钮取消真实 Turn。
- [-] 历史会话已恢复用户/助手消息；附件摘要和完整运行状态恢复待补齐。
- [x] 思考摘要固定一行，逐轮保留精简活动与聚合工具状态。
- [x] Composer 支持多行、`/`、`$`、文本粘贴、图片粘贴、预览和删除。
- [ ] 文本、图片、视觉模型降级、Skills 和审批全部复用 Runtime 协议。
- [ ] 桌面三栏、平板双栏、手机单栏；保持 React、Chakra UI、纯 CSR 和完整 i18n。
- [ ] 单用户且不内置认证，明确依赖 Nginx、VPN 或 SSH Tunnel，并提供安全部署说明。

### 7.3 TUI 聊天区与 Composer

- [x] 聊天滚动、搜索、最后一行完整显示、基础 Markdown、工具活动聚合和原生文本选择。
- [ ] 自动跟随可解除，新消息提示，按消息/轮次跳转，折叠长输出和代码块语法高亮。
- [ ] 表格横向查看，复制单条消息/代码块，导出 Markdown/JSON。
- [x] 多行编辑、鼠标定位、历史、文本/图片粘贴、附件删除、`/` 命令和 `$` Skills 候选。
- [ ] 选择、撤销/重做、按词移动、文件拖放、`@` 文件引用和路径补全。
- [ ] 大段粘贴折叠卡片、草稿自动保存、Token/附件/上下文占用提示。

### 7.4 TUI 状态侧栏

- [x] 焦点切换、折叠展开、滚动、Attention Inbox、后台任务与 Agent 基础状态。
- [ ] Session、Turn、后台任务和 Agent 树分组，显示模型、Token、费用、耗时、Diff 与待审批项。
- [ ] 查看详情、跳转轮次、停止、重试、重新运行和补充 Prompt。
- [ ] 可调宽度以及紧凑、标准、详细三种密度。

### 7.5 审批与 Ask User

- [x] Allow once、Disallow、窄作用域 Always Allow、智能审核和规则管理。
- [x] 工作区内安全写入及已确认的测试/只读过滤命令免审。
- [ ] 统一风险分类器覆盖只读、测试、格式化、网络、MCP、工作区外写入和破坏性操作。
- [ ] Always Allow 可按命令、工具、Session、Workspace 和永久规则精确授权，并可撤销和审计。
- [x] Ask User 支持候选、自由输入、单选和多选。
- [ ] 支持默认项、危险标记、超时、持久等待、移动通知和回答后恢复原 Turn。
- [ ] TUI、Web、CLI 和移动端共享完全相同的审批/提问状态机。

### 7.6 流式过程与多模态

- [x] Provider SSE、思考摘要、工具状态、TUI/Web 图片粘贴和视觉模型降级基础链路。
- [ ] 统一事件序号、断线续传、去重、背压和慢消费者保护。
- [ ] 明确区分思考、工具、审批、提问、后台任务和整理答案阶段。
- [ ] CLI 参数附件、文件拖放、图片压缩/格式转换、大文件摘要、哈希去重和持久附件。
- [ ] 发送附件前明确目标 Provider，并让视觉解析结果可查看和修正。

### 7.7 后台任务与子 Agent

- [x] 后台 Shell、完成/失败回流、主 Harness 唤醒、基础 `spawn_agent` 和稳定 Child Agent ID。
- [ ] Runtime 原生调度、等待/订阅、优先级、并发上限、日志分页、取消、重试和重新附着。
- [ ] 内置 deep-research、scout、reader、editor、tester、reviewer 和 general Profile。
- [ ] TOML Profile 定义 Provider、模型、Prompt、工具、Skills、权限、预算和递归能力。
- [ ] 主 Agent 自动或用户显式选择 Profile，子 Agent 结构化回流，支持并行和依赖图。
- [ ] 限制最大深度、并发、轮次、Token、费用和时长，并对连续失败熔断。

### 7.8 Agent Team 与 Herdr 借鉴

- [ ] Agent Team、共享任务看板、消息传递、自动汇总和可观察任务图。
- [ ] 每个 Agent 可绑定独立 Workspace、Worktree、PTY 和模型 Profile。
- [ ] TUI 可用树或网格查看、切换、接管、停止和重试 Agent。
- [ ] Agent 独立提交，Review Agent 审查，合并前检测 Worktree 冲突。
- [ ] Runtime/PTY/Agent Team 保持跨平台；Tmux 和 Herdr 仅作为可选适配器。
- [-] Herdr 生命周期上报和诊断已完成；Pane 关联、精确跳转及外部 Codex/Claude/OpenCode Agent 启动待完成。

### 7.9 Workspace、Git 与 Review Center

- [ ] 注册、删除、切换和多 Workspace Session，每个 Workspace 有独立权限、Provider、Skills 和 MCP。
- [ ] 只读/可写策略、路径边界重建、任务跨切换继续运行和并发修改保护。
- [ ] Worktree 创建、绑定、回收、孤儿检测、冲突检测和保守合并。
- [ ] 按 Turn/Agent/文件追踪 Diff，TUI Unified/Side-by-side Review。
- [ ] 接受、打回、请求重改、标记已审和不覆盖用户修改的安全撤销。
- [x] 测试结果、Commit Preview、敏感文件检查、Tag 和推送目标确认。

### 7.10 工具与浏览器能力

- [x] 文件、Git、Shell、Web Search/Fetch、MCP、Skills、后台任务和子 Agent 基础工具。
- [ ] 符号索引、引用查找、诊断、结构化测试结果、Git Log/Blame、下载和转换工具。
- [x] Web Fetch 支持常用同域重定向。
- [ ] 跨域重定向风险控制、循环/次数/大小/超时限制、HTML 正文、PDF 和引用来源。
- [ ] 浏览器截图与自动化；Cookie 和登录态必须明确授权并防止 SSRF/云元数据访问。
- [ ] 盘点 Xedit 工具并以统一权限描述逐项迁移。

### 7.11 远程、移动与 Computer Use

- [ ] Rust 客户端通过 `j.niuwoai.com` WebSocket 注册、心跳、重连、确认和去重。
- [ ] 移动端查看 Session/Agent/Inbox，回答问题、审批、停止、重试并接收后台完成通知。
- [ ] 网关只中继，不保存 Provider Key；多设备冲突、附件和端到端安全有明确协议。
- [ ] macOS Computer Use 接入 Accessibility、ScreenCaptureKit 和受控输入，所有动作结构化、可审计、可停止。
- [ ] Windows UI Automation 与 Linux Wayland/X11 按能力协商降级。
- [ ] Computer Use 使用独立权限域且不能绕过 Workspace、审批或敏感信息策略。

### 7.12 CLI、配置、Skills 与上下文

- [ ] `willdeep run`、stdin、JSON/NDJSON、quiet、稳定退出码、Session 继续/查询/停止和附件参数。
- [ ] Bash、Zsh、Fish、PowerShell 补全和 man page。
- [x] TOML Provider/Profile、API Base、API Key 环境变量引用和 some.im 区分。
- [ ] 多 Provider 能力、视觉回退、审批、Workspace、Runtime、Web、移动网关和分层覆盖配置。
- [ ] `config init/check/show`，系统 Keychain/Secret Service，严格避免密钥进入仓库或日志。
- [x] Skills 名称/描述发现、按需正文读取和 `$` 触发。
- [ ] Skills 分层、版本、参数、权限、安装、更新、禁用和执行审计。
- [x] `/compress` 手动压缩。
- [ ] 自动阈值、关键事实保留、工具摘要、可查看/撤销压缩和 Provider Context 自适应。

### 7.13 跨平台、质量与发布

- [x] macOS、Windows x64、Linux AMD64/ARM64 标签构建和 GitHub Release 工作流。
- [ ] 统一 PTY、剪贴板、图片粘贴、系统通知和文件打开抽象。
- [ ] Runtime 单测、进程 E2E、TUI PTY E2E、Web SSE/停止/恢复 E2E 和跨平台 CI。
- [ ] Clippy、格式化、依赖审计、配置兼容、崩溃恢复和长时间资源上限测试。
- [ ] Release 校验和、安装脚本、包管理器、快速开始、配置、安全、Profile 和协议文档。
- [ ] 每批代码同步版本、CHANGELOG、PRODUCT_OVERVIEW、本路线图、测试、Commit、Tag 和推送。

### 7.14 Swift Harness 替换验收

- [ ] Swift App 先只读观察 Rust Runtime，双读验证 Session、Agent、审批和事件一致性。
- [ ] Rust 接管后台任务、子 Agent 和持久事件，再接管完整 Harness、Tools、Skills、MCP 与 Provider。
- [ ] 完成数据迁移、故障回退和跨平台一致性测试后移除 Swift 旧 Harness。
- [ ] 替换前必须证明会话、附件、审批、事件和用户已有修改在迁移与回退中零丢失。
