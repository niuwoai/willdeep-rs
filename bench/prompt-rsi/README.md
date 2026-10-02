# 提示词对照评测归档

`scripts/prompt_rsi_eval.rb` 的产出，流程与门禁见 [操作手册](../../docs/PROMPT_RSI_OPERATIONS.md)。

| 路径 | 内容 |
|---|---|
| `reports/<日期>/<run_id>.json` | 结论、逐条门禁、各分组汇总、validation/regression 的逐题对照（holdout 只有汇总）、出处 |
| `reports/<日期>/<run_id>.md` | 同一份报告的人读版；`candidate_passes` 时附人工门清单，`needs_human_judging` 时列出待判样本 |
| `suggestion-runs/<run_id>/{baseline,candidate}.json` | 输入建议套件双方的原始样本，人工判定填在 `judged` 字段 |
| `history.jsonl` | 每次评测（含复评）一行：run_id、结论、变体、新旧版本号、模型、commit、没过的检查、报告路径与报告 sha256。只追加 |

`run_id` 形如 `<UTC 时间>-<变体>-<模型>-<6 位随机十六进制>`，复评在随机后缀前多一段 `rescored`，并用 `rescored_from` 指回原报告。同一天、同一变体、同一模型重跑多少次，都各存一份，不会互相覆盖；归档文件已存在时脚本报错，不覆盖旧证据。

报告只含计数、任务 id 与出处，不含模型正文，也不含变体正文（只记变体文件的 sha256）。`suggestion-runs/` 是例外：样本是合成的，里面有模型给出的建议原文，与 `bench/input-suggestion/runs/` 同一口径。
