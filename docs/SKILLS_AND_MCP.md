# Skills 与 MCP

两套扩展机制：Skills 是"给模型看的说明书"，MCP 是"给模型用的外部工具"。

## Skills

### 发现规则

WillDeep 自动扫描工作区和用户主目录下的三个位置：

```text
<workspace>/.willdeep/skills/
<workspace>/.agents/skills/
<workspace>/.codex/skills/
~/.willdeep/skills/
~/.agents/skills/
~/.codex/skills/
```

每个 Skill 是一个包含 `SKILL.md` 的子目录：

```text
.willdeep/skills/
  reviewer/
    SKILL.md
    checklist.md
  release/
    SKILL.md
```

兼容 Codex 与 WillDeep 两种布局。可在配置中追加额外的搜索根：

```toml
[skills]
roots = ["/path/to/shared/skills"]
```

### 使用方式

模型通过 `list_skills` 和 `read_skill` 按需加载，**资源路径被严格限制在该 Skill 目录内**，无法借此读取工作区其他文件。

用户侧：

- TUI 与 Web 输入 `$` 弹出技能候选，Web 的候选层还带独立搜索框；
- Prompt 中的 `$skill-name` 会显式读取并附加对应的 `SKILL.md`；
- TUI 输入 `/skills` 查看当前目录发现的全部技能。

Workspace 注册表里非空的 Skill 允许列表会作为白名单生效，见 [Runtime Daemon 与工作区](RUNTIME_DAEMON.md)。Skills 在每轮执行前按当前 Workspace 策略重新绑定，**撤权立即生效**。

Web 端返回技能描述时，含 `password`、`api_key`、`secret`、`token=` 的描述会被替换为 `[sensitive description hidden]`。

## MCP

### 传输与配置

一个服务要么是 stdio（`command`，宿主拉起子进程），要么是 Streamable HTTP（`url`，远程服务），
不能两者都写。TOML 与 Codex 风格接近：

```toml
# stdio：本地子进程
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/safe/root"]
startup_timeout_seconds = 30
enabled = true

[mcp_servers.filesystem.env]
NODE_NO_WARNINGS = "1"

# Streamable HTTP：远程服务，静态 token 从环境变量来
[mcp_servers.issues]
url = "https://mcp.example.com/mcp"
bearer_token_env = "ISSUES_MCP_TOKEN"

[mcp_servers.issues.headers]
X-Trace = "willdeep"                 # 静态头；值可写 ${env:NAME}

# Streamable HTTP：OAuth 登录（`willdeep mcp login docs`）
[mcp_servers.docs]
url = "https://docs.example.com/mcp"

[mcp_servers.docs.oauth]
# client_id = "preregistered"        # 不写就动态注册（RFC 7591）
# client_secret_env = "DOCS_MCP_CLIENT_SECRET"
scopes = []                          # 空则用资源元数据里 scopes_supported 的全部
```

`url` 必须是 `https`，只有回环地址允许 `http`。名字看着像凭据的头（`X-Api-Key`、`*-Token` …）
必须写成 `${env:NAME}`，`Authorization` 头一律不收——凭据只走 `bearer_token_env` 或 `oauth`。

Streamable HTTP 按规范走：每条 JSON-RPC 一个 POST，响应是 JSON 或 SSE 流都认；`initialize`
回的 `Mcp-Session-Id` 之后每个请求都带，服务端 404 表示会话没了就重新握手一次再重发；
服务端选的协议版本按 `MCP-Protocol-Version` 头带回去；退出时 DELETE 会话。远程服务连不上、
没登录、对方 5xx **只警告并跳过这一个服务**，宿主照常启动（stdio 配置错了仍然直接失败）。

启动时完成 `initialize` 和 `tools/list`，远端工具保留命名空间名称：

```text
mcp__filesystem__read_file
mcp__filesystem__write_file
```

完整 MCP Schema 不再塞进每一轮 Provider 请求。模型先调用 `list_mcp_tools`
按关键字读取匹配工具的名称、说明和参数 Schema，再通过 `call_mcp_tool` 传入精确
命名空间名称与参数。这样连接大量 MCP server 时，固定上下文仍适合 32K/48K Worker。

这一版不处理服务端主动发起的请求（`sampling/*`、`roots/*`）和 GET 事件流：SSE 流里
不是本请求响应的消息一律丢弃。宿主没有替远端代发模型请求的授权，这是有意的。

### 凭据与 OAuth

**不要把 Token 写进配置文件。** stdio 服务的敏感值由子进程继承环境变量，或在
`[mcp_servers.*.env]` 里引用已有变量；HTTP 服务用 `bearer_token_env` 或头里的 `${env:NAME}`。
Runtime 控制 Token 不会传给任何 MCP 服务。

需要用户授权的远程服务配 `[mcp_servers.<name>.oauth]`，然后：

```bash
willdeep mcp login docs        # 授权码 + PKCE；本机 127.0.0.1 随机端口收一次回调
willdeep mcp list              # 每个服务的传输、鉴权与登录状态
willdeep mcp tools docs        # 连上去列工具，验证配置与凭据
willdeep mcp logout docs       # 删掉 token
```

登录的发现链：向 MCP 端点发一次未鉴权的 `initialize`，401 的 `WWW-Authenticate` 给出受保护
资源元数据（RFC 9728，没给就按 `/.well-known/oauth-protected-resource` 猜）→ 授权服务器
元数据（RFC 8414，退回 OIDC discovery，再退回 `/authorize` `/token` `/register` 固定端点）→
没有预注册 `client_id` 就动态注册。token 请求带 `resource`（RFC 8707）绑定到这个服务。
token 存 `$WILLDEEP_HOME/mcp-oauth/<name>.json`（0600），Harness 连服务时自动带上、
到期自动刷新；刷新不了会提示重新登录。回调 `state` 对不上一律拒绝。

### 审批

**除 `full-access` 外，MCP 调用在所有审批模式下均逐次确认。** `smart`、`workspace-write` 和兼容参数 `--full-auto` 只免审当前工作区内的创建、编辑操作，不涉及 MCP。

Always Allow 对 MCP 的粒度仍是精确的 `server/tool` 组合，不是通用
`call_mcp_tool`，更不是整个 server。

`read-only` 策略的 Workspace 会在审批前直接拒绝 MCP 调用。

详见 [审批与自动化](APPROVALS.md)。

## 项目上下文文件

除 Skills 外，WillDeep 每轮还会加载：

- `~/.willdeep/CLAUDE.md`
- 工作区根的 `PRODUCT_OVERVIEW.md`
- 工作区根的 `AGENTS.md`
- 工作区根的 `CLAUDE.md`

这些是长期项目约定的落点，比在每次对话里重复交代更可靠。

## 相关文档

- [配置指南](CONFIGURATION.md)
- [审批与自动化](APPROVALS.md)
- [Runtime Daemon 与工作区](RUNTIME_DAEMON.md)
