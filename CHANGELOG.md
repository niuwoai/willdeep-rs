# Changelog

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
