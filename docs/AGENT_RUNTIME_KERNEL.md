# Agent Runtime Kernel（willdeep-rs 移植任务书）

> 状态：阶段 0-7 与并行任务 P1 已落地（0.62.0-rc1），并行任务 P2-P5 待做。创建于 2026-09-03，对标 Xedit 1.315.0-rc15 → 1.317.0-rc5 的 Runtime Kernel 分支。
> 上游事实来源：`Xedit/docs/AGENT_RUNTIME_KERNEL.md`（内核语义）、`Xedit/CHANGELOG.md` 1.315.0-rc10 ~ 1.317.0-rc2（逐条实现记录）。
> 相关：[SUBAGENTS.md](SUBAGENTS.md)、[SKILL_WORKERS.md](SKILL_WORKERS.md)、[MODEL_TIERS.md](MODEL_TIERS.md)、[RUNTIME_CONTROL_API.md](RUNTIME_CONTROL_API.md)、[XEDIT_INTEROP_STATUS.md](XEDIT_INTEROP_STATUS.md)。

---

## 1. 上游发生了什么

Xedit 在 2026-09-01 到 09-03 之间把「长生命周期 Agent 的宿主职责」重新分层，对外叙事叫 **Agent OS**：不是自称操作系统，而是借操作系统的三个概念解释一层 Agent 运行时。

| OS 概念 | WillDeep 对应物 |
| --- | --- |
| 进程 | 会话（Session） |
| 中断 / 信号 | 内核事件（Worker 完成、终端退出、审批、定时任务、Webhook、邮件） |
| `init` + `systemd` + 调度器 | 主 Agent：理解事件、派工、收报告、交付用户 |
| 内核 | 宿主 Runtime：接收、鉴信、限流、去重、持久化，并决定何时唤醒或打断模型 |

一句话的边界：**模型不拥有进程调度权**。它只能看到宿主已经裁决过、大小受限的事件通知。

### 1.1 统一事件信封

所有信号收敛成一个 `AgentKernelEvent`，字段包括目标会话、来源、类型、优先级、中断策略、调度权限、正文来源、用户/模型双 audience、去重键、待用户操作标记、生命周期时间戳。

两条**独立**的轴，这是整套设计里最容易做错的地方：

- `authority` 决定**能不能唤醒或抢占**；
- `contentProvenance` 记录正文是 `host` / `model` / `tool` / `file` / `network` 中的哪一种。

宿主可以允许终端完成事件唤醒模型，但终端输出仍按 `tool` 来源做注入净化；Worker 报告标 `model`，不因为宿主代为转发就冒充可信系统正文。

### 1.2 三档中断

| 策略 | 行为 | 典型来源 |
| --- | --- | --- |
| `enqueue` | 只入队，等别的原因启动下一轮 | 文件变化、仅供用户查看的记录、审批投影 |
| `yield-at-boundary` | 当前模型或工具步骤结束后投递；会话空闲时立即唤醒 | Worker、终端、任务、媒体、工作流、外部消息 |
| `preempt` | 取消当前 provider 步骤，保留 transcript 后继续 | 仅宿主签发的 critical 事件 |

不可信或仅通过传输认证的外部事件，即使自称 `critical` / `preempt`，也强制降为 `yield-at-boundary`。Relay Token 只证明通道获准，不证明正文可信。

### 1.3 持久化、恢复与配额

- 按会话写入带 schema 版本的 `agent-events/<session-id>.json`，App 重启后继续投递；
- 相同瞬时信号 5 秒窗口内合并；Relay 事件用日志周期幂等键，服务端 ACK 前重投不会二次启动模型；
- 每会话 200 条、全局 1000 条上限，优先淘汰已处理与低优先级；
- 投递给模型前取 **lease**，只有 provider 成功完成包含该事件的请求才标 handled；进程中途退出，未完成 lease 重启后回到 pending；
- 「已由模型处理」与「仍待用户操作」是两条独立状态轴；
- 已认证外部事件每会话 5 分钟最多主动唤醒 6 次，未认证事件只能排队；每会话最多同时挂 20 条外部待用户提醒；
- 外部正文限长 24,000 字符、32 个 metadata 项，净化模型控制标记并转义 Runtime 自己的 XML frame；
- 事件中心只是观察面，**不是权限中心**：终端、文件、浏览器、MCP 仍走各自原有的确定性授权路径。

### 1.4 Worker 定义的三处改动

1. **职责 × 档位正交**（rs 已在 0.51.0-rc1 落地）：五个公开职责 `generalist` / `implementer` / `tester` / `reviewer` / `ops_runner`，三档模型 `standard` / `advanced` / `expert`。
2. **档位含义按用户最新口径收敛**：基础档是日常干活的主力（`someim-32b`、`qwen3.8-27b` 这一级），进阶档换的是**上下文**（1M 窗口），专家档换的是**智力**（`gpt-5.6-sol` / `opus-5` 这一级）。上下文预算档位由 128K/256K 两档扩到 **64K / 128K / 256K / 1M** 四档（Xedit 1.315.0-rc8）。预算只是「我们最多发多少」的上限，派发时仍与 provider 实际窗口取小。
3. **`generalist` 成为真正的兜底通用 Worker**（Xedit 1.315.0-rc12）：新增可选精确写文件范围、四项受限终端工具、完整 Chrome 工具面、`list_mcp_servers` / `list_mcp_tools` / `call_mcp_tool` 动态 MCP 网关；路由无法判定工种时一律落到它，不再打回父 Agent。工具变多不等于扩权——写入仍要 `target_files`、终端改动仍走审核、MCP 调用仍按信任级别判定。

配套还有：单会话后台 Worker 并发上限 3 → **5**；后台 Worker 获得完整父会话执行上下文（可见终端、精确批准命令、附加工作区、验证命令、命令/MCP/浏览器复核回调）；待办徽标改为逐请求计数。

---

## 2. willdeep-rs 现状盘点

盘点结论：**地基有一半，内核没有**。

| 能力 | rs 现状 | 位置 |
| --- | --- | --- |
| 五职责工种 | ✅ 已对齐 | `crates/willdeep-core/src/subagent/profiles.rs` |
| 三档模型 + 逐档 provider/model 配置 | ✅ 已对齐（0.52.0-rc1） | `crates/willdeep-core/src/worker_tier.rs` |
| 上下文预算档位 | ✅ 0.56.0-rc1 对齐口径：六档共享表 + 标签规则 + 可存性上界 | `worker_tier.rs` 的 `SELECTABLE_CONTEXT_WINDOWS` |
| 后台任务事件 | ✅ 0.61.0-rc1 起统一走内核，旧通知队列退役 | `crates/willdeep-core/src/background.rs` |
| turn 边界注入 | ⚠️ `AgentInstructionInbox` 只有一条 lane、只收用户指令、不落盘 | `crates/willdeep-core/src/agent.rs:585` |
| 注意力聚合 | ⚠️ `RuntimeStatus::priority` 是展示排序，不是调度裁决 | `crates/willdeep-core/src/attention.rs` |
| 统一事件信封 | ✅ 0.56.0-rc1 契约与类型，0.57.0-rc1 接入主循环 | `crates/willdeep-runtime-protocol/src/kernel_event.rs` |
| 三档中断策略 | ✅ 0.57.0-rc1，含抢占取消 provider 请求 | `crates/willdeep-core/src/kernel.rs` |
| 事件持久化与崩溃恢复 | ✅ 0.58.0-rc1 | `crates/willdeep-core/src/kernel_store.rs` |
| lease / 双消费者语义 | ✅ 0.59.0-rc1，含跨连接单一所有权 | `crates/willdeep-core/src/kernel.rs` |
| 本机入站与唤醒额度 | ✅ 0.60.0-rc1（端点待阶段 7）；云端中继不做 | `crates/willdeep-core/src/kernel_ingress.rs` |
| 定时任务触发 | ❌ 无 | — |
| 事件中心 UI | ✅ 0.61.0-rc1：Inbox 投影 + `willdeep event` | `crates/willdeep-cli/src/event_cmd.rs` |

**命名地雷**：rs 控制面已有 `event.list` / `event.stream`，那是 Runtime **出向**的观察事件（Runtime → 客户端）。本任务书的内核事件是**入向**信号（外界 → 主 Agent），方向相反。落地时统一用 `kernel_event` / `kernel.*` 前缀，不要复用 `event`，也不要把内核事件顺手灌进现有公共事件流。

---

## 3. 任务分解

原则：每阶段独立可发布、独立有测试；先把内核语义做对，再谈外部事件；UI 最后接。

### 阶段 0 · 契约先行 ✅ 0.56.0-rc1

- `docs/schemas/agent-kernel-event.v1.schema.json` 定义信封字段与四个枚举（source / priority / interrupt / authority / provenance / delivery state），并把上限、authority 降级表、自动唤醒表分别收在 `x-limits`、`x-authority-interrupt-ceiling`、`x-auto-wake` 三个扩展块里；`docs/examples/agent-kernel-event.v1.json` 是四条覆盖不同判定分支的样例；
- `crates/willdeep-runtime-protocol/src/kernel_event.rs` 是 Rust 侧类型、常量与判定规则，七项守卫测试逐项比对契约文件：枚举取值、十个上限、两张表、样例往返、伪造 preempt 的降级、未认证不唤醒、leased 必带 lease_id；
- **canonical 归属：rs**（2026-09-03 rocky 拍板）。Xedit 侧为 mirror，其守卫测试待 Swift 落地时补，与 `WORKER_ROUTING_CONTRACT.json` 的归属方向相反；
- 已在 [XEDIT_INTEROP_STATUS.md](XEDIT_INTEROP_STATUS.md) 的契约层一节登记。

**两条落地时定死的判定**（改之前先看这里）：

1. **降级在入口做，不在校验做**。`clamp_to_authority()` 把中断策略压到本档上限，`validate()` 才拒绝越权信封。外部来源伪造 `preempt` 是日常流量不是异常，降级后照常投递；做成校验报错会直接丢事件。
2. **降级只压中断，不压优先级**。伪造 critical 的外部事件仍排在前面，只是不许抢占。两者一起压会让真正紧急的外部通知沉底。

### 阶段 1 · 进程内事件内核 ✅ 0.57.0-rc1

- `crates/willdeep-core/src/kernel.rs` 是队列与调度：发布时降级、按去重键合并、加权公平取用、lease 往返、双 lane、上限淘汰、正文净化；
- `agent.rs` 接线：`with_event_kernel` 挂上之后，事件在 turn 顶部作为用户消息进对话，宿主 critical 事件与 provider 请求 select 竞争，抢占发 `AgentEvent::TurnPreempted`；
- 后台任务的生产者适配器 `background_task_event` 就位。

**四条落地时定死的判定**：

1. **边界在 turn 顶部，不在工具与工具之间。** 一次 assistant 的 `tool_calls` 必须紧跟它那批 tool 结果，中间插用户消息会拆散配对。turn 顶部就是「模型或工具步骤已结束」那个边界。
2. **lease 的分界是「事件有没有进到留得下来的对话」**，不是「请求成不成功」。被抢占取消的那一轮，事件文本已在 transcript 里，算投递完成；请求报错时整轮 messages 一起丢，才放回待投递。
3. **抢占标志跟着队列走，不靠 `Notify` 的一次性许可。** 许可会在没人等待时到达（请求已发出、select 还没进入等待）而丢掉抢占，也会在事件已被正常投递后残留而取消一个无辜的请求。
4. **净化只中和看起来像标签的东西。** 正文里的 `</runtime-events>` 会当场关掉边界，而数学与代码里的 `<` 必须原样保留。

**顺序调整（原计划的一半推迟）**：TUI 与非交互 CLI 的后台通知路径**没有**切到内核，仍走原有的 notice 队列。两条路径同时投递会让同一个结果向模型讲两遍，而切换需要事件中心来承接用户侧展示，因此并入阶段 6 一起做。本阶段生产者侧只交付适配器。

**验收**：已达成——三档调度、优先级防饥饿、两种去重、lease 往返与重启回收、双 lane 互不代劳、伪造抢占降级、frame 注入防护、上限淘汰；主循环侧三项接线测试。

### 阶段 2 · 持久化与恢复 ✅ 0.58.0-rc1

- `crates/willdeep-core/src/kernel_store.rs`：每会话一个 0600 文件、原子替换、带 schema 版本；
- `EventKernel::restore` 把 lease 变回待投递并重新记住去重键；`take_dirty_sessions` + `flush` 负责写回；
- 全局淘汰牵连到的会话各自标脏重写；会话被遗忘时标脏，下次刷盘删文件；
- 整份解不出来的文件改名 `.corrupt-<时间戳>` 隔离，单条坏事件只丢自己，加载失败不阻塞启动。

**验收**：已达成，九项测试覆盖。

**未做（留给阶段 5 与 6）**：唤醒预算的持久化要等阶段 5 有了预算本体；「归档会话拒绝迟到事件」要等阶段 6 接上会话生命周期，现在内核不知道哪个会话被归档了。

### 阶段 3 · lease 与双消费者 ✅ 0.59.0-rc1

- lease 往返与崩溃恢复、双 lane 互不代劳在阶段 1、2 已完成；
- 本阶段补齐**单一所有权**：`claim_session` / `release_session` / `release_owner`，lease 记住取走它的消费者，`ack_as` / `release_as` 只结算自己的；
- 不带身份的老方法等价于一个固定的默认消费者（`SOLE_CONSUMER`），单进程路径行为不变。

**验收**：已达成——认领不可抢占且可交还、跨连接 lease 隔离、断开连接交还 lease 与会话。

**唤醒额度的共享**（多客户端共用一份额度）留到阶段 5，那里才有额度本体。

### 阶段 4 · 信任与正文净化 ✅ 0.59.0-rc1

- 两条轴在阶段 0 就落在类型上，阶段 1 完成了 frame 转义与用户消息承载；
- 本阶段补齐**入队剪枝**：标题、正文、metadata 条目数与值长度按契约上限钳制，模型控制标记（`<|im_start|>`、`[INST]`、`<<SYS>>` 等）中和为可见记号。

**两者分工**（改之前先看）：入队剪枝管**体量与控制标记**，渲染转义管**frame 边界**。体量必须当场砍掉，否则几兆的正文会跟着每次落盘写一遍；frame 转义只在拼进对话时才有意义，存储保持原文，事件中心才显示得出用户实际收到的东西。

**验收**：已达成——frame 注入防护、伪造 critical 降级、Worker 报告按不可信正文处理、入队剪枝后仍是合法信封、控制标记留痕中和。

### 阶段 5 · 外部事件入站

**范围已定（2026-09-03 rocky 拍板）：只做本机入站，云端中继留给桌面端。** rs 不连 `collab-relay.v2`，不实现 poll / ACK / 退避 / 中继统计，也不承接飞书、钉钉、邮件这类平台事件。那条路仍归 Xedit 加 Go relay。

本机入站只有三个来源，共用同一个受限入口：

- 已认证的本机 daemon 端点（持 Runtime Token，走现有控制面鉴权）；
- hooks 与本机脚本注入（`crates/willdeep-core/src/hooks.rs` 已有落点）；
- rs 自己产生的定时任务触发，如果阶段 6 之后要做。

规则：

- 入站正文一律按 `network` 或 `tool` 来源处理，持 Runtime Token 不提升正文信任，只提升调度权限；
- 唤醒额度：已认证外部事件每会话 5 分钟 6 次，且**首次到达、启动恢复、provider slot 释放、turn 收尾走同一个额度出口**（Xedit 1.315.0-rc16 就是在补这条旁路）；只有真正启动新 provider turn 才记账；
- 未认证来源永不自动唤醒；每会话最多 20 条外部待用户提醒；
- 额度耗尽时安排一次去重的到期重试，不让事件无限搁置。

留给未来的口子：信封本身保留 relay 相关字段的位置，将来若接云端中继不必改 schema 版本，但 rs 侧不实现对应路径。

**状态：内核侧已落地（0.60.0-rc1）**，`crates/willdeep-core/src/kernel_ingress.rs` 是额度记账本与入站构造器，`EventKernel::admit_wake` 是那个唯一出口。一条落地时定死的判定：**持 Token 的本机调用方按已认证外部记账，不按本机可信档**——这条路上进来的正文终究是外面给的，给它免限流等于开一条不限速的注入通道。

**未做**：控制面上的入站端点本身与 `kernel.*` 操作一起放在阶段 7；hooks 注入点等阶段 6 接线时一并挂。

**验收**：已达成内核侧——未认证不唤醒、已认证按额度并给出重试时刻、窗口滑过后额度回来、跨消费者共用同一份额度、正文信任不随 Token 提升。

### 阶段 6 · CLI / TUI 事件中心与生产者切换 ✅ 0.61.0-rc1

- **生产者切换完成**（阶段 1 推迟的那一半）：TUI 与非交互 CLI 的后台通知队列退役，结果统一进内核，由主 Agent 在轮次边界收走；
- 唤醒额度收口在 `wake_for_kernel_events` 一个函数里，所有会开新一轮的路径都走它；
- Inbox 增加运行时事件投影（`AttentionSource::RuntimeEvent`），带来源、合并次数与模型侧状态；
- 新增 `willdeep event list|show|ignore`，读日志而不是连内核，Agent 没跑时也能查。

**三条落地时定死的判定**：

1. **待投递事件优先于排队的用户提示词。** 用户排在后面那句话，很可能正是基于还没看到的后台结果说的。
2. **「模型已经看过」与「还要人处理」在界面上必须分开显示**，否则用户会以为 Agent 读过就等于办过了。CLI 的投递标签写成 `seen/you` 就是为此。
3. **`event ignore` 只结算用户这一侧**，事件留着，且命令输出里明说它不批准任何操作——这条命令最容易被当成批准。

**验收**：已达成——投影只显示不决策、忽略后落盘且事件仍在、未知 ID 明确报错、投递标签分得开两条 lane。

**未做**：`AttentionSource::RuntimeEvent` 的 TUI 渲染沿用现有 Inbox 行，没有单独的事件中心面板；相对时间与「处理人」两列留到有真实使用反馈之后再决定要不要加。

### 阶段 7 · 控制面与互操作 ✅ 0.62.0-rc1

- `kernel.list` / `kernel.get` / `kernel.ignore` 加入 [RUNTIME_CONTROL_API.md](RUNTIME_CONTROL_API.md)，并进 `public-api-v1.json` fixture；
- 脱敏口径按 §7 现有规则：正文默认不进公共 DTO，最多给一个与 `prompt_excerpt` 同规则的打码摘要；
- 更新 `docs/XEDIT_INTEROP_STATUS.md`，标注两端 schema 版本与兼容窗口。

**落地时定死的两条**：

1. **正文不进公共 DTO。** 只给打码截断的标题摘要，字段白名单，往信封上加字段不会自动顺流到浏览器。
2. **读日志不读内存。** 跑 Agent 的可能是别的进程，日志是唯一共享事实；代价是最多落后一次刷盘，这条写进了 API 文档。

**验收**：已达成——fixture 契约测试通过、公共投影不带正文且标题打码截断、`kernel.ignore` 计入变更类操作。

### 并行任务（不依赖内核，可先做）

| 任务 | 内容 | 参照 |
| --- | --- | --- |
| ~~P1 上下文预算档位对齐~~ ✅ 0.56.0-rc1 | 见下方更正 | Xedit 1.315.0-rc8 |
| P2 档位语义对齐 | 进阶档明确为「1M 上下文」而非只放宽预算；专家档为「更聪明」。注意 0.52.0-rc1 的教训：只有测试引用的常量表等于文档不是代码，改完 grep 非 `cfg(test)` 调用点 | [worker-tier 记忆] |
| P3 `generalist` 工具面 | 补 MCP 动态网关三件套、受限终端四工具、可选精确写文件范围；含糊请求兜底到 `generalist` 而非返回空 profile | Xedit 1.315.0-rc11 / rc12 |
| P4 后台 Worker 并发 5 | 上限 3 → 5；带精确写文件集合的 Worker 仍前台锁定，不用扩容换文件冲突 | Xedit 1.315.0-rc13 |
| P5 后台 Worker 继承父上下文 | 可见终端、精确批准命令、附加工作区、验证命令、复核回调；回调按 runtime ID 弱引用重解析，不强持已结束会话 | Xedit 1.317.0-rc2 |

**P1 的更正（本文初版写错了，留档）**：初版说 rs「只有 128K / 256K 两档，要扩到四档」。那是照着 `TIER_WINDOW_STANDARD` / `TIER_WINDOW_EXTENDED` 两个常量下的判断，而**可选档位根本不在那里**——TUI 面板自带一份 `CONTEXT_WINDOWS` 字面量，本来就有六档。真正的问题不是数量而是口径：

- 面板最大档写的是十进制 `1_000_000`，Xedit 的「1M」是 `1_048_576`。同一个标签在两个 App 里是两个数，而它最终进同一份 `config.toml`；
- 配置校验的上界也是 `1_000_000`，正好和旧面板擦边通过。把面板对齐到 2^20 之后若不同时抬上界，用户选完 1M 保存就撞 `must be between`。这条现在有测试钉着（`every_selectable_window_is_storable`）；
- 标签函数用 `value >= 1_000_000` 判 M、其余一律除 1024，于是 `config.toml` 里钉的 400000 会显示成「390K」，界面和实际预算对不上。

落地方式是把档位表和标签规则提到 `willdeep_core::worker_tier`，TUI 与配置校验都从那里取。32K / 48K 两档保留为 rs 独有，理由写在常量注释里：Skill Worker 的小上下文纪律要求有界任务钉在 32K，而 Xedit 面板服务的不是同一批工种。**共享的是每一档的数值口径，不是档位数量。**

---

## 4. 明确不做的事

- 不把内核事件当权限中心：授权路径一律保持现状；
- 不因为宿主转发就提升正文信任；
- 不做云端中继：rs 不实现 relay 服务端，也不做 `collab-relay.v2` 的客户端，平台消息与邮件类事件归桌面端；
- 不改动 `spawn_agent` 的 profile 准入语义：准入跟档位走，不跟工种名走（0.51 踩过）；
- 不给 `someim-32b-compressor` / `someim-32b-security-guard` 做工种归一化，它们不是工种别名。

## 5. 风险登记

1. **双 lane 状态被做成一条**：模型消费与用户待办合并会导致「模型读过就算办过」，Xedit 修过一次。类型上就要分开。
2. **额度旁路**：任何一条新的「启动模型」路径都必须经过同一个唤醒策略函数，否则限流形同虚设。
3. **命名撞车**：与现有 `event.*` 控制面操作混读会毒害协议演进。
4. **契约只有测试引用**：档位模型表、事件枚举表若无运行时调用点，就是文档不是代码。
5. **跳过的端到端测试**：写了跳过分支就必须另行验证它没在静默跳过（互操作记忆里的老账）。

## 6. 版本与交付纪律

按仓库规范：每个阶段合入时同步 `CHANGELOG.md` 与版本号，功能新增提 MINOR 并把 rc 重置为 rc1，bugfix 只递增 rc。涉及协议字段的阶段（0、7）必须同时更新 `public-api-v1.json` fixture 与 `docs/XEDIT_INTEROP_STATUS.md`。
