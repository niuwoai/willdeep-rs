# 路线图：主 Agent 协调、长任务、Xedit 插件移植与 RSI 反馈数据

状态：讨论稿（2026-09-29）。依据是代码的只读审查。Phase A 的第一部分已经实现，见 [本机反馈账本](FEEDBACK_LEDGER.md)。

本文覆盖三件事：

1. 提升主 Agent 下发任务、协调 Worker 的能力，以及完成长任务和 goal 任务的能力。
2. 把 Xedit 的定时任务与专家团沟通能力以插件方式移植过来。
3. 有针对性地收集有助于 RSI 的强反馈数据。

## 一、主 agent 协调 & 长任务（现状自查）

已有的强项：`spawn_agent` + TaskPacket（goal/read_files/write_files/verifier/max_attempts）、5 种 trade、runner 内 verifier 由运行时直接执行（不靠模型自评）、file lease、circuit breaker、goal 续跑 + 无进展阶梯 + 交接快照。

### 缺口（按影响排序）
1. **完成由模型自报**：`goal.rs:244` 仅检查回复是否以 `<goal-status>complete</goal-status>` 开头；wrap-up 路径接受任何回复。goal 只是一个字符串（`session.rs:83`），无验收清单/证据。
2. **无父侧计划/任务图**：扇出、依赖、汇合只存在于模型上下文；`update_plan`（RA2）与计划驱动恢复（RA4）未实现。压缩（`agent/context.rs`）没有 pin 区，goal/清单/在飞 worker 可能被摘要掉。
3. **goal 与后台 worker 脱钩**：worker 未返回时 goal 可被宣告完成，之后报告到达时 goal 已清。
4. **goal 状态在内存**：重启后 4h/64 次预算被重置（`harness.rs:1017`），daemon 重启把 turn 标为 Interrupted 而非续跑。
5. **前台 worker 串行**（`spawn_agent` 不在 `parallel_reads`），并行只靠后台且上限 5；无 worker 间通道（`PARENT_ONLY_TOOLS`），依赖只能经父上下文绕行。
6. **worker 报告是自由文本**，64KiB 尾截断可能丢结论；无结构化 claims/evidence/files_touched，无并行 worker 的合并/冲突检查。
7. reader/reviewer/generalist 无强制 verifier；circuit breaker 按 profile 而非按任务，无衰减；无 token/成本预算；backoff 只是提示词。

### 建议方案（分阶段，先小后大）
- **P1 Goal 持久化 + 验收清单**（最高性价比）：`update_plan` 工具 + Session 内结构化 `GoalState{criteria[], status, evidence, elapsed, continuations, workers_in_flight}`，落盘并在 resume 时累计预算；完成门槛 = 所有 criteria 有 evidence 且运行时 verifier 通过且无在飞 worker。改动：`goal.rs`、`session.rs`、`harness.rs:1016`、新工具于 `tools.rs`、`agent.rs:1010` 完成门。
- **P2 压缩 pin 区**：`agent/context.rs` 中把 GoalState + worker 台账作为不可摘要块。
- **P3 结构化 worker 报告**：TaskPacket 增加 `report_schema`，`runner.rs` 输出 `{claims, evidence, files_touched, verdict, open_questions}`；截断改为保头保结论；`subagent/text.rs`。
- **P4 并行/汇合**：允许同一轮多个 `spawn_agent` 并发（写集经 file_leases 不相交才可并发）；增加 `await_agents` 汇合语义，goal 完成门读取台账。
- **P5 worker 间共享黑板**（只读订阅 + 追加写，由父授权），替代 worker→worker 直连，避免破坏 `PARENT_ONLY_TOOLS` 安全模型。
- **P6 韧性**：circuit breaker 按 task 计数 + 衰减；失败自动升级 tier 重派；goal 增加 token 预算。

---

## 二、Xedit 定时任务 + 专家团 → 插件

### 现状
- 插件系统成熟（`core/plugin/*`、`plugin_cmd.rs`、`plugin_web.rs`），但扩展点只有：commands/menus/pages/settings + 插件自带 stdio MCP 服务器 + 反向请求（仅 `willdeep/ai/complete`、`willdeep/images/generate`）。**插件不能注册 agent 原生工具、子 agent profile、hook、slash 命令、后台任务。**
- **定时任务完全未实现**：`docs/PLUGINS.md` 写明 `schedules` 设计中；`EventSource::Schedule`（`kernel_event.rs:62`）是预留钩子；daemon 有 `task_manager.rs`、`detached_job.rs`、`background.rs` 可复用。`docs/XEDIT_TOOL_PARITY.md:24` 列出 Xedit 的 `schedule_task`/`complete_scheduled_task`，本仓库无实现。
- **专家团完全不存在**：无辩论循环/主持人/共享 transcript；Goal Teams 仅为设计稿。
- Xedit 设计文档（PLUGIN_SYSTEM_DESIGN、PLUGIN_HOST_CAPABILITIES_DESIGN、plugin JSON schema）不在本地。

### 建议方案
**定时任务插件**（宿主需补的能力是主要工作量）
1. 宿主新增 `contributes.schedules` manifest 键（先在 `manifest.rs` 已知键中加入并校验）。
2. **由 daemon 持有 tick**（web 与 daemon 是不同进程，插件 MCP 按需启动，不能自己计时）；触发时以 `EventSource::Schedule` 提交 turn（走 `task_manager.submit`）。
3. 新增反向请求/host action：`willdeep/turn/submit`（受 `conversation.write` + 新权限 `schedule.run` 约束）。
4. 无人值守审批策略：定时 turn 默认最严权限（只读 + 显式白名单），否则挂起为待审批而不是自动放行。
5. 持久化、停机补跑策略（跳过/补一次）、状态面板（declarative page）。
6. 插件本体：MCP server 提供 `schedule_task / list / cancel / complete_scheduled_task`，语义与 Xedit 对齐。

**专家团插件**
1. MVP：MCP 插件，用 `willdeep/ai/complete` 循环调用各专家 persona（现限制：24 条消息、4096 输出 token、无流式、无工具）。可快速验证交互，但能力弱。
2. 完整版依赖宿主新增：反向请求 `willdeep/agent/spawn`（以 subagent 形式跑专家，可带工具与 tier）、可读会话 transcript、`events.on` 对无页面插件开放或新增推送通道。
3. 展示：`declarative`/`localWeb` 页面渲染讨论 transcript；主持人 + 多轮 + 收敛判据 + 产出结论写回会话。
4. 与第一节 P5（黑板）复用同一共享状态机制，专家团 = 一组 worker + 黑板 + 主持人。

**前置依赖**：需要 Xedit 源码或设计文档来对齐行为。计划通过挂载 Xedit 仓库获取，owner/repo 待提供。

---

## 三、RSI 强反馈数据（重点）

已有 `docs/PROMPT_RSI_DESIGN.md`（提案，Phase 0 = 数据收集，未实现）。代码现状：建议的展示/采纳/忽略**完全不落盘**（`input_suggestion.rs` 头注释明确“不落盘、不进 Runtime 协议”）；`events.ndjson` 的 `turn.queued` 不含 prompt 文本；steer、rewind 无事件；无 prompt 版本戳、无保留期配置。

### 已实现（Phase A 第一部分）
- 建议生命周期（TUI）：shown、accepted、dismissed、ignored_typed、superseded、sent_verbatim、sent_edited、sent_rewritten。
- 工具失败 `tool_failed`：按 `ToolError::class` 分类，主 Agent 与 Worker 都记。
- 未收敛运行 `agent_incomplete`：`max_turns` 等，带 `report_len`，为 0 表示没有结果。
- Worker 没有结果的收尾：`worker_timed_out`、`worker_verifier_exhausted`。

### 信号分级
- **S 级（强、明确）**：Tab 采纳建议；采纳后原样发送 vs 编辑后发送；建议被忽略并输入了别的内容；审批 allow/deny；diff review accept/reject/changes_requested；rewind/fork 回退；turn 被打断。
- **A 级（强，需分类）**：后续用户输入 = 纠正 / 补充背景 / 重做 / 追问 / 新话题 / 认可。
- **B 级（客观结果）**：编辑后验证 pass/fail、tool error、retry、后续 git revert/restore 本轮文件。
- 反 reward-hacking：不单以 Tab 训练；“采纳并原样发送且本轮未被纠正”远强于“仅按 Tab”；主观判断（plausible/wrong-voice）保留人工标签（沿用 `bench/input-suggestion`）。

### 设计
1. **单一 sink**：`feedback/YYYY-MM.jsonl`（0600，单行 <4096B，仿 `usage_ledger.rs`/`sink.rs` 的有界通道、单写、永不失败）。字段：`schema, ts, session_id, turn_id, suggestion_id, client, prompt_bundle_ver, signal, payload`。默认只存 hash+长度，正文需显式 opt-in（PROMPT_RSI_DESIGN §10.3）；新增 `[feedback]` 配置含保留期，并补上现有日志缺失的 retention。
2. **建议生命周期埋点**（TUI，`app_state.rs`）：shown=`adopt_input_suggestion:1009`、accepted=`accept_input_suggestion:1037`、dismissed=`dismiss_input_suggestion:1046`、ignored_typed=`edit_input:745`、superseded=`begin_turn:1058`、sent_verbatim/sent_edited=提交路径（`event_loop.rs ~1338`、`runtime_ui::submit_turn`，比较编辑距离）。Web：新增 `POST /api/feedback`（`web.rs`），`inputSuggestion.ts` 与 `App.tsx:1114-1117` 回传，`suggestion_id` 关联两端。
3. **后续输入上下文**：在 `dispatch_prompt` / `runtime_ui::submit_turn` / `turn.steer` 入口发 `user_followup`：上一 turn 状态（Completed/Partial/Cancelled/Interrupted）、期间是否发生 rewind/拒绝/review 驳回、间隔时间、steer/queued/direct、长度、词法类别；纠正/补充的精确分类离线做（session JSON + 事件日志），入口只保证有序 turn_id 链接。
4. **补小缺口**：`session.rewound`（含目标位置）、steer 事件、`approval_resolved`（HOOKS.md 已注明未接线）、turn.cancelled 带来源、post-turn git 结果。
5. **join 与消费**：`audit_cmd.rs` 已有 per-session join，扩展读取 feedback；`bench/input-suggestion/samples` 由真实 accept/reject 晋升；离线优化器按 PROMPT_RSI_DESIGN 分层使用。
6. 数据质量：给 suggestion 记录生成模型/版本，便于 A/B 比较采纳率；不采集含敏感标记的文本（复用 `looks_sensitive`、`redact_credentials`）。

---

## 建议的推进顺序
1. **Phase A（已完成）**：三-2/3/4 反馈埋点 + sink；Web 建议生命周期；后续输入、插话、回退、审批、取消信号；保留期；审计汇总。详见 [本机反馈账本](FEEDBACK_LEDGER.md)。
2. **Phase B（已完成 P1/P2）**：GoalState 持久化 + 验收清单 + `update_plan` + 完成门禁 + system 消息固定区。详见 [长程自治 §3 落地状态](LONG_HORIZON_AUTONOMY.md)。未做：RA3 token/cost 预算、RA4 重启自动续推。
3. **Phase C（已完成）**：Worker 报告末尾的 `<worker-facts>`、保头保尾截断、`await_agents` 汇合、派工时写集冲突预检、熔断冷却后试探；审计里的纠正率离线标注。详见 [子 Agent](SUBAGENTS.md)「并行派工与汇合」。未做：同一轮多个前台派工并发（审批不能并发）、Worker 间共享黑板（P5）。
4. **Phase D（已完成）**：定时任务与专家圆桌以内置插件形式移植（`willdeep plugin builtin install scheduler|roundtable`），宿主补了 `${willdeepExe}`、`WILLDEEP_PLUGIN_DATA` 与 daemon 调度器。详见 [内置插件](BUILTIN_PLUGINS.md)。未做：圆桌的流式气泡与态势看板、定时任务的 Web / TUI 管理界面。
5. **Phase E（进行中）**：
   - 已完成：Worker 共享黑板 `board_post` / `board_read`（P5），详见 [子 Agent](SUBAGENTS.md)。
   - 已完成：反馈行的提示词版本戳 `prompt_bundle`，以及 `willdeep feedback bundles`。
   - 已完成：跨会话报告与确定性改进候选 `willdeep feedback report [--candidates]`，对应 RSI §8.1 优化器的输入。详见 [本机反馈账本](FEEDBACK_LEDGER.md)。
   - 未做：专家圆桌的流式看板。
6. **Phase F（已完成）**：提示词分段与单段变体（`willdeep prompt sections|show|check|draft`），带结构门，只在 `run --local` 中通过 `WILLDEEP_PROMPT_VARIANT` 加载；任务集按 train / validation / holdout / regression 分组；`scripts/prompt_rsi_eval.rb` 做 baseline 与候选的对照评测，并按 §8.4 门禁给出结论，永不自动上线。详见 [操作手册](PROMPT_RSI_OPERATIONS.md)。未做：由模型撰写变体文本、服务端数据接收与 canary 灰度。

## 验证
- 单元测试：GoalState 序列化/resume 预算累计、完成门（有在飞 worker 不得完成）、sink 行大小与脱敏 keyset 测试（仿 usage ledger）。
- TUI 测试套件（`tui/test_suite/`）新增：Tab/Esc/打字/原样发送/编辑后发送各产生对应 feedback 行。
- `crates/willdeep-cli/tests/headless_runtime.rs` 增加：定时触发提交 turn、daemon 重启后 goal 续跑。
- 端到端：本地起 `willdeep`，走一轮建议→Tab→发送，检查 `feedback/*.jsonl`；`audit_cmd` 报告含新信号。
- 现有 `cargo test --workspace`、`cargo clippy` 必须保持绿。
