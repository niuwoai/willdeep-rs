# 本机反馈账本

`willdeep.feedback.v1` 为 RSI（递归自我改进）收集**强反馈信号**：用户是否采用了我们的建议、工具失败了多少次、Worker 是否没交出结果就停了。设计背景见 [Prompt RSI 设计提案](PROMPT_RSI_DESIGN.md) §6 与 §10，后续阶段见 [路线图](ROADMAP_ORCHESTRATION_RSI.md)。

代码在 `crates/willdeep-core/src/feedback.rs`。

## 位置与格式

- 文件是 `$WILLDEEP_HOME/feedback/YYYY-MM.jsonl`，按 UTC 月份分片，权限 0600。
- 每个信号一行 JSON，整行小于 4096 字节，用 `O_APPEND` 一次写完。daemon 与多个 TUI 可以并发写同一个文件。
- **所有键都会输出**，字段未知时值为 `null`。读端遇到不认识的 `schema` 或 `signal` 应跳过该行。
- 记录是旁路：热路径只做一次 `try_send`，队列满了就丢弃并计数；IO 错误只警告一次，不会让界面或轮次失败或变慢。

## 配置

```toml
[feedback]
enabled = true      # 默认开；只写本机，不上传
store_text = false  # 默认只记 hash 与长度
retain_months = 12  # daemon 启动时删掉更早的月份分片；0 = 不清理
```

`store_text = true` 时，会额外写入建议文本与发送文本，各截断到 400 字符。含凭据特征的文本（`looks_sensitive`）即使打开也不写。工具失败只记工具名与错误类别，**从不**记参数或输出。

## 信号

| `signal` | 何时 | 关键字段 | 强度 |
|---|---|---|---|
| `suggestion_shown` | 下一句建议以灰字出现 | `suggestion_id`, `text_hash`, `text_len` | 分母 |
| `suggestion_accepted` | Tab 采用（只填入，没发送） | `dwell_ms` | 中间态 |
| `suggestion_sent_verbatim` | 采用后原样发送 | `sent_hash`, `edit_distance = 0` | 最强正向 |
| `suggestion_sent_edited` | 采用后小改、或保留原文再补充后发送 | `edit_distance`, `sent_len` | 正向：方向对但不完整 |
| `suggestion_sent_rewritten` | 采用后基本重写（编辑距离超过建议长度一半）再发送 | `edit_distance` | 弱负向 |
| `suggestion_ignored_typed` | 没动建议，直接打了别的字 | `dwell_ms` | 负向 |
| `suggestion_dismissed` | Esc 放弃 | `dwell_ms` | 明确负向 |
| `suggestion_superseded` | 没有结局就被新一轮或新建议顶掉；已采用但始终没发送也记这个 | | 中性 |
| `tool_failed` | 一次工具调用返回错误（主 Agent 与 Worker） | `tool`, `error_class` | 客观失败 |
| `agent_incomplete` | 一次运行未收敛就停了：`max_turns`、`incomplete`、`unverified`、`budget_limited` | `stop_reason`, `turns`, `report_len` | 客观失败；`report_len = 0` 表示没有结果 |
| `worker_timed_out` | Worker 超时被中止 | `report_len = 0` | 客观失败 |
| `worker_verifier_exhausted` | Worker 用完全部尝试仍未通过验证命令 | `attempts` | 客观失败 |
| `user_followup` | 同一会话提交了第二句及以后的话（所有前端经 Runtime 提交的轮次；会话第一句不记） | `prev_status`, `gap_ms`, `queued_behind`, `followup_hint`, `text_hash` | 需结合上一轮结局判读 |
| `user_steer` | 轮次进行中插话 | `delivered`, `followup_hint` | 模型跑偏或信息不足的信号 |
| `session_rewound` | 用户回退会话 | `count`（丢掉的轮数）、`decision`（`workspace_restored` / `transcript_only`） | 强负向：那几轮做错了 |
| `approval_resolved` | 审批 / 提问得到处置 | `interaction_kind`, `decision`, `latency_ms` | `deny` 是负向；`cancelled` 是轮次被停下时自动撤销，**不是**人的拒绝 |
| `turn_cancelled` | 用户中途停下一轮 | `prev_status` | 负向：方向错或太慢 |
| `goal_completed` / `goal_completion_rejected` / `goal_budget_limited` | 目标收尾（见 [长程自治](LONG_HORIZON_AUTONOMY.md)） | `count`（未完成的验收项）、`continuations`, `elapsed_ms` | 「完成被拒」是虚报完成的强信号 |

同一条建议的各行共享 `suggestion_id`。Web 端的 `suggestion_id` 由服务端签发，服务端记住建议原文与发出时间，浏览器只回传 id 与信号，所以账本里的原文与停留时长不采信浏览器给的值。Worker 的行带 `agent_id` 与 `worker_profile`，主 Agent 的行这两个字段是 `null`。Runtime 轮次的行带 `turn_id`。

`error_class` 的取值来自 `ToolError::class`，例如 `io`、`invalid_arguments`、`approval_denied`、`hook_denied`、`edit_text_not_found`、`edit_text_not_unique`、`command_timeout`、`network`、`mcp`、`unknown_tool`。

## 如何读这些信号

- **不要只看 Tab。** 只按了 Tab、之后没有发送，价值很低。「采用并原样发送，且下一轮没有被纠正」才是强正向。
- `dwell_ms` 越短、`edit_distance` 越小，说明建议越接近用户原本想说的话。
- `tool_failed` 按 `(tool, error_class)` 聚合，可以看出哪类工具描述或提示词让模型反复犯错。`edit_text_not_found` 与 `invalid_arguments` 是最直接的提示词改进目标。
- `agent_incomplete` 中 `stop_reason = max_turns` 且 `report_len = 0`，就是「轮次耗尽也没有结果」。按 `worker_profile` 聚合，可以看出哪个工种的轮次预算或任务拆分有问题。
- `user_followup` 的 `followup_hint` 只是入口处的词法粗标签（`correction` / `redo` / `supplement` / `approval` / `other`）。判断上一轮是否做错，要把它和上一轮的结局一起看：`prev_status`、之后有没有 `session_rewound` 或 `turn_cancelled`、审批有没有被 `deny`。`gap_ms` 很短的纠正比隔了一天的纠正更可能是在纠正上一轮。
- `willdeep audit export` 的「反馈信号」一节按会话汇总以上计数，不出任何正文。

## 尚未覆盖

- `/local` 进程内轮次不经过 Runtime，没有 `user_followup`。
- 取消的来源（TUI Esc、Web、手机）：记录来源需要改协议，会让新客户端连不上旧 daemon，暂不做。
- 轮次结束后的 git 还原（`git restore` 了本轮改过的文件）。
