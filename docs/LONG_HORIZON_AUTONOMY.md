# 长程自主执行（long-horizon.v1）— CLI 侧引用与落地映射

> Last updated: 2026-08-13 | 状态：引用文档（设计草案，默认假设待确认，确认前不进入编码）
> Canonical 规范：Xedit 仓库 `docs/LONG_HORIZON_AUTONOMY.md`（`~/Sites/Xedit`）。
> 本文件只做两件事：摘录 willdeep-rs 实现者必须知道的契约要点；给出 rs 侧代码锚点与实施步骤。规范冲突时以 canonical 为准。
> 与 `docs/GOAL_TEAMS.md`（goal-teams.v1）互补：那份管「谁来做、做完谁验收」，本份管「怎么一直做下去、按什么清单做」。R2 与 goal-teams 的 R3 是同一块持久化改造，**合并做一次**（§3）。
> **锚点基线：`develop` @ 0.21.0-rc67。** `feat/smart-command-approval`（0.22.0-rc5）对 `agent.rs` / `tools.rs` / `harness.rs` / `tui.rs` 有改动，该分支合并后需复核本文行号。

## 1. 这是什么

对标 Codex 目标模式的**续推构件**（goal-teams.v1 对标的是它的组织构件）。核心判断一句话：

> 差距不在「有没有 goal」，而在「模型想停的时候，harness 站在哪一边」。

rs 侧目前**没有立场**——`crates/willdeep-core/src/agent.rs:275` 一旦发现 `completion.tool_calls.is_empty()` 就直接 return，判定任务完成；system prompt（`crates/willdeep-core/src/prompt.rs:30`）还在主动鼓励尽早收手；`/goal` 只是 `enrich_prompt`（`crates/willdeep-cli/src/tui.rs:2266`）在每条 prompt 前拼的一段 `<goal>` 字符串，背后是 `Session.goal: Option<String>`（`crates/willdeep-core/src/session.rs:50`），没有任何结构、进度或完成判定。

本设计引入四件：

1. **续推契约**：目标未达 + 预算未尽 = 无条件注入 continuation，四出口（`complete` / `continue` / `backoff` / `soft_stop`），**没有硬暂停出口**；
2. **预算与软停**：wall-clock / token / cost 三维预算；耗尽不是报错退出，而是标记 `budget_limited` + 注入收尾引导 + 产出状态快照，可恢复；
3. **计划工具化**：新增 `update_plan` 工具（schema 与 Xedit 同构），rs 侧从零引入计划态；
4. **重启可续**：由计划态驱动恢复，取代当前「动过工具就一律 `Interrupted`」。

## 2. 契约要点（实现者速查）

### 2.1 续推四出口

| 出口 | 条件 | 动作 |
|---|---|---|
| `complete` | 验收清单全勾且验收标准满足 | 正常收口 |
| `continue` | 目标未达 且 预算未尽 | 注入 continuation steering，继续循环 |
| `backoff` | 目标未达 且 无进展证据 且 预算未尽 | 按阶梯退避后再 `continue` |
| `soft_stop` | 任一预算维度耗尽 | 注入收尾引导，产出状态快照，标记 `budget_limited` |

退避阶梯：1–2 轮引导（对照清单指名下一个未完成项）→ 3–4 轮只读对账（封锁写工具，用 git / 文件系统核对清单）→ 5+ 指数退避（1 / 5 / 15 分钟封顶）。**有进展证据即归零**；进展证据 = 本轮有工具成功执行 ∨ 有后台任务/子 Agent 存活 ∨ 清单勾选发生变化。「在等一个长 CI」因此不会被误判为卡死。

### 2.2 continuation steering 内容契约（四段，顺序固定）

1. 目标与剩余清单（未完成项带 1-based 索引，已完成项只给计数）；
2. 态势：已耗时、已用 token / 成本、各维度预算余量；
3. 上一轮判定：为什么 harness 认为目标未达；若在退避档位，本轮的额外约束；
4. 动作要求：指名下一个清单项并立即执行；**明确禁止「总结已完成工作」与「询问用户是否继续」**。

第 4 段是关键——长程运行最常见的失败不是拒绝干活，是用漂亮总结冒充进展。

### 2.3 `update_plan` 工具 schema（双端同构，勿各写各的）

```json
{
  "name": "update_plan",
  "input": {
    "merge": true,
    "steps": [
      { "id": "s1", "title": "…", "status": "pending|in_progress|done|skipped|failed", "note": "…" }
    ],
    "checklist": [
      { "index": 1, "done": true, "evidence": "cargo test --workspace: 412 passed / 0 failed" }
    ],
    "blocked_reason": "…"
  }
}
```

- `merge: true` 按 `id` / `index` 局部更新，避免长清单每轮全量重发；
- `evidence` 是硬要求：**没有证据的勾选视为未勾选**，同时为 goal-teams.v1 的交叉验收供料；
- 校验失败应回灌重试且**不消耗续推预算**（rs 侧当前无 outputSchema 机制，与 goal-teams R4 的 capsule 一并建设）。

### 2.4 预算维度与恢复

| 维度 | 默认 | 备注 |
|---|---|---|
| `wallClock` | 4 小时/段（可配置至 72 小时） | 暂停期间不计，跨重启累计 |
| `tokens` | 无默认上限（可配置） | 锚定 provider 报告的真实用量，不用本地 `chars/4` 估算 |
| `cost` | 无默认上限 | 依赖成本引擎，未落地前该维度为空，不阻塞其余两维 |

恢复规则：wall-clock 耗尽且由后台任务完成事件触发 → 自动续期；**token / cost 耗尽必须用户确认**（花钱的闸门不自动放行）。

## 3. rs 侧落地步骤与代码锚点（develop @ 0.21.0-rc67）

### 落地状态（2026-09-30）

- **RA1 已落地**：`crates/willdeep-core/src/goal.rs` 的 `GoalContinuation::evaluate`，在 `agent.rs` 的无工具调用分支里判定。
- **RA2 已落地（持久化 + 清单部分）**：
  - `GoalState`（目标、验收标准、步骤、阻塞原因、状态、已用时长、续推计数）随检查点与消息同一次写入 `Session.goal_state`。它属于执行快照，并发合并时计数取单调较大者。`Session.goal` 单字符串保留，作为向后兼容的目标语句。
  - `update_plan` 工具只在有激活目标时出现在主 Agent 的工具面里，属于 `PARENT_ONLY_TOOLS`。`steps` / `checklist` / `blocked_reason` / `merge` 与 §2.3 同构。另有一个本端扩展 `criteria`，用来写下验收标准本身，因为 canonical 只规定了按序号勾选。没有证据的勾选会被拒绝，校验失败不消耗续推预算，清单有变化算作一次进展。
  - **完成门禁**：模型声明完成时，只要还有未带证据的验收标准，或还有在跑的委派 Worker（只数 `Subagent`，不数 shell / monitor），就返回 `CompletionRejected` 并点名缺什么。这一次计入续推预算，免得虚报完成绕开预算。没有定义验收标准时行为不变。
  - **压缩固定区**：`<goal-state>` 块每轮重建进 system 消息。system 消息不参与压缩摘要，所以目标、清单与在飞 Worker 不会被摘要掉；本地摘要提示也附上它，避免摘要与之矛盾。
  - Web 端 `/goal` 仍是前端状态、随每轮 `<goal>` 信封到达。持久化状态在会话里只「停放」，信封带着同一句目标来才接回，所以关掉的目标不会复活。
- **wall-clock 跨重启累计**：计时只在轮次运行时走，轮次之间与进程不在时不计。daemon 每轮重建 harness，从 `session.goal_state` 恢复（`harness::restore_goal`），不再每轮从满格预算重来。
- **RA3 token 预算**：
  - **记账**：`GoalState.tokens_used` 累计为这个目标用掉的 token，主 Agent 和它派出的 Worker（前台、后台）都算；每次调用优先取 `total_tokens`，否则取输入 + 输出（`Usage::billable_tokens`）。它随检查点落盘，跨轮次、跨重启累计。
  - **收尾**：超过 `GoalBudget.max_tokens` 时，与 wall-clock、续推次数用同一条收尾路径（`SoftStopReason::Tokens` → 交接快照 → `BudgetLimited`）。
  - **配置**：`[agent] goal_token_budget`（缺省不限）、`goal_wall_clock_minutes`（缺省 240）、`goal_max_continuations`（缺省 64）。预算显示在 `<goal-state>` 与续推引导的“态势”一段里。
- **RA4 重启后自动续推**：`daemon/goal_resume.rs` 在 daemon 启动时跑一次。
  - **续推谁**：最后一轮被重启打断（`Runtime restarted after Turn history became ambiguous`）、没有排队轮次、core 会话里目标仍在进行且预算未尽的会话。
  - **续推轮次**：各排一轮续推，`origin_client` 为 `goal-resume:<被打断轮次>`。提示词开头带 `<goal>` 信封，这样才能接回停放的状态；随后要求先用只读工具核对实际状态、不要重做已完成的步骤，并列出未完成的验收项与步骤。
  - **兜底**：
    - 已完成、已在收尾、预算用尽的目标不续推（事件 `goal.resume_skipped reason=`）；
    - 连续 3 个续推轮次都被重启打断就停手（`repeated_restarts`）；
    - `request_id` 由被打断的轮次派生，同一次中断只排一轮；
    - `[agent] goal_auto_resume = false` 关掉。
  - **留下的记录**：事件 `goal.resumed`，反馈账本信号 `goal_resumed`。
- **尚未做**：
  - 只有 token 维度，没有 cost（缺价格表）；
  - 退避阶梯的真实延时。

| 步骤 | 内容 | 关键位点 |
|---|---|---|
| RA1 | 停止条件改造：goal 激活时 `tool_calls.is_empty()` 先过目标判定，未达则注入 continuation；goal 模式下改写早停引导。**建议复用既有续推钩子**：`append_pending_instructions` 已是现成的「给出终稿但 inbox 非空则 continue」通道 | `crates/willdeep-core/src/agent.rs:244`（主循环 `for turn in 1..=max_turns`）、`:275`（现停止条件）、`:312`（续推钩子）、`crates/willdeep-core/src/prompt.rs:30`（早停引导） |
| RA2 | 计划态对象 + `update_plan` 工具（§2.3）。**与 goal-teams.v1 的 R3 合并推进**：一次性把 `Session.goal` 单字符串换成结构化 goal / roadmap / plan，顺带修 Web 端 `/goal` 刷新即丢 | `crates/willdeep-core/src/session.rs:50`、`crates/willdeep-core/src/tools.rs:271`（`definitions`）、`:441`（`execute` 分发）、`crates/willdeep-cli/src/tui.rs:2266`（`enrich_prompt`）、`crates/willdeep-cli/src/web.rs:351`、`web/src/App.tsx:108,477-480` |
| RA3 | 预算与软停：`MaxTurns` 从 `Err` 改为可续软停状态；主 Agent 补 token / wall-clock 预算（当前恒为 `None`）；压缩摘要把「计划与剩余步骤」列为**不可压缩固定区** | `agent.rs:309`（现 `Err(AgentError::MaxTurns)`）、`crates/willdeep-cli/src/harness.rs:269`（默认 24 轮、上限 100）、`:509`（主 Agent `token_budget: None`）、`agent.rs:367`（`compress_history`）、`:407`（在途压缩 `request_messages`） |
| RA4 | 重启由计划态驱动恢复：动过工具的任务不再一律 `Interrupted`，按已落盘 plan 状态续推 | `crates/willdeep-cli/src/daemon.rs:2366`（现一刀切 `Interrupted`）、`crates/willdeep-cli/src/daemon/session_store.rs:247-251`（`has_tool_activity` 卡死的 turn 重放救生索）、`:1875`（`prepare_core_for_turn_replay`）、`:811/828`（turn 调度队列，可复用） |

### 步序

**RA1 无前置依赖，建议最先做**——它是「能不能连续跑一天」的直接开关，改动面最小、可独立验收。RA3 依赖 RA1 的判定骨架；RA2 与 goal-teams R3 合并；RA4 依赖 RA2 的计划态落盘。每步独立提交、独立验收。

canonical 侧默认 Xedit 先行（XA1–XA4 见 canonical §6.1），但 RA1 与 Xedit 的 XA1 之间**没有依赖**，两端可并行起步；真正必须同期定稿的只有 §2.3 的 `update_plan` schema。

### 已有基建（别重造）

rs 侧地基其实相当齐，本设计要补的全在「意图层」：

- 续推钩子：`AgentInstructionInbox` + `append_pending_instructions`（`agent.rs:312`）；
- 后台完成回流：`<subagent-report>` 注入 + 重跑（`crates/willdeep-core/src/background.rs:404`、`crates/willdeep-cli/src/harness.rs:213-230`）——注意它当前**由后台事件驱动，不由目标进度驱动**，RA1 要把触发器改对；
- turn 调度队列与事件游标：`daemon/session_store.rs:811/828`、daemon 事件日志；
- 主→子 `instruct` 指令通道（rs 独有，Xedit 尚缺）：`agent.rs:312` + 控制面 `agent.prompt`。

## 4. 待确认假设（与 canonical §7 同步）

wall-clock 4 小时/段（可配至 72 小时）；退避阶梯 1–2 引导 / 3–4 只读对账 / 5+ 指数退避（1、5、15 分钟封顶）；软停后仅 wall-clock 维度可自动续期，token / cost 需用户确认；`update_plan` 与围栏块共存 2 个 minor 版本（rs 侧无围栏块历史包袱，直接上工具）；计划态唯一真源为 plan / checklist 对象。任何一条被推翻，**先改 canonical 再同步本文件**。

## 5. Non-goals

- 不做「永不停止」：预算是硬边界，软停是有序收尾而非无限重试；
- 不放宽审批边界：续推不得成为绕过危险操作确认的通道；
- 不合并两端 runtime、不引入共享控制协议（那是 `CLI_TUI_RUNTIME_ROADMAP` 的另一条线），沿用文件层共享 schema；
- 不重做 goal-teams.v1 的角色 / 里程碑 / capsule 设计，两份互补；
- 不改用户主动中断的语义：用户 stop 永远是终态，优先级高于任何续推判定。
