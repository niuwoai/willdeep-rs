# Product Overview

> 最后更新：2026-08-10 | 当前版本：v0.16.0-rc2

## 项目简介

WillDeep CLI 是跨平台 AI Coding Agent 客户端。当前阶段通过用户提供的 API Base、API Key 和模型 ID，在受限工作区内完成模型推理、工具执行和结果验证。

## 核心功能

- Chat Completions、Responses、Anthropic Messages 三协议；
- some.im 与 BYOK Provider；
- 文件搜索、读取、创建和精确编辑；
- Git 状态与 Shell 命令；
- 多轮 Tool Call Harness；
- 工作区路径边界和写操作审批；
- 人类输出与 NDJSON 自动化输出；
- TOML 多 Provider Profile 与安全凭据引用；
- Ratatui 多轮 TUI、可滚动聊天记录、聚合工具活动和界面内审批；
- 空白新会话的即时工作区欢迎引导；
- 多行 Prompt 编辑、鼠标光标定位、文本粘贴附件和可删除图片附件；
- TUI 与 Web 的简体中文、英语、日语界面及持久语言偏好；
- TUI 临时单行思考摘要，以及 Web 单行工作状态、逐轮工具轨迹与停止生成；
- Web/TUI 独立聊天历史滚动与 TUI 常用 Markdown 终端渲染；
- TUI 可切换终端原生文本选择与复制模式；
- TUI 全局快捷键帮助、Prompt/状态栏焦点高亮与状态行焦点提示；
- TUI 聊天搜索、高亮与匹配跳转，以及可点击、可滚动的状态栏和后台任务详情；
- TUI `Ctrl+P` 全局命令面板，可模糊搜索命令、Skills、会话、Agent/任务和工作区文件；
- TUI Prompt、聊天区和状态栏三态焦点循环，以及候选、审批和 ask_user 的鼠标操作；
- Core 统一 Agent、后台任务、审批与提问的运行状态、Attention 分组和父级状态聚合语义；
- TUI 右栏 Attention Inbox 按人工介入优先级聚合待处理、工作中和最近完成事项；
- Attention Inbox 支持键盘/鼠标选择、后台任务与子 Agent 详情跳转、停止运行项和标记已读；
- Inbox 已读状态随会话持久化，后台 Shell 与子 Agent 支持真实重试，任务结束时触发终端提示；
- Git 冲突和待审 Diff 使用内容指纹进入 Inbox，子 Agent 审批阻塞结构化上报，状态按 Agent→会话→Workspace 上卷；
- 跨平台 Runtime Daemon 提供 `start/status/stop/logs`，以受 Token 保护的本地回环控制 API、原子状态和私有日志独立于 TUI 运行；
- Runtime 事件以 NDJSON 和单调序号持久化，`attach --after` 支持按游标补读并安全分离客户端；
- 非交互 Harness 可通过 Runtime 提交、查询和取消；Daemon 持有进程生命周期并把模型输出、session_id 和终态写入可续传事件流；
- Runtime 使用单实例租约锁协调并发启动，重启时将遗留运行任务标记为 Interrupted，停止时取消仍在运行的任务；
- Web 文本/图片粘贴附件、发送前删除、`/` 命令和 `$` 技能候选；
- JSON 会话持久化、列表与恢复；
- Codex 兼容 Skills 发现和按需读取；
- MCP stdio 工具发现、注册和调用。
- `/goal` 命令模式和 `$skill-name` 显式技能触发；
- 分阶段 CLI/TUI/Runtime 产品路线图与逐项验收状态；
- `/mobile` Relay 配对二维码和手机控制当前 CLI 会话；
- 区分角色的 TUI 配色；
- macOS Universal、Windows x64、Linux AMD64/ARM64 自动构建与 tag 发布。
- `rg` 优先、内置扫描兜底的跨平台文件搜索；
- some.im 纯文本模型的 `qwen3-vl-plus` 图片描述降级链路；
- 受审批保护的网页搜索和公网网页正文读取；
- 上下文用量、Token、耗时、自动摘要压缩及宽屏后台状态侧栏。
- `/compress` 手动压缩当前会话上下文并立即保存；
- 后台 Shell Job 与完成/失败后自动唤醒主 Harness 的结果回流；
- `spawn_agent` 前台/后台子 Agent，内置 scout、reader、deep、editor 工种并支持独立模型绑定；
- TUI 右栏实时后台任务状态、耗时及输出查询/取消工具；
- Core `ask_user` 候选单选/多选与自由输入交互；
- Allow once、Disallow、窄作用域持久 Always Allow 审批状态机及规则管理命令；
- 首次运行交互式 onboarding 与 some.im 浏览器登录；
- Swift Project 元数据和历史会话兼容读取；
- Git diff/worktree 原生工具与 TUI 工作区状态；
- 全局、项目根 `AGENTS.md` / `CLAUDE.md` 指令加载；
- React + Chakra UI 纯 CSR、SSE 进度、多工作区切换和并发限制的内嵌 Web 服务；

## 技术栈

- Rust 1.94；
- Tokio 异步运行时；
- Reqwest + rustls；
- Clap CLI；
- Ratatui + Crossterm TUI；
- Serde 协议编解码；
- ignore、regex、globset 工作区搜索。

## 项目结构

```text
crates/willdeep-core/   Agent Loop、Prompt、Provider、工具
crates/willdeep-cli/    参数解析、审批和终端事件输出
config.example.toml      Provider/Profile 配置模板
docs/                   架构与协议说明
```

## 运行

```bash
SOMEIM_API_KEY='<your-key>' cargo run -p willdeep -- \
  --provider some-im \
  --model deepseek-v4-flash \
  --workspace . \
  '检查项目状态'
```

生产使用时应通过环境变量或后续凭据存储提供 API Key，避免出现在命令历史中。

## 已知问题与后续

- [ ] Provider 原生 token streaming；当前 SSE 已实时传输 Harness 阶段、工具进度和最终回答。
- [ ] ACP/Codex App Server/Goose 接入；
- [ ] MCP Streamable HTTP 与 OAuth；
- [ ] 手机端工具审批和跨设备 Patch 审核；
- [ ] 更强的命令风险分类与平台沙箱；
- [ ] 流式真实 reasoning 摘要；当前单行区域显示可验证的运行阶段，不伪造模型思考内容；
- [ ] Swift/Rust 共享会话 schema 稳定后开放双向原地写入；当前采用安全副本。
- [ ] 抽取 Swift/Rust 共用的签名 Computer Use Helper 协议，再开放 AX 检查与短效控制租约。
- [ ] 将 Harness、任务和会话生命周期迁入 Runtime Daemon，并提供 attach/detach 与事件断线续传。
