# CLI Harness 架构

## 边界

当前实现以同一个 Agent Runtime 支撑一次性 CLI、TUI 与内嵌 Web 模式。子 Agent、后台任务与常驻 Runtime Daemon 均已接入，任务在 Daemon 进程内执行并可跨客户端断连恢复（见 [Runtime Daemon 与工作区](RUNTIME_DAEMON.md)）；Computer Use 与 Browser Use 仍在后续范围内。

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
- `strict` 对写入和命令逐次审批；默认 `smart` 放行已通过工作区边界校验的创建和编辑；
- Shell 命令走两级审批：`willdeep-core::safety` 的静态分类器（按 Shell 语义分段，只读/受限即放行，破坏性形状直接交用户且**不送 AI**），中间地带交 `willdeep-core::judge` 的 AI 判官——判官只能免审，不能扩权，NO / 不可用一律回落用户；命令在出网前本地脱敏，裁决只认唯一一个完整 `<verdict>` 标签；
- 非交互审批失败时拒绝，不自动升级；
- HTTP 错误体限长并进行基础凭据脱敏。

### 会话、Skills 与 MCP

会话按 UUID 写入 `$WILLDEEP_HOME/sessions`，使用临时文件加同目录 rename 保证单文件原子替换。恢复历史时会丢弃旧 system message 并注入当前工作区和 Skill 清单，避免陈旧边界跨会话延续。

Skill Catalog 只扫描配置根目录的直接子目录，读取入口固定为 `SKILL.md`；附属资源 canonicalize 后必须仍位于 Skill 根目录，单次读取最多 48,000 字符。

MCP server 由 TOML 声明 command、args、env 和启动超时。客户端建立 stdio 长连接，执行 initialize/initialized/tools/list，并以 `mcp__<server>__<tool>` 建立内部索引；Provider 侧只常驻 `list_mcp_tools` / `call_mcp_tool` 两个固定工具，匹配 Schema 按需加载，避免全量目录占满小上下文。所有 MCP 调用在 `strict`、`smart`、`workspace-write` 三种模式下都进入审批链；后两种模式只免审当前工作区内的 `create_file`、`edit_file`。MCP stderr 继承到宿主终端，stdout 仅作为 JSON-RPC 通道。

### Prompt 与附件

TUI 使用独立的多行 Prompt Editor，光标以 UTF-8 边界存储，并按 Unicode 显示宽度计算换行、上下移动、点击定位和内部滚动。终端启用 Bracketed Paste，长文本保留为消息附件而不是撑满编辑框。

图片从本机系统剪贴板读取 RGBA，限制为最多 64 MB 原始像素，编码为 PNG/Base64 后进入版本兼容的 `Message.attachments`。发送前附件可从草稿删除；发送后随会话 JSON 持久化。Provider Adapter 分别转换为 Chat Completions `image_url`、Responses `input_image` 和 Anthropic `image/source`。some.im 已知纯文本主模型由 Agent 通过同一 API Base/API Key 的 `qwen3-vl-plus`（或 `vision_model` 覆盖值）生成描述，再移除发往主模型的图片负载。

### 上下文与网络工具

Provider Profile 可声明 `context_window`。在途自动压缩按四道水位工作，全部只作用于临时请求视图，会话存档始终保留完整消息：

| 水位 | 常量 | 行为 |
| --- | --- | --- |
| 单条 25% | `OVERSIZED_MESSAGE_PERCENT` | 任何单条消息超过窗口该比例即就地裁掉中段、保留首尾。纯字符串处理，不花 Provider 调用——超大消息通常是刚读进来的文件，正躺在摘要够不着的保留区里 |
| 75% | `AUTO_COMPRESSION_TRIGGER_PERCENT` | 历史不少于 16 条时，用当前 Provider 总结较旧历史，保留首条消息、摘要和最近 10 条 |
| 90% | `AUTO_COMPRESSION_ESCAPE_PERCENT` | 逃生水位：无视 16 条门槛照常压缩，并按历史长度收缩保留区。少数几条巨型消息凑不够条数门槛却足以撑爆窗口 |
| 95% | `AUTO_COMPRESSION_CEILING_PERCENT` | 摘要后仍超窗时从保留区头部继续丢，至少保住首条、摘要和最近一轮问答；丢弃条数通过 `CompressionCompleted.dropped_messages` 如实上报 |

用户执行 `/compress` 时则显式生成摘要、保留最近六条消息并原子保存精简后的会话。TUI 展示最近一次 Provider Usage、缓存命中率、耗时和压缩阶段，不把运行状态伪装成模型私有推理文本。

`web_search` 调用同一 some.im Origin 的 `/api/v1/customer/web-search`。`web_fetch` 只接受 HTTP(S) 公网目标，在请求前及每次重定向时解析 DNS，并拒绝私网、回环、链路本地和文档保留地址。客户端自行处理最多 8 次跳转：同 hostname 自动跟随，跨 hostname 重新审批，HTTPS→HTTP 降级拒绝；最终响应限制为 3 MiB 和最多 100,000 字符。两项网络工具的初始访问在所有审批模式下都逐次审批。

### Mobile Relay

`/mobile` 按需创建独立于 Swift App 的 CLI room/token，并主动连接 `wss://j.niuwoai.com/ws/broadcast/<room>`。二维码里是 `mobile-gateway.v1` 的紧凑配对 URL（`?r=<room>&t=<token>&d=<桌面名>`，自建中继另带 `u`），由手机端补全成完整配对 JSON；`base_url`/`pairing_token`/`expires_at` 是中继字段的副本或常量，不进二维码（详见 [手机中继](MOBILE.md)）。Android 与 CLI 使用同一 Bearer Token 加入广播房间。CLI 不开放本地监听端口。

Relay 凭据写入 `$WILLDEEP_HOME/mobile-relay.toml`，Unix 下强制 `0600`。手机的 `message.send` 进入当前会话；Agent 忙碌时请求按到达顺序排队，最终回复使用 Android 已支持的 `message.append` 与 `message.done` 事件返回。

### 后台任务回流与子 Agent

`BackgroundTaskRegistry` 统一跟踪 Shell Job 与后台 Subagent，保存有界输出、状态、耗时、退出码和取消通道。启动 Shell 的入口保持 crate 私有，且只能在 `run_command` 完成既有逐次审批后注册。任务终态会同时发布事件并形成 `<background-task-notification>` 或 `<subagent-report>`；TUI 把事件排队，主 Agent 空闲时立即续跑，繁忙时在当前 turn 结束后续跑。非交互 CLI 等待所有关联任务完成并逐个处理通知。

`SubagentCatalog` 内置 scout、reader、log_inspector、git_detective、deep、editor、implementer、test_fixer、build_fixer。每个工种绑定 Provider、模型、工具白名单、上下文窗口、工具输出字节上限和最大轮数；子 Agent 创建的 `Agent` 不装载 Subagent Catalog，因此不能递归派生。只读工种不提供 Shell 或写工具。

除 `deep` 外的工种运行在可私有部署的 32K / 48K / 64K / 256K 档位并各自限制工具输出字节数——**窗口纪律由客户端执行**，不依赖模型自觉。16K 已取消；`implementer` 是 256K 日常多文件编码主力，`deep` 只在任务确实无法有界拆分时跑父模型并继承会话窗口。

`spawn_agent` 的可选 `task` 参数是 Task Packet：目标、已知事实、约束、相关文件与 verifier 命令。Runtime 按窗口档位把相关文件内联进 Worker 首条消息，并在每次尝试后**由 Runtime 自己执行** verifier 命令来判定成败——退出码是唯一裁决，模型的自述不是。失败输出经纯 Rust 的确定性消化（失败聚焦段 + 尾部，断言原文逐字保留）后回灌下一次尝试；尝试打满即判运行失败并要求升档。verifier 命令过与 `run_command` 同一条门禁链：静态只读放行、破坏性形状直接拒绝、其余交 AI 判官；判官缺席时拒绝，因为子 Agent 没有审批 UI 可回退。

写通道是一条通路的两种规模：写入型工种的目标文件先由父 ToolRegistry canonicalize 并作为**一个集合**整体审批，子 ToolRegistry 再以 canonical 绝对路径做集合成员匹配，符号链接或路径别名不能扩大授权范围；editor 是集合大小为 1 的特例。同一 Catalog 内运行中 Worker 的文件集互斥，冲突时后来者被拒绝并点名冲突文件，锁在运行结束、超时、取消或 panic 时随 `Drop` 释放。详见 [小上下文 Skill Worker](SKILL_WORKERS.md)。

### 用户交互与持久授权

Core 的 `Approver` 同时承载结构化 `ApprovalDecision`（AllowOnce / Deny / AlwaysAllow）和 `UserQuestion`。因此 `ask_user`、审批状态机及其工具结果属于 Harness，不依赖 SwiftUI 或 Ratatui；不同宿主只实现交互适配器。`ask_user` 支持 options、multi_select 和自由答案，答案使用 `<user_answer>` 边界并转义标记字符。

Always Allow 使用可撤销的窄签名。当前 Shell 仅保存无组合操作符的完整规范化命令，MCP 保存精确工具名；没有安全签名的调用不会向前端提供 AlwaysAllow。规则保存在 `$WILLDEEP_HOME/always-allow.json`，可用 CLI 列出或清空。后续 Swift App 复用 Rust Core 时，可替换规则存储适配器而保持相同决策语义。

## 尚未满足的长期架构要求

阶段 0 文档要求的 ACP 双轨验证、crate 级复用清单、协议 Schema 与跨客户端会话均尚未完成。当前实现是原生 Harness 起点，不能视为完整阶段 0 或阶段 1 产品。
