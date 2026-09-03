# 配置指南

## 配置文件位置

默认配置文件为 `~/.willdeep/config.toml`。两种改变方式：

- 设置 `WILLDEEP_HOME` 改变整个配置目录；
- 用 `--config /path/to/config.toml` 指定单个文件。

`--config` 必须放在子命令**之前**：

```bash
willdeep --config ./config.toml config check
```

首次运行且没有配置时会自动进入交互式设置，也可随时执行 `willdeep --onboarding`。生成的配置在 Unix 上自动设为 `0600`。

## 完整示例

完整模板见仓库根目录的 [config.example.toml](../config.example.toml)。

```toml
version = 1
default_provider = "some-im"

[agent]
max_turns = 24
approval = "smart"
language = "zh-CN"      # zh-CN | en | ja
small_model_routing = true
auto_dispatch_read_only = true
max_deep_calls_per_harness = 1
auto_title = true

[local_model]
enabled = false
base_url = "http://127.0.0.1:11434/v1"
summary_model = "gemma4:e4b-it-qat"
prefer_for_titles = true
prefer_for_context_summaries = false
prefer_for_worker_routing = true

[providers.some-im]
provider = "some-im"
api = "chat-completions"
api_base = "https://some.im/v1"
api_key_env = "SOMEIM_API_KEY"
model = "glm-5"
vision_model = "qwen3-vl-plus"
context_window = 131072

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

选择 Provider：

```bash
willdeep --profile anthropic --workspace . "检查当前项目"
```

如果只配置了一个 Provider，可以省略 `default_provider`，WillDeep 会自动选择唯一项。

## `[agent]` 段

| 键 | 说明 |
|---|---|
| `max_turns` | 模型/工具轮次上限 |
| `approval` | `strict` / `smart`（默认）/ `workspace-write`，见 [审批与自动化](APPROVALS.md) |
| `language` | 界面语言 `zh-CN` / `en` / `ja` |
| `small_model_routing` | Runtime 小模型优先路由；默认 `true` |
| `auto_dispatch_read_only` | 自动把高置信度定位、阅读、日志、Git 追溯派给窄工种；默认 `true` |
| `max_deep_calls_per_harness` | 每个 Harness 允许的 1M Deep 升级次数，`0..16`，默认 `1` |
| `auto_title` | 自动整理会话标题；默认 `true`。关掉后标题停在第一条提示词的确定性派生，见 [会话标题](TUI_GUIDE.md#会话标题怎么来的) |
| `title_model` | 标题摘要模型；默认取会话模型。请求只发一问一答各 800 字，与对话长度无关 |

## `[local_model]` 段

本段对齐 macOS Swift App 的“本地模型辅助”语义，复用一个轻量文本模型做短任务，避免标题、压缩和路由各自常驻一套模型。当前通过配置文件启用：

| 键 | 说明 |
|---|---|
| `enabled` | 本地辅助模型总开关；默认 `false` |
| `base_url` | OpenAI-compatible API Base；支持域名、局域网 IP 与回环地址，默认 `http://127.0.0.1:11434/v1` |
| `summary_model` | 三类辅助任务复用的模型 ID；默认 `gemma4:e4b-it-qat` |
| `prefer_for_titles` | 会话标题优先本地生成；默认 `true` |
| `prefer_for_context_summaries` | 上下文压缩优先本地生成；默认 `false`，避免弱模型损失长期上下文 |
| `prefer_for_worker_routing` | 低置信度 Worker 路由优先咨询本地模型；默认 `true` |

辅助端点可以不设置 API Key/Token，此时不会发送空的 `Authorization`。它可以部署在当前机器、家庭局域网或用户控制的域名后面；如果使用明文 HTTP，传输内容不会加密，应只放在可信网络。标题和压缩会按候选链自动回退；Worker 路由仅在关键词规则置信度低于 86 时调用模型，并且只允许更换已知 Worker Profile，不会改变 Tier、自动派工或安全约束。关闭总开关后行为与旧版本一致。

## `[providers.*]` 段

| 键 | 说明 |
|---|---|
| `provider` | 身份：`some-im` / `openai-compatible` / `anthropic` |
| `api` | 线格式：`chat-completions` / `responses` / `anthropic-messages` |
| `api_base` | API Base URL |
| `api_key_env` | 存放 Key 的环境变量名（**推荐**） |
| `api_key` | 内联明文 Key（触发 `0600` 权限强制） |
| `model` | 模型 ID |
| `vision_model` | some.im 纯文本模型的视觉回退模型 |
| `context_window` | 上下文窗口，用于状态栏占比显示 |
| `max_output_tokens` | Anthropic Messages 的输出上限 |

`api_key` 与 `api_key_env` 不能同时定义，也不允许为空字符串。

## Provider 与 API 是两个维度

这是最容易混淆的一点：**`--provider` 决定身份、鉴权和专属请求头；`--api` 决定请求/响应的线格式。**

```text
--provider auto | openai-compatible | some-im | anthropic
--api      auto | chat-completions  | responses | anthropic-messages
```

`auto` 的规则：

- host 精确等于 `some.im`、`api.some.im`、`api.niuwoai.com` → 识别为 some.im；
- host 精确等于 `api.anthropic.com` → 识别为 Anthropic Messages；
- 其他 API Base → OpenAI-compatible Chat Completions；
- Responses 必须显式指定 `--api responses`，`auto` 永远不会选它。

两者可以自由组合。例如 some.im 中转提供 Anthropic Messages 线格式时：

```bash
willdeep --provider some-im --api anthropic-messages ...
```

此时仍使用 some.im 的 Bearer 鉴权和上下文头，而消息体使用 Anthropic 格式。

三种线格式的具体契约见 [Provider 协议契约](PROVIDER_PROTOCOLS.md)。

## 三种 Provider 的最小配置

### some.im

```bash
export SOMEIM_API_KEY="<your-key>"
willdeep --provider some-im --model glm-5 --workspace . "检查当前仓库"
```

详见 [some.im 集成](SOMEIM_INTEGRATION.md)。

### OpenAI-compatible Chat Completions

```bash
export WILLDEEP_API_BASE="https://provider.example/v1"
export WILLDEEP_API_KEY="<your-key>"
export WILLDEEP_MODEL="model-id"

willdeep --api chat-completions --workspace . "解释这个项目"
```

### OpenAI Responses

```bash
willdeep \
  --api-base https://api.openai.com/v1 \
  --api responses \
  --model gpt-5 \
  --workspace . \
  "检查 Git 状态并总结风险"
```

### Anthropic Messages

```bash
export ANTHROPIC_API_KEY="<your-key>"

willdeep \
  --provider anthropic \
  --api anthropic-messages \
  --model claude-sonnet-4-5 \
  --workspace . \
  "阅读 README 并提出改进建议"
```

原生 Anthropic 使用 `x-api-key` 和 `anthropic-version: 2023-06-01`。API Base 可写成 `https://api.anthropic.com` 或带尾部 `/v1` 的形式。

## 配置优先级

```text
命令行参数 / WILLDEEP_* 环境变量
  → 当前 Provider Profile
    → Provider 专属环境变量
      → 内置安全默认值
```

API Key 的完整解析链和权限强制规则见 [认证与凭据](AUTHENTICATION.md)。

## 环境变量

| 环境变量 | 用途 |
|---|---|
| `WILLDEEP_API_BASE` | API Base |
| `WILLDEEP_API_KEY` | 通用 API Key |
| `WILLDEEP_MODEL` | 模型 ID |
| `WILLDEEP_CONFIG` | 显式 TOML 配置文件路径 |
| `WILLDEEP_HOME` | 配置与运行时目录，默认 `~/.willdeep` |
| `WILLDEEP_LANGUAGE` | 界面语言 `zh-CN` / `en` / `ja` |
| `WILLDEEP_CLIENT_LOGIN_SECRET` | some.im 浏览器登录的客户端密钥，构建时注入 |
| `SOMEIM_API_KEY` | some.im Key 回退 |
| `ANTHROPIC_API_KEY` | Anthropic Key 回退 |
| `OPENAI_API_KEY` | OpenAI-compatible Key 回退 |

## 界面语言

支持简体中文、英语和日语。三种设置方式，优先级从高到低：

1. `--language` 或 `WILLDEEP_LANGUAGE`；
2. `[agent]` 的 `language`；
3. 自动探测（Web 端读浏览器语言，默认 `zh-CN`）。

Web 左栏可直接切换语言，选择保存在当前浏览器的 localStorage 中。

## 图形化模型路由设置

- TUI 输入 `/routing`；
- Web 左栏点击“模型与路由”。

两者编辑同一个 `config.toml`，覆盖 Root Provider/模型、`[agent]` 路由开关与 Deep
预算，以及两根正交的轴：

- **五个公开职责**（`[subagents.*]`）：`generalist` / `implementer` / `tester` /
  `reviewer` / `ops_runner` 的 Provider、模型和上下文窗口——「这个职责平时用什么」。
- **三个模型档位**（`[worker_tiers.*]`）：基础 / 进阶 / 专家的 Provider、模型和
  上下文预算——「派工时说要贵一档，贵成什么样」。专家档标着「需票据」。

空的 Provider/模型表示采用推荐默认：职责在 some.im 上落到基础档 `someim-32b`，
档位落到网关默认表（`someim-32b` / `deepseek-v4-flash` / `gpt-5.6-sol`），其余情况
继承 Root/Provider。旧专门工种配置不会被设置页删除，继续服务自动路由和已保存流程；
显式值始终优先。

想让**某次派工**更贵是传 `worker_tier`，给某个职责绑贵模型是让它**每次**都贵——
别用第二种去达成第一种。

保存使用配置内容指纹检测并发修改，并通过 `0600` 临时文件原子替换；不会重写无关
字段或整份文件的注释。若页面打开后又手改了文件，保存会失败，重新打开设置即可。
正在运行的 Harness 不热切配置，新值从下一次 Harness/子 Agent 创建开始生效。

## 其他配置段

- `[subagents.*]` — 子 Agent 的模型绑定、上下文窗口、工具输出上限（`tool_output_limit`）、验证重试次数（`max_attempts`）、Token 预算、超时与熔断，见 [子 Agent 与后台任务](SUBAGENTS.md) 与 [小上下文 Skill Worker](SKILL_WORKERS.md)；
- `[mcp_servers.*]` — MCP stdio server，见 [Skills 与 MCP](SKILLS_AND_MCP.md)；
- `[skills]` 的 `roots` — 额外的 Skill 搜索根目录，见 [Skills 与 MCP](SKILLS_AND_MCP.md)。

## 配置命令

```bash
willdeep config init     # 用私有权限创建示例文件，不覆盖已有配置
willdeep config check    # 解析并严格校验
willdeep config show     # 打印校验后配置，内联 api_key 替换为 [REDACTED]
```

生产配置推荐只使用 `api_key_env`。
