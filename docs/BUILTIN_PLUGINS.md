# 内置插件

以下能力以**插件**的形式随 willdeep 一起发：

| 插件 | 用途 | 对应 Xedit |
|---|---|---|
| `willdeep-favorites` 2.3.0 | 富文本备忘、图片收藏、搜索、标签与撤销；Web 插件页面与聊天选区收藏 | 同源第一方收藏夹插件 |
| `willdeep-scheduler` | 定时任务：到点在全新会话里自动跑一个 prompt，可带自我完成的目标 | `schedule_task` / `complete_scheduled_task`、Automations（`AgentRoutine`） |
| `willdeep-roundtable` | 专家圆桌：几位立场各异的领域专家多轮讨论一个开放问题，收敛成决策文档 | Expert Roundtable（`AgentRoundtable`） |

它们是普通的插件包，走和第三方插件一样的安装、批准、启用流程，权限逐条声明。
定时任务与圆桌的 MCP 服务端使用 `${willdeepExe} plugin serve-builtin <id>`，无需额外运行时。
收藏夹内嵌原版 Ruby 服务和页面，需要 `/usr/bin/ruby`；启动 CLI 或 WebApp 时自动安装，
仍须批准并启用。重复启动保留相同版本、已有更高版本、审批及数据。
收藏数据位于 `$WILLDEEP_HOME/plugin-data/willdeep-favorites/favorites.json`，图片在同名 `.media` 目录。
macOS App 的历史收藏文件不会自动迁移或修改。

## 安装

```bash
willdeep plugin builtin list
willdeep plugin builtin install favorites --enable   # 明确批准并启用随包收藏夹
willdeep plugin builtin install scheduler      # 装好后按提示 approve + enable
willdeep plugin builtin install roundtable --enable   # 或者一步批准并启用
```

启用之后，聊天里通过 `list_mcp_tools` / `call_mcp_tool` 就能用到它们的工具，工具名是
`mcp__scheduler__schedule_task`、`mcp__roundtable__roundtable_start` 这样的形式。
和其他插件工具一样，调用前会按当前审批档位询问。

## 定时任务 `willdeep-scheduler`

**权限**：`process.execute`、`conversation.write`、`workspace.read`。

| 工具 | 作用 |
|---|---|
| `schedule_task` | 建一个定时任务。`prompt` 必填，而且必须自给自足：每次运行都在一个看不到别的上下文的新会话里。调度方式四选一：`interval_minutes`、`daily_at`（`HH:MM`）、`weekdays_at`、`weekly_at` + `weekday`（1 = 周日 … 7 = 周六）。可选参数：`goal`、`approval_mode`（`strict` / `smart` / `workspace-write` / `full-access`，默认跟随工作区）、`workspace`（默认为当前目录）、`title`。调用时不会立刻运行。 |
| `complete_scheduled_task` | 删除一个任务。带 goal 的任务在运行中判断目标已达成时，会用自己的 `task_id` 调它；用户要取消任务时也用它。 |
| `list_scheduled_tasks` | 列出任务：调度方式、下次运行的本地时间、工作区、goal、上次运行的会话。只读。 |

**怎么触发**：
- **谁来触发**：任务存在 `$WILLDEEP_HOME/schedules.json`（权限 0600，读写带文件锁）。常驻的 Runtime daemon 每 30 秒检查一次，只在插件**已启用**时触发；停用插件就等于停掉全部定时任务。
- **每次触发做什么**：到期的任务各开一个全新会话，标题为 `⏰ <任务名>`，并按任务的档位设好审批。第一句话是任务的 prompt；有 goal 时，后面再附上“完成后判断目标是否达成，达成就调 `complete_scheduled_task`”的说明，话术与 Xedit 相同。
- **错过与重叠**：daemon 停机期间错过的触发只补一次，不会补跑 N 次。上一次触发的会话还有没跑完的轮次时，这次跳过，避免同一个任务越堆越多。
- **留下的记录**：事件流里会有 `schedule.fired`、`schedule.skipped`、`schedule.failed`。在反馈账本里，这些轮次的来源是 `schedule`。

**无人值守的审批**：定时运行时没有人在看。凡是需要审批的工具调用，都会停在 TUI / Web 的收件箱里，等人处理。
- 带 goal 的任务要自己移除自己，需要调用 `complete_scheduled_task`。它第一次请求时选一次 Always Allow，以后就不会再卡住。
- 想让任务完全无人值守，就给它设 `approval_mode = "full-access"`。破坏性命令的黑名单在这个档位下仍然生效。

**与 Xedit 的差异**：
- 多了 `weekdays_at`、`weekly_at` 两种调度方式（Xedit 的数据模型里本来就有，只是没暴露给工具）。
- 多了 `list_scheduled_tasks`，因为 CLI 没有侧栏卡片可看。
- 触发端在 daemon，所以需要 Runtime 在运行（TUI / Web 会按需拉起它）。

## 专家圆桌 `willdeep-roundtable`

**权限**：`process.execute`、`ai.chat`。模型调用全部通过宿主的 `willdeep/ai/complete`，使用默认的 Provider 档案，并以辅助请求的形式记入用量账本。

| 工具 | 作用 |
|---|---|
| `roundtable_start` | `topic` 必填。可选参数：`context`、`experts`（2–6 位）、`max_rounds`（1–10，默认 3）。不指定专家时，会先做会前准备：精炼议题、提出讨论框架、推荐 2–4 位专家。讨论会一直跑到以下三种情况之一：主持人判定收敛、达到轮数上限、有专家需要先问用户一个问题。 |
| `roundtable_continue` | 带上用户的答复（`answer`）接着讨论；`extra_rounds` 可以再加几轮；`action: "finish"` 表示现在就收尾并生成文档。 |
| `roundtable_experts` | 列出专家池。 |

**专家池**：产品、架构、财务、风险、战略、心理、法律、研究、增长、运营，共 10 位。人设原文照搬 Xedit：身份、思维模式、**偏见与执念**、口癖。故意让他们的立场不同，这是防止大家趋同的第一道防线。

**讨论机制**：
- **发言硬性要求**：每位专家每轮发言，必须点名回应至少一位前面的发言者，明确表态（支持 / 反对 / 中立 / 待定），并且不许复述已有结论。
- **立场抽取**：发言之后，单独抽取一次结构化立场：子议题、态度、论据、建议，以及是否需要用户回答。
- **每轮小结**：主持人每轮做一次小结，并判断讨论是否已经收敛。
- **决策文档**：结构固定为：核心结论 / 各方立场 / 已达成共识 / 待你决策 / 建议的下一步。
- **防注入**：议题和对话记录都包在惰性标签里，其中的指令不会被执行。

**落盘位置**：状态和决策文档保存在 `$WILLDEEP_PLUGIN_DATA/roundtables/<id>.json` 与同名的 `.md`。工具结果里包含完整文档，要不要写进工作区由主 Agent 决定。

**与 Xedit 的差异**：
- 没有流式发言气泡和立场态势看板，每轮每位专家的立场以文本返回。
- 收敛后不会停下来问主持人是继续还是结束，而是直接生成文档。之后仍可以用 `roundtable_continue` 加轮次。

## 实现位置

- 插件包与服务端：`crates/willdeep-cli/src/builtin_plugins/`
  - `mod.rs`：包的定义、`install`、`serve`
  - `stdio_server.rs`：最小的 MCP stdio 服务端，支持反向请求
  - `scheduler.rs`、`roundtable.rs`
- 任务模型与存储：`crates/willdeep-core/src/schedule.rs`
- 触发端：`crates/willdeep-cli/src/daemon/scheduler.rs`
- 宿主变量与数据目录：`crates/willdeep-core/src/plugin/host.rs`（`${willdeepExe}`、`WILLDEEP_PLUGIN_DATA`）
