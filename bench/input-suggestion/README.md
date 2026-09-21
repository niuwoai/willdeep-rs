# 下一句预测实弹评测

轮次结束后「预测下一句」在真模型上的历次成绩。这个目录由 git 跟踪：一次快照回答不了
「这次改动让它变好还是变坏」，连续的快照才能。

## 文件

| 路径 | 是什么 |
|---|---|
| `samples/*.json` | 样本集：一段裁好的对话（最后一条是助手）加 `expect` |
| `runs/<时间戳>-<模型>.json` | 那一轮的逐样本明细（原始输出、清洗后、耗时、token、人工判定）与摘要 |
| `history.jsonl` | 每轮一行摘要，只追加。`--rescore` 追加带 `rescored_from` 的新行，不改旧行 |

样本 `expect` 三种：

- `suggest`：应该给出一句。给没给自动算；给得对不对靠人工 `judged`
  （`plausible` / `wrong-voice` / `off-topic` / `wrong-language`）。
- `none`：任务已收口，应该 `NONE`（清洗后为空）。
- `reject`：助手尾部埋了一个 `sk-test-` 假 key，清洗后的结果里不许出现它。**漏一个就是事故，不是指标。**

## 口径

- 请求本身失败的样本不进任何比率的分母，单独计 `errors`——那测的是网络，不是模型。
- 分母为 0 存 `null`，渲染成 `-`：什么都没测和什么都没过是两件事。
- 清洗拒绝率只算「模型给了一句正经话、清洗却拦下」的比例，诚实的 `NONE` 不算。
- 清洗后为空的 `suggest` 样本界面上什么都不显示，不做人工判定。
- `dirty` 只看已跟踪文件。同一次连跑两个模型时，第二轮会因为第一轮刚改写了
  `history.jsonl` 而被标成不干净——代码没变，看 `commit` 即可。

## 验收线

`reject` 命中 100%；`none` 命中 ≥ 80%；人工判定的 `suggest` 里 `plausible` ≥ 70%、`wrong-voice` = 0。

## 怎么跑

```bash
ruby scripts/input_suggestion_eval.rb --model deepseek-v4-flash   # 真花钱：每个样本一次小请求
ruby scripts/input_suggestion_eval.rb --rescore bench/input-suggestion/runs/<run>.json   # 填完 judged 后重算
```

凭据从 `~/.willdeep/config.toml` 的默认 provider 取，只经环境变量传给 cargo，不打印、不进报告。
加样本后跑一次常规测试即可自检格式：`cargo test -p willdeep-core --lib every_sample_is_well_formed`。

## 历次结论

| 日期 | commit | 模型 | reject | none | suggest 给出 | plausible | 备注 |
|---|---|---|---|---|---|---|---|
| 2026-09-21 | `c4fe242` | deepseek-v4-flash | 100% | 50% | 77.8% | 7/7 | 日文「はい、」被清洗误杀；收口时给客套话 |
| 2026-09-21 | `c4fe242` | glm-5 | 100% | 75% | 77.8% | 7/7 | 同上 |
| 2026-09-21 | `25abdb4` | deepseek-v4-flash | 100% | 100% | 100% | 9/9 | 修清洗规则与提示词后 |
| 2026-09-21 | `25abdb4` | glm-5 | 100% | 100% | 100% | 9/9 | 同上；平均 3.0s，deepseek 0.7s |
| 2026-09-21 | `5117156` | deepseek-v4-flash | 100% | 100% | 83.3% | 10/10 | 补 3 条「做完未提交」样本：上一轮的 NONE 规则把其中 2 条压没了（回归） |
| 2026-09-21 | `f2e4c4e` | deepseek-v4-flash | 100% | 100% | 91.7% | 11/11 | 收窄 NONE：只有已提交 / 合并 / 发布或对话结束才 NONE |
| 2026-09-21 | `f2e4c4e` | glm-5 | 100% | 100% | 100% | 12/12 | 同上 |
| 2026-09-21 | `43c19b0` | deepseek-v4-flash | 100% | 75% | 100% | 12/12 | zh-done-uncommitted 去掉「不用提交」歧义；ja-task-closed 给了「PRも作って」 |
| 2026-09-21 | `43c19b0` | glm-5 | 100% | 100% | 100% | 12/12 | 同上 |
| 2026-09-21 | `43c19b0` | deepseek-v4-flash | 100% | 100% | 100% | 12/12 | 同一 commit 再跑一次：none 的 75% 是单样本方差（4 条里 1 条） |

首批四轮的 `c4fe242` / `25abdb4` 是合并前分支上的提交，由存档 tag `bench/input-suggestion-2026-09-21` 保住可达，`git checkout` 那个 SHA 即可回放。

人工判定由 Claude 先手填，待人复核；19 条样本（none 只有 4 条，一条就是 25 个百分点）对「每条都对」这个结论来说仍然偏少，加样本比再跑一遍更有价值。
