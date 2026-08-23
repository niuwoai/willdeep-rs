# 实弹靶场成绩归档

小上下文工种在真实缺陷上的历次成绩。**这个目录是 git 跟踪的，而且必须是。**

在此之前，靶场的产出全部落在 `target/skill-worker-range/`，而 `target/` 是
`.gitignore` 的第一行。2026-08-16 那轮 12 样本的原始数据就是这么没的——只剩
`docs/SKILL_WORKERS.md` 里一张手抄的汇总表。一次快照能回答「小模型行不行」，
回答不了「我这次改动让它变好了还是变坏了」，而后者才是回归。

## 两个文件

| 路径 | 是什么 |
|---|---|
| `history.jsonl` | 每轮一行摘要，append-only。趋势曲线读的就是它 |
| `runs/<时间戳>-<模型>.json` | 那一轮的完整报告（含逐样本明细）＋同一份摘要 |

逐字记录（`traces/*.log`）**不进这里**：它按样本逐轮记录模型的完整往返，体量大，
且 `.gitignore` 里 `*.log` 已经拦掉。要看某轮为什么失败，去跑它的机器上翻
`target/skill-worker-range/traces/`。

## 摘要字段里最重要的那个

`commit`。没有它，一行成绩就是无主的：既不知道测的是哪版代码，也没法把两轮
之间的涨跌归因到任何改动上。跑靶场时如果工作区不干净，脚本会警告——那轮成绩
挂在一个没提交的状态上，回放不了。

## 口径

三条比率（`worker_verified_success` / `citation_accuracy` / `report_answer_rate`）
**分母为 0 时存 `null`，不存 `0`**。「什么都没验证」和「什么都没通过」是两件事，
一个分不清它们的指标比没有指标更糟。渲染时 `null` 在表里是 `-`，在曲线里是 `·`，
一路都不会被误读成谷底。

成功的定义是 **verifier 通过且测试块逐字未改**。退出码只知道绿了，不知道绿得
干不干净；把测试删掉也能变绿，而那是最省力的通关方式。作弊的样本进 `cheated`，
不进分子。

## 怎么跑

```bash
ruby scripts/skill_worker_range.rb          # 真花钱：每个样本都真调 Provider
ruby scripts/range_trend.rb --inject        # 把趋势写回 README 与 SKILL_WORKERS.md
```

定时跑法见 [`docs/SKILL_WORKERS.md`](../../docs/SKILL_WORKERS.md) 的「常态化」一节。
