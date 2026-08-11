# WillDeep 文档

项目介绍与快速上手见仓库根目录的 [README](../README.md)。本目录是完整文档，按"先会用、再懂原理"的顺序组织。

## 上手

| 文档 | 内容 |
|---|---|
| [安装与构建](INSTALL.md) | 从源码构建、Shell 补全、首次运行、开发者验证 |
| [配置指南](CONFIGURATION.md) | TOML 配置、Provider 与 API 两个维度、环境变量、界面语言 |
| [认证与凭据](AUTHENTICATION.md) | API Key 解析链、some.im 登录、Runtime Token、权限强制 |

## 三种使用方式

| 文档 | 内容 |
|---|---|
| [TUI 使用指南](TUI_GUIDE.md) | 快捷键、鼠标（含 SSH / tmux）、斜杠命令、工具活动显示 |
| [Web 端使用指南](WEB_GUIDE.md) | 启动参数、界面功能、断线重连、安全模型、JSON API、前端开发 |
| [CLI 参考](CLI_REFERENCE.md) | 完整命令树、`run` 自动化入口、退出码 |

## 能力专题

| 文档 | 内容 |
|---|---|
| [some.im 集成](SOMEIM_INTEGRATION.md) | 登录、自动识别、请求头、视觉回退、web_search |
| [Runtime Daemon 与工作区](RUNTIME_DAEMON.md) | 常驻控制面、工作区策略、Session/Turn、事件流、Diff |
| [子 Agent 与后台任务](SUBAGENTS.md) | 四种 Profile、模型绑定、Worktree 审查与合并 |
| [审批与自动化](APPROVALS.md) | 三档审批模式、Always Allow 边界、CI 用法、`ask_user` |
| [Skills 与 MCP](SKILLS_AND_MCP.md) | Skill 发现规则、MCP 配置、项目上下文文件 |
| [手机中继](MOBILE.md) | `/mobile` 二维码配对与凭据安全 |

## 排查

| 文档 | 内容 |
|---|---|
| [故障排查](TROUBLESHOOTING.md) | doctor、常见报错、SSH 鼠标与剪贴板、退出码对照 |

## 协议与架构

| 文档 | 内容 |
|---|---|
| [架构](ARCHITECTURE.md) | 调用链、边界与模块划分 |
| [Provider 协议契约](PROVIDER_PROTOCOLS.md) | 三种线格式的请求/响应契约 |
| [Runtime 控制 API](RUNTIME_CONTROL_API.md) | 统一控制协议的完整契约 |
| [Runtime 会话协议](RUNTIME_SESSION_PROTOCOL.md) | Session / Turn 状态机与持久化 |
| [进程内 Runtime Harness](IN_PROCESS_RUNTIME_HARNESS.md) | Daemon 进程内执行模型 |

## 路线与调研

| 文档 | 内容 |
|---|---|
| [CLI / TUI / Runtime 路线图](CLI_TUI_RUNTIME_ROADMAP.md) | 分阶段实施记录与验收 |
| [Xedit 工具能力对照](XEDIT_TOOL_PARITY.md) | 工具覆盖度与 Computer Use 路线 |
| [Herdr 研究与集成方案](HERDR_RESEARCH_AND_INTEGRATION.md) | 终端复用器集成的取舍与边界 |

## 文档约定

- 根目录 [README](../README.md) 面向"这是什么、值不值得用"，保持简短；
- 本目录面向"怎么用、为什么这么设计"，是唯一的详细事实来源；
- 根目录 [PRODUCT_OVERVIEW.md](../PRODUCT_OVERVIEW.md) 是产品功能台账，同时被 WillDeep 自己作为项目上下文加载；
- 涉及安全边界的描述必须与实现一致。宁可写"当前不支持"，也不写模糊的承诺。
