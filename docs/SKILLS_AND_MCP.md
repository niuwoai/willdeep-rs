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

### 配置

使用与 Codex 风格接近的 TOML：

```toml
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/safe/root"]
startup_timeout_seconds = 30
enabled = true

[mcp_servers.filesystem.env]
NODE_NO_WARNINGS = "1"
```

启动时完成 `initialize` 和 `tools/list`，远端工具保留命名空间名称：

```text
mcp__filesystem__read_file
mcp__filesystem__write_file
```

完整 MCP Schema 不再塞进每一轮 Provider 请求。模型先调用 `list_mcp_tools`
按关键字读取匹配工具的名称、说明和参数 Schema，再通过 `call_mcp_tool` 传入精确
命名空间名称与参数。这样连接大量 MCP server 时，固定上下文仍适合 32K/48K Worker。

当前支持 stdio 传输。

### 凭据

**不要把 Token 写进配置文件。** 敏感值应由 MCP 子进程继承环境变量，或在 `[mcp_servers.*.env]` 中引用已有的环境变量。

Runtime 控制 Token 不会传给 MCP 子进程。

### 审批

**MCP 调用在所有审批模式下均逐次确认。** `smart`、`workspace-write` 和兼容参数 `--full-auto` 只免审当前工作区内的创建、编辑操作，不涉及 MCP。

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
