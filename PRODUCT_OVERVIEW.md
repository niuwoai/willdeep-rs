# Product Overview

> 最后更新：2026-08-09 | 当前版本：v0.4.0-rc1

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
- 多行 Prompt 编辑、鼠标光标定位、文本粘贴附件和可删除图片附件；
- JSON 会话持久化、列表与恢复；
- Codex 兼容 Skills 发现和按需读取；
- MCP stdio 工具发现、注册和调用。

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

- [ ] SSE 流式增量输出；
- [ ] ACP/Codex App Server/Goose 接入；
- [ ] MCP Streamable HTTP 与 OAuth；
- [ ] 后台 daemon 和跨设备审批；
- [ ] 更强的命令风险分类与平台沙箱；
- [ ] 多 Agent 与 Goal 编排。
