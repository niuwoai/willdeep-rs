# 线上派工指标

> 决策记录：`docs/decisions/2026-09-20-experience-baseline-and-model-eval.md` 第 6 项。
> 目的：「小模型路线」不靠形容词，靠线上真实派工的 Deep Share 与 Worker Verified Success 定期公开。

靶场（[`bench/skill-worker-range/`](../bench/skill-worker-range/)）是实验室：固定样本、固定缺陷，
回答「这次改动让小模型变好还是变坏」。这里是线上：Runtime 里真实跑过的子 Agent，回答
「派工路线离设计目标还有多远」。两边同一套口径，同一条「分母为 0 打 `-`」的纪律。

## 口径

| 指标 | 定义 | 目标 |
|---|---|---|
| **Deep Share** | `deep` 工种运行数 / 全部子 Agent 运行数 | ≤ 5% |
| Skill Coverage | 窄工种运行数 / 全部子 Agent 运行数 | ≥ 50% |
| **Worker Verified Success** | verifier 通过数 / 有 verifier 的运行数 | ≥ 85% |
| Escalation Rate | 有 verifier 但没通过（尝试打满、要更大模型）的运行占比 | ≤ 15% |
| 只读工种引用准确率 | 只读工种引用的位置真实存在的比例 | — |
| 未验证 | 没给 verifier 的运行数，独立于通过与失败的第三种答案 | — |

窄工种是 `scout` / `reader` / `log_inspector` / `git_detective` / `editor` / `test_fixer` / `build_fixer`；
`deep` 按设计跑父模型，不算派工；`implementer` 是标准档。目标值与 CLI 打印在每个比率旁边的是
同一组常量（`crates/willdeep-cli/src/agent_metrics.rs`），发布脚本报警也认它。

三条边界，与 [小上下文 Skill Worker](SKILL_WORKERS.md) 的「遥测与指标」同源：

- Skill Coverage 的分母是全部子 Agent 运行数。rs 侧没有「主模型内联轮次」的计数，这个口径比 Xedit 设计里的略宽。
- Escalation Rate 是「有 verifier 且没通过」的占比。rs 的升档是人工 `retry-agent --model`，没有自动升档记录可统计。
- **分母为 0 打 `-`，不打 0%**。「什么都没验证」和「什么都没通过」是两件事。

## 拿数

```bash
willdeep daemon agent-metrics                  # 人看的 TSV，每个比率旁边带分母和目标
willdeep daemon agent-metrics --json           # 同一份数的 JSON，分母为 0 的比率是 null
willdeep daemon agent-metrics --since 7d       # 只算窗口内新建的子 Agent
```

`--since` 接受 `24h` / `7d` / `2w`、UTC 日期 `2026-09-14`、UTC 时间 `2026-09-14T08:00:00Z`；
光秃秃的数字拒收——`--since 7` 是七个什么没人知道，猜错一次周报就悄悄变成了总账。
三种输出都只有计数和比率：**没有 prompt、路径、命令、agent id**。Runtime 没起时会顺手拉起来。

## 定期发布

```bash
ruby scripts/agent_metrics_publish.rb          # 拍一张：近 7 天 + 累计，追加进 bench/agent-metrics/history.jsonl
ruby scripts/agent_metrics_trend.rb --inject   # 渲染趋势，写回 README.md 与本页的 marker 区块
ruby scripts/agent_metrics_trend.rb --alarm    # 最新快照的窗口指标有没达标的就退出 1
./scripts/agent_metrics_weekly.sh              # 上面三步串起来；launchd / cron 模板见 scripts/launchd/README.md
```

每周一拍一张（`WILLDEEP_METRICS_WINDOW` 改窗口，跟拍照周期对齐）。它不花钱、不联网。
跑完之后工作区里多出这些**未提交**的变更：

| 路径 | 变化 |
|---|---|
| `bench/agent-metrics/history.jsonl` | 多一行 |
| `README.md`、`docs/AGENT_METRICS.md` | 趋势区块被重写 |

**不自动提交，不自动 `git pull`**：一张快照进不进公开历史得有人看一眼，尤其是当它不好看的时候。
退出码 0 正常；1 快照没拍成（趋势不更新）；2 拍成了但有指标没达到目标。

报警对的是**绝对目标**而不是上一次：靶场问「变好还是变坏」，这里问「离目标多远」。
只有一次快照也报；分母为 0 的比率不报——它是「没得比」，不是没达标。

## 趋势

下面这段由 `ruby scripts/agent_metrics_trend.rb --inject` 生成，别手改。

<!-- agent-metrics:begin -->
还没有拍过快照（`bench/agent-metrics/history.jsonl` 为空）。

```bash
ruby scripts/agent_metrics_publish.rb
```

它只读本机 Runtime 的 agent 记录，不花钱，拍完自动归档并在这里长出趋势。
<!-- agent-metrics:end -->

## 局限

- 只统计**这台机器**的 Runtime 还留着的 agent 记录：换机器、清 `$WILLDEEP_HOME` 都会让累计数归零，窗口数不受影响。
- 快照来自维护者自己的日常使用，不是受控实验；样本小的时候波动大，看趋势不看单点。要可复现的对照，去跑靶场。
- 指标只知道 verifier 绿没绿，不知道绿得干不干净；「改测试蒙混过关」的检查在靶场里（`cheated`），线上没有对照的原始测试块可比。
