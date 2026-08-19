# WillDeep CLI

**跨平台 AI Coding Agent。一个二进制，三种界面，任务不会因为你关掉窗口就死掉。**

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

```bash
willdeep --workspace . "检查当前仓库并修复测试"
```

---

## 为什么是 WillDeep

**任务活得比你的终端久。** 常驻 Runtime Daemon 在自己进程内跑任务。关掉 CLI、断开 SSH、刷新浏览器，活儿照跑；回来时按事件游标续上，一条消息都不丢。升级二进制也不用杀任务——`daemon upgrade` 排空后无损接管。

**终端、浏览器、手机，同一个会话。** Ratatui 终端界面、内嵌 React Web 界面、手机扫码中继，三端共用同一套 Session、审批和 Agent 状态。在终端起个头，通勤路上用手机补一句，回家在浏览器里看结果。

**自带模型，不锁厂商。** OpenAI Chat Completions、OpenAI Responses、Anthropic Messages 三种线格式，加上 some.im 一键登录。Provider 身份和线格式是两个独立维度，可以自由组合——比如用 some.im 的鉴权跑 Anthropic 格式。

**改你的代码这件事，得先过你这关。** 三档审批模式，写文件、跑 Shell、调 MCP、访问网络各有独立闸门。"始终允许"只记住规范化后的完整命令，不是命令前缀——不给自己留后门。工作区策略由服务端强制，客户端无法自报可写。

**小模型先干，主上下文保持干净。** Runtime 会把定位、阅读、日志和 Git 追溯自动派给 `someim-32b-<工种>`，普通编码由 GLM-5 控制；1M 的 `deep` 必须提交带低档尝试证据的升级票据。九种子 Agent 各自绑定窗口、Token 预算和熔断阈值，写入工种在专属 Git Worktree 中接受审查后合并。

**模型路由不是焊死的。** TUI `/routing` 和 Web“模型与路由”面板都能持久修改 Root、Worker、Deep 的 Provider、模型、上下文窗口与预算；也能一键恢复 some.im 推荐映射。手改 `config.toml` 仍是一等路径，并发保存会检测冲突，不拿旧页面盖掉新配置。

**看得见的进度，看不见的秘密。** 工具调用、Token、耗时、Diff 归属全部结构化上报，Diff 能精确追溯到是哪个 Turn、哪个 Agent、哪次工具调用改的。而 Prompt、命令、工具参数、输出、本地路径不会下发到 Web 前端，也不会进日志。

---

## 30 秒上手

```bash
# 1. 构建（前端会嵌入二进制）
cd web && yarn install --frozen-lockfile && yarn build && cd ..
cargo build --release

# 2. 登录
willdeep --onboarding

# 3. 开干
willdeep --workspace .
```

不带 Prompt 启动进入终端界面；想用浏览器就加 `--web`；想跑自动化就用 `willdeep run`。

```bash
willdeep --web --workspace /path/to/project     # 浏览器界面，默认 127.0.0.1:9847
willdeep run --output json "总结当前风险"        # 自动化，稳定退出码
```

详细步骤见 [安装与构建](docs/INSTALL.md) 与 [配置指南](docs/CONFIGURATION.md)。

---

## 能力一览

| | |
|---|---|
| **模型** | Chat Completions · Responses · Anthropic Messages · some.im |
| **工具** | 文件搜索/读写/精确编辑 · Git 状态/Diff/Blame · Shell · 后台 Job · Web 搜索与抓取 |
| **界面** | Ratatui TUI · React Web · 手机中继 · NDJSON 自动化输出 |
| **扩展** | `SKILL.md` 技能 · MCP stdio server · 项目上下文文件 |
| **协作** | 持久 Session/Turn · Fork 与归档 · 多工作区 · 子 Agent 树 |
| **审查** | Diff 快照与归属 · Worktree 审查合并 · Commit Preview · 安全撤销 |
| **语言** | 简体中文 · English · 日本語 |

当前暂不包含 Computer Use 与 Browser Use。

---

## 文档

完整文档在 **[docs/](docs/README.md)**。常用入口：

- [TUI 使用指南](docs/TUI_GUIDE.md) — 快捷键、鼠标（含 SSH / tmux）、斜杠命令
- [Web 端使用指南](docs/WEB_GUIDE.md) — 界面功能、JSON API、安全模型
- [CLI 参考](docs/CLI_REFERENCE.md) — 完整命令树与退出码
- [配置指南](docs/CONFIGURATION.md) — TOML、Provider 与 API 两个维度
- [认证与凭据](docs/AUTHENTICATION.md) — Key 解析链、登录、Token 边界
- [some.im 集成](docs/SOMEIM_INTEGRATION.md) — 登录、视觉回退、联网搜索
- [Runtime Daemon 与工作区](docs/RUNTIME_DAEMON.md) — 常驻控制面
- [子 Agent 与后台任务](docs/SUBAGENTS.md) — 九种工种、模型路由与 Worktree
- [审批与自动化](docs/APPROVALS.md) — 三档模式与 CI 用法
- [故障排查](docs/TROUBLESHOOTING.md) — 出问题先看这里

---

## 安全须知

- API Key 优先用 `api_key_env`；配置里出现明文 `api_key` 时，Unix 下文件权限必须是 `0600`，否则拒绝启动。
- Runtime 控制面只监听回环，认证 Token 不会传给 Shell 或 MCP 子进程。
- **Web 模式是单用户模式，没有应用层鉴权。** 跨机器访问必须由 Nginx、VPN 或 SSH Tunnel 提供认证与 HTTPS，不要把端口直接暴露公网。
- 手机配对二维码明文携带中继 Token，只对自己的设备扫码。

---

## 参与开发

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

三种 Provider 协议均有本地 Mock HTTP 契约测试，覆盖完整工具往返，不调用真实 API，也不消耗 Key。

架构见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)，功能台账见 [PRODUCT_OVERVIEW.md](PRODUCT_OVERVIEW.md)。

---

## 许可证

Apache License 2.0。`WillDeep` 名称和商标不随源代码许可证授权。
