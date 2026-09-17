# 审批与自动化

只读工具默认直接执行。会改变世界的操作——写文件、跑 Shell、调 MCP、访问网络——都要经过审批闸门。

## 四档审批模式

与 macOS 版（Xedit `AgentApprovalMode`）逐档对齐。默认档在 `[agent]` 的 `approval` 中配置，会话中途可以用 `/permissions` 或 Shift+Tab 切换（见下文「在终端里切换」）。

| 模式 | 工作区内创建/编辑 | Shell 命令 | 网络 POST、MCP、Worktree |
|---|---|---|---|
| `strict` | 逐次审批 | 逐次审批 | 逐次审批 |
| `smart`（默认） | 免审 | 静态规则 → AI 判官 → 拿不准才问 | 审批 |
| `workspace-write` | 免审 | 静态规则 → **写入围栏内且不出工作区**则放行 → 其余问人；**不请 AI 判官** | 审批 |
| `full-access` | 免审 | 放行；**破坏性形态（`rm -rf`、`sudo`、`git push --force`…）照样问** | 放行 |

另有 `read-only`，它是 Runtime 工作区策略而不是会话档位：写入、Shell、MCP、Worktree 在审批前直接拒绝，会话切档无法越过它。

未显式配置时默认采用 `smart`。别名：`ask` / `request-every-time` = `strict`，`auto-review` = `smart`，`workspace-access` = `workspace-write`，`full` / `silent` = `full-access`。

### `workspace-write` 靠什么「不请 AI」

这一档的承诺是「工作区里的事不打扰你」，所以它只在两件事都**确定**时放行一条未分类命令：

1. **内核写入围栏在起作用**（macOS `sandbox-exec` / Linux `bwrap`，见 [沙箱](SANDBOX.md)）。没配 `agent.sandbox` 时这一档默认就套上围栏；显式 `sandbox = false` 或机器上没有围栏实现时，未分类命令一律问人；
2. **命令看不出要出工作区**（`safety::reaches_outside_workspace`）：`curl`/`ssh`/`docker`/`sudo`/`kubectl`、`git push/pull/fetch/clone`、`cargo publish`、`npm i -g` 等，以及含 heredoc、`$(…)`、反引号或解析不了的命令，都算「出去」。

放行记审计来源 `workspace-access`。

### `full-access` 的边界

- 免的是审核，不是黑名单：静态分类器判为 `AlwaysDangerous` 的命令仍弹卡，AI 判官也不会被请来替它说话；
- 写入围栏摘掉（用户已允许工作区外写入）；文件工具 `create_file` / `edit_file` 仍按工作区路径解析，工作区外的文件经 Shell 访问；
- `ask_user` 提问、「效果未知的中断调用重放」仍需要人；
- 放行记审计来源 `full-access`。

## 在终端里切换

TUI 输入框标题常驻当前档位（完全访问为红色）。

| 操作 | 效果 |
|---|---|
| `/permissions` 或 `/permission-mode` | 打开选择面板：↑/↓、1-4、Enter；选完全访问进入确认页，按 `y` 才生效 |
| `/permissions <档位>` | 直接切换；`full-access` 同样先进确认页 |
| Shift+Tab | 在 `strict → smart → workspace-write` 间循环，**永远不会切到完全访问** |
| `/permissions default <档位>` | 写入配置文件 `[agent] approval`（保留注释），下次启动生效 |

- 切档**只对当前 TUI 进程**有效，退出后回到配置里的默认档；
- 本轮正在跑时也能切：进程内 Agent 立即换档；Runtime 会话经 `session.update_approval_mode` 同步，**这一轮已经在跑的任务**下一次工具调用就按新档位判；
- 每次切换在 `approvals.jsonl` 记一行来源 `mode-change`。

## 两级审批：本地静态规则 + AI 判官

`smart` 下的每条 Shell 命令先过**本地静态分类器**（`willdeep-core::safety`），得到三种结论之一（`workspace-write`、`full-access` 也先过这一步，之后的处理见上文）：

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

每次自动放行/升级都追加一行到 `$WILLDEEP_HOME/approvals.jsonl`（`0600`，命令已脱敏），记录 `static` / `judge` / `always-allow` / `user` / `workspace-access` / `full-access` / `mode-change` / `not-required` 等来源和原因——这是「为什么这条命令没问我」的审计入口。判官来源的记录里带上实际使用的模型（如 `AI review (someim-security-guard): …`），模型被换掉或判官掉线都能在日志里直接看出来。

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
- MCP 只记住精确的 `server/tool` 组合；
- `web_fetch` 的 POST 记住**注册域名**，不是完整 URL——URL 带一次性 id、body 每次都不同，逐字规则下一次就对不上，等于没有；作为交换，规则一次覆盖该域名下的全部 POST。

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

Runtime 注册表里每个 Workspace 保存独立的访问策略（`read_only` / `strict` / `smart` / `workspace_write` / `full_access`），并在任务入队时**由服务端合成**这一轮的档位：

- 会话里用 `/permissions` 选过档位（`session.update_approval_mode`），以会话档位为准——它与工作区策略出自同一个本机用户，而且更晚；
- 否则用工作区策略；
- **`read_only` 工作区是硬上限**，会话档位越不过它，客户端也无法替只读工作区自报可写。

`read-only` 策略下，Shell、文件写入、Worktree 创建、MCP 和 `editor` 子 Agent 会在进入审批流程**之前**就被拒绝。

自动登记的工作区默认 `smart`。0.78.0 之前的注册表里 `workspace_write` 与 `smart` 判定完全相同（而且多数是自动登记的默认值），首次启动时一次性迁移为 `smart`，升级前后行为不变；迁移后显式选择的 `workspace_write` 是新语义。详见 [Runtime Daemon 与工作区](RUNTIME_DAEMON.md)。

## 网络工具

`web_search` 和 `web_fetch` 的 GET 只在 `strict` 模式下逐次确认。其余模式（`read-only`、`smart`、`workspace-write`、`full-access`）把它们当作只读操作放行：抓一个公网页面不改动本地任何状态，私网目标在审批之前就已经被公网校验拒绝。

`web_fetch` 带 `method: "POST"` 时是对外写操作，规矩完全不同：

- 所有审批模式下都逐次确认，`read-only` 策略在审批之前直接拒绝；
- 审批卡多出「始终允许」一项，记下的规则是**注册域名**级的 `web-post:<域名>`：批准 `api.example.com` 之后，`upload.example.com` 也不再问，换成 `other.com` 则重新问。域名按公共后缀表切分，`example.co.uk` 整体算一个注册域名，不会被截成 `co.uk`；IP 直连按字面量单独成规则；
- 规则里只有域名，URL 上的一次性 id 和 body 里的内容都不会写进 `always-allow.json`；
- 请求体上限 1 MiB，默认 `Content-Type: application/json`，可用 `content_type` 覆盖；
- POST **不跟随任何重定向**：用户批准的是当前这个地址，跳转后的端点得重新申请。

`web_fetch` 的额外硬约束：

- 拒绝私网、回环和链路本地地址；
- GET 的同 hostname 重定向自动跟随，跨 hostname 重定向在 `strict` 模式下重新审批，POST 一律不跟随；
- HTTPS 降级到 HTTP 一律拒绝；
- 每次跳转重做公网目标校验；
- 以环路、次数、超时和流式 3 MiB 硬限制约束响应。

## MCP

除 `full-access` 外，MCP 调用在所有审批模式下均逐次确认（可「始终允许」精确的 `server/tool`）。`smart`、`workspace-write` 和兼容参数 `--full-auto` 只免审当前工作区内的创建、编辑操作，不涉及 MCP。详见 [Skills 与 MCP](SKILLS_AND_MCP.md)。

## 非交互与 CI

在 CI 或已隔离的容器中：

```bash
willdeep --full-auto --json ...
```

`--full-auto` 是兼容参数，等价于 `smart`（它发布时 `workspace-write` 与 `smart` 是同一套判定，拆档后保留原行为）。

非交互输入下，`smart` 允许当前工作区内的创建和编辑，以及静态分类器判定为 `AlwaysSafe` 的 Shell 命令；判官可用时 `NeedsJudgment` 也能放行。**其余 Shell、MCP 和外部操作仍因无法交互审批而拒绝。** Harness 会把拒绝结果作为工具结果返回给模型，不会静默放行，也不会假装成功。

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
