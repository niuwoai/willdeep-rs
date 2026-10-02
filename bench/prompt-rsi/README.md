# 提示词对照评测归档

`scripts/prompt_rsi_eval.rb` 的产出，流程与门禁见 [操作手册](../../docs/PROMPT_RSI_OPERATIONS.md)。

| 路径 | 内容 |
|---|---|
| `reports/<日期>/<变体>-<模型>.json` | 结论、逐条门禁、各分组汇总、validation/regression 的逐题对照（holdout 只有汇总）、出处 |
| `reports/<日期>/<变体>-<模型>.md` | 同一份报告的人读版；`candidate_passes` 时附人工门清单，`needs_human_judging` 时列出待判样本 |
| `reports/<日期>/<变体>-<模型>-rescored-<时间>.{json,md}` | 人工判定后 `--rescore` 重算的结论，`rescored_from` 指回原报告 |
| `suggestion-runs/<时间>-<变体>-<模型>/{baseline,candidate}.json` | 输入建议套件双方的原始样本，人工判定填在 `judged` 字段 |
| `history.jsonl` | 每次评测（含复评）一行：结论、变体、新旧版本号、模型、commit、没过的检查。只追加 |

报告只含计数、任务 id 与出处，不含模型正文，也不含变体正文（只记变体文件的 sha256）。`suggestion-runs/` 是例外：样本是合成的，里面有模型给出的建议原文，与 `bench/input-suggestion/runs/` 同一口径。
