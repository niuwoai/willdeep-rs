# 模型行为评测归档

固定任务集上各模型的历次成绩。**这个目录是 git 跟踪的，而且必须是**——理由与
隔壁 `skill-worker-range/` 一样：一次快照能回答「这个模型行不行」，回答不了
「我这次改提示词让它变好了还是变坏了」，而后者才是把评测常态化的全部理由。

## 目录

| 路径 | 是什么 |
|---|---|
| `tasks/<id>/` | 20 个任务。每个带 `task.json` 说明书、`prompt.md`、`fixture/` 起始工作区、`solution/` 参考答案、`mutants/`（kind=test 才有） |
| `history.jsonl` | 每个模型每轮一行摘要，append-only。趋势与报警读的就是它 |
| `reports/<日期>/<模型>.<时刻>.json` / `.md` | 那一轮的完整报告（逐任务状态与行为指标）＋同一份摘要 |

模型正文、会话文件、凭据**不进这里**：它们只在私有临时目录里活到那轮结束
（排查时用 `--keep` 留住）。报告里只有计数与状态。

## 口径

- **passed** = 外部 verifier 通过 **且** 受保护文件逐字未动 **且**（kind=test）每个变异都被新测试抓住。
  验收在模型碰不到的干净目录里重建：原始 fixture + 模型改过的 editable 文件。改测试、
  改 Cargo 别名、加 build script 都进不了那个目录，绿不了。
- **cheated** = verifier 绿了但动了受保护文件。进 `cheated`，不进分子。
- **skipped**（缺可执行文件没跑）与 **error**（宿主 / Provider 出错没让模型开工）**不进分母**。
  分母为 0 时比率存 `null`，不存 `0`——「什么都没跑」和「什么都没过」是两件事。
- **人话率**按模型调用次数加权：60 轮的任务和 3 轮的任务不该各占一票。
- `commit` 是摘要里最重要的字段：没有它，一行成绩就是无主的。工作区不干净时脚本会警告。

## 怎么跑

```bash
ruby scripts/model_eval.rb --check-tasks                      # 不联网：每个任务 fixture 红、solution 绿
ruby scripts/model_eval.rb --model glm-5                      # 真花钱：20 个任务各跑一次 willdeep run
ruby scripts/model_eval_trend.rb --alarm --inject             # 趋势写回 docs/MODEL_EVAL.md，掉 10 个点退出 1
```

`--check-tasks` 进 CI（Linux），它保证任务集本身没坏；真跑不进 CI，定时跑法见
[`docs/MODEL_EVAL.md`](../../docs/MODEL_EVAL.md) 与 [`scripts/launchd/`](../../scripts/launchd/README.md)。

## 加任务

复制一个同语言的目录改名；`id` 必须等于目录名；`solution/` 只放 `editable` 里的文件；
kind=test 必须带 `mutants/`，变异只能改 `editable` 之外的实现文件。写完跑一次
`--check-tasks`，红不了或绿不了的任务不许进任务集。
