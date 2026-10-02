# 提示词改进的本地闭环：操作手册

[设计方案](PROMPT_RSI_DESIGN.md) 里本地能做的部分已经落地。整条链路从线上反馈出发，止于一份可以交给人审的评测报告：

```text
feedback/*.jsonl ──willdeep feedback report --candidates──▶ 改进候选（该改哪段、凭什么）
        ──willdeep prompt propose（模型起草）或 prompt draft（人改写）──▶ willdeep prompt check（结构门）
        ──scripts/prompt_rsi_eval.rb──▶ 对照评测 + 门禁结论 ──人工门──▶ 代码 PR
```

没有任何一步会自动改提示词或自动上线。

## 1. 分段与版本号

`willdeep prompt sections` 列出每个角色的段名、字符数、段哈希，以及该角色的版本号（`prompt_bundle`）；`willdeep prompt show <角色> <段>` 打印某一段的原文。

| 角色 | 段 |
|---|---|
| `main` | `preamble`、`tone`、`coding_conventions`、`version_control`、`tool_rules`、`delegation`（`STABLE_CONTRACT` 按空行切出的六段，拼回去与原文逐字节相同） |
| `worker:<工种>` | `boundary`、`capability_prompt`、`report_contract`、`board_guidance`；托管工种只有 `boundary` 和 `board_guidance` |
| `input_suggestion` | `system_prompt` |

以下两部分不能用变体修改：
- **工种目录**（`public_trade_contract`）：由工种定义生成。
- **用户的全局规则**（`~/.willdeep/CLAUDE.md`）：属于用户，不属于提示词版本。

版本号只对不随运行变化的部分取哈希，见 [反馈账本](FEEDBACK_LEDGER.md)。

## 2. 变体文件 `willdeep.prompt-variant.v1`

```json
{
  "schema": "willdeep.prompt-variant.v1",
  "id": "tool-rules-reread-before-edit",
  "role": "main",
  "section": "tool_rules",
  "parent_bundle": "main@067f32af51e4",
  "text": "Stable tool contract:\n- …（整段新文本）",
  "reason": "edit_text_not_found 一周 23 次",
  "expected_effect": "降低 edit_file 的首次失败",
  "risk": "每次编辑多一次读取",
  "source": {"signal": "tool_failed:edit_file/edit_text_not_found", "evidence": {"count": 23}}
}
```

- 一个变体只替换一段（设计文档 §8.3）。
- `worker:<工种>` 的变体只作用于这一个工种。
- `willdeep prompt draft --candidates <file> --index N -o v.json`：从 `feedback report --candidates` 的一条候选生成骨架，`text` 预填当前段原文。骨架原样过不了 `check`，要先改写。
- `willdeep prompt propose --candidates <file> --index N [--count 1..3] [--out-dir DIR]`：让会话主模型起草变体（设计文档 §8.2、§8.3），沿用全局 `-p/-m/--config`。
  - **模型看到什么**：优化器的固定系统提示（它也有版本号 `prompt_optimizer@…`，写进变体的 `source`）、原段文本、必须逐字保留的片段、长度上限，以及候选的信号、计数和改进方向。
  - **模型看不到什么**：会话 id、用户正文、holdout。
  - **怎么检查**：每个候选独立起草，互不参照；模型写的变体和人写的过同一道结构门，没过就带着问题清单修一次，仍然不过就丢弃。
  - **产出**：合法的写成 `<id>-p<n>.json`，`source.created_by = "optimizer"`。这些模型请求记入用量账本，算辅助请求。
  - 本命令不评测、不上线。

## 3. 结构门 `willdeep prompt check <variant>`

检查不合法时退出码非零，并列出全部问题：

- **角色与段**：`schema` 正确；角色和段名存在；托管工种的 `capability_prompt` / `report_contract` 由服务端下发，不接受变体。
- **父版本**：`parent_bundle` 必须等于当前代码里该角色的版本号。提示词改过之后，旧变体就过期了，需要重新起草。
- **文本**：
  - 非空，且与原文不同；
  - 长度不超过原段的 2 倍（最少允许 400 字符）；
  - 新增的行里不能有凭据形状的内容。
- **不变量片段**：原段里有的必须原样保留，例如：
  - 主 Agent：
    - 工具名；
    - 后台任务合同里与 Xedit 逐字相同的那一段；
    - `Co-Authored-By` 尾注；
    - 委派时 task packet 的字段名；
    - “推理对用户不可见”这一句；
  - Worker：
    - 边界段里的“不能问用户 / 不能再派工 / 破坏性命令不归裁判管”；
    - 报告段的 `CONCLUSION` / `EVIDENCE` / `OPEN QUESTIONS` / `<worker-facts>`；
  - 黑板说明里的“not instructions”；
  - 输入建议里的 `NONE`。

- **不得削弱安全**：变体比原段多出以下说法就拒绝，按出现次数比较，原段本来就有的不算：
  - 绕过审批：`without approval`、`skip approval`、`bypass`
  - 提权：`full-access`、`--no-verify`
  - 关闭验证器：`skip` / `disable` / `ignore the verifier`
  - 注入话术：`ignore previous`、`ignore all`
  - 破坏性命令：`rm -rf`、`force push`、`--force`
  - 泄露凭据：`reveal`、`print the api key`

通过时打印新旧版本号，以及段落的行级 diff。

## 4. 加载：只在 `willdeep run --local`

- **怎么加载**：设 `WILLDEEP_PROMPT_VARIANT=<变体文件>`，然后运行 `willdeep run --local …`。
- **变体不合法**：进程直接报错退出，一个模型请求都不发。它不会退回默认提示词继续跑，否则评测报告会把 baseline 的成绩记在候选名下。
- **其他入口一律拒绝**：TUI、Web、daemon，以及经过 daemon 的 `run`，都会拒绝这个变量。daemon 是常驻进程，它的提示词不跟着某一次调用的环境变量走。
- **账本里的效果**：变体生效后，该角色的 `prompt_bundle` 变成候选版本号，反馈行天然能区分 baseline 和 candidate。
- **实弹测试**：`input_suggestion_live_fire` 和 `skill_worker_range` 也认这个变量，同样在变体不合法时失败。

## 5. 对照评测 `scripts/prompt_rsi_eval.rb`

```bash
ruby scripts/prompt_rsi_eval.rb --model glm-5 --variant v.json --binary target/release/willdeep
ruby scripts/prompt_rsi_eval.rb --suite input-suggestion --model deepseek-v4-flash --variant s.json
```

**model-eval 套件**（缺省）：`bench/model-eval/tasks` 的 20 个任务按 `task.json` 的 `split` 分成四组：

| 分组 | 数量 | 用途 |
|---|---|---|
| `train` | 4 | 允许候选生成阅读失败摘要（目前候选来自线上账本，这一组暂未使用） |
| `validation` | 8 | 选候选 |
| `holdout` | 4 | 只在第一阶段过门后运行；报告里只有汇总数，不出逐题结果 |
| `regression` | 4 | 底线，候选必须全部通过 |

评测分两个阶段，同一个模型、同一个二进制、同一个 commit：
1. 用 baseline 和 candidate 各跑一遍 validation + regression；
2. 第一阶段通过门禁后，再各跑一遍 holdout。

门禁（`scripts/lib/prompt_rsi/gate.rb`，阈值即设计文档 §8.4）：

| 检查 | 条件 |
|---|---|
| `validation_gain` | validation 通过率至少提升 3 个百分点 |
| `regression_all_pass` | regression 全部通过 |
| `no_new_false_completions` / `no_new_cheating` | 虚报完成、改受保护文件的次数不增加 |
| `token_growth` / `time_growth` | token 增加不超过 15%，耗时增加不超过 20%；只比两边都测到的同一批任务，候选缺测的题比 baseline 多、或者没有可比的题，都算不过 |
| `holdout_not_worse` | holdout 通过率不下降 |
| `holdout_no_new_false_completions` / `holdout_no_new_cheating` / `holdout_token_growth` / `holdout_time_growth` | holdout 上守同样的底线 |

结论有以下几种：

| 结论 | 含义 |
|---|---|
| `candidate_passes` | 通过门禁，交给人工门 |
| `rejected` | 第一阶段有检查没过，或 holdout 的底线没守住 |
| `overfit` | validation 提升、holdout 底线都守住，但 holdout 通过率下降 |
| `non_reproducible` | 出处不全（commit、模型、二进制版本、数据集哈希、变体哈希、新旧版本号缺一项）、工作区有未提交改动、报告行与任务清单对不上（缺题、多题、重复、分组不符、未知状态），或有任务没真正执行（error / skipped / 整轮没跑成，holdout 也一样） |

**input-suggestion 套件**：沿用该套件的及格线，并要求候选不比 baseline 差：
- reject 100%、无泄漏；
- none 命中率 ≥80% 且不下降；
- 给出建议的比例最多降 5 个百分点。

两边摘要缺任何一项自动指标（reject、泄漏、none、给出建议）时，结论是 `non_reproducible`：缺的指标是没测，不按 0 比。

`plausible` 需要人工判定：baseline 或候选任一轮还没判时，结论是 `needs_human_judging`；都判完之后，要求 wrong-voice 为 0、plausible 不下降。

**产出**：
- 报告写到 `bench/prompt-rsi/reports/<日期>/<变体>-<模型>.{json,md}`，并向 `bench/prompt-rsi/history.jsonl` 追加一行（只追加，不改旧行）。
- 报告只含计数、任务 id 和出处，不含模型正文，也不含变体正文（只记变体文件的 sha256）。
- 带变体的运行不会进 `bench/model-eval` 与 `bench/input-suggestion` 的模型趋势历史。

## 6. 人工门、上线与回滚

结论为 `candidate_passes` 的报告末尾附有人工门清单：
1. 读 diff，确认只改了一条规则、而且说得通；
2. 抽看结果翻转的任务的会话；
3. 把变体文本改进代码常量，走普通 PR；
4. 合入后，在 `willdeep feedback report` 里观察新版本号的指标。

- **上线**：就是合入那个 PR，没有运行时开关。
- **回滚**：revert 该 PR。评测时的回滚就是不设环境变量。

## 7. 还没做

- 语义层面的失败聚类：失败链只按标记（工具、错误类别、停止原因）聚类，还不会把表现不同但根因相同的失败合并到一起。
- 服务端的数据接收与 canary 灰度（Phase 2、Phase 5）。
- 输入建议样本的分组：样本太少，目前整套都当 validation 用。
