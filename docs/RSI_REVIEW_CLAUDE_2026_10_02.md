# RSI 审查独立复核（Claude Code）

复核日期：2026 年 10 月 2 日。复核对象：[RSI_REVIEW_2026_10_02.md](RSI_REVIEW_2026_10_02.md) 的 R1–R8。
复核者：Claude Code，模型 `claude-opus-5-5`。基线：`develop` @ `4bd4c5a`，仓库版本 `0.88.0-rc1`。

本次只做审查：没有修改产品代码，没有运行付费模型评测，没有提交 Git。

## 结论摘要

R1–R8 八项**全部确认**，没有需要反驳的。其中 R2 比首轮描述的更严重：token 缺失这条路径在正常驱动下就会发生，不需要构造异常报告。R4 则相反，在当前默认流程里还触发不了，因此建议从 P1 降为 P2，但必须和 R6 一起修。另外新增 5 项发现（N1–N5），都不高于 P2。

| 编号 | 首轮 | 复核 | 结论 | 主要依据 |
| --- | --- | --- | --- | --- |
| R1 | P1 | **P1** | 确认，并补充两种反例 | 源码 + 纯函数复现 |
| R2 | P1 | **P1**（加重） | 确认；部分缺失 token 按 0 计入，正常驱动下超时就会触发 | 源码 + 纯函数复现 |
| R3 | P1 | **P1** | 确认 | 源码 + 真实 `AgentEvalProcess.run` 复现 |
| R4 | P1 | **P2**（降级） | 确认，但当前流程触发不了；修 R6 时必须一起修 | 源码 + 纯函数复现 |
| R5 | P2 | **P2** | 确认，补充幸存者偏差 | 源码 |
| R6 | P2 | **P2** | 确认；代码注释指向不存在的 `--rescore` | 源码 |
| R7 | P2 | **P2** | 确认，又找到 3 种绕过写法 | 只读 `willdeep prompt check` 复现 |
| R8 | P2 | **P2** | 确认 | 归档函数复现 |
| N1 | — | P3 | holdout 没跑成被记为 `overfit` | 纯函数复现 |
| N2 | — | P2 | 未判定的 baseline 指标从 null 变成 0，三项输入建议检查都受影响 | 源码 |
| N3 | — | P2 | 出处没有把二进制和 commit 绑定；输入建议套件根本不用 `--binary` | 源码 |
| N4 | — | P2 | 数据集哈希只在开跑前算一次，没有逐行绑定 | 源码 |
| N5 | — | P3 | 输入建议的不变片段只有 `NONE` | 源码 |

## 实际执行的检查

- 通读源码：`scripts/lib/prompt_rsi/gate.rb`、`report.rb`、`scripts/prompt_rsi_eval.rb`、`scripts/model_eval.rb`、`scripts/lib/agent_eval_process.rb`、`scripts/lib/agent_eval_observation.rb`、`scripts/lib/model_eval/verifier.rb`、`scripts/lib/model_eval/report.rb`、`scripts/input_suggestion_eval.rb`、`scripts/lib/suggestion_report.rb`、`crates/willdeep-core/src/prompt_sections.rs`（变体加载与安全门）、`crates/willdeep-cli/src/main.rs::enforce_prompt_variant_scope`、`crates/willdeep-cli/src/feedback_cmd.rs::episodes_of`、`crates/willdeep-cli/src/audit_cmd.rs::classify_followups`，以及 `docs/PROMPT_RSI_OPERATIONS.md`。
- 现有测试：`prompt_rsi_test.rb` 8 runs / 52 assertions、`model_eval_test.rb` 25 runs / 300 assertions，全部通过，与首轮一致。
- 离线复现脚本（放在会话暂存目录，不入库）：
  - `rsi_repro.rb`：直接调用 `PromptRsi::Gate`、`PromptRsi::Report.archive` 和 `AgentEvalProcess.run`，全部使用合成输入。
  - `r7_check.rb`：生成 5 份 `main/tool_rules` 变体，只运行 `willdeep prompt check`。用的是本机 `~/.cargo/bin/willdeep`，版本 `0.88.0-rc1`，`main@067f32af51e4`。没有核实这个二进制是否正好从 `4bd4c5a` 构建。
- 任务集分组计数：train 4、validation 8、regression 4、holdout 4，与首轮描述一致。
- 没做的：没有运行 Rust 测试，没有读取本机反馈账本（因此没有复核「近期数据快照」一节的数字），没有发起任何 Provider 请求。

## 逐项复核

### R1 任务集合完整性：确认，P1

`reproducibility` 只检查出处字段、`dirty`、空报告，以及 `error/skipped` 的数量；`stats` 用 `EXECUTED` 白名单算分母。复现结果都是 `candidate_passes`，`problems` 为空：

- 候选只保留 4 道通过的 validation 和 1 道 regression（首轮已复现）。
- 候选 task ID 全部换掉（首轮已复现）。
- **补充：未知状态。** 把 4 道失败的 validation 改成 `interrupted`，这些行不算 executed，也不算 error/skipped，于是直接从分母里消失。validation 通过率从 50% 变成 100%。
- **补充：重复 ID。** 8 行 validation 全部用同一个 `v1`，照样通过。

真实发生的可能性：`model_eval.rb` 自己只会产出已知状态。提前中止（`abort_early?`）留下的是 error 行，会被拦住。所以在正常驱动下，R1 需要报告被改过或拼错过才会触发。但门禁是晋升前最后一道自动检查，修起来也便宜，P1 合理。另见 N4：数据集在两轮之间如果变了，也会表现成集合不一致。

### R2 全阶段底线与缺失测量：确认，P1，比首轮更重

- holdout 阶段只有 `holdout_not_worse` 一项检查。复现：holdout 里一道题设为 `cheated`、`false_completion=true`、10 万 token、1 万秒，结论仍是 `candidate_passes`。
- 候选 token 全为 null 时，`stats` 返回 `tokens=nil`，`growth` 返回 nil，`token_growth` 记为通过（首轮已复现）。
- **补充：部分缺失，而且正常驱动下就会发生。** `stats` 只要有一行带 token，就用 `to_i` 把缺失行当 0 相加。`AgentEvalObservation.checkpoint` 读不到会话或检查点时返回 `{}`；超时任务被强杀后 stdout 里没有最终 JSON，拿不到 `session_id`，token 就是 nil。复现：候选一半行缺 token、另一半 token 涨 30%，门禁显示「token -36%」，结论是通过。也就是说，**候选越容易超时，看起来越省 token**。这条路径不需要任何构造输入。
- 耗时用 `to_f` 求和，同样有 nil 变 0 的问题。但超时行有 `elapsed_seconds`，影响比 token 小。

建议与首轮一致，另加一条：token 覆盖率（有测量的行数 / 执行行数）两边必须相等，否则记为「测量不完整」，不能算通过。

### R3 baseline 环境隔离：确认，P1

`model_eval.rb:175` 构造的 env 只有 `WILLDEEP_HOME`。`AgentEvalProcess.run` 用 `Process.spawn(env, …)`，没有 `unsetenv_others`，所以子进程会继承父进程的环境。`main.rs::enforce_prompt_variant_scope` 对 `run --local` 会读取这个变量并套用变体。复现：父进程设好变量后，子进程读到了 `/tmp/candidate.json`；在 env 里显式写 `'WILLDEEP_PROMPT_VARIANT' => nil`，子进程读到的就是 nil，这说明首轮的修法有效。`input_suggestion_eval.rb:102` 也是只在有变体时才设置，存在同样的问题。

触发条件是使用者在 shell 里 export 过这个变量。`PROMPT_RSI_OPERATIONS.md:91` 正是这样教人手动加载变体的，所以这个场景是现实的。另外，报告每一行都没有记录实际生效的 bundle，出了问题事后也查不出来。

### R4 人工判定完整性：确认，降为 P2

门禁逻辑和首轮描述一致，复现结果为 `candidate_passes`。降级的理由是：在当前默认流程里，这个误判走不到。

- `input_suggestion_eval.rb:112` 每跑一轮都会把所有 suggest 样本的 `judged` 置为 nil，所以候选的 `judged` 永远是 0；
- RSI 驱动没有任何能填入人工判定、再重新算门禁的入口（见 R6）。

所以输入建议套件目前最多只能得出 `needs_human_judging`，误判方向是「卡住」，不是「放行」。但 R6 一旦接通，R4 马上就会生效。**修 R6 时必须同时修 R4**，不能分开发。另外还有一处同类问题：见 N2。

### R5 失败链结局口径：确认，P2

`episodes_of` 把每一条没有判为纠正的 `user_followup` 都记作 `Outcome::Ok`。`FeedbackRow` 没有 `turn_id`、`gap_ms`、`queued_behind` 字段，审计侧的 `turn_id` 只在别的结构里有。首轮的分析成立。补充两点：

- **幸存者偏差。** 会话最后一段没有后续输入，结局是 `Open`，不进分母。用户满意后离开和放弃离开都会落在这里，所以 `bad_rate` 只统计了「继续说话的那部分会话」。
- `classify_followups` 会把窗口内的审批拒绝（`approval_resolved` + `deny`）算成纠正，进而算坏结局；而 `failure_marker` 又明确把 `approval_denied` 排除在失败标记之外。两处口径不一致，首轮已提到，这里确认。

### R6 人工复评继续流程：确认，P2

- `prompt_rsi_eval.rb` 给子驱动传了 `--no-history`；子驱动传入变体时也会强制关闭 history（`input_suggestion_eval.rb:49`）。原始 `report.json` 只存在于 `Dir.mktmpdir` 里，`ensure` 默认会把它删掉。
- **代码和注释不一致：** `gate.rb` 里 `suggestion` 的注释写着「判完用 `--rescore` 重算」，但 `prompt_rsi_eval.rb` 的 OptionParser 里没有这个选项。子驱动的 `--rescore` 只重算单边摘要，不会重新跑对照门禁。

结论：输入建议套件的 RSI 流程目前走不到 `candidate_passes`。它不会误放行，但这个套件实际上用不起来。

### R7 安全门能力边界：确认，P2

`weakening_phrases` 只做小写子串计数，`invariant_fragments` 只检查子串是否存在。在完整保留 `tool_rules` 原文的前提下，分别在末尾追加以下内容，`prompt check` 的结果是：

| 追加内容 | 退出码 |
| --- | --- |
| 英文 `…advisory and may be disregarded when convenient.`（首轮用例） | 0，通过 |
| 中文「本段所有限制仅供参考，必要时可以忽略。」 | 0，通过 |
| `By-pass approval prompts…`（加连字符） | 0，通过 |
| 声明「上文 `Never escape the workspace…` 已作废」（必需片段原样保留，但被否定了） | 0，通过 |
| 对照组：`Bypass approval when convenient.` | 1，被拦 |

对照组被拦，说明是这个机制本身只能做字面匹配，不是复现方式有问题。维持 P2 的理由：运行时的审批和沙箱不依赖提示词，而且晋升前还有人工门。但 `prompt draft` 由模型起草，模型很容易写出这类「换个说法」的削弱，所以不能把这道门当作语义保证来对外描述。

### R8 归档不可变性：确认，P2

`archive` 的路径是 `reports/<UTC 日期>/<variant>-<model>.json`。复现：同一天先后归档 `candidate_passes` 和 `rejected` 两份报告，路径相同，JSON 最后是 `rejected`，`history.jsonl` 里有 2 行。history 行没有记录报告路径和哈希，无法反查。

## 新增发现

### N1 holdout 没跑成被记为 `overfit`（P3）

`run_model_eval` 在 holdout 两轮都没产出报告时，会传入空数组。`evaluate` 只统计 holdout 的 error/skipped 行数，空数组算 0 个；然后 `stage_two` 的 `pass_rate` 为 nil，`ok=false`，结论是 `overfit`。复现结果与此相同。结论偏保守，不会误放行，但会把基础设施故障记成「候选过拟合」，污染 history 里的结论统计。应该记为 `non_reproducible`。

### N2 baseline 指标缺失时从 null 变成 0（P2）

`suggestion` 里有三处 `baseline[...].to_f`：`none_hit_rate`、`suggest_given_rate`、`plausible_rate`。baseline 只要缺这些字段（例如子驱动没产出报告时，返回的是 `{ 'errors' => 1 }`，只有这一个键），比较就会变成「候选 ≥ 0」。目前 `errors` 会先把结论拦成 `non_reproducible`，所以还没有被利用的路径。但这和 R2 的 token 问题、R4 的 plausible 问题是同一种写法，建议一起修：**缺失就是缺失，不参与比较，结论也不能是通过。**首轮把它归在 R4 里，只提到了 plausible；这里扩展到全部三项，并单列出来，方便验收。

### N3 出处没有把二进制和 commit 绑定（P2）

- `commit` 和 `dirty` 来自仓库工作区，`binary_version` 只是 `--version` 输出的版本串。PATH 里的 `willdeep`（cargo 或 brew 安装的）可能是从别的 commit 构建的，版本号却相同，出处检查照样通过。
- 输入建议套件用 `cargo test` 从当前工作区编译后执行，**根本不使用 `--binary`**；但 RSI 驱动仍然用 `--binary` 去跑 `prompt check`、取 `binary_version`。所以报告里的二进制出处和实际执行的代码可能不是同一份。
- `dirty` 用的是 `git diff --quiet HEAD`，未跟踪的文件不算。

建议：二进制暴露构建时的 commit（或者评测前从当前 commit 构建），出处校验要求它等于 `commit`；输入建议套件把 `cargo test` 实际编译的 commit 写进摘要。

### N4 数据集哈希只在开跑前算一次（P2）

`run_model_eval` 在 baseline 开跑前算一次 `dataset_sha256`，之后 4 轮 model-eval 各自重新 `Task.load_all`。整个对照可能要跑数小时，期间如果有人改了任务（切分支、编辑任务文件），两边跑的就不是同一个任务集，报告却只有一个哈希。这和 R1 是同一个根因，修 R1 时一起处理：每轮报告记录自己用的任务清单和哈希，门禁校验两边一致。

### N5 输入建议的不变片段只有 `NONE`（P3）

`invariants` 对 `(InputSuggestion, "system_prompt")` 只要求保留 `NONE`。这一段的拒答规则（reject 样本）完全依赖实弹评测的 `reject_all` 检查来兜底，结构门几乎不起作用。这是已知的设计取舍，但安全门文档应写明这一点。

## 对首轮其他内容的意见

- 「改进顺序与验收要求」认同。建议在验收清单里补充：未知状态不能晋升；token 部分缺失要得出明确的检查结论；holdout 空报告记为 `non_reproducible`；baseline 摘要缺字段不能通过；报告每行记录实际生效的 bundle。
- 「已排除的误报」：`section_for` 的映射这次没有复查，沿用首轮结论。
- 「近期数据快照」：本次没有读取本机账本，不确认也不否认里面的数字。
- 首轮说 8 道 validation 的分数步长是 12.5pp，那么 3pp 的阈值实际上等于「至少多过 1 道」。这一点认同；holdout 4 道题时，「不下降」也只能分辨 25pp 的差距。

## 建议的修复分组

1. **门禁输入（P1）**：R1 + N4（任务清单与哈希）、R2 + N2（全阶段底线，缺失不能当通过）、N1（空 holdout 的结论）。都在 `gate.rb` 和 `prompt_rsi_eval.rb` 里，可以作为一个 PR。
2. **环境与出处（P1/P2）**：R3（显式写 nil，报告记录生效的 bundle）+ N3（二进制与 commit 绑定）。
3. **输入建议人工门（P2）**：R6 + R4，必须一起改。
4. **档案（P2）**：R8。
5. **指标口径与文档（P2/P3）**：R5、R7、N5。主要是改名、补充文档说明，以及把不可变的安全段和可优化的段分开装配。

## 修复进度

2026-10-02，第一组的 R1、R2、N1、N2 已修好，见 [niuwoai/willdeep-rs#41](https://github.com/niuwoai/willdeep-rs/pull/41)（0.88.0-rc2）：

- `gate.rb`：新增 `integrity`，按任务清单逐题核对；新增 `floors` 和 `resource_check`，两个阶段共用，token 和耗时只比两边都测到的题；新增 `holdout_problems`；输入建议的指标缺失记为问题，不再当 0 比较，plausible 要两边都判过才比。`evaluate` 和 `reproducibility` 现在必须传入 `tasks:`。
- `prompt_rsi_eval.rb`：从 `bench/model-eval/tasks` 读出任务清单，传给门禁。
- `prompt_rsi_test.rb`：新增 5 个测试，对应上文的反例。这 5 个测试在旧门禁上全部失败，在新门禁上全部通过。

第二组的 R3、N3 随后修好（0.88.0-rc3）：

- **R3**：两个驱动给子进程的环境总是显式写出变体变量，没有变体时传 nil 把它清掉。`run --output json` 和输入建议实弹报告新增 `prompt_variant`（实际生效的变体，没有则为 null），评测逐行记录；门禁核对 baseline 没套变体、候选套上了预期的 bundle，执行完却没报告的任务也拦下（超时的除外）。另用假二进制做了端到端检查：父进程 export 了候选时，baseline 子进程收到的仍是 nil。
- **N3**：`build.rs` 把构建 commit 编进二进制，`prompt check` 打印 `build <commit>[-dirty]`；门禁要求它以仓库 commit 开头且不带 `-dirty`，两个套件都要求。`git_context` 把源码相关路径下的未跟踪文件也算作改动。
- **已知局限**：`-dirty` 只看 `crates`、`web/src`、`Cargo.toml`、`Cargo.lock`。这些路径之外、同样会影响二进制的改动（`.cargo/config.toml`、工具链文件、前端构建配置等）不会被标出。反过来的情况偏保守：带着改动构建后又撤销改动、但没有重新构建时，二进制仍标着 `-dirty`，会被拦下。

第三组的 R4、R6 随后修好（0.88.0-rc4）：

- **R6**：输入建议套件跑完后，双方原始样本存进 `bench/prompt-rsi/suggestion-runs/<时间>-<变体>-<模型>/`，每条 suggest 样本带 `judged: null`；报告记下 `evidence` 路径和「除 judged 以外内容」的指纹。新增 `prompt_rsi_eval.rb --rescore <报告>`：只读归档重算门禁，不请求模型；样本除 judged 以外被改过就拒绝；结论另存为 `-rescored-<时间>` 报告并带 `rescored_from`，原报告不动。`needs_human_judging` 的报告会列出每一侧还差哪些样本，以及怎么重算。
- **R4**：摘要新增 `judgeable`（给出了建议的 suggest 样本数）和 `unjudged_ids`。只有两边都一条不落地判完，才比 plausible；只判一部分、baseline 没判、旧摘要没有待判清单，结论都是 `needs_human_judging`。两边样本数不一致记为 `non_reproducible`。
- **口径说明**：两边各判自己给出的建议，所以判定的样本集合可能不同（一边给了、另一边没给）。这部分差异由 `suggest_given` 检查管，没有强行要求同一批样本配对。
- **已知局限**：不做盲评，判的人能从文件名看出哪边是候选。同一秒内对同一份报告复评两次，后一份会覆盖前一份；这一点已随第四组解决。

第四组的 R8 随后修好（0.88.0-rc5）：

- 每份报告在生成时拿到唯一 `run_id`，格式为 `<UTC 时间>-<变体>-<模型>[-rescored]-<6 位随机十六进制>`。报告文件名、输入建议样本目录都用它，同一天同一变体同一模型重跑也不会撞名。
- 归档改为独占创建：目标文件或样本目录已存在就抛 `ArchiveExists`，不覆盖旧证据。
- history 每行新增 `run_id`、`report`（报告相对路径）和 `report_sha256`，能从历史摘要找回并核对原件。
- 复评报告的 `run_id` 带 `rescored` 标记；同一秒内复评两次各存一份。
- **未处理**：旧版本按 `<变体>-<模型>.json` 命名的报告不迁移。目前 `bench/prompt-rsi/` 里只有 README，没有旧报告需要迁移。

还没做的：
- **N4**：两轮之间的数据集哈希仍然只算一次。现在能拦住「任务 id 或分组变了」，拦不住「同一 id 的内容变了」。
- 第 5 组。
