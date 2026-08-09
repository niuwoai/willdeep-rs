# WillDeep CLI

WillDeep CLI 是跨平台 Coding Agent 的第一阶段实现。它接受一个 API Base、API Key 和模型名称，在本地工作区中运行模型—工具循环。

当前版本为 `0.13.0-rc4`，支持：

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
- `/goal`、`/compress`、`/skills`、`/clear`、`/help` 命令及 `$skill-name` 显式技能引用，TUI 与 Web 均提供 `/` 命令及 `$` 技能候选；
- `/mobile` 二维码入口，通过 `j.niuwoai.com` WebSocket Relay 连接 WillDeep Mobile；
- GitHub Actions 的三系统测试、Linux AMD64/ARM64 交叉测试、WSL ABI 烟测和 tag 自动发布。
- some.im 纯文本模型通过同一账号下的视觉模型理解图片；
- 上下文自动摘要压缩、Token/耗时状态和宽屏后台状态栏。
- 后台 Shell Job 完成/失败自动回流主 Harness；
- 四种可独立绑定模型的子 Agent Profile：scout、reader、deep、editor。
- `ask_user` 候选选择、多选和自由输入，以及 Allow once / Disallow / Always allow 审批。

当前暂不包含 Computer Use、Browser Use 与常驻后台 daemon；子 Agent 与可回传主 Harness 的后台任务已经可用。

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

也可用 `--project <名称或UUID>` 一次载入 Swift Project 的全部文件夹。Web 服务只接受启动时 allowlist 内的规范化工作区，不能由请求传入任意目录。当前是单用户模式，不实现应用层鉴权；跨机器访问必须由 Nginx、VPN 或 SSH Tunnel 提供认证与 HTTPS，不应把端口直接暴露到公网。接口和 Computer Use 路线详见 [Xedit 工具能力对照](docs/XEDIT_TOOL_PARITY.md)。

终端中不带 Prompt 启动时会进入 TUI：

```bash
willdeep --profile some-im --workspace .
```

聊天记录默认自动跟随最新内容；手动查看历史后暂停跟随，回到底部后自动恢复：

| 按键 | 行为 |
|---|---|
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

子 Agent 看不到父对话、不能询问用户、不能继续派生。`editor` 必须提供 `target_file`，主 Harness 会对 canonicalize 后的现有文件单独请求批准；批准后的子 Agent 仍只能修改这一个文件。

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
