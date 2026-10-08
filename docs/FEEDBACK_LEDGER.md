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
| `suggestion_ignored_typed` | 未采用建议，最终发送了其他内容；只打字、再删空不会结算 | `dwell_ms` | 负向 |
| `suggestion_dismissed` | Esc 放弃 | `dwell_ms` | 明确负向 |
| `suggestion_superseded` | 没有结局就被新一轮或新建议顶掉；已采用但始终没发送也记这个 | | 中性 |
| `tool_failed` | 一次工具调用返回错误（主 Agent 与 Worker） | `tool`, `error_class` | 客观失败 |
| `tool_succeeded` | 一次工具调用成功返回（主 Agent 与 Worker） | `tool`, `run_id` | 已结算工具调用分母；不代表任务完成 |
| `run_started` | Agent 开始一次运行（主 Agent 与 Worker） | `run_id`, `provider`, `model` | 运行分母；provider / model 为运行起始归属，未知时 null |
| `agent_incomplete` | 一次运行未收敛就停了：`max_turns`、`incomplete`、`unverified`、`budget_limited` | `stop_reason`, `turns`, `report_len` | 客观失败；`report_len = 0` 表示没有结果 |
| `worker_started` | 一次 Worker 派工开始（重试不另记） | | 分母：按工种算「没有结果」比例 |
| `worker_timed_out` | Worker 超时被中止 | `report_len = 0` | 客观失败 |
| `worker_verifier_exhausted` | Worker 用完全部尝试仍未通过验证命令 | `attempts` | 客观失败 |
| `user_followup` | 同一会话提交了第二句及以后的话（所有前端经 Runtime 提交的轮次；会话第一句不记） | `prev_status`, `gap_ms`, `queued_behind`, `prev_turn_id`（这句话反应的那一轮；本行 `turn_id` 是新提交的一轮）, `followup_hint`, `text_hash` | 需结合上一轮结局判读 |
| `user_steer` | 轮次进行中插话 | `delivered`, `followup_hint` | 模型跑偏或信息不足的信号 |
| `session_rewound` | 用户回退会话 | `count`（丢掉的轮数）、`decision`（`workspace_restored` / `transcript_only`） | 强负向：那几轮做错了 |
| `approval_resolved` | 审批 / 提问得到处置 | `interaction_kind`, `decision`, `latency_ms` | `deny` 是负向；`cancelled` 是轮次被停下时自动撤销，**不是**人的拒绝 |
| `turn_cancelled` | 用户中途停下一轮 | `prev_status` | 负向：方向错或太慢 |
| `goal_resumed` | daemon 重启打断了进行中的目标，运行时自动排了一轮续推 | `count`（未完成的验收项）、`continuations`, `elapsed_ms` | 中性：用来看重启续推之后的结局 |
| `run_verified` | 每次 Agent 运行（主 Agent 与 Worker）收尾，不论成败 | `verification`（`passed` / `failed` / `stale` / `unverified`）、`count`（这次运行实际跑过的验证命令数） | 运行时事实：`passed` 正向、`failed` 负向；`stale`（验证后又改了文件）与 `unverified` 中性 |
| `goal_completed` / `goal_completion_rejected` / `goal_budget_limited` | 目标收尾（见 [长程自治](LONG_HORIZON_AUTONOMY.md)） | `count`（未完成的验收项）、`continuations`, `elapsed_ms` | 「完成被拒」是虚报完成的强信号 |

同一条建议的各行共享 `suggestion_id`。Web 端的 `suggestion_id` 由服务端签发，服务端记住建议原文与发出时间，浏览器只回传 id 与信号，所以账本里的原文与停留时长不采信浏览器给的值。Worker 的行带 `agent_id` 与 `worker_profile`，主 Agent 的行这两个字段是 `null`。Runtime 轮次的行带 `turn_id`。

从 0.91.0-rc1 起，一次 Agent 运行的开始、工具结果和收尾共享 `run_id`，并带运行起始 provider / model。运行错误或中断可能只有开始而没有 `run_verified`，不能算作已验证成功。工具成功 / 失败仅覆盖已返回的调用，尚未返回或取消的调用不在这个分母内。旧账本缺少这些字段，不能用新分母计算旧事件失败率。

`ruby scripts/feedback_health.rb --out target/rsi-health/latest` 生成无正文的 JSON / Markdown 健康报告，检查坏行、关联覆盖、建议终结和分母缺口，并列出仅供复现的工具失败调查队列。它不晋升提示词，也不降低候选阈值。

每一行都带 `prompt_bundle`：产生它的提示词版本，形如 `角色@<sha256 前 12 位>`。
- 角色有 `main`（主 Agent）、`worker:<工种>`（托管工种为 `worker:<工种>@hosted`）和 `input_suggestion`。
- 哈希只取提示词里**不随运行变化**的部分：稳定契约、工种契约、Worker 边界模板、报告约定、黑板说明、建议的系统提示。工作区路径、平台和用户的全局规则都不计入。所以同一份代码在哪台机器上跑，版本号都一样；提示词改一个字，版本号就变。
- 建议相关的行一律记为 `input_suggestion@…`。
- 追问、插话、回退、审批、取消这几类，是用户对主 Agent 输出的反应，记为 `main@…`。
- 旧行没有这个字段，读的时候按 `null` 处理。
- `willdeep feedback bundles` 打印当前构建里各角色的版本号。

`error_class` 的取值来自 `ToolError::class`，例如 `io`、`invalid_arguments`、`approval_denied`、`hook_denied`、`edit_text_not_found`、`edit_text_not_unique`、`command_timeout`、`network`、`mcp`、`unknown_tool`。

## 如何读这些信号

- **不要只看 Tab。** 只按了 Tab、之后没有发送，价值很低。「采用并原样发送，且下一轮没有被纠正」才是强正向。
- `dwell_ms` 越短、`edit_distance` 越小，说明建议越接近用户原本想说的话。
- `tool_failed` 按 `(tool, error_class)` 聚合，可以看出哪类工具描述或提示词让模型反复犯错。`edit_text_not_found` 与 `invalid_arguments` 是最直接的提示词改进目标。
- `agent_incomplete` 中 `stop_reason = max_turns` 且 `report_len = 0`，就是「轮次耗尽也没有结果」。按 `worker_profile` 聚合，可以看出哪个工种的轮次预算或任务拆分有问题。
- `user_followup` 的 `followup_hint` 只是入口处的词法粗标签（`correction` / `redo` / `supplement` / `approval` / `other`）。判断上一轮是否做错，要把它和上一轮的结局一起看：`prev_status`、之后有没有 `session_rewound` 或 `turn_cancelled`、审批有没有被 `deny`。`gap_ms` 很短的纠正比隔了一天的纠正更可能是在纠正上一轮。
- `willdeep audit export` 的「反馈信号」一节按会话汇总以上计数，不出任何正文。
- 审计里的**纠正率**是离线标注，不调模型：一句后续输入算纠正，条件是词法粗分类为 `correction`，或者从上一句后续输入（最多往前 10 分钟）到它之间出现了 `session_rewound`、`turn_cancelled` 或 `decision=deny` 的 `approval_resolved`（`cancelled` 不算，那是取消时的自动撤销）。其中上一轮以 `completed` 收尾却被纠正的，单独计为「已完成却被纠正」：模型自以为做完、用户不认，是最强的负样本。

## 跨会话报告与改进候选

`willdeep feedback report [--since TIME] [--json] [--candidates PATH]` 读取整个账本，跨会话汇总以下几类数据：
- **输入建议**：按 `prompt_bundle` 分组，统计展示、采用、原样发送、改后发送、放弃，以及采用率。
- **Worker**：按工种统计派工次数、没有结果的次数及比例（空报告的未收敛、超时、验证用尽，都按 Worker 去重）、未收敛次数、工具失败次数。
- **工具失败**：按 `(工具, 错误类别)` 排行。
- **goal**：完成、完成被拒、预算耗尽的次数，以及被拒比例。
- **纠正率**：总体一个值，另外按主 Agent 版本、按周（取周一，UTC）分别给出，并列出纠正最集中的会话。

报告最后一节是**改进候选**，全部由确定性规则得出，不调用模型：

| 规则 | 阈值 | 指向 |
|---|---|---|
| 同一 `(工具, 错误类别)` 反复失败 | ≥ 5 次；`approval_denied`、`hook_denied` 是人的决定，不计入 | 工具说明或 `tool_rules` 中对应的段落（例如 `edit_text_not_found` 对应“先读再改”） |
| 某工种“没有结果”的比例 | ≥ 20%，且至少 5 次派工 | 该工种的能力提示，或任务简报交给它的范围、轮次预算 |
| 某个建议版本的采用率 | < 15%，且至少展示 20 次 | 输入建议的系统提示 |
| goal 完成被拒的比例 | ≥ 30%，且至少 5 次完成声明 | goal 续推的话术，以及先定义验收标准 |
| 纠正率 | ≥ 25%，且至少 10 句后续输入 | 主 Agent；列出纠正集中的会话，供人工复核 |

### 失败链与危害度

按次数排行只说明“什么失败得多”，不说明“什么失败真的坏事”。报告的“失败链”一节回答后一个问题：
- **分段**：
  - **按轮次**：会话里的每一句后续输入都带 `prev_turn_id` 时（Runtime 下的新账本），一段就是一轮。这一轮的失败标记只取 `turn_id` 等于它的行；对它的反应，是 `prev_turn_id` 指向它的那句后续输入。排队、并行的轮次不会把别的轮次的失败算到自己头上。
  - **按时间**：进程内执行、旧账本（有后续输入没带 `prev_turn_id`）时，退回以每一句后续输入为界切段，口径与之前相同。
- **结局**：按下面的顺序判定，先命中的为准：
  1. 对这一段的反应之前的时间窗里有会话回退或中途喊停（不看间隔），或者这一轮本身被停下 → `bad`（`rewind_or_cancel`）；
  2. 反应是纠正（`correction`）或要求重做（`redo`）→ `bad`（`correction`）；
  3. 这一段主 Agent 的 `run_verified` 为 `failed` → `bad`（`verifier_failed`）；
  4. 反应是明确认可（`approval`），而且上一轮是 `completed` → `good`（`acceptance`）；
  5. 段内有目标通过了完成门禁（`goal_completed`）→ `good`（`goal_completed`）；
  6. 这一段主 Agent 的 `run_verified` 为 `passed` → `good`（`verifier_passed`）；
  7. 有后续输入但以上都没有 → `unknown`（`no_clear_signal`），包括普通追问和补充、只拒绝了审批（那是人的决定）、在“继续”之类的认可之前上一轮并没有完成；没有后续输入 → `open`（`no_reaction`）。

  词法反应只有在算得上是对这一段结果的反应时才算数：距上一轮结束超过 30 分钟、排在还没跑完的上一轮后面（`queued_behind`）、或者旧账本没有 `gap_ms`，都不算。第 3、5、6 条是运行时事实，用户不说话也成立，所以没有后续输入的段也可能有结局；Worker 的 `run_verified` 不代表这一轮，Worker 的验证失败另有 `worker:*:verifier_exhausted` 标记。

  只有 `good` 和 `bad` 算“有结局”，进入坏结局率和危害统计；`unknown` 单独计数，不当作成功。报表的 `sources` 给出每种证据各定了多少段，可以看出坏结局率有多少来自运行时事实、多少来自词法启发式。这些比率仍是关联指标，不能当作因果结论。注意这里的口径与上面的纠正率不同：纠正率仍把拒绝审批算作纠正，也不看间隔。
- **失败标记**：只用计数和标识：
  - `tool_failed:<工具>/<类别>`（人拒绝审批、hook 拦下不算）；
  - `incomplete:<stop_reason>`（主 Agent）；
  - `worker:<工种>:incomplete|timed_out|verifier_exhausted`；
  - `goal_completion_rejected`。
- **聚类**：段内失败按首次出现排序，工具失败附次数档，再接上结局，就是这一段的签名，例如 `tool_failed:edit_file/edit_text_not_found×4+ → incomplete:max_turns ⇒ bad`。报告列出最常见的 15 种签名。
- **危害度**：每种失败出现过、且有结局的段里，坏结局的比例，以及它是基线（所有有结局的段的坏结局率）的几倍。
- **危害候选**：某失败出现在至少 5 段里、坏结局率 ≥ 40%、且至少是基线的 2 倍，就进入改进候选：
  - 已有按次数产生的同一失败候选时，不另起一条，只在原候选上补 `evidence.harm = {bad_rate, lift}`；
  - 没有时新起一条 `harmful_failure:<标记>`；
  - 候选按危害（倍数 × 次数）排序，危害最大的排在最前。

`--candidates` 把候选另外写成 JSON：`{generated_at, window, candidates:[{target:{role, bundle, section}, signal, evidence:{count, rate, examples:[session_id…]}, suggestion}]}`。它是 `docs/PROMPT_RSI_DESIGN.md` §8.2 离线优化器的输入。
- 报告和候选文件都只有计数和 id，即使账本里存了正文，也一个字都不会带出来。
- 本命令不会改动任何提示词；提示词的晋升要走设计文档 §11 的门禁。

## 尚未覆盖

- `/local` 进程内轮次不经过 Runtime，没有 `user_followup`。
- 取消的来源（TUI Esc、Web、手机）：记录来源需要改协议，会让新客户端连不上旧 daemon，暂不做。
- 轮次结束后的 git 还原（`git restore` 了本轮改过的文件）。
