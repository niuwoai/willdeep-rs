# Product Overview

> 最后更新：2026-08-11 | 当前版本：v0.21.0-rc36

## 项目简介

WillDeep CLI 是跨平台 AI Coding Agent 客户端。当前阶段通过用户提供的 API Base、API Key 和模型 ID，在受限工作区内完成模型推理、工具执行和结果验证。

## 核心功能

- Chat Completions、Responses、Anthropic Messages 三协议；
- some.im 与 BYOK Provider；
- 文件搜索、读取、创建和精确编辑；
- Web Fetch 对每次跳转重做公网目标校验，同域自动跟随、跨域重新审批，并以环路、次数、超时和流式 3 MiB 硬限制约束响应；
- Git 状态、Diff、受限提交历史与逐行归因，以及 Shell 命令；
- 多轮 Tool Call Harness；
- 工作区路径边界和写操作审批；
- 人类输出与 NDJSON 自动化输出；
- `willdeep run` 默认通过持久 Runtime 执行，支持 Prompt/stdin、文本与图片附件、Session 续接、断开后继续、text/JSON/NDJSON、静默模式和稳定退出码；`--local` 保留显式进程内兼容入口；
- Bash、Zsh、Fish、PowerShell 补全和 roff man page 从同一 Clap 命令树生成；
- 顶层 `willdeep session list/get/turns/stop` 查询持久会话并精确停止其 active Turn；
- `willdeep doctor [--json] [--bundle PATH]` 在不联系 Provider 的前提下生成本地就绪诊断或私有脱敏 ZIP；
- TOML 多 Provider Profile 与安全凭据引用；
- TUI Goal 按 Core Session 持久保存，重启以及 Session/Workspace 切换时恢复；
- Provider Profile、模型和配置按 Session 恢复；Skills/MCP 在每轮执行前按当前 Workspace 策略重新绑定，撤权立即生效；
- Daemon 重启后对“无工具活动且历史边界完全匹配”的活跃 Turn 自动重放；已写用户消息原样复用，存在副作用证据或歧义历史时完整保留并停止自动恢复；
- `willdeep config init/check/show` 可安全创建、严格校验并脱敏展示 TOML 配置；
- Ratatui 多轮 TUI、可滚动聊天记录、聚合工具活动和界面内审批；
- 空白新会话的即时工作区欢迎引导；
- 多行 Prompt 编辑、鼠标光标定位、文本粘贴附件和可删除图片附件；
- TUI 与 Web 的简体中文、英语、日语界面及持久语言偏好；
- TUI 临时单行思考摘要，以及 Web 单行工作状态、逐轮工具轨迹与停止生成；
- Web/TUI 独立聊天历史滚动与 TUI 常用 Markdown 终端渲染；
- Rust Runtime Client 为 Tool 与 Artifact 观察提供类型化方法，并通过本地 Socket 往返测试约束协议操作与 DTO；
- Rust Runtime Client 覆盖 Workspace、Session、Agent、Turn、Task、Approval、Question、Event 的高频观察与控制方法，TUI bridge 直接复用；
- 统一 `agent.spawn` API 与 Rust Client 可在活跃 Session 中创建稳定 ID 的后台只读子 Agent，并通过 `agent.wait` 观察完成；父级、Task 和 Workspace 均由服务端推导，外部调用不能选择写目标；
- Web Runtime 侧栏按当前 Workspace 展示 Agent、待审批/回答、关注项、Tool 与 Artifact 摘要，Agent 私有路径、报告和内部错误不会下发；
- Web Runtime 侧栏可解决三类审批、回答单选/多选/自定义问题，并停止、重试或补充后台 Agent；写操作重新验证 Workspace 和目标归属；
- Rust Runtime Client 覆盖 Diff 快照、内容、审查、验证、归因、Commit Preview 和安全撤销，TUI Diff Center 直接复用；
- Worktree Review、Merge、Audit、Quarantine 已进入统一 API 和 Rust Client，精确 Review/Snapshot ID 与确认字段继续约束写操作；
- 公共 API 兼容夹具覆盖 Runtime 的 11 类稳定对象，供 Swift、Android 和第三方客户端做跨语言解码回归；
- TUI 可切换终端原生文本选择与复制模式；
- TUI 全局快捷键帮助、Prompt/状态栏焦点高亮与状态行焦点提示；
- TUI 聊天搜索、高亮与匹配跳转，以及可点击、可滚动的状态栏和后台任务详情；
- TUI `Ctrl+P` 全局命令面板，可模糊搜索命令、Skills、会话、Agent/任务和工作区文件；
- TUI Prompt、聊天区和状态栏三态焦点循环，以及候选、审批和 ask_user 的鼠标操作；
- Core 统一 Agent、后台任务、审批与提问的运行状态、Attention 分组和父级状态聚合语义；
- TUI 右栏 Attention Inbox 按人工介入优先级聚合待处理、工作中和最近完成事项；
- Attention Inbox 支持键盘/鼠标选择、后台任务与子 Agent 详情跳转、停止运行项和标记已读；
- Inbox 已读状态随会话持久化，后台 Shell 与子 Agent 支持真实重试，任务结束时触发终端提示；
- Git 冲突和待审 Diff 使用内容指纹进入 Inbox，子 Agent 审批阻塞结构化上报，状态按 Agent→会话→Workspace 上卷；
- 跨平台 Runtime Daemon 提供 `start/status/stop/logs/upgrade`；Upgrade 以 draining 闸门拒绝新工作、保留活跃任务和持久排队 Turn，任务归零后由当前二进制接管。Headless 观察者识别 Runtime 身份更替后沿事件游标自动重附着；本机客户端优先通过权限为 `0600` 的 Unix Socket 或拒绝远程客户端的 Windows Named Pipe 通信，并兼容旧状态的受 Token 回环 TCP；
- Session/Turn CLI、TUI 与 Web 共用统一 Runtime Client；收养既有 Session 时由 Runtime 从 Core 存储恢复私有配置引用，公共协议不传输配置路径；
- Runtime Session 元数据使用可迁移 schema；schema 1 首次由 rc33 打开时先写私有原始备份，再原子升级为 schema 2。新建未命名会话以首次 Turn 生成本地有界标题，疑似凭据 Prompt 不复制到标题；显式 Rename、收养旧会话和 Fork 标题保持人工所有权；
- Runtime 持久记录主 Agent 与子 Agent 的 Tool Activity，支持按 Session、Turn、Task、Agent 和状态查询；公开记录只含工具名与生命周期，不含参数、输出或 Workspace 路径；
- 工具窗口内经内容指纹确认的真实文件变化会形成 Workspace Change Artifact；公开元数据包含来源快照和变更数量，文件路径与内容继续由受授权 Diff API 控制；
- TUI 右栏通过统一 API 显示当前 Workspace 的结构化 Tool/Artifact 数量、运行态和最近工具，不从聊天文本反推持久状态；
- Web 侧栏通过 Workspace 白名单约束的 Activity API 周期刷新结构化 Tool/Artifact 摘要，支持中英日显示；
- Runtime 事件以 NDJSON 和单调序号持久化，`attach --after` 支持按游标补读并安全分离客户端；
- Runtime 事件日志读写使用同一互斥边界，活跃追加期间的控制 API 不会读取到未完成的 NDJSON 末行；
- Runtime 提供受 Token 保护的 SSE 事件流，按游标分页补历史后切换实时广播；慢客户端从持久日志恢复，TUI 长连接消费并对旧 Daemon 保留轮询降级；
- 非交互 Harness 可通过 Runtime 提交、查询和取消；Daemon 直接持有进程内 Harness Future，并把模型输出、session_id 和终态写入可续传事件流，不再为每个 Turn 启动 CLI 子进程；
- Headless CLI 默认创建或续接同一 Runtime Session/Turn，按游标分页追平脱敏持久事件并按失败域维持自动化退出码；进程级敏感覆盖保守留在 `--local` 路径；
- CLI 与 Runtime 共用 Provider、视觉降级、审批、Skills、MCP、Tools、子 Agent Profile、会话写入和后台结果回流的 Harness Factory；Agent 事件直接写入 Runtime EventLog，不经过 stdout 文本中转；
- Runtime 使用带心跳的单实例租约锁协调并发启动；异常退出后一次启动请求可在旧租约过期后安全接管，重启时将遗留 Running/Waiting Task、Turn、Agent 标记为 Interrupted、取消 Pending Interaction，并补写可续传恢复事件；
- Runtime 优雅停止会先取消并收敛进程内 Harness Future，再关闭 HTTP Server，等待审批或回答的请求不会阻塞 Daemon 退出；
- Runtime Session/Turn API 与 CLI 提供稳定 Root Agent、幂等请求 ID、持久严格串行队列、排队/运行取消、终态事件和重启恢复；成功后 Core Session 保留唯一消息历史并清除队列私密正文；
- Runtime Session 支持 Rename、完整消息快照 Fork、Archive/Unarchive、精确确认 Delete、安全 JSON Export 和标题/消息 Search；活跃/排队会话禁止破坏性管理，Fork 不复制 Turn/Task/Interaction/游标/已读状态，Export 不包含队列私密正文或凭据；
- CLI、TUI `/session` 与 Web 会话侧栏接入统一管理 API；支持按已完成 Turn 的持久消息边界精确 Fork、Fork 时覆盖 Provider Profile/模型、组合搜索，以及 TUI 在同一 Workspace 内原地切换并按 Session 隔离聊天事件；Web 仅在已允许 Workspace 内操作，匿名 Web 不开放消息摘要全文搜索端点；
- 可选 Herdr 适配器从 Runtime Task 聚合状态并上报当前 Pane，提供不泄露 Socket 路径的诊断命令；Herdr 仅作终端承载和状态投影，不成为 Harness 状态来源；
- TUI 普通 Prompt 默认幂等收养当前 Core Session 并提交长期 Session Turn，`/runtime` 为明确别名、`/local` 为单轮兼容入口；即时展示输入，终态从 Harness 独占写入的 Core 历史同步，恢复 TUI 后继续同一 Root Agent 且不会双写消息；
- Runtime 托管任务支持持久审批和 ask_user 待处理项，可由其他 CLI 客户端允许、拒绝或自由回答后继续原 Harness；
- TUI 右栏统一展示 Runtime 任务与待处理项，支持远端审批、回答和停止；Composer 可用 `/runtime` 提交含文本或图片附件的可分离任务；
- TUI 按 Session 事件游标补读当前 Workspace 的 Runtime 模型、工具、用量与完成事件；用户请求和正式回复写入会话，退出重连后完整恢复且不重复；
- TUI 对话区仅展示用户输入、AI 正式内容和必要错误；Runtime Turn/Task/Agent ID、轮次与提交确认只进入状态栏、右栏或诊断接口；
- Runtime 持久维护 Root Agent 的 ID、父子关系预留、Profile、状态、轮次、当前工具与 Token；受 Token 保护的 Agent API/CLI 可查询，TUI 右栏显示当前 Workspace 的 Agent 摘要；
- `spawn_agent` 通过稳定 UUID 上报启动、轮次、工具、用量和完成事件；Runtime 持久建立 Root→Child 树，TUI 分层展示前后台子 Agent 的实时状态；
- 子 Agent Profile 支持 Token 总预算、执行超时和连续失败熔断；成功任务自动复位失败计数，Runtime 持久化策略快照，TUI 同行显示当前/最大轮次、已用/预算 Token 和时限；
- 子 Agent 结束时将最多 64 KiB 的报告随结构化生命周期写入 Runtime Agent；CLI 单 Agent 详情与 TUI Enter 详情层可查看，报告不会进入公开候选接口；
- 运行中的后台子 Agent 支持追加父级指令；Core 在下一次 Provider 请求前注入并在结束竞态时继续下一轮，Runtime 命令正文投递后立即清除，仅保留脱敏命令审计；
- Runtime 为已注册 Workspace 生成包含 HEAD、文件状态、暂存/未暂存范围、二进制标记、增删统计和内容指纹的 Diff 快照；读取文件 Diff 必须携带当前快照 ID，变更后拒绝陈旧审查结果；
- CLI `daemon diff-snapshot`、`daemon diff-file` 与 TUI `/diff` 共用受 Token 保护的 Review API；TUI 支持文件导航、滚动和 Unified Diff 语法着色；
- TUI Diff Review 可用 `V` 切换 Unified/Side-by-side、`S` 循环 Combined/Staged/Unstaged，并通过 `/` 搜索当前文件、Enter/Shift+Enter 或 N 前后跳转匹配；
- Diff 审查决定以精确 Snapshot/Workspace/文件为键持久化，支持 Accepted、Rejected、Changes Requested 和 Reviewed；TUI 用 A/D/C/M 操作并在文件列表显示结果；
- 单文件安全撤销必须通过快照一致性检查和 TUI 二次确认；tracked 内容按选择范围恢复 Index/Worktree，untracked 或 HEAD 中不存在的内容移入 Runtime Recovery 目录而非直接删除；
- `run_command` 自动识别 Cargo/Go/Python/Ruby/Swift/Node/.NET/Java/Make 等测试命令，在完成瞬间把命令、退出码、通过/失败/超时状态与最多 8 KiB 摘要绑定到精确 Diff 快照；前后台命令共用协议；
- Runtime Verification API、CLI `daemon diff-verifications` 与 TUI Diff Review 可查询绑定结果；普通 Shell 和疑似包含 API Key、Token、Secret、Password 的命令不会持久化；
- Runtime Commit Preview API、CLI `daemon diff-commit-preview` 与 TUI Diff Review 的 `P` 面板共用精确快照；预览提交消息、分支、暂存/未暂存文件、脱敏 Remote、推送目标与可选 Tag，并在敏感文件/凭据、冲突、空暂存区、Detached HEAD 或无效目标时返回阻断原因；该阶段只预览，不执行 Commit、Tag 或 Push；
- Runtime TUI 对话记录只保留用户输入与 AI 最终回复；轮次、Task/Agent ID、工具和排队状态保留在活动状态层，不污染聊天历史；
- Runtime 在 `create_file`、`edit_file`、Shell、Worktree、Computer Use 和 MCP 等潜在写工具的调用窗口前后采集工作区内容指纹，持久记录 `Session → Turn → Task → Agent → Tool → Paths`；主 Agent 与子 Agent 使用各自身份，已有脏文件只有在窗口内真实变化时才归属；
- Diff Attribution API、CLI `daemon diff-attributions` 和 TUI Diff Review 沿快照链显示文件最近责任 Agent 与工具；记录采用 Tool Window 置信度，共享工作区并发的强隔离由后续独立 Worktree 阶段完成；
- TUI Inbox 自动隐藏完成或取消超过 5 分钟的 Runtime 任务；等待审批/回答的任务与其 Interaction 建立直接关联，鼠标点击或 Enter 可进入实际审批/回答控件；
- Runtime 持久维护多 Workspace 注册表，提供注册、更新、列表、激活和保守移除 API/CLI；每项独立保存规范化根目录、访问策略、默认 Provider、Skill 与 MCP 允许列表，切换默认项不影响旧 Workspace 任务；
- Runtime 在任务入队时以服务端注册表覆盖客户端 Workspace 策略；只读 Workspace 在审批前阻止 Shell、文件写入、Worktree、MCP 与 Editor 子 Agent，默认 Provider 和非空 Skill/MCP 允许列表进入同一 Harness；
- TUI `/workspace list|switch <id>` 接入 Runtime 注册表；切换保存/恢复 Workspace 专属 Session 与事件游标，重启事件订阅、状态和 Skill 视图，不取消旧 Workspace 后台任务；启动时绑定旧路径的 `/local` 在跨 Workspace 后保守禁用；
- Web 工作区 API/选择器改读 Runtime 注册表，并与服务启动时的路径白名单取交集；展示当前项与 read-only/smart/workspace-write 模式，Composer Skills 使用 Workspace 允许列表；默认 Workspace 内文件写入免审，Shell/MCP/网络仍按审批策略执行；
- 内置 Editor 子 Agent 默认创建 `willdeep/agent-<id>` 专属 Git Worktree；已审批目标按根工作区相对路径映射，Runtime 持久显示实际目录、根目录和分支，Diff 归因在 Child Worktree 内采集，任务结束后保留供审查；
- 子 Agent 完成报告结构化回流有界 Worktree 状态；Runtime/CLI/TUI 提供两阶段 Worktree Review 与显式合并，Review ID 同时绑定 Child/Root 快照和二进制补丁，任一侧变化即拒绝陈旧操作，同文件冲突、未跟踪内容、未解决冲突和超大补丁均阻断；
- Runtime Worktree 审计识别 Active、Reviewable、Merged、Clean、Quarantined、Missing 和无 Agent 记录的 Unknown 目录；保守清理仅允许精确快照的终态干净/已合并 Worktree，并整体移动到 Recovery，保留所有内容、Git 关联和分支；
- 独立 `willdeep-runtime-protocol` crate 定义协议版本、Runtime/Workspace/Session/Agent/Turn/Tool/Task/Approval/Question/Artifact/Event 对象类别、稳定操作名、能力、传输类型、限制及统一成功/错误信封；Runtime 提供受 Token 保护的能力协商端点和 CLI；
- Runtime 提供统一 `POST /v1/api` 与可续传 NDJSON 事件流；修改类请求按 Request ID 有界去重，共享 `willdeep-runtime-client` 负责回环校验、鉴权、信封解析和流式解码；
- Session、Turn、Agent 与 Event 使用协议 crate 的公开 DTO；TUI 实时事件、Agent 观察/控制、审批和回答均通过共享 Runtime Client，公共事件边界兼容净化旧日志中的工具参数、输出、报告、路径和内部错误；
- Task、Pending Approval 与 Pending Question 也使用公开 DTO；TUI Inbox 和 Web/TUI 事件补读通过共享 Client，修改请求以 Pending→Completed 私有日志跨 Runtime 重启去重，不确定崩溃窗口拒绝自动重放；
- Workspace 与访问模式使用公开 DTO；注册、自动确保、激活、移除和 TUI/Web Workspace bridge 均通过共享 Client，服务端继续规范化路径并覆盖任务权限；
- Diff Snapshot、File、Review、Verification、Attribution、Commit Preview 与 Revert 使用协议 crate 的稳定 DTO；TUI Diff Center 的全部读写均通过共享 Runtime Client，精确快照、Workspace 授权、敏感命令过滤和 Recovery 撤销语义保持不变；
- Web Session 列表、重命名、指定 Turn Fork、归档/恢复、精确确认删除和显式导出通过统一 Runtime API 与共享 Client；浏览器仍先校验启动时 Workspace 白名单，公开 Session DTO 不返回配置路径、内部错误或排队 Prompt；
- TUI Session 搜索以及 TUI/Web Turn 提交与停止通过统一 Runtime API；文本和图片附件使用协议 crate DTO，服务端强制附件数量、文本长度、图片 MIME/尺寸、总载荷和 1 MiB Prompt 上限，客户端不能在 Turn 请求中覆盖 Workspace 或权限；
- TUI Prompt 可用 `/webapp` 在当前 Workspace 启动仅监听回环地址的内嵌 Web App，并以 `/webapp status` 查看地址和进程状态；启动继承当前配置/Profile，并等待健康检查成功后才报告完成；
- 后台子 Agent 的取消、失败和重试沿用稳定 Agent UUID；Runtime 通过受 Token 保护的持久命令队列向原 Harness 下发精确 stop/retry，CLI 与 TUI 均可操作并查看结果事件；
- Web 文本/图片粘贴附件、发送前删除、`/` 命令和 `$` 技能候选；
- Web 与 TUI 共用持久 Runtime Session/Turn；Web SSE 转发 Runtime 事件，真实停止 Turn，并可加载持久历史会话；浏览器断开不再杀死后台 Harness；
- JSON 会话持久化、列表与恢复；
- Codex 兼容 Skills 发现和按需读取；
- MCP stdio 工具发现、注册和调用。
- `/goal` 命令模式和 `$skill-name` 显式技能触发；
- 分阶段 CLI/TUI/Runtime 产品路线图与逐项验收状态；
- `/mobile` Relay 配对二维码和手机控制当前 CLI 会话；
- 区分角色的 TUI 配色；
- macOS Universal、Windows x64、Linux AMD64/ARM64 自动构建与 tag 发布。
- `rg` 优先、内置扫描兜底的跨平台文件搜索；
- some.im 纯文本模型的 `qwen3-vl-plus` 图片描述降级链路；
- 受审批保护的网页搜索和公网网页正文读取；
- 上下文用量、Token、耗时、自动摘要压缩及宽屏后台状态侧栏。
- `/compress` 手动压缩当前会话上下文并立即保存；
- 后台 Shell Job 与完成/失败后自动唤醒主 Harness 的结果回流；
- `spawn_agent` 前台/后台子 Agent，内置 scout、reader、deep、editor 工种并支持独立模型绑定；
- TUI 右栏实时后台任务状态、耗时及输出查询/取消工具；
- Core `ask_user` 候选单选/多选与自由输入交互；
- Allow once、Disallow、窄作用域持久 Always Allow 审批状态机及规则管理命令；
- 首次运行交互式 onboarding 与 some.im 浏览器登录；
- Swift Project 元数据和历史会话兼容读取；
- Git diff/worktree 原生工具与 TUI 工作区状态；
- 全局、项目根 `AGENTS.md` / `CLAUDE.md` 指令加载；
- React + Chakra UI 纯 CSR、SSE 进度、多工作区切换和并发限制的内嵌 Web 服务；

## 技术栈

- Rust 1.94；
- Tokio 异步运行时；
- Reqwest + rustls；
- Clap CLI；
- Ratatui + Crossterm TUI；
- Serde 协议编解码；
- ignore、regex、globset 工作区搜索。

## 项目结构

```text
crates/willdeep-core/   Agent Loop、Prompt、Provider、工具
crates/willdeep-cli/    参数解析、审批和终端事件输出
config.example.toml      Provider/Profile 配置模板
docs/                   架构与协议说明
```

## 运行

```bash
SOMEIM_API_KEY='<your-key>' cargo run -p willdeep -- \
  --provider some-im \
  --model deepseek-v4-flash \
  --workspace . \
  '检查项目状态'
```

生产使用时应通过环境变量或后续凭据存储提供 API Key，避免出现在命令历史中。

## 已知问题与后续

- [ ] Provider 原生 token streaming；当前 SSE 已实时传输 Harness 阶段、工具进度和最终回答。
- [ ] ACP/Codex App Server/Goose 接入；
- [ ] MCP Streamable HTTP 与 OAuth；
- [ ] 手机端工具审批和跨设备 Patch 审核；
- [ ] 更强的命令风险分类与平台沙箱；
- [ ] 流式真实 reasoning 摘要；当前单行区域显示可验证的运行阶段，不伪造模型思考内容；
- [ ] Swift/Rust 共享会话 schema 稳定后开放双向原地写入；当前采用安全副本。
- [ ] 抽取 Swift/Rust 共用的签名 Computer Use Helper 协议，再开放 AX 检查与短效控制租约。
- [-] 将 Harness、任务和会话生命周期迁入 Runtime Daemon；持久 Session/Turn、attach/detach、事件断线续传、TUI/Web Session/Turn 及 Runtime 进程内 Harness 已完成，跨重启恢复活动执行资源和统一客户端 API 尚待完成。
