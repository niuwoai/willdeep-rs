# RSI 加速与完整性实施清单

日期：2026-10-08，版本：0.91.0-rc1。基线为 develop，独立工作分支 `codex/rsi-throughput`，保留原工作树圆桌任务的改动。

目标是更快获得可复现的质量改进证据。模型评测提速不等于 Agent 自身提速；事件数量、Tab 采用率和未识别纠正不等于任务成功率。

## 第一批实现

| 项目 | 行为与验收 |
| --- | --- |
| 受控并发 | `model_eval.rb --jobs 1..4`；每题独立目录，结果按任务清单排序。先串行跑三题探测 Provider，全部 error 时不继续放大请求。线程异常向上返回，不能静默归档缺题结果。 |
| 逐题断点 | `--checkpoint PATH` 每完成一题原子落盘，权限 0600，运行锁禁止多个进程共用。`--resume` 复用 passed/failed/cheated/timeout，重跑 error/skipped。 |
| 断点出处 | commit、源码内容、任务内容、二进制 SHA256、派生配置 SHA256、环境指纹、模型、profile、变体、预算及 jobs 全部相同才复用；配置正文和环境值不进入断点。日志及凭据仍在私有临时目录，按原规则清理。 |
| RSI 预检查 | dirty 源码、构建 commit / 版本不一致、缺任务依赖时在模型调用前失败。`--preflight` 只检查，不请求 Provider。输入建议套件也支持 `--jobs 1..4`，但暂不支持断点。 |
| 运行归因 | Agent 运行新增 `run_started` 和唯一 `run_id`；关联运行开始时的 provider / model，工具成功新增 `tool_succeeded`。运行结束和工具失败使用同一 run_id。模型在运行中切换时，归属字段仍代表开始时的模型。 |
| 健康报告 | `feedback_health.rb` 读本机账本，去重、记录坏行，输出追问关联率、建议未终结数、新运行的验证覆盖率和已结算工具失败率，以及待调查问题。旧事件缺少分母时明确写未知；审批拒绝不进入模型失败调查队列。 |
| 输入建议恢复 | TUI / Web 打字仅隐藏已展示建议，删空再次显示；不重复记 shown。Tab 后真正发送才结算，发送别的话才记 ignored_typed。Esc、换会话、新轮次、刷新作废，采用后删空不重复采用同一条。 |

## 常用命令

```sh
ruby scripts/model_eval.rb --model MODEL --jobs 4 --checkpoint target/eval/checkpoint.json
ruby scripts/model_eval.rb --model MODEL --jobs 4 --checkpoint target/eval/checkpoint.json --resume
ruby scripts/prompt_rsi_eval.rb --model MODEL --variant variant.json --preflight
ruby scripts/prompt_rsi_eval.rb --model MODEL --variant variant.json --jobs 2 --checkpoint-dir target/rsi-checkpoints
ruby scripts/prompt_rsi_eval.rb --model MODEL --variant variant.json --jobs 2 --checkpoint-dir target/rsi-checkpoints --resume
ruby scripts/feedback_health.rb --out target/rsi-health/latest
```

正式对照评测必须先提交实现并从同一 clean commit 构建二进制。模型名、配置、预算、任务集或并发数改变时使用新断点路径。断点保留上一轮的任务耗时，续跑加速的是本次执行；不能把复用耗时解读为本次墙钟耗时。

并发会竞争 CPU / Provider 配额：耗时作为晋升目标时用 `--jobs 1`，或先设计等负载的独立配对实验；不把并发下的耗时变化归因于提示词。不要复用长期旧 baseline 来决定晋升。断点是本机恢复材料，正式报告仍须经过完整任务清单、内容哈希和变体门禁。

## 后续批次与验收

1. **观测补齐**：本批已增加运行开始、已结算工具结果及起始模型归属。后续补运行中未结算工具、首次失败与重试恢复；建议生成的 requested / none / failed / expired 及耗时；请求级 request id 和 sink 丢弃 / 写失败计数。只有各入口真实产生数据后，才把健康报告对应的 null 换成数值。
2. **上下文分类**：本地关联上一轮输出和后续输入，区分纠正、重做、信息补充、新需求、认可、新话题、未知。规则先判，含糊样本按显式配置请求模型；记录依据、置信度、分类器版本，抽样人工复核。默认不上传正文、不改变 store_text；模型分类不能覆盖 verifier 的事实。
3. **评测扩充**：旧 20 题保留为历史回归，新版本化套件增加多文件修改、压缩、派工汇合、工具失败恢复、停止 / 插话 / 审批、插件参数和虚报完成。每题有独立 verifier 或人工 rubric；训练 / 筛选 / 留出严格隔离。当前质量提升门禁不因旧题饱和而自动放宽。
4. **高质量下的资源优化**：增加明确的 efficiency 目标，要求质量不退化、作弊和虚报不增加，再比较 Token / 耗时 / 重试；先定义配对重复运行与噪声处理，再修改评分公式。
5. **首轮真实闭环**：选一个可复现失败，补成回归题，起草一个变体，跑 clean commit 上的 baseline/candidate，人工复核并留存唯一报告。健康报告的 investigate_only 不等于可晋升候选；不降低现有样本阈值凑结果。
6. **周期运营**：后续提供预算、候选去重、待判样本队列与汇总；调度由用户选择。没有自动上线或替换线上提示词。

## 当前数据与推进顺序

本次只读扫描本机反馈：99 条事件、5 个会话，8 条后续输入中 3 条关联上一轮（37.5%）；14 条已展示建议均已有终结事件。后续输入粗分类为 7 条 other、1 条 approval，不能由此推断满意度或纠正率为零。旧账本没有 run_started 和工具成功分母，新健康指标保持未知。

调查队列最先复现 `run_command / command_timeout`（3 次）和 `list_directory / io`（2 次）。次数尚不足正式候选门槛，先确认是否为宿主、环境或任务设计问题，再补回归题；不要直接归咎于提示词。

下一批优先补建议生成和丢事件观测，再做带上下文且可人工复核的输入分类；随后增加任务覆盖，最后做首轮真实配对实验。当前未完成后续分类器、新任务套件和正式 baseline/candidate 报告，本批不宣称已形成自动自我改进闭环。

## 验证记录

无需 Provider 的回归验证并发隔离、锁、断点出处、异常传播和健康报告隐私；TUI 单测与真实浏览器 fixture 验证输入建议恢复及反馈结算。付费评测与效果改善另行记录，不能用离线测试冒充。

- Ruby：原模型评测 28 个测试 / 310 个断言、原 RSI 21 个测试 / 183 个断言、新执行器 5 个测试 / 46 个断言、新健康报告 3 个测试 / 15 个断言通过；任务自检 20 题、0 个错误。
- Web：93 个 Vitest 测试通过，lint 与生产构建通过；真实浏览器的 7 项流程检查通过，并检查打字、删空不重复 shown / 不误记 ignored，Tab 只产生一次 accepted。
- Rust：输入建议相关 5 个测试通过；最终 `cargo test --workspace --offline --quiet -- --test-threads=4` 全工作区 1388 个测试通过、7 个按既有声明忽略，Clippy 全工作区 / 全 targets 且 `-D warnings` 通过。新增运行归属测试确认工具成功关联同一 run_id、未知子运行不继承父模型。
- 初次全回归中的既有进程清理测试发生一次超时，单独复测通过；最终降低测试并发后全套通过。沙箱阻止本机监听的测试已在允许回环网络的环境重跑。未修改这些既有测试来掩盖失败。
- 调度基准：24 个独立等待任务，jobs=1 为 0.5761 秒，jobs=4 为 0.1431 秒，约 4.03 倍。仅证明受控调度吞吐，不代表真实模型调用、任务质量或 Agent 端到端速度提升。
- 本机历史账本仍为旧版本事件；本批未安装运行，因此新增 run_id / 分母尚未产生生产数据。健康报告不得回填或推断旧记录的成功率。
