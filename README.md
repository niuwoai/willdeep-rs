# WillDeep CLI

WillDeep CLI 是跨平台 Coding Agent 的第一阶段实现。它接受一个 API Base、API Key 和模型名称，在本地工作区中运行模型—工具循环。

当前版本为 `0.21.0-rc7`，支持：

- OpenAI Chat Completions；
- OpenAI Responses；
- Anthropic Messages；
- some.im 自动识别和显式 Provider 模式；
- `search_files`、`grep_files`、`read_file`、`list_directory`、`git_status`；
- `run_command`、`create_file`、`edit_file`；
- `web_search`、安全公网 `web_fetch`，以及 `rg` 优先的工作区搜索；
- 工作区路径隔离；
- `strict`、`smart`、`workspace-write` 三档审批模式；
- 面向自动化的 NDJSON 事件输出；
- `~/.willdeep/config.toml` 多 Provider 配置；
- Ratatui 交互界面与同一套审批机制；
- 版本化 JSON 会话持久化、列表和恢复；
- Codex/WillDeep 兼容的 `SKILL.md` 发现、列表和安全资源读取；
- MCP stdio server 初始化、工具发现、命名空间注册与调用。
- `/goal`、`/compress`、`/webapp`、`/skills`、`/clear`、`/help` 命令及 `$skill-name` 显式技能引用，TUI 与 Web 均提供 `/` 命令及 `$` 技能候选；
- `/mobile` 二维码入口，通过 `j.niuwoai.com` WebSocket Relay 连接 WillDeep Mobile；
- GitHub Actions 的三系统测试、Linux AMD64/ARM64 交叉测试、WSL ABI 烟测和 tag 自动发布。
- some.im 纯文本模型通过同一账号下的视觉模型理解图片；
- 上下文自动摘要压缩、Token/耗时状态和宽屏后台状态栏。
- 后台 Shell Job 完成/失败自动回流主 Harness；
- 四种可独立绑定模型的子 Agent Profile：scout、reader、deep、editor。
- 子 Agent Profile 可配置 Token 总预算、执行超时和连续失败熔断；Runtime/TUI 显示轮次、Token 与时限策略。
- Runtime 持久保存子 Agent 的有界最终报告；CLI `daemon agent` 与 TUI 详情层可审计查看，运行中后台 Agent 可通过 `daemon instruct-agent` 或 TUI `/agent instruct` 接收补充指令。
- Runtime 提供带内容指纹的 Workspace Diff 快照与文件内容 API；CLI 可脚本化查询，TUI `/diff` 可浏览文件、增删统计和着色 Unified Diff。
- TUI Diff Review 支持 Unified/Side-by-side 切换、Combined/Staged/Unstaged 范围切换，以及当前文件内搜索和前后匹配跳转。
- Diff Review 支持接受、打回、请求修改和标记已审；安全撤销要求精确快照并二次确认，未跟踪内容移入可恢复回收区。
- 常见前后台测试命令完成后自动把命令、退出码、结果和有界摘要绑定到当时 Diff 快照；疑似含凭据命令拒绝记录。
- TUI Diff Review 可生成只读 Commit Preview，汇总提交消息、暂存/未暂存文件、分支、脱敏后的 Remote/推送目标和可选 Tag；敏感文件、疑似凭据、冲突、空暂存区或无效目标会明确阻止确认。
- Runtime TUI 聊天区只显示用户消息和 AI 最终回复；轮次、Task/Agent ID、工具活动和提交状态仅进入活动状态层。
- Runtime 在可能写入工作区的主/子 Agent 工具调用前后采集内容指纹，将真实变化路径绑定到 Session、Turn、Task、Agent 和 Tool；CLI `daemon diff-attributions` 与 TUI Diff Review 可沿快照链查看归属，调用窗口外已有脏文件不会被误算。
- TUI Inbox 的已完成 Runtime 任务仅保留 5 分钟；点击或 Enter 打开等待审批的任务时，直接进入可执行 Allow、Disallow、Always Allow 的审批框。
- Runtime 持久 Workspace 注册表、默认工作区切换与受 Token 保护的 CRUD API；每个 Workspace 保存独立访问策略、默认 Provider、Skill/MCP 允许列表。
- Workspace 的 `read-only` 策略由 Runtime 服务端注入，客户端无法自报可写；Shell、文件写入、Worktree 创建、MCP 和 Editor 子 Agent 会在审批前被拒绝。
- TUI 新增 `/workspace list` 与 `/workspace switch <id>`；切换后重载目标 Session、Workspace 状态、Skills、Runtime 事件跟随和右栏视图，原工作区后台任务继续由 Daemon 运行。
- Web 工作区选择器读取 Runtime 注册表的名称、当前项和访问模式，同时继续与 `--workspace/--web-workspace` 启动白名单取交集；被移除或未授权的目录不会因浏览器请求重新开放。
- `ask_user` 候选选择、多选和自由输入，以及 Allow once / Disallow / Always allow 审批。
- `willdeep daemon start/status/stop/logs` 跨平台本地 Runtime 控制面。
- Runtime 进程内持有的非交互 Harness Future、任务提交、查询、取消及断线后事件补读，不再为每个 Turn 启动 CLI 子进程。
- Runtime 后台审批与 ask_user 待处理列表、跨客户端解决和原任务续跑。
- TUI Runtime Inbox、远端审批/回答/停止，以及 `/runtime` 可分离任务提交、附件透传和按事件游标恢复聊天时间线。
- Runtime Root Agent 持久生命周期、受 Token 保护的 Agent 查询 API/CLI，以及 TUI 右栏 Agent 状态摘要。
- `spawn_agent` 稳定 UUID、Root→Child 父子关系以及子 Agent 轮次、工具、Token 和终态的 Runtime/TUI 实时展示。
- Runtime Session 重命名、完整快照或指定已完成 Turn 的精确 Fork、Fork 时切换 Provider Profile/模型、归档/取消归档、带确认删除、安全 JSON 导出，以及文本、Workspace、状态、Profile、模型和时间组合搜索；CLI、TUI `/session` 与 Web 会话操作共用同一受保护状态机。
- 可选 Herdr 集成：`willdeep integrations herdr status [--json]` 诊断 CLI/Pane/Socket；在 Herdr Pane 内将 Runtime Task 聚合为 `working/blocked/idle` 并非阻塞上报，Herdr 缺失或失败不影响任务。

当前暂不包含 Computer Use 与 Browser Use；Runtime Daemon 控制面和进程内 Harness 已经可用，会话恢复、统一客户端 API 与后台资源恢复仍在继续完善。

## Runtime Daemon

Daemon 管理命令不要求先配置 Provider，可独立启动和检查：

```bash
willdeep daemon start
willdeep daemon status
willdeep daemon logs --lines 100
willdeep daemon submit --workspace . --profile some-im "检查项目并运行测试"
willdeep daemon tasks
willdeep daemon task <task-id>
willdeep daemon agents
willdeep daemon agent <agent-id>
willdeep daemon register-workspace . --name "项目" --access workspace-write --provider-profile some-im --skill reader --mcp-server docs
willdeep daemon workspaces
willdeep daemon activate-workspace <workspace-id>
willdeep daemon remove-workspace <workspace-id> --yes
willdeep daemon create-session --workspace . --profile some-im --title "长期任务"
willdeep daemon sessions
willdeep daemon session <session-id>
willdeep daemon search-sessions "关键词"
willdeep daemon rename-session <session-id> "新名称"
willdeep daemon fork-session <session-id> --title "分叉名称"
willdeep daemon archive-session <session-id>
willdeep daemon unarchive-session <session-id>
willdeep daemon export-session <session-id> --output session.json
willdeep daemon delete-session <session-id> --yes
willdeep daemon submit-turn <session-id> --request-id <uuid> "继续处理下一步"
willdeep daemon turns <session-id>
willdeep daemon turn <turn-id>
willdeep daemon stop-turn <turn-id>
willdeep daemon stop-agent <agent-id>
willdeep daemon retry-agent <agent-id>
willdeep daemon cancel <task-id>
willdeep daemon pending
willdeep daemon resolve <interaction-id> allow-once
willdeep api session.list --ndjson
willdeep api event.stream --params-file events.json --ndjson
willdeep daemon resolve <interaction-id> deny
willdeep daemon resolve <interaction-id> always-allow
willdeep daemon answer <interaction-id> "自由输入答案"
willdeep attach --after 0
willdeep detach
willdeep daemon stop
```

本地控制端点只监听随机 `127.0.0.1` 端口，认证 Token 保存在 `$WILLDEEP_HOME/runtime/daemon.json`。Daemon 通过短周期心跳续租单实例锁；异常退出后，一次 `daemon start` 会等待旧租约过期并安全接管。Unix 下状态文件与日志权限为 `0600`；不要复制或提交该运行时目录。

任务或 Session 首次使用目录时会自动注册 Workspace；也可用 `register-workspace` 显式保存名称、`read-only/smart/workspace-write`、默认 Provider Profile、Skill 与 MCP 允许列表。Coding Agent 的默认语义是 `workspace-write`：Workspace 内 `create_file/edit_file` 免审，Shell、MCP、网络和越界访问仍走原审批；`read-only` 仅在用户显式选择时启用。`activate-workspace` 只改变新客户端使用的默认项，已启动任务继续绑定原规范化根目录。`remove-workspace --yes` 仅移除注册信息，不删除文件、Session 或历史。Workspace 策略由 Runtime 在任务入队时覆盖客户端输入，非空 Skill/MCP 列表作为允许列表，空列表保持全局配置兼容行为。

TUI 中输入 `/workspace list` 可查看注册表，输入 `/workspace switch <WORKSPACE_ID>` 可原地切换 Runtime 视图。切换会保存当前 Session 的 Inbox/事件游标，恢复目标 Workspace 最近 Session（没有则创建），重启该 Workspace 的事件跟随并清空旧右栏瞬态；Daemon 中旧 Workspace 的任务不受影响。由于进程内 Local Harness 的工具边界在启动时固定，跨 Workspace 切换后 `/local` 会保守禁用；从目标目录重新启动 TUI 后才可再次使用 `/local`。

Runtime 事件按递增序号写入私有 NDJSON 日志。`GET /v1/events/stream?after=<序号>` 使用 SSE 先分页补齐持久事件，再实时推送新事件；慢客户端落后广播窗口时自动从日志追赶并按序号去重。TUI 默认使用该实时通道，连接旧 Daemon 时回退到轮询接口。`attach --after <序号>` 同样按游标补读并持续跟随；按 `Ctrl+C` 只断开当前客户端，Daemon 继续运行。

`daemon submit` 会在必要时自动启动 Runtime，并立即返回任务 ID。Prompt 保留在 Runtime 内存与受保护的 Session/Turn 存储中，不会出现在进程参数中。Daemon 直接调度进程内 Harness Future；任务执行、模型输出、最终 session_id 和终态都可以在 `attach` 事件流中恢复，`daemon cancel` 可停止仍在运行的任务。

`daemon create-session` 创建长期 Runtime Session，并同时建立同 ID 的 Core Session 和生命周期稳定的 Root Agent。`submit-turn` 使用客户端 `request_id` 幂等入队；同一 Session 的 Turn 严格串行，不同 Session 可并发。成功 Turn 写入 Core Session 后会清除队列中的私密 Prompt/附件副本；`stop-turn` 可取消排队中或运行中的 Turn。Daemon 重启后排队项继续调度，遗留 Running/Waiting 活动项明确标记为 Interrupted，并向事件日志补写可续传的 Task/Turn 中断事件。

Session 管理命令只允许在没有活跃或排队 Turn 时执行 Rename、Fork、Archive 和 Delete，避免覆盖 Harness 正在写入的历史。Fork 复制 Core 消息快照并创建新的 Session/Root Agent，不复制旧 Turn、Task、Interaction、事件游标或 Inbox 已读状态；`daemon fork-session <SESSION_ID> --through-turn <TURN_ID> --provider-profile <PROFILE> --model <MODEL>` 或 TUI `/session fork --through <TURN_ID> --profile <PROFILE> --model <MODEL> [名称]` 可精确保留到指定已完成 Turn 并切换推理配置。`daemon search-sessions` 支持 `--workspace/--status/--profile/--model/--updated-after/--updated-before`；TUI `/session search` 对应使用 `--workspace/--status/--profile/--model/--after/--before`。TUI 可用 `/session switch <SESSION_ID>` 在同一 Workspace 内原地切换，会话聊天只消费属于当前 Session 的 Runtime 事件。Export 不包含队列私密 Prompt、Runtime Token 或 Provider 凭据；Delete 必须显式使用 `--yes`。

TUI 的普通 Prompt 默认幂等收养当前 Core Session，并把输入作为该长期 Session 的新 Turn；`/runtime <任务>` 是相同路径的明确别名，`/local <任务>` 仅让单轮使用旧进程内 Harness。重新进入同一 TUI 会话后继续复用原 Root Agent 和历史上下文。用户输入立即显示，消息文件只由后台 Harness 写入，Turn 结束时 TUI 从唯一 Core Session 历史同步，避免前后台双写产生重复或丢消息。

后台 Harness 需要审批或调用 `ask_user` 时会进入 WaitingApproval/WaitingAnswer。`daemon pending` 查看待处理项，使用 `resolve` 或 `answer` 后，原进程内 Future 从等待点继续。Runtime 控制 Token 只保留在 Daemon 与 Harness 内存中，不会作为环境变量传给 Shell 或 MCP。

`willdeep daemon agents` 列出 Runtime 持有的结构化 Agent；`willdeep daemon agent <id>` 查看单个 Agent 的 Workspace、Profile、状态、轮次、当前工具和 Token。后台 Child Agent 可用 `stop-agent` 精确停止，进入终态后可用 `retry-agent` 重试；重试沿用同一 Agent UUID。对应查询和控制 API 与其他 Runtime API 一样必须携带私有 `x-willdeep-token`，命令通过持久队列交给所属的原 Harness 执行并确认。

在普通 TUI Composer 输入 `/runtime <任务>`，可把当前 Workspace、Profile 和输入附件交给 Runtime。右栏 Inbox 会同步该任务及其审批/提问；在待处理条目按 Enter 使用原有弹窗，选中任务按 `K` 停止。在右栏 Runtime 区展开后用 `↑/↓` 选择 Agent，按 `K` 停止运行中的后台 Child Agent，按 `R` 重试已结束的后台 Child Agent。退出 TUI 后任务继续运行；模型轮次、工具状态、用量和正式回复按持久事件游标补读。用户请求和正式回复同时写入 Session，重新进入后聊天记录完整恢复且不会重复插入。

## 构建

要求 Rust 1.94、Node.js 22 与 Yarn。Web 前端会嵌入最终二进制：

```bash
cd web
yarn install --frozen-lockfile
yarn build
cd ..
cargo build --release
```

产物位于 `target/release/willdeep`，Windows 下为 `target/release/willdeep.exe`。

## 快速开始

API Key 建议放在环境变量中，不要直接写进命令历史。

## TOML 配置

首次运行且没有配置时会自动进入交互式设置，也可随时执行 `willdeep --onboarding`。some.im 模式会输出浏览器登录 URL 并等待授权；发布构建需通过 `WILLDEEP_CLIENT_LOGIN_SECRET` 注入客户端登录密钥，该密钥不得提交到 Git。生成的配置在 Unix 上自动设为 `0600`。

默认配置文件为 `~/.willdeep/config.toml`。也可以通过 `WILLDEEP_HOME` 改变配置目录，或使用 `--config /path/to/config.toml` 指定文件。

```toml
version = 1
default_provider = "some-im"

[agent]
max_turns = 24
approval = "smart"

[providers.some-im]
provider = "some-im"
api = "chat-completions"
api_base = "https://some.im/v1"
api_key_env = "SOMEIM_API_KEY"
model = "deepseek-v4-flash"
vision_model = "qwen3-vl-plus"
context_window = 128000

[providers.openai]
provider = "openai-compatible"
api = "responses"
api_base = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
model = "gpt-5"

[providers.anthropic]
provider = "anthropic"
api = "anthropic-messages"
api_base = "https://api.anthropic.com"
api_key_env = "ANTHROPIC_API_KEY"
model = "claude-sonnet-4-5"
max_output_tokens = 16384
```

完整模板见 [config.example.toml](config.example.toml)。选择 Provider：

```bash
willdeep --profile anthropic --workspace . "检查当前项目"
```

如果只有一个 Provider，可以省略 `default_provider`；WillDeep 会自动选择唯一配置。优先级为：

```text
命令行参数 / WILLDEEP_* 环境变量
→ 当前 Provider Profile
→ Provider 专属环境变量
→ 内置安全默认值
```

配置中可以直接使用 `api_key = "..."`，但推荐使用 `api_key_env`。在 Unix 系统上，只要 TOML 中出现明文 `api_key`，文件权限必须为 `0600`，否则 WillDeep 拒绝启动：

```bash
chmod 600 ~/.willdeep/config.toml
```

界面语言支持简体中文、英语和日语。可在 `[agent]` 中设置 `language = "zh-CN"`、`"en"` 或 `"ja"`，也可用 `--language` / `WILLDEEP_LANGUAGE` 临时覆盖。Web 左栏可直接切换语言，选择会保存在当前浏览器。

## TUI 与会话

macOS 会同时发现 Swift WillDeep 的 Project 与历史会话：`willdeep --list-projects`、`willdeep --project "项目名"`、`willdeep --list-sessions`、`willdeep --resume <UUID>`。从 Swift 会话续聊时，结果保存为 `~/.willdeep/sessions` 下的 Rust 副本，不覆盖 Swift 原文件。

WillDeep 会加载 `~/.willdeep/CLAUDE.md`，以及工作区根的 `PRODUCT_OVERVIEW.md`、`AGENTS.md` 和 `CLAUDE.md`。Agent 可调用 `git_diff`、`list_worktrees`、`create_worktree`；TUI 右栏显示当前项目、分支、变更文件数与 worktree 数。

## Web 模式

本机使用：

```bash
willdeep --web --workspace /path/to/project
```

浏览器打开 `http://127.0.0.1:9847`。前端采用 React + Chakra UI + Vite 纯客户端渲染，用户消息立即显示；单行工作状态吸附在 Composer 上方，每轮工具调用在聊天区精简保留。发送按钮在运行时切换为停止按钮，断开 SSE 的同时终止对应 Harness。Composer 支持粘贴长文本和图片、发送前删除、`/` 命令候选及 `$` 技能候选。

允许多个明确授权的工作区：

```bash
willdeep --web \
  --workspace /path/to/main \
  --web-workspace /path/to/frontend \
  --web-workspace /path/to/backend
```

也可用 `--project <名称或UUID>` 一次载入 Swift Project 的全部文件夹。Web 服务只接受启动时 allowlist 内的规范化工作区，不能由请求传入任意目录。当前是单用户模式，不实现应用层鉴权；跨机器访问必须由 Nginx、VPN 或 SSH Tunnel 提供认证与 HTTPS，不应把端口直接暴露到公网。接口和 Computer Use 路线详见 [Xedit 工具能力对照](docs/XEDIT_TOOL_PARITY.md)，Herdr 取舍与集成边界见 [Herdr 研究与集成方案](docs/HERDR_RESEARCH_AND_INTEGRATION.md)。

终端中不带 Prompt 启动时会进入 TUI：

```bash
willdeep --profile some-im --workspace .
```

聊天记录默认自动跟随最新内容；手动查看历史后暂停跟随，回到底部后自动恢复：

| 按键 | 行为 |
|---|---|
| `F1`（空 Prompt 时也可按 `?`） | 打开全局快捷键帮助；`F1`、`?` 或 `Esc` 关闭 |
| `Ctrl+W` | 在 Prompt、聊天区与右侧状态栏间循环切换焦点 |
| `Ctrl+B` | 显示或隐藏右侧状态栏；窄终端中打开覆盖层 |
| `Ctrl+F` | 搜索聊天记录；`Enter`/`Shift+Enter` 前后跳转，`Esc` 关闭 |
| `Ctrl+P` | 搜索命令、Skills、会话、Agent/任务和工作区文件 |
| `↑` / `↓` | 在 Prompt 多行之间移动光标 |
| `←` / `→`、`Home` / `End` | 移动编辑光标；也可鼠标点击定位 |
| `Alt+↑` / `Alt+↓` | 按显示行滚动聊天记录 |
| `PageUp` / `PageDown` | 按页滚动 |
| 鼠标滚轮 | 浏览聊天历史；回到底部后恢复自动跟随 |
| `Ctrl+Home` / `Ctrl+End` | 跳到聊天顶部 / 回到底部并恢复自动跟随 |
| `Ctrl+O` | 展开或收起最近 Tool Use 明细 |
| `Ctrl+S` | 切换文本选择模式；启用后可拖选聊天文字并使用终端复制快捷键 |
| `Enter` | 发送 Prompt |
| `Shift+Enter` / `Alt+Enter` / `Ctrl+J` | 插入换行 |
| `Ctrl+Shift+V` 或 `Cmd+V` | 从本机系统剪贴板附加图片 |
| `Ctrl+D` | 删除当前（最近）附件，可重复删除 |
| `Ctrl+C` | 退出并恢复终端 |

Tool Use 默认显示为单条聚合活动摘要，例如：

```text
Tools: 6 calls · list_directory×3 · read_file×2 · git_status×1
```

失败数量不会被隐藏；需要排查时使用 `Ctrl+O` 查看最近明细。
运行时活动区显示最近三条可验证进度，包括当前轮次、工具调用/完成、上下文压缩和后台结果；不展示或伪造模型私有思维链。宽终端会在右侧显示 Agent、Mobile Relay、手机队列和工具完成情况。状态栏按 Provider Profile 的 `context_window` 显示上下文占比，并显示最近一次输入/输出 Token 与耗时。

输入 `/help` 可查看本地命令；`/goal <目标>` 会为后续消息持续注入目标约束，`/goal off` 关闭。输入 `/compress` 会立即调用当前 Provider 总结较旧历史，保留最近六条消息并保存当前会话；历史不足八条时不会消耗模型请求。Prompt 中的 `$skill-name` 会显式读取并附加对应 `SKILL.md`，`/skills` 可查看当前目录发现的技能。用户消息、助手回复、系统状态和错误使用不同颜色显示。

TUI 会按终端能力渐进渲染常用 Markdown：标题、粗体、行内代码、引用、列表、代码块和链接；原始会话内容仍以 Markdown 文本保存。

输入 `/mobile` 会连接 `wss://j.niuwoai.com/ws/broadcast/<room>` 并弹出配对二维码；使用 WillDeep Mobile 扫码后可从手机向当前 CLI 会话发消息。`Esc` 或 `/mobile hide` 只隐藏二维码，`/mobile show` 再次显示，`/mobile off` 断开 Relay。CLI 使用独立于 Swift App 的 room/token，凭据保存在 `~/.willdeep/mobile-relay.toml`；Unix 权限为 `0600`，不会写入仓库。CLI 不监听本地端口。

每次成功回复会原子保存到 `$WILLDEEP_HOME/sessions/<uuid>.json`。命令行可查看或恢复：

```bash
willdeep --list-sessions
willdeep --resume latest "继续检查刚才的问题"
willdeep --resume 550e8400-e29b-41d4-a716-446655440000 "继续"
```

终端启用 Bracketed Paste：短单行粘贴直接插入光标处，多行或超过 200 字符的内容显示为 `Pasted text` 附件行。图片粘贴会读取本机系统剪贴板、转为 PNG 并显示尺寸和大小；发送前可用 `Ctrl+D` 删除。SSH 会话无法读取远端电脑的系统剪贴板图片，这是终端边界，不会静默伪装成功。

## Skills 与 MCP

WillDeep 自动扫描工作区和用户目录下的 `.willdeep/skills`、`.agents/skills`、`.codex/skills`；每个 Skill 是包含 `SKILL.md` 的子目录。模型通过 `list_skills` 和 `read_skill` 按需加载，资源路径被限制在 Skill 目录内。

MCP 使用与 Codex 风格接近的 TOML：

```toml
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/safe/root"]
startup_timeout_seconds = 30
enabled = true
```

启动时完成 initialize 和 `tools/list`，远端工具暴露为 `mcp__filesystem__<tool>`。MCP 调用在所有审批模式下均逐次确认；`smart`、`workspace-write` 和兼容参数 `--full-auto` 只免审当前工作区内的创建、编辑操作。敏感值应由 MCP 子进程继承环境变量；不要把 Token 写入配置。

## 后台任务与子 Agent

模型可用 `run_command` 的 `run_in_background = true` 启动后台 Shell，立即获得 `job_xxxxxx`。任务完成、失败、超时或被取消后，CLI 会把带输出尾部的 `<background-task-notification>` 自动送回主 Harness；主 Agent 空闲时立即续跑，忙碌时在当前回复结束后续跑。`get_job_output` 查看捕获输出，`kill_job` 请求取消。非 TUI 模式会保持进程存活，直到相关后台任务完成并处理完回流结果。

`spawn_agent` 把自包含任务交给隔离上下文的子 Agent，可选择同步等待或 `run_in_background = true`：

| Profile | 用途 | 默认工具 |
|---|---|---|
| `scout` | 快速定位文件、符号和调用点 | search/grep/list/read |
| `reader` | 阅读和总结长文件或文档 | read/list/search |
| `deep` | 跨文件深入调查 | search/grep/read/list/git status |
| `editor` | 修改一个明确目标文件 | read/edit |

子 Agent 看不到父对话、不能询问用户、不能继续派生。`editor` 必须提供 `target_file`，主 Harness 会对 canonicalize 后的现有文件单独请求批准；批准后会创建专属 `willdeep/agent-<id>` Git Worktree，并把目标文件映射到隔离目录，子 Agent 仍只能修改这一个文件。Worktree 在任务结束后保留供审查，不会自动删除。

子 Agent 报告会附带有界的 Worktree 路径、分支和变更列表。可用 `willdeep daemon agent-worktree-review <AGENT_ID>` 获取绑定 Child/Root 精确快照的冲突预检；确认 Review ID 未陈旧后，执行 `willdeep daemon merge-agent-worktree <AGENT_ID> --review <REVIEW_ID> --yes`。TUI 中在 Agent 详情按 `W` 打开同一审查，再按 `M` 显式批准合并。Root 或 Child 任一侧变化、同文件冲突、未跟踪文件、未解决冲突或超大补丁都会阻断，系统不会自动合并。

`willdeep daemon worktrees-audit` 会列出 Active、Reviewable、Merged、Clean、Quarantined、Missing 和 Unknown 状态。只有终态且干净，或已按精确 Child 快照完成合并的 Worktree 才可执行 `quarantine-agent-worktree <AGENT_ID> --snapshot <CHILD_SNAPSHOT_ID> --yes`。该操作不会删除目录、文件或分支，而是通过 `git worktree move` 将完整 Worktree 移入 `~/.willdeep/recovery/worktrees/`；状态持久化失败时会尝试原路回滚。

统一控制协议从 `v0.21.0-rc1` 起由独立 `willdeep-runtime-protocol` crate 定义。`v0.21.0-rc7` 已覆盖 Workspace/Session/Turn/Agent/Event/Task/Approval/Question 与 Diff Review 公开 DTO；TUI 的事件、Inbox、控制、Workspace、Diff Center以及 Web Session 管理均通过共享 Runtime Client，修改请求使用跨重启 Request ID 幂等日志。运行 `willdeep daemon capabilities` 可读取受 Token 保护的协议版本、对象类型、操作名、传输方式与大小限制。完整契约见 [`docs/RUNTIME_CONTROL_API.md`](docs/RUNTIME_CONTROL_API.md)。

各工种可绑定不同模型：

```toml
[subagents.scout]
provider_profile = "some-im"
model = "glm-5"
max_turns = 8
context_window = 128000

[subagents.deep]
# 省略绑定时继承当前会话模型
max_turns = 12

[subagents.editor]
# 可显式改为 shared；内置 editor 默认为 dedicated
worktree = "dedicated"
```

## ask_user 与审批

模型需要用户做实质选择时可调用 `ask_user`，传入 `question`、可选 `options` 和 `multi_select`。TUI 弹层支持方向键选择、空格多选，也可以直接键入未列出的其他答案；普通终端支持输入序号或自由文本。用户答案经长度限制和标记转义后回到同一工具轮次。

需要审批的操作提供三种决定：

- `Y`：Allow once，仅当前调用；
- `N`：Disallow，拒绝当前调用；
- `A`：Always allow，仅在界面明确显示该选项时可用。

Always Allow 不是全局“免死金牌”：Shell 只记住规范化后的完整命令，MCP 只记住精确 `server/tool`。含管道、重定向、命令连接符或换行的 Shell 命令，以及文件写入、网络重定向、任务取消和 editor 授权都不提供持久放行。规则存放于 `$WILLDEEP_HOME/always-allow.json`，Unix 权限为 `0600`：

未显式配置时默认采用 `smart`。该模式还会免审 `cargo test`，以及它后面只由 `grep`、`head`、`tail` 构成的输出过滤管道；`tee`、重定向、`&&`、命令替换和其他 Cargo 子命令不在此范围内。

```bash
willdeep --list-approvals
willdeep --clear-approvals
```

### some.im

```bash
export SOMEIM_API_KEY="<your-key>"
cargo run -p willdeep -- \
  --provider some-im \
  --model deepseek-v4-flash \
  --workspace ~/Sites/project \
  "检查当前仓库并修复测试"
```

`--provider some-im` 默认使用 `https://some.im/v1`。也可以显式传入国内线路：

```bash
export WILLDEEP_API_BASE="https://api.niuwoai.com/v1"
```

some.im 请求使用 Bearer API Key，并附带不含本地路径的 `x-willdeep-session-id` 与 `x-willdeep-workspace-id`。

当 some.im 主模型属于已知纯文本模型且消息包含图片时，WillDeep 会使用同一 API Base、同一 API Key 调用 `qwen3-vl-plus` 描述图片，只把描述文本交给主模型。可用 Profile 的 `vision_model` 覆盖视觉模型。`web_search` 也复用该 some.im 配置；`web_search`、`web_fetch` 在所有审批模式下都需要确认。`web_fetch` 拒绝私网、回环和链路本地地址；同 hostname 重定向自动跟随，跨 hostname 重定向重新审批，HTTPS 降级到 HTTP 一律拒绝。

### OpenAI-compatible Chat Completions

```bash
export WILLDEEP_API_BASE="https://provider.example/v1"
export WILLDEEP_API_KEY="<your-key>"
export WILLDEEP_MODEL="model-id"

cargo run -p willdeep -- --api chat-completions --workspace . "解释这个项目"
```

### OpenAI Responses

```bash
cargo run -p willdeep -- \
  --api-base https://api.openai.com/v1 \
  --api responses \
  --model gpt-5 \
  --workspace . \
  "检查 Git 状态并总结风险"
```

### Anthropic Messages

```bash
export ANTHROPIC_API_KEY="<your-key>"

cargo run -p willdeep -- \
  --provider anthropic \
  --api anthropic-messages \
  --model claude-sonnet-4-5 \
  --workspace . \
  "阅读 README 并提出改进建议"
```

原生 Anthropic 使用 `x-api-key` 和 `anthropic-version: 2023-06-01`。API Base 可写成 `https://api.anthropic.com` 或带尾部 `/v1` 的形式。

## API 与 Provider 是两个维度

`--provider` 决定身份、鉴权和 Provider 专属请求头；`--api` 决定请求/响应的线格式。

```text
--provider auto|openai-compatible|some-im|anthropic
--api auto|chat-completions|responses|anthropic-messages
```

自动模式规则：

- `some.im`、`api.some.im`、`api.niuwoai.com` 识别为 some.im；
- `api.anthropic.com` 识别为 Anthropic Messages；
- 其他 API Base 默认为 OpenAI-compatible Chat Completions；
- Responses 需要显式指定 `--api responses`。

这两个选项可以组合，例如 some.im Relay 提供 Anthropic Messages 线格式时，可使用：

```bash
willdeep --provider some-im --api anthropic-messages ...
```

此时仍采用 some.im Bearer 鉴权，而消息体使用 Anthropic 格式。

## 审批与自动化

只读工具默认执行。审批模式与 Swift App 对齐为三档：

- `strict`：创建、编辑、Shell、MCP 都逐次审批；
- `smart`（默认）：当前工作区内的创建、编辑免审；另精确放行 `cargo test` 与其只读输出过滤管道，其他 Shell、MCP、网络仍审批；
- `workspace-write`：与当前 Rust 阶段的 `smart` 保持相同安全边界，为后续自动审核器预留独立语义。

需要审批时显示：

```text
Approval required: edit file: src/main.rs
Allow once? [y/N]
```

在 CI 或已隔离容器中可以使用：

```bash
willdeep --full-auto --json ...
```

非交互输入下，`smart` / `workspace-write` 允许当前工作区内的创建和编辑；`smart` 另允许上述测试管道。其他 Shell、MCP 和外部操作仍因无法交互审批而拒绝。Harness 会把拒绝结果返回模型，不会静默放行。

## 配置环境变量

| 环境变量 | 用途 |
|---|---|
| `WILLDEEP_API_BASE` | API Base |
| `WILLDEEP_API_KEY` | 通用 API Key |
| `WILLDEEP_MODEL` | 模型 ID |
| `WILLDEEP_CONFIG` | 显式 TOML 配置文件路径 |
| `WILLDEEP_HOME` | WillDeep 配置目录，默认 `~/.willdeep` |
| `WILLDEEP_LANGUAGE` | 界面语言：`zh-CN`、`en` 或 `ja` |
| `SOMEIM_API_KEY` | some.im Key 回退 |
| `ANTHROPIC_API_KEY` | Anthropic Key 回退 |
| `OPENAI_API_KEY` | OpenAI-compatible Key 回退 |

API Key 优先级为显式 `--api-key` / `WILLDEEP_API_KEY`、Profile 的 `api_key`、Profile 的 `api_key_env`、Provider 专属环境变量。

## 验证

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

三种协议均有本地 Mock HTTP 契约测试，覆盖完整工具往返，不会调用真实 API，也不会消耗 Key。

## 许可证

Apache License 2.0。`WillDeep` 名称和商标不随源代码许可证授权。
