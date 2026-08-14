# 审批与自动化

只读工具默认直接执行。会改变世界的操作——写文件、跑 Shell、调 MCP、访问网络——都要经过审批闸门。

## 三档审批模式

与 Swift App 对齐，在 `[agent]` 的 `approval` 中配置：

| 模式 | 语义 |
|---|---|
| `strict` | 创建、编辑、Shell、MCP 都逐次审批 |
| `smart`（默认） | 当前工作区内的创建、编辑免审；Shell 命令走下面的「两级审批」；MCP、网络仍审批 |
| `workspace-write` | 与当前阶段的 `smart` 保持相同安全边界，为后续自动审核器预留独立语义 |

未显式配置时默认采用 `smart`。

## 两级审批：本地静态规则 + AI 判官

`smart` / `workspace-write` 下的每条 Shell 命令先过**本地静态分类器**（`willdeep-core::safety`），得到三种结论之一：

| 结论 | 处理 | 例子 |
|---|---|---|
| `AlwaysSafe` | 直接执行，不弹卡 | `ls -la`、`cat x`、`rg foo`、`git status`、`git log`、`cargo test`、`cargo clippy`、`find . -name '*.rs'`、`mkdir -p build` |
| `AlwaysDangerous` | **不送 AI**，直接交用户 | `rm -rf`、`sudo …`、`chmod -R 777`、`git push --force`、`git reset --hard`、`mv`、`kill`、`dd if=`、fork 炸弹、`xargs rm` |
| `NeedsJudgment` | 交 AI 判官 | `curl …`、`npm install`、`git commit`、`sed -i`、`echo x > file`、`ssh host …`、任意脚本 |

分类器按 Shell 语义工作，不是子串匹配：

- 按 `|`、`&&`、`||`、`;`、`&`、换行切段，**任一段不安全整条命令不安全**；
- 引号内的内容是数据不是命令——`grep -rn 'rm -rf' logs` 依然直接放行；
- `$(…)` 只有内层被证明只读才展开，反引号和进程替换一律拒绝；
- `2>&1`、`2>/dev/null` 视为无副作用，其余重定向要复核；
- Heredoc、引号不闭合、解析不了的语法一律降级，不会误放行。

判不了的交给 **AI 判官**：一次非流式调用，只回 `<verdict>YES</verdict>` 或 `<verdict>NO</verdict>`。YES 才免审，NO / 回复畸形 / 网络失败一律回落到用户审批卡——判官只能减少打扰，不能扩大权限。

判官面对的是不可信文本，三层防御：

1. **本地脱敏**：`KEY=…`、`--password …`、`Bearer …`、`sk-…` 在出网前替换成 `[REDACTED]`，密钥不会为了被分类而离开本机；
2. **注入隔离**：命令、工具名、任务意图分别封进 XML 标签，其中的闭合标签和 `<verdict>` 用零宽空格打断，命令无法自己结束区块或伪造裁决；
3. **裁决解析**：只接受**唯一一个**格式完整的 `<verdict>` 标签，回声里的 "YES" 不算数。

同一「工具 + 命令 + 任务意图」的 YES 缓存 30 分钟，避免一轮工作里重复问同一个 `git commit`；**NO 从不缓存**。

### 判官用哪个模型

| Provider | 默认判官模型 | 说明 |
| --- | --- | --- |
| some.im | `someim-security-guard` | 网关托管的安全策略，服务端可随时收紧，无需发客户端；与 macOS 版 Xedit 同一套判决 |
| 其它（OpenAI 兼容 / Anthropic） | 当前会话模型 | 没有第二个端点可用，换模型等于换一套凭据；判官拿不到凭据就等于没有判官 |

`[agent] judge_model` 可覆盖两者。

`someim-security-guard` 是**推理模型**：出裁决前会先写一段私有推理，命令越复杂推理越长。因此判官请求**不设紧的输出上限**——上限太小会把回复截断在 `<verdict>YES` 这种没有闭合标签的半截上，解析失败后回落人工审批。这个失败模式的方向最坏：越是需要判官的复杂命令越容易掉线。判官因此被截断时，审计里会写明 `finish_reason=length`，而不是笼统的「回复畸形」。

配置：

```toml
[agent]
approval = "smart"
safety_judge = true                      # 默认开启；关掉后拿不准的命令直接弹卡
# judge_model = "someim-security-guard"  # some.im 默认值；其它 provider 默认取会话模型
```

每次自动放行/升级都追加一行到 `$WILLDEEP_HOME/approvals.jsonl`（`0600`，命令已脱敏），记录 `static` / `judge` / `always-allow` / `user` 四种来源和原因——这是「为什么这条命令没问我」的审计入口。判官来源的记录里带上实际使用的模型（如 `AI review (someim-security-guard): …`），模型被换掉或判官掉线都能在日志里直接看出来。

## 交互式审批

需要审批时终端显示：

```text
Approval required: edit file: src/main.rs
Allow once? [y/N]
```

审批**到达即弹**：本地回合和 Runtime 任务一样，一旦需要确认就立刻打开确认框并响铃，同时在活动流写一行 `等待你确认 · <描述>`，不需要用户去侧栏里找。

`ask_user` 提问同样到达即弹。弹窗自带输入框，主输入框里打了一半的草稿原样保留，只是从弹出那刻起按键交给弹窗。

审批和提问各自**排队**，不互相覆盖：处理完一个立刻弹出下一个，标题显示「还有 N」。切换会话会显式拒绝排队中的审批、作废排队中的提问，并给出提示。

三种决定：

- `Y` — Allow once，仅放行当前这一次调用；
- `N` — Disallow，拒绝当前调用；
- `A` — Always allow，**仅在界面明确显示该选项时**可用。

### Always Allow 不是免死金牌

持久放行的粒度被刻意收窄：

- Shell 只记住**规范化后的完整命令**，不是命令前缀，也不是可执行文件名；
- MCP 只记住精确的 `server/tool` 组合。

以下情况一律不提供持久放行：

- 含管道、重定向、命令连接符或换行的 Shell 命令；
- 文件写入；
- 网络重定向；
- 任务取消；
- `editor` 子 Agent 授权。

规则存放于 `$WILLDEEP_HOME/always-allow.json`，Unix 权限为 `0600`。管理命令：

```bash
willdeep --list-approvals
willdeep --clear-approvals
```

## Workspace 策略优先

Runtime 注册表里每个 Workspace 保存独立的访问策略，并在任务入队时**由服务端覆盖客户端输入**——客户端无法自报可写。

`read-only` 策略下，Shell、文件写入、Worktree 创建、MCP 和 `editor` 子 Agent 会在进入审批流程**之前**就被拒绝。

Coding Agent 的默认语义是 `workspace-write`：Workspace 内 `create_file` / `edit_file` 免审，Shell、MCP、网络和越界访问仍走原审批。`read-only` 只在用户显式选择时启用。详见 [Runtime Daemon 与工作区](RUNTIME_DAEMON.md)。

## 网络工具

`web_search` 和 `web_fetch` 在**所有**审批模式下都需要确认。

`web_fetch` 的额外硬约束：

- 拒绝私网、回环和链路本地地址；
- 同 hostname 重定向自动跟随，跨 hostname 重定向重新审批；
- HTTPS 降级到 HTTP 一律拒绝；
- 每次跳转重做公网目标校验；
- 以环路、次数、超时和流式 3 MiB 硬限制约束响应。

## MCP

MCP 调用在所有审批模式下均逐次确认。`smart`、`workspace-write` 和兼容参数 `--full-auto` 只免审当前工作区内的创建、编辑操作，不涉及 MCP。详见 [Skills 与 MCP](SKILLS_AND_MCP.md)。

## 非交互与 CI

在 CI 或已隔离的容器中：

```bash
willdeep --full-auto --json ...
```

非交互输入下，`smart` / `workspace-write` 允许当前工作区内的创建和编辑，以及静态分类器判定为 `AlwaysSafe` 的 Shell 命令；判官可用时 `NeedsJudgment` 也能放行。**其余 Shell、MCP 和外部操作仍因无法交互审批而拒绝。** Harness 会把拒绝结果作为工具结果返回给模型，不会静默放行，也不会假装成功。

被审批拒绝或被 Workspace 策略拒绝时，`willdeep run` 的退出码为 `4`。

## 后台审批

后台 Harness 需要审批或调用 `ask_user` 时进入 `WaitingApproval` / `WaitingAnswer` 状态，等待任意客户端处理：

```bash
willdeep daemon pending
willdeep daemon resolve <interaction-id> allow-once
willdeep daemon resolve <interaction-id> deny
willdeep daemon resolve <interaction-id> always-allow
willdeep daemon answer <interaction-id> "自由输入答案"
```

处理后原进程内 Future 从等待点继续。TUI 右栏 Inbox、Web Runtime 侧栏同样可以解决这三类审批。

TUI Inbox 中已完成的 Runtime 任务只保留 5 分钟；点击或按 `Enter` 打开等待审批的任务时，直接进入可执行 Allow、Disallow、Always Allow 的审批框。

Runtime 控制 Token 只保留在 Daemon 与 Harness 内存中，不会作为环境变量传给 Shell 或 MCP。

## `ask_user`

模型需要用户做实质选择时可调用 `ask_user`，传入 `question`、可选 `options` 和 `multi_select`。

- TUI 弹层到达即弹（含 Runtime 任务），支持方向键选择、空格多选，也可以直接键入未列出的其他答案；
- 普通终端支持输入序号或自由文本；
- Web Runtime 侧栏支持单选、多选和自定义回答。

用户答案经长度限制和标记转义后回到同一工具轮次。

## 相关文档

- [Runtime Daemon 与工作区](RUNTIME_DAEMON.md)
- [子 Agent 与后台任务](SUBAGENTS.md)
- [配置指南](CONFIGURATION.md)
