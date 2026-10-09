# SWE-bench 失败证据分析（第一轮）

两轮各 30 题：DeepSeek 11/30，GLM-5 19/30。共有 30 次失败尝试、22 个不同失败题，其中 15 次为空补丁（12 次标记预算耗尽，3 次为其他空补丁）。8 次空补丁的末条助手消息提议 spawn_agent，但没有保存对应工具结果；这仍是观察，不是全部已证实的未派发。

范围：WillDeep CLI 0.92.0-rc1 的公开脱敏轨迹；不是 Mac 版，也不是完整 Verified 成绩。历史报告保持只读。

## 逐题观察

缺失工具返回只表示没有保存结果，不能单凭此认定工具失败或未运行。读取次数不代表浪费；测试目录/core-only 是产物观察，不是自动根因判定。零 token 且轨迹缺失不能证明未调用模型。测试数为 — 表示没有官方逐题测试报告，不表示零失败或通过。

| 模型轮次 | 题目 | 终态 | Token | 修改文件数 | 末条提议无结果 | 官方失败测试数 FTP / PTP |
|---|---|---|---:|---:|---|---:|
| deepseek-flash-20261008 | [django__django-11555](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/django__django-11555/evidence.json) | unresolved | 0 | 1 |  | 1 / 0 |
| deepseek-flash-20261008 | [django__django-11885](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/django__django-11885/evidence.json) | budget_exhausted_empty_patch | 1034172 | 0 | await_agents | — |
| deepseek-flash-20261008 | [django__django-12050](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/django__django-12050/evidence.json) | budget_exhausted_empty_patch | 1015318 | 0 | spawn_agent | — |
| deepseek-flash-20261008 | [django__django-13297](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/django__django-13297/evidence.json) | unresolved | 1045994 | 1 |  | 1 / 0 |
| deepseek-flash-20261008 | [django__django-13449](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/django__django-13449/evidence.json) | unresolved | 1035222 | 1 |  | 1 / 0 |
| deepseek-flash-20261008 | [django__django-13837](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/django__django-13837/evidence.json) | budget_exhausted_empty_patch | 1050754 | 0 | spawn_agent | — |
| deepseek-flash-20261008 | [django__django-15930](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/django__django-15930/evidence.json) | budget_exhausted_empty_patch | 1038077 | 0 | await_agents | — |
| deepseek-flash-20261008 | [django__django-16032](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/django__django-16032/evidence.json) | unresolved | 1004153 | 1 |  | 2 / 0 |
| deepseek-flash-20261008 | [django__django-16145](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/django__django-16145/evidence.json) | unresolved | 1011290 | 1 |  | 0 / 1 |
| deepseek-flash-20261008 | [matplotlib__matplotlib-25122](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/matplotlib__matplotlib-25122/evidence.json) | budget_exhausted_empty_patch | 1066638 | 0 | spawn_agent | — |
| deepseek-flash-20261008 | [mwaskom__seaborn-3187](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/mwaskom__seaborn-3187/evidence.json) | empty_patch | 1037138 | 0 |  | — |
| deepseek-flash-20261008 | [pylint-dev__pylint-4551](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/pylint-dev__pylint-4551/evidence.json) | unresolved | 762244 | 1 |  | 10 / 0 |
| deepseek-flash-20261008 | [pytest-dev__pytest-10081](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/pytest-dev__pytest-10081/evidence.json) | budget_exhausted_empty_patch | 1029958 | 0 | await_agents | — |
| deepseek-flash-20261008 | [scikit-learn__scikit-learn-13496](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/scikit-learn__scikit-learn-13496/evidence.json) | budget_exhausted_empty_patch | 1046794 | 0 | spawn_agent | — |
| deepseek-flash-20261008 | [sphinx-doc__sphinx-8035](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/sphinx-doc__sphinx-8035/evidence.json) | unresolved | 1062771 | 1 | spawn_agent | 1 / 0 |
| deepseek-flash-20261008 | [sphinx-doc__sphinx-8551](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/sphinx-doc__sphinx-8551/evidence.json) | budget_exhausted_empty_patch | 1021920 | 0 | spawn_agent | — |
| deepseek-flash-20261008 | [sympy__sympy-11618](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/sympy__sympy-11618/evidence.json) | unresolved | 1006212 | 1 |  | 1 / 0 |
| deepseek-flash-20261008 | [sympy__sympy-20438](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/sympy__sympy-20438/evidence.json) | empty_patch | 1020134 | 0 |  | — |
| deepseek-flash-20261008 | [sympy__sympy-22714](https://willdeep.com/benchmarks/runs/deepseek-flash-20261008/tasks/sympy__sympy-22714/evidence.json) | budget_exhausted_empty_patch | 1002842 | 0 | spawn_agent | — |
| glm5-20261009 | [astropy__astropy-13977](https://willdeep.com/benchmarks/runs/glm5-20261009/tasks/astropy__astropy-13977/evidence.json) | unresolved | 1005450 | 1 |  | 8 / 4 |
| glm5-20261009 | [django__django-11885](https://willdeep.com/benchmarks/runs/glm5-20261009/tasks/django__django-11885/evidence.json) | unresolved | 1009098 | 2 |  | 0 / 2 |
| glm5-20261009 | [django__django-13297](https://willdeep.com/benchmarks/runs/glm5-20261009/tasks/django__django-13297/evidence.json) | budget_exhausted_empty_patch | 1001922 | 0 | run_command | — |
| glm5-20261009 | [django__django-13449](https://willdeep.com/benchmarks/runs/glm5-20261009/tasks/django__django-13449/evidence.json) | empty_patch | 1001865 | 0 |  | — |
| glm5-20261009 | [django__django-16032](https://willdeep.com/benchmarks/runs/glm5-20261009/tasks/django__django-16032/evidence.json) | unresolved | 1001463 | 3 |  | 2 / 0 |
| glm5-20261009 | [django__django-16145](https://willdeep.com/benchmarks/runs/glm5-20261009/tasks/django__django-16145/evidence.json) | budget_exhausted_empty_patch | 1021246 | 0 | spawn_agent | — |
| glm5-20261009 | [matplotlib__matplotlib-14623](https://willdeep.com/benchmarks/runs/glm5-20261009/tasks/matplotlib__matplotlib-14623/evidence.json) | unresolved | 1007742 | 1 | run_command | 0 / 0 |
| glm5-20261009 | [mwaskom__seaborn-3187](https://willdeep.com/benchmarks/runs/glm5-20261009/tasks/mwaskom__seaborn-3187/evidence.json) | unresolved | 1057439 | 1 |  | 1 / 0 |
| glm5-20261009 | [pylint-dev__pylint-4551](https://willdeep.com/benchmarks/runs/glm5-20261009/tasks/pylint-dev__pylint-4551/evidence.json) | unresolved | 1030400 | 1 | run_command | 10 / 0 |
| glm5-20261009 | [sympy__sympy-12096](https://willdeep.com/benchmarks/runs/glm5-20261009/tasks/sympy__sympy-12096/evidence.json) | budget_exhausted_empty_patch | 1008810 | 0 | spawn_agent | — |
| glm5-20261009 | [sympy__sympy-20438](https://willdeep.com/benchmarks/runs/glm5-20261009/tasks/sympy__sympy-20438/evidence.json) | unresolved | 1029376 | 1 | run_command | 2 / 0 |

## 可行动的证据与假设

- 空补丁：部分预算耗尽题最后仍在提议委派/等待，尚无保存结果。应在边界可确定后及时委派，避免完整重复探索；这并不能证明所有空补丁都是委派造成。
- django-13449：GLM 轨迹包含 25 次 grep、24 次 read，无修改；DeepSeek 同题产物仅诊断测试。应把探索推进到可验证的实现修改，临时复现文件不能替代修复。
- sympy-11618：DeepSeek 局部测试通过，但以拒绝不同维度点的方式修复距离计算，与问题要求的计算结果不符。应先写出原始例子的预期行为，再验证修复；不能把自行设计的测试全绿当作完成。
- django-16145：DeepSeek 子 Agent 报告在独立工作树及 /testbed 重复修改以适配绝对路径 verifier。应让命令基于执行工作树运行，合并后检查父工作树实际差异；不要扩大写权限来绕过。
- django-11555：DeepSeek 轨迹缺失，只有 core 产物与官方未解决判定。原因未知，单独列为采集缺口。

## 证据定位

- django-13449：工具计数见 report.json 中对应两条记录。
- sympy-11618：DeepSeek 会话 41ecabb8-1181-42f9-93ba-1e676fb6d92e，第 58 条助手报告明确选择抛出异常，第 62 条只确认局部测试通过。
- django-16145：DeepSeek 第 39 条工具结果明确报告独立工作树与 /testbed 的重复修改；这是 Agent 的报告，尚不能单凭它认定运行时围栏失效。
- 其余失败题已保留官方失败测试及产物观察，未逐一证明语义根因；不得自动套用上述原因。

## 第一轮候选与验证边界

候选仅修改通用主 Agent 与 implementer 提示词：原始行为验收、有限探索后执行、及时委派、工作树内验证、合并后检查实现差异。不写入题号、gold patch 或特定项目答案，不改变模型、预算、权限与裁判。

历史评测为 0.92.0-rc1；候选基于 develop 的 0.94.0-rc1，标记为 0.94.0-rc2。比较历史分数不能隔离提示词效果。下一步须同版本、同模型、同参数分别运行 baseline/candidate，保留成功题回归，新增 30 题作为尚未分析的 holdout。未重跑前，不宣称解决率提升。

本地验证结果见 [validation.md](validation.md)，候选与基线源码指纹见 [candidate.json](candidate.json)。
