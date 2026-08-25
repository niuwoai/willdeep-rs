# CLI 参考

WillDeep 的命令树由 Clap 定义，补全脚本和 man page 从同一棵树生成。本文覆盖日常使用的命令；Runtime 内部操作见 [Runtime Daemon](RUNTIME_DAEMON.md)。

```text
willdeep [OPTIONS] [PROMPT]... [COMMAND]
```

## 顶层子命令

| 命令 | 用途 |
|---|---|
| `run` | 执行一次非交互 Coding Agent 轮次 |
| `session` | 查询或停止持久 Runtime Session |
| `config` | 创建、校验或脱敏查看 TOML 配置 |
| `daemon` | 管理持久本地 Runtime Daemon |
| `api` | 用 JSON 请求信封调用一个稳定 Runtime 操作 |
| `attach` | 附着到持久 Runtime 事件流 |
| `detach` | 确认当前客户端可以断开而不停止 Runtime |
| `integrations` | 查看和管理可选外部集成 |
| `doctor` | 不联系 Provider 的本地就绪诊断 |
| `completions` | 生成 Shell 补全脚本 |
| `man` | 输出 roff man page |

## 全局选项

这些选项对大多数子命令都可用：

| 选项 | 说明 |
|---|---|
| `-c`, `--config <PATH>` | TOML 配置路径，默认 `$WILLDEEP_HOME/config.toml` 或 `~/.willdeep/config.toml` |
| `-p`, `--profile <NAME>` | 使用 TOML 中的某个 Provider Profile |
| `--api-base <URL>` | Provider API Base |
| `--api-key <KEY>` | Provider API Key，建议改用环境变量以免进入命令历史 |
| `-m`, `--model <ID>` | 模型标识 |
| `--provider <P>` | `auto` / `openai-compatible` / `some-im` / `anthropic` |
| `--api <A>` | `auto` / `chat-completions` / `responses` / `anthropic-messages` |
| `-w`, `--workspace <PATH>` | 工具可访问的工作区根目录。**缺省为当前目录** |
| `--full-auto` | 工作区内创建/编辑免审批；Shell 与 MCP 仍逐次确认 |
| `--max-turns <N>` | 模型/工具轮次上限 |
| `--max-output-tokens <N>` | Anthropic Messages 的输出 Token 上限 |
| `--language <L>` | 界面语言 `zh-CN` / `en` / `ja` |

`--config` 必须放在子命令**之前**：

```bash
willdeep --config ./config.toml config check
```

## 交互式入口

不带 Prompt 在终端启动进入 TUI：

```bash
willdeep --profile some-im --workspace .
```

相关顶层开关：

| 选项 | 说明 |
|---|---|
| `--no-tui` | 无 Prompt 时也不进入 TUI |
| `--web` | 启动内嵌浏览器 UI 与 JSON API，见 [Web 端指南](WEB_GUIDE.md) |
| `--listen <ADDR>` | Web 监听地址，默认 `127.0.0.1:9847` |
| `--web-workspace <PATH>` | Web 模式额外允许的工作区，可重复 |
| `--onboarding` | 重新运行交互式首次设置 |
| `--json` | 在 stdout 输出 NDJSON 事件 |
| `-r`, `--resume <ID\|latest>` | 恢复已保存的会话 |

短选项一览：`-c` 配置、`-p` Profile、`-m` 模型、`-w` 工作区、`-r` 恢复会话。前四个是全局选项，子命令前后都能写。

## 会话与项目

```bash
willdeep --list-sessions
willdeep -r latest "继续检查刚才的问题"          # -r 即 --resume
willdeep --resume 550e8400-e29b-41d4-a716-446655440000 "继续"
```

macOS 上还会发现 Swift WillDeep 的 Project 与历史会话：

```bash
willdeep --list-projects
willdeep --project "项目名"
```

从 Swift 会话续聊时，结果保存为 `~/.willdeep/sessions` 下的 Rust 副本，不覆盖 Swift 原文件。
这些桥接会话也出现在 TUI 的历史面板里（标 `[Xedit]`），标题读的是 Xedit 写下的那一个。

每次成功回复会原子保存到 `$WILLDEEP_HOME/sessions/<uuid>.json`。会话标题由两级自动生成
（提交时确定性派生 + 第一轮回复后一次模型摘要），关掉写 `[agent] auto_title = false`，
详见 [会话标题](TUI_GUIDE.md#会话标题怎么来的)。

## `willdeep run` — 自动化入口

自动化脚本应使用正式的 `run` 子命令，而不是顶层 Prompt：

```bash
willdeep run "检查当前项目并总结风险"
printf '检查测试失败原因' | willdeep run --output json
willdeep run --input prompt.txt --attachment error.log --attachment screenshot.png
willdeep run --session latest --output ndjson "继续修复"
willdeep run --quiet "只用退出码报告成败"
willdeep run --local "显式使用进程内兼容 Harness"
```

| 选项 | 说明 |
|---|---|
| `--input <PATH\|->` | 从 UTF-8 文件或 stdin 读取任务 |
| `--attachment <PATH>` | 附加文本或 PNG/JPEG/WebP/GIF，可重复 |
| `--session <ID\|latest>` | 续接已有 Session |
| `--output <FMT>` | `text`（默认）/ `json` / `ndjson` |
| `--quiet` | 成功时不输出；错误仍走 stderr 与非零退出码 |
| `--local` | 使用旧的进程内 Harness |

`run` 默认把任务提交到持久 Runtime：Session/Turn、事件和终态可恢复，CLI 断开不会终止任务，`Ctrl+C` 只停止本次提交的精确 Turn。

`--api-key`、`--api-base`、Provider/API 方言和本轮限制等进程级覆盖不会写入 Runtime；使用这些覆盖时自动保留本地路径。需要持久 Runtime 时请把配置写入受保护的 TOML Profile。

### 输出与限制

`--output json` 只输出一个最终 JSON 对象；`ndjson` 输出逐行脱敏的生命周期事件，并以 `completed` 结束。

附件最多 12 个、原始内容合计 10 MiB；文本限制 20 万字符，图片支持 PNG/JPEG/WebP/GIF 并验证尺寸。

### 退出码

| 码 | 含义 |
|---|---|
| `0` | 成功 |
| `1` | 配置或内部错误 |
| `2` | 调用/输入错误（Clap 参数语法错误沿用同一码） |
| `3` | Provider 错误 |
| `4` | 等待/拒绝审批，或 Workspace 策略拒绝 |
| `5` | Harness / Tool 执行失败 |

## `willdeep session` — 会话查询与停止

常用 Session 操作不必记住 `daemon` 下的内部命令：

```bash
willdeep session list
willdeep session get <SESSION_ID>
willdeep session turns <SESSION_ID>
willdeep session stop <SESSION_ID>
```

`session stop` 读取目标 Session 当前明确绑定的 active/queued Turn，再以新的幂等 Request ID 请求停止。空闲 Session 会直接报错，不会猜测或停止其他 Session 的任务。

## `willdeep config` — 配置管理

```bash
willdeep config init
willdeep config check
willdeep config show
```

- `init` 使用私有权限创建示例文件，且不会覆盖已有配置；
- `check` 解析并严格校验生效配置；
- `show` 打印校验后的配置，内联 `api_key` 替换为 `[REDACTED]`。

详见 [配置指南](CONFIGURATION.md)。

## `willdeep api` — 稳定 Runtime 操作

```bash
willdeep api session.list --ndjson
willdeep api event.stream --params-file events.json --ndjson
```

| 选项 | 说明 |
|---|---|
| `--params-file <PATH\|->` | JSON 对象文件或 stdin，省略则为空对象 |
| `--request-id <ID>` | 客户端生成的稳定 Request ID，省略则生成 UUID |
| `--ndjson` | 输出适合 NDJSON 管道的紧凑 JSON |

完整契约见 [Runtime 控制 API](RUNTIME_CONTROL_API.md)。

## `willdeep attach` / `detach`

```bash
willdeep attach --after 0
willdeep detach
```

`attach --after <序号>` 按事件游标补读并持续跟随。按 `Ctrl+C` 只断开当前客户端，Daemon 继续运行。

## `willdeep doctor`

```bash
willdeep doctor
willdeep doctor --json
willdeep doctor --bundle ./willdeep-diagnostic.zip
```

`--bundle` 导出不含日志和本地路径的私有脱敏 ZIP，便于提交问题报告。详见 [故障排查](TROUBLESHOOTING.md)。

## `willdeep integrations`

```bash
willdeep integrations herdr status --json
```

可选的 Herdr 终端复用器集成诊断，取舍与边界见 [Herdr 研究与集成方案](HERDR_RESEARCH_AND_INTEGRATION.md)。

## 审批规则管理

```bash
willdeep --list-approvals
willdeep --clear-approvals
```

详见 [审批与自动化](APPROVALS.md)。
