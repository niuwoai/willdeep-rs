# CLI Harness 架构

## 边界

当前实现以同一个 Agent Runtime 支撑一次性 CLI 与 TUI。Computer Use、Browser Use、daemon 与多 Agent 暂不在范围内。

## 调用链

```text
CLI 参数、环境变量与 TOML Profile
        │
        ▼
ProviderConfig ── Provider 身份（鉴权/专属头）
        │         API Dialect（线格式）
        ▼
统一 Message / ToolCall / Completion
        │
        ▼
Agent Loop ── 工具注册表 ── 工作区边界与审批
        ├── Skills Catalog（SKILL.md）
        ├── MCP stdio（动态工具）
        ├── Session Store（版本化 JSON）
        └── 工具结果回传 Provider，直到最终文本
```

## 核心设计

### 配置优先级

默认读取 `~/.willdeep/config.toml`，`WILLDEEP_HOME` 可替换配置目录，`--config` 可指定任意 TOML 文件。命令行与 `WILLDEEP_*` 环境变量覆盖 Profile；Profile 覆盖 Provider 专属环境变量和内置默认值。

配置 Schema 使用 `version = 1`，未知字段直接报错，避免拼写错误被静默忽略。明文 `api_key` 在 Unix 上要求配置文件权限为 `0600`；推荐使用 `api_key_env`。

### Provider 身份与 API Dialect 分离

Provider 身份回答“如何鉴权、是否附带 some.im 上下文”；API Dialect 回答“如何编码消息和 Tool Call”。因此同一个 some.im Provider 可以承载 Chat Completions、Responses 或 Anthropic Messages。

### 统一领域模型

Agent Loop 只理解：

- `Message { role, content, tool_call_id, tool_calls }`；
- `ToolCall { id, name, arguments }`；
- `Completion { content, tool_calls, finish_reason, usage }`。

三种 Provider Adapter 负责协议翻译，工具与 Agent Loop 不包含 Provider 分支。

### Swift 资产的复用方式

本阶段没有复制 Swift 源码，而是迁移其行为契约：

- 工具名称和参数尽量一致；
- `read_file` 使用行号、offset、limit、max_bytes；
- 新文件只能由 `create_file` 创建；
- `edit_file` 使用精确字符串替换并要求唯一匹配；
- 原生 Anthropic 的系统消息、tool_use/tool_result 和 Token 上限规则；
- Responses 的 typed input、function_call/function_call_output；
- some.im 专属上下文头不得包含路径。

### 安全边界

- 工具仅接受工作区相对路径；
- 拒绝绝对路径和 `..`；
- 已有文件 canonicalize 后再次检查工作区前缀，阻止符号链接逃逸；
- 新文件检查最近存在的父目录，阻止通过符号链接写出工作区；
- `strict` 对写入和命令逐次审批，`smart` / `workspace-write` 仅放行已通过工作区边界校验的创建和编辑；
- 非交互审批失败时拒绝，不自动升级；
- HTTP 错误体限长并进行基础凭据脱敏。

### 会话、Skills 与 MCP

会话按 UUID 写入 `$WILLDEEP_HOME/sessions`，使用临时文件加同目录 rename 保证单文件原子替换。恢复历史时会丢弃旧 system message 并注入当前工作区和 Skill 清单，避免陈旧边界跨会话延续。

Skill Catalog 只扫描配置根目录的直接子目录，读取入口固定为 `SKILL.md`；附属资源 canonicalize 后必须仍位于 Skill 根目录，单次读取最多 48,000 字符。

MCP server 由 TOML 声明 command、args、env 和启动超时。客户端建立 stdio 长连接，执行 initialize/initialized/tools/list，并将工具动态注册成 `mcp__<server>__<tool>`。所有 MCP 调用在 `strict`、`smart`、`workspace-write` 三种模式下都进入审批链；后两种模式只免审当前工作区内的 `create_file`、`edit_file`。MCP stderr 继承到宿主终端，stdout 仅作为 JSON-RPC 通道。

### Prompt 与附件

TUI 使用独立的多行 Prompt Editor，光标以 UTF-8 边界存储，并按 Unicode 显示宽度计算换行、上下移动、点击定位和内部滚动。终端启用 Bracketed Paste，长文本保留为消息附件而不是撑满编辑框。

图片从本机系统剪贴板读取 RGBA，限制为最多 64 MB 原始像素，编码为 PNG/Base64 后进入版本兼容的 `Message.attachments`。发送前附件可从草稿删除；发送后随会话 JSON 持久化。Provider Adapter 分别转换为 Chat Completions `image_url`、Responses `input_image` 和 Anthropic `image/source`，Agent Loop 不包含协议分支。

### Mobile Relay

`/mobile` 按需创建独立于 Swift App 的 CLI room/token，并主动连接 `wss://j.niuwoai.com/ws/broadcast/<room>`。二维码沿用 `mobile-gateway.v1` 的 `relay_base_url`、`relay_room`、`relay_token` 字段；Android 与 CLI 使用同一 Bearer Token 加入广播房间。CLI 不开放本地监听端口。

Relay 凭据写入 `$WILLDEEP_HOME/mobile-relay.toml`，Unix 下强制 `0600`。手机的 `message.send` 进入当前会话；Agent 忙碌时请求按到达顺序排队，最终回复使用 Android 已支持的 `message.append` 与 `message.done` 事件返回。

## 尚未满足的长期架构要求

阶段 0 文档要求的 ACP 双轨验证、crate 级复用清单、协议 Schema 与跨客户端会话均尚未完成。当前实现是原生 Harness 起点，不能视为完整阶段 0 或阶段 1 产品。
