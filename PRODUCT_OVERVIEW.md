# Product Overview

> 最后更新：2026-08-10 | 当前版本：v0.19.0-rc4

## 项目简介

WillDeep CLI 是跨平台 AI Coding Agent 客户端。当前阶段通过用户提供的 API Base、API Key 和模型 ID，在受限工作区内完成模型推理、工具执行和结果验证。

## 核心功能

- Chat Completions、Responses、Anthropic Messages 三协议；
- some.im 与 BYOK Provider；
- 文件搜索、读取、创建和精确编辑；
- Git 状态与 Shell 命令；
- 多轮 Tool Call Harness；
- 工作区路径边界和写操作审批；
- 人类输出与 NDJSON 自动化输出；
- TOML 多 Provider Profile 与安全凭据引用；
- Ratatui 多轮 TUI、可滚动聊天记录、聚合工具活动和界面内审批；
- 空白新会话的即时工作区欢迎引导；
- 多行 Prompt 编辑、鼠标光标定位、文本粘贴附件和可删除图片附件；
- TUI 与 Web 的简体中文、英语、日语界面及持久语言偏好；
- TUI 临时单行思考摘要，以及 Web 单行工作状态、逐轮工具轨迹与停止生成；
- Web/TUI 独立聊天历史滚动与 TUI 常用 Markdown 终端渲染；
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
- 跨平台 Runtime Daemon 提供 `start/status/stop/logs`，以受 Token 保护的本地回环控制 API、原子状态和私有日志独立于 TUI 运行；
- Runtime 事件以 NDJSON 和单调序号持久化，`attach --after` 支持按游标补读并安全分离客户端；
- Runtime 提供受 Token 保护的 SSE 事件流，按游标分页补历史后切换实时广播；慢客户端从持久日志恢复，TUI 长连接消费并对旧 Daemon 保留轮询降级；
- 非交互 Harness 可通过 Runtime 提交、查询和取消；Daemon 直接持有进程内 Harness Future，并把模型输出、session_id 和终态写入可续传事件流，不再为每个 Turn 启动 CLI 子进程；
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
