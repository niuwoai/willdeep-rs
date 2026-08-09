# WillDeep CLI

WillDeep CLI 是跨平台 Coding Agent 的第一阶段实现。它接受一个 API Base、API Key 和模型名称，在本地工作区中运行模型—工具循环。

当前版本为 `0.3.0-rc2`，支持：

- OpenAI Chat Completions；
- OpenAI Responses；
- Anthropic Messages；
- some.im 自动识别和显式 Provider 模式；
- `search_files`、`grep_files`、`read_file`、`list_directory`、`git_status`；
- `run_command`、`create_file`、`edit_file`；
- 工作区路径隔离；
- 交互式写入/命令审批与 `--full-auto`；
- 面向自动化的 NDJSON 事件输出；
- `~/.willdeep/config.toml` 多 Provider 配置；
- Ratatui 交互界面与同一套审批机制；
- 版本化 JSON 会话持久化、列表和恢复；
- Codex/WillDeep 兼容的 `SKILL.md` 发现、列表和安全资源读取；
- MCP stdio server 初始化、工具发现、命名空间注册与调用。

当前暂不包含 Computer Use、Browser Use、多 Agent 与后台 daemon。

## 构建

要求 Rust 1.94 或更新版本：

```bash
cargo build --release
```

产物位于 `target/release/willdeep`，Windows 下为 `target/release/willdeep.exe`。

## 快速开始

API Key 建议放在环境变量中，不要直接写进命令历史。

## TOML 配置

默认配置文件为 `~/.willdeep/config.toml`。也可以通过 `WILLDEEP_HOME` 改变配置目录，或使用 `--config /path/to/config.toml` 指定文件。

```toml
version = 1
default_provider = "some-im"

[agent]
max_turns = 24
approval = "ask"

[providers.some-im]
provider = "some-im"
api = "chat-completions"
api_base = "https://some.im/v1"
api_key_env = "SOMEIM_API_KEY"
model = "deepseek-v4-flash"

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

## TUI 与会话

终端中不带 Prompt 启动时会进入 TUI：

```bash
willdeep --profile some-im --workspace .
```

聊天记录默认自动跟随最新内容；手动查看历史后暂停跟随，回到底部后自动恢复：

| 按键 | 行为 |
|---|---|
| `↑` / `↓` | 按显示行滚动聊天记录 |
| `PageUp` / `PageDown` | 按页滚动 |
| `Home` / `End` | 跳到顶部 / 回到底部并恢复自动跟随 |
| `Ctrl+O` | 展开或收起最近 Tool Use 明细 |
| `Enter` | 发送 Prompt |
| `Ctrl+C` | 退出并恢复终端 |

Tool Use 默认显示为单条聚合活动摘要，例如：

```text
Tools: 6 calls · list_directory×3 · read_file×2 · git_status×1
```

失败数量不会被隐藏；需要排查时使用 `Ctrl+O` 查看最近明细。

每次成功回复会原子保存到 `$WILLDEEP_HOME/sessions/<uuid>.json`。命令行可查看或恢复：

```bash
willdeep --list-sessions
willdeep --resume latest "继续检查刚才的问题"
willdeep --resume 550e8400-e29b-41d4-a716-446655440000 "继续"
```

当前 Prompt 编辑器仍是基础单行输入。多行编辑、光标移动、Bracketed Paste 文本附件和图片附件属于下一阶段，尚未包含在 `0.3.0-rc2`，README 不把设计稿当成已交付功能。

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

启动时完成 initialize 和 `tools/list`，远端工具暴露为 `mcp__filesystem__<tool>`。MCP 调用默认逐次审批，只有 `--full-auto` / `workspace-access` 才自动执行。敏感值应由 MCP 子进程继承环境变量；不要把 Token 写入配置。

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

只读工具默认执行。`create_file`、`edit_file` 和 `run_command` 默认逐次询问：

```text
Approval required: edit file: src/main.rs
Allow once? [y/N]
```

在 CI 或已隔离容器中可以使用：

```bash
willdeep --full-auto --json ...
```

非交互输入下，如果没有 `--full-auto`，所有写入和命令请求都会拒绝；Harness 会把拒绝结果返回模型，不会静默放行。

## 配置环境变量

| 环境变量 | 用途 |
|---|---|
| `WILLDEEP_API_BASE` | API Base |
| `WILLDEEP_API_KEY` | 通用 API Key |
| `WILLDEEP_MODEL` | 模型 ID |
| `WILLDEEP_CONFIG` | 显式 TOML 配置文件路径 |
| `WILLDEEP_HOME` | WillDeep 配置目录，默认 `~/.willdeep` |
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
