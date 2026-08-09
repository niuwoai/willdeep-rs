# Xedit 工具能力对照

> 最后更新：2026-08-10 | Rust CLI：v0.11.0-rc1

## 结论

Swift App 的工具注册表已经超过一百项，但它们并不都属于 Coding Agent 内核。Rust 版应优先复用通用 Harness 能力，把短剧生产等产品域功能留给 Skill、MCP 或插件。否则内核很快会胖成一只不肯运动的猫。

## 已在 Rust CLI 实现

- 工作区：`search_files`、`grep_files`、`read_file`、`list_directory`、`create_file`、`edit_file`；
- Git：`git_status`、`git_diff`、`list_worktrees`、`create_worktree`；
- 执行：`run_command`、后台 Job、`get_job_output`、`kill_job`；
- 协作：`ask_user`、`spawn_agent`、后台结果回流；
- 扩展：Skills、MCP stdio、`web_search`、`web_fetch`；
- 会话：持久化、恢复、压缩，以及 Swift 历史会话兼容读取；
- 多端：TUI、Mobile Gateway、内嵌 Web Server。

## 下一阶段应进入 Rust 内核

1. `apply_patch` / `write_file`：提供比精确字符串替换更强的可审计编辑协议。
2. 会话工具：`search_sessions`、`rename_session`、`list_workspace_sessions`。
3. MCP 管理：`list_mcp_servers`、`list_mcp_tools`、`search_mcp_servers`、`configure_mcp_server`。
4. 任务与计划：`list_jobs`、`list_queued_tasks`、`schedule_task`、`complete_scheduled_task`。
5. 浏览器自动化：优先实现 `embedded_browser_*` 的状态、导航、DOM 快照、点击、输入、网络和截图；Chrome Extension 工具作为可选连接器。
6. 媒体基础能力：`generate_image`、`edit_generated_image`、`visual_qa_screenshot`，通过 Provider/Skill 抽象提供。
7. 外部 Coding Agent：Codex、Claude Code 等应作为可配置子 Agent Provider，而不是写死多个工具名。

## macOS Computer Use

理论上完全可行，但不能等价成几条 AppleScript。Swift 版的安全边界包括：

- 独立签名 Helper，分别承接 Accessibility 和 Screen Recording 权限；
- AX 语义树优先，截图坐标只作降级；
- 每次观察生成新的 `snapshot_id` 和元素引用，拒绝使用过期目标；
- 控制授权绑定当前 runtime、turn 和目标 App，不跨任务持久化；
- 点击、输入、发送、删除、购买等动作带明确 consequence 分类；
- 密码、验证码、支付和 CAPTCHA 必须由用户私密接管；
- 用户随时暂停、接管或终止控制。

Rust 版应抽出与 Swift 共用的 Computer Use Helper 协议，而不是让 CLI 直接绕过 Helper 操纵系统。推荐顺序：只读权限/应用列表 → AX 检查 → 截图 → 短效控制租约 → 点击/输入 → 用户接管。完整边界具备前，不注册可写 Computer Use 工具。

## 不应进入通用内核

- `short_drama_*` 全系列；
- Replicate/some.im 特定媒体任务 CRUD；
- 图片选择、故事板等 App 专属 Widget；
- App 内终端标签与快捷方式管理。

这些能力适合由 Skill、MCP 或上层产品插件提供，Rust Harness 只保留稳定扩展接口。

## Web 模式

`willdeep --web` 提供：

- `GET /`：浏览器聊天页面；
- `GET /health`：版本和健康状态；
- `GET /api/sessions`：历史会话索引；
- `GET /api/workspaces`：允许切换的工作区；
- `POST /api/chat/stream`：通过 SSE 新建或继续会话，并实时返回 Harness 阶段。

前端采用 React + Chakra UI + Vite 纯 CSR，不使用 SSR 或 Next.js。应用为单用户模式，不做鉴权；认证与 TLS 由 Nginx、VPN 或 SSH Tunnel 负责。服务端仅接受启动时明确授权的多个规范化工作区，限制请求体、Prompt 长度及并发 Harness 数量。SSE 只公开轮次、工具名称、成功/失败和最终回答，不公开工具参数、工具输出或模型私有思维链。
