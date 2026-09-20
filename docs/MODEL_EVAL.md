# 模型行为评测

> 决策记录：`docs/decisions/2026-09-20-experience-baseline-and-model-eval.md` 第 2 项。
> 目的：提示词、路由、上下文压缩的改动要拿数字说话，而不是肉眼看一轮。

## 第一步（已落地）：离线指标

`scripts/session_metrics.rb` 读本机会话记录，只输出计数，不输出任何正文。

```bash
ruby scripts/session_metrics.rb --limit 20 --json /tmp/metrics.json --markdown /tmp/metrics.md
```

| 指标 | 定义 | 为什么看它 |
|---|---|---|
| 人话率 `narration_ratio` | 有正文的 assistant 消息 / 全部 assistant 消息 | 思考型模型把话全写进 reasoning、正文留空，用户整轮看不到一句人话。rc6 之前线上一个会话是 18 / 397 |
| 静默工具轮 `silent_tool_turns` | 只带工具调用、正文为空的 assistant 消息数 | 人话率的另一面，直接对应「几十次工具调用一言不发」 |
| 思维链占比 `reasoning_ratio` | 带 `reasoning` 的 assistant 消息占比 | 判断模型是否在思考模式，解释人话率的成因 |
| 工具调用 / 失败 `tool_calls` / `tool_failures` | 工具调用总数与失败数（失败按结果开头启发式判定） | 失败率抬头往往是提示词或工具描述改坏了 |
| 调用次数 `turns` | assistant 消息数 | 同一任务变长说明模型在绕 |

用法约定：提示词或路由改动前跑一次留底（`--since` 卡改动日期），改动后用同类任务再跑一次，把两份 Markdown 贴进 PR。

## 第二步（待做）：固定任务集定期跑

- 任务集：20 个可自动验收的小任务（改一个函数、加一条测试、修一个 lint），放在 `bench/model-eval/`，每个任务带 verifier 命令。
- 执行：`willdeep run --json` 无头模式，对 DeepSeek V4、GLM、Claude 各跑一遍；会话落盘后用 `session_metrics.rb` 出指标，加上 verifier 通过率。
- 产出：JSON + Markdown 报告归档到 `bench/model-eval/reports/<date>/`，趋势图由报告聚合脚本生成。
- 门槛：人话率、verifier 通过率任一指标较前一周下降超过 10 个百分点即报警。

第二步依赖模型凭据，不进公共 CI；先做成本机定时任务（`scripts/launchd/`），稳定后再评估是否上专用 runner。
