# 线上派工指标快照归档

`willdeep daemon agent-metrics --json` 的历次快照。**这个目录是 git 跟踪的，而且必须是**：
一次 `agent-metrics` 回答的是「现在派工得怎么样」，只有归档下来才能回答
「派工提示词和路由改了之后，Deep Share 和 Worker Verified Success 往哪边走了」。

## 一个文件

| 路径 | 是什么 |
|---|---|
| `history.jsonl` | 每次快照一行，append-only。趋势曲线读的就是它 |

每一行只有计数和比率：`recent`（窗口内新建的子 Agent，默认近 7 天）与 `total`（Runtime 还留着的全部记录），
外加 `ran_at` / `commit` / `dirty` / `version` / `window`。**没有 prompt、路径、命令、agent id**——
它是从公开 API 的 agent 列表算出来的，CLI 本来就不出这些。

## 口径

分母为 0 时存 `null`，不存 `0`：「什么都没验证」和「什么都没通过」是两件事。
渲染时 `null` 在表里是 `-`，在曲线里是 `·`。指标定义、目标值与发布流程见
[`docs/AGENT_METRICS.md`](../../docs/AGENT_METRICS.md)。

## 怎么拍

```bash
ruby scripts/agent_metrics_publish.rb          # 不花钱：只读本机 Runtime
ruby scripts/agent_metrics_trend.rb --inject   # 把趋势写回 README 与 docs/AGENT_METRICS.md
```

定时拍法见 [`scripts/launchd/README.md`](../../scripts/launchd/README.md)。
