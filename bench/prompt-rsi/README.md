# 提示词对照评测归档

`scripts/prompt_rsi_eval.rb` 的产出，流程与门禁见 [操作手册](../../docs/PROMPT_RSI_OPERATIONS.md)。

| 路径 | 内容 |
|---|---|
| `reports/<日期>/<变体>-<模型>.json` | 结论、逐条门禁、各分组汇总、validation/regression 的逐题对照（holdout 只有汇总）、出处 |
| `reports/<日期>/<变体>-<模型>.md` | 同一份报告的人读版；`candidate_passes` 时附人工门清单 |
| `history.jsonl` | 每次评测一行：结论、变体、新旧版本号、模型、commit、没过的检查。只追加 |

报告只含计数、任务 id 与出处，不含模型正文，也不含变体正文（只记变体文件的 sha256）。
