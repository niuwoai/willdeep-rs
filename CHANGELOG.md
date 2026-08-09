# Changelog

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
