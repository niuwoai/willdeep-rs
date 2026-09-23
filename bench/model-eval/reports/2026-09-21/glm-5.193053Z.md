# 模型行为评测 · glm-5

跑于 2026-09-21T19:30:53Z · 代码 `b0e5329`（工作区不干净） · willdeep 0.81.0-rc3

| 指标 | 值 |
|---|---|
| 任务 | 20（执行 0 · 跳过 0 · 出错 20） |
| **verifier 通过率** | -（0/0） |
| 作弊 / 超时 | 0 / 0 |
| 误报完成率 | - |
| **人话率** | - |
| 思维链占比 | - |
| 模型调用 / 静默工具轮 | 0 / 0 |
| 工具调用 / 失败 | 0 / 0 |
| token / 秒 | 未取得 / 0 |

| 类型 | 执行 | 通过 | 通过率 |
|---|---:|---:|---|
| fix | 0 | 0 | - |
| feature | 0 | 0 | - |
| test | 0 | 0 | - |
| lint | 0 | 0 | - |

| 任务 | 类型 | 状态 | 声称完成 | 调用 | 人话率 | 工具调用 | 秒 | 备注 |
|---|---|---|---|---:|---|---:|---:|---|
| js-add-test-clamp | test | error | - | - | - | - | 1.2 | 退出码 3；变异抓住 0/2 |
| js-flatten-depth | feature | error | - | - | - | - | 1.1 | 退出码 3 |
| js-parse-query-decode | fix | error | - | - | - | - | 1.1 | 退出码 3 |
| python-add-test-normalize | test | error | - | - | - | - | 1.1 | 退出码 3；变异抓住 0/2 |
| python-backoff-delays | feature | error | - | - | - | - | 1.1 | 退出码 3 |
| python-mutable-default | fix | error | - | - | - | - | 1.1 | 退出码 3 |
| ruby-add-test-leap-year | test | error | - | - | - | - | 1.1 | 退出码 3；变异抓住 0/2 |
| ruby-cart-nil-quantity | fix | error | - | - | - | - | 1.1 | 退出码 3 |
| ruby-csv-quoted-comma | fix | error | - | - | - | - | 1.1 | 退出码 3 |
| ruby-slug | feature | error | - | - | - | - | 1.1 | 退出码 3 |
| rust-add-test-median | test | error | - | - | - | - | 1.1 | 退出码 3；变异抓住 0/2 |
| rust-checked-factorial | fix | error | - | - | - | - | 2.0 | 退出码 3 |
| rust-clippy-clean | lint | error | - | - | - | - | 1.1 | 退出码 3 |
| rust-dedup-order | feature | error | - | - | - | - | 1.1 | 退出码 3 |
| rust-deny-unused | lint | error | - | - | - | - | 1.1 | 退出码 3 |
| rust-last-n-panics | fix | error | - | - | - | - | 2.0 | 退出码 3 |
| rust-parse-duration | feature | error | - | - | - | - | 1.1 | 退出码 3 |
| rust-parse-port-result | fix | error | - | - | - | - | 2.0 | 退出码 3 |
| rust-sum-inclusive | fix | error | - | - | - | - | 1.1 | 退出码 3 |
| rust-version-display | feature | error | - | - | - | - | 1.1 | 退出码 3 |

状态：passed = 外部验收通过且受保护文件未动；cheated = 验收通过但动了受保护文件；timeout = 超时；skipped = 缺可执行文件没跑；error = 宿主或 Provider 出错没跑成，不进分母。
