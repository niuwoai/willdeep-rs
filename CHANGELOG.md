# Changelog

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
