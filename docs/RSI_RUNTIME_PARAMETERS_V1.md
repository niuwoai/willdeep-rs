# RSI 运行参数与轻量反馈 v1

适用版本：WillDeep Rust 0.92.0-rc1、WillDeep Mac 1.415.0-rc1。

## 目标与范围

统一运行参数的名称、类型、范围、默认值、生效时机和可复现记录。模型辅助反馈只做异常分流，人工一键决定结果是否符合预期；模型结论不授予人工验收，也不替代独立 verifier。

## 共享文件

两端读取 WILLDEEP_HOME/runtime-parameters.json；没有设置 WILLDEEP_HOME 时使用 ~/.willdeep/runtime-parameters.json。该文件只包含下表字段，不包含 provider URL、模型凭据或对话正文。完整示例见 runtime-parameters.example.json。

schema 必须是 willdeep.runtime-parameters.v1。所有字段必须出现，未配置的 token 预算显式写 null。拒绝未知字段、重复字段、错误类型、越界和超过 64 KiB 的文件。文件不存在才回退旧配置；已有文件损坏或不可读时停止本轮执行。

| 字段 | 默认值 | 合法范围 / 意义 |
| --- | --- | --- |
| max_turns | 200 | 整数 1..1000；一次根 Agent 工具循环的模型轮次上限 |
| token_budget | null | null 或整数 1000..10000000；本轮主请求与自动压缩的累计 token 上限 |
| goal_token_budget | null | null 或整数 1000..10000000；同一 Goal 跨轮预算 |
| goal_wall_clock_minutes | 240 | 整数 1..10080；累计实际执行时间，不计暂停及退出应用时间 |
| goal_max_continuations | 64 | 整数 1..10000；Goal 自动续推次数上限 |
| input_suggestions | true | 布尔值；是否生成下一句输入建议 |
| small_model_routing | true | 布尔值；是否启用小模型优先路由 |
| auto_dispatch_read_only | false | 布尔值；是否自动执行只读预检 |
| max_deep_calls_per_harness | 1 | 整数 0..16；每轮运行的 deep 准入次数 |

共享 JSON 优先于旧 [agent] TOML 和 Mac 本机设置；Rust 显式 --max-turns 高于 JSON。没有共享文件时保留已有配置与 UI 设置。已有 Goal 的累计消费不能因重载配置被清空；Mac 还保留已绑定 Goal 的更紧限额。生效快照报告本轮约束，而非只报告文件里的原始值；Mac 原有秒级或零续推限额属于额外宿主约束，导出时按 v1 的整分钟和最小 1 次表示。 Mac owner ledger 中超出 v1 表示范围的既有 token 限额继续作为额外宿主约束执行，不写入共享 JSON；导出配置不包含这些宿主状态，不能用它单独复现完整 Goal 准入。

在下一轮开始时读取并冻结参数；运行中的文件变化不能悄悄改变这一轮。max_turns 限制根 Agent 的一个工具执行段，不是整个会话的消息数量。普通任务达到上限保留部分结果并暂停；Mac 的 Goal 在累计时间、token 和续推预算仍允许时，可交接为新的运行段，并保留同一目标、检查清单与消费。Rust 的调用方收到 max_turns 部分结果后决定后续续跑。token 用尽或 usage 不明时停止本轮自动请求、压缩和输入预测。

子 Agent 保留工种自身的执行上限。Rust 在其反馈和检查点快照中记录实际执行限额；Mac Worker 的轮次上限仍由工种 profile 控制，并显示在已有进度证据中，尚未统一导出完整 v1 快照。不能把根 Agent 的快照当成所有 Worker 的配置。

token 以 provider 报告为依据，重复累计流式用量只计增量。若总数小于输入加输出，采用较大的数。设置预算后缺少可用 usage 不视作零消费，停止继续购买请求或执行响应提出的工具。预算是响应边界上的上限，不保证正在进行的单个请求恰好停在第 N 个 token。

两端执行架构仍有差异：Rust Goal 统计根请求及根压缩；Mac 的既有 owner ledger 还约束其 Worker 与辅助请求，并预留在途额度。这使 Mac 的 Goal 准入可能更早停止。共享参数不承诺两端同成本或相同轨迹；比较时必须记录 client、模型、子任务和 usage 范围。Mac 的并行 scout 仍是本机扩展，不列入 v1 字段。

## 查看、创建、导出

Rust：
- willdeep config runtime：只输出无凭据的有效 JSON。
- willdeep config runtime --max-turns 24：显示包含这次显式覆盖的配置。
- willdeep config runtime --init：创建默认完整 JSON，0600 权限，已有文件不覆盖。
- 修改 ~/.willdeep/runtime-parameters.json 后，下一轮生效。

Mac：输入区上方展开“运行配置”，可查看、复制和导出本轮实际 JSON。导出不会自动更改正在运行的参数。将审核过的 JSON 保存到共享文件即可供两端下一轮使用。

规范 JSON 按示例的字段顺序紧凑编码，null 保留。SHA-256 使用规范 JSON 的 UTF-8 字节；完整默认配置指纹为 cbbe63a9094a7d8546d52f38775bf9e8e0f5e010cfabc767a9d4ffb42ee00e1b。JSON 展示字段顺序变化不影响指纹。

## 反馈闭环

Mac 在一轮结束后提供“符合预期 / 需要修改 / 跳过”，以及按需的模型辅助检查。无需逐轮填写长表单；不点击不会生成肯定结论。新一轮开始时旧检查任务取消，晚到结果不能覆盖新运行。输入建议在用户键入时隐藏，删空后恢复；Tab 采用，Esc 放弃，建议仍与原来的建议 ID 关联。

Rust：
- willdeep feedback review-queue --limit 10：列出至多十条待审，失败或无验证证据优先。
- willdeep feedback review --run UUID：查看结构化运行证据。
- willdeep feedback review --run UUID --assist：按需请求模型，最多等待 60 秒。
- willdeep feedback review --run UUID --decision accepted：人工符合预期。
- --decision needs_changes / unknown：需要修改或跳过。

模型收到工具成功/失败数量、停止原因和验证状态，不收到对话、工具参数、工作区路径或错误正文。反馈 schema 为 willdeep.feedback-assessment.v1；仅允许 judgment=needs_review 或 unknown，confidence=0..100，以及枚举 reason_code。它不具备 accepted 或 verified 输出能力。confidence 是模型自报的分流置信度，不能解释成质量通过率。

账本分别记录 model_assessment / human_disposition，judgment_source、原 run_id / turn_id、参数指纹；模型反馈另记录评审 provider、模型和评审 prompt SHA-256。正常结束、工具成功、Tab 接受建议均不等于任务质量验收。Mac 没有独立 verifier 的运行明确记 unverified。反馈保持本机旁路，不自动上传。

## RSI 评测与门禁

Rust 检查点保存完整生效参数和指纹，即使关闭反馈账本仍可提供参数证据。model_eval 在开跑前解析有效参数，每题的隔离 HOME 使用同一冻结副本，checkpoint 续跑清单也包含参数指纹。

prompt_rsi_eval 的 baseline、candidate、holdout 使用同一份冻结配置。门禁重新计算每题快照指纹，并与实验出处比对；未报告、被改动或双方配置不一致判 non_reproducible。原有任务完整性、受保护文件、verifier、质量/成本/耗时门限与人工判断覆盖要求继续保留。模型辅助反馈不能增加人工 judged 分母或让候选自行上线。

下一阶段可增加抽样策略、按人工结论校准分流模型、候选参数实验，以及经过批准的真实 provider 回放。当前实现不自动修改参数、不自动发布候选，也未把模型分流包装成递归训练。
