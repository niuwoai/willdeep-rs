# 本机用量账本（willdeep.usage-ledger.v1）

> 最后更新：2026-09-21 | 起始版本：v0.81.0-rc1
> **Canonical 规范：Xedit 仓库 `docs/USAGE_LEDGER_DESIGN.md`（`~/Sites/Xedit`）**（行格式 §5.2、实时写入 §6.1、回填 §6.2、不变量 §7、已拍板决策 §16）。本文只写 rs 侧怎么落地、每个字段由哪段代码填，不另立一份规范；两边说法不一致时以 Xedit 那份为准并回头改这里。

## 为什么有它

rs 的会话文件没有时间戳、没有逐条用量，`runtime/events.ndjson` 只覆盖 daemon 回合且字段塞在字符串里，Mac 端的 Token 活动因此一条 CLI 用量都看不到。账本是 rs 在唯一的用量汇合点写的一份公开、版本化的记录：**每次模型调用一行**，daemon 回合与进程内回合走同一个写入口，Mac 只读合并。

## 文件与格式

- 位置：`$WILLDEEP_HOME/usage/YYYY-MM.jsonl`（默认 `~/.willdeep/usage/`），按记录的 **UTC** 月份分片，文件权限 0600。
- 每行一个 JSON 对象，`\n` 结尾，整行小于 4096 字节；以 `O_APPEND|O_CREAT` 打开、一次 `write` 写完，daemon 与多个进程内 CLI 并发写同一文件不会交错。超长时按 `workspace` → 实例与回合 id → `task_id` → `provider`/`model` 的顺序置空再写，计数字段永不丢。
- 只追加，永不改写旧行，不删除、不压缩。
- **可选字段一律输出，未知即 `null`，从不省略键。** 键集固定为 §5.2 的 23 个，单测 `serialized_keys_are_exactly_the_spec_set_and_carry_no_content` 钉死，且断言没有 `content / prompt / messages / arguments / reasoning` 之类的内容字段。
- 记账是旁路：写线程遇到任何 IO 错误只在 stderr 打一条警告（每个写入器只打一次，免得把 TUI 刷花），不上抛、不重试、不让回合失败或变慢。

## 字段来源（rs 代码映射）

| 字段 | 由谁填 | 说明 |
| --- | --- | --- |
| `schema` | `usage_ledger::SCHEMA` | 固定 `willdeep.usage-ledger.v1` |
| `id` | 实时：`UsageLedgerRecord::new` 的 UUIDv4；回填：`usage_ledger::backfill_id(sequence)` | 回填 id = `uuidv5(BACKFILL_NAMESPACE, "events.ndjson#<sequence>")`，命名空间常量永不改 |
| `ts` | 实时：`ModelCall::settle` 的完成时刻；回填：事件 `timestamp`（秒）× 1000 | RFC 3339 UTC 毫秒，`format_ts` / `parse_ts` |
| `client` / `client_instance` | `harness::usage_ledger_context`：进程内 Terminal 前端 = `cli`、TUI = `tui`；Runtime 任务按 `origin_client` 前缀（`ClientKind::parse_origin`）；Web 端预测与插件页请求 = `unknown` | 回填同样按 `tasks.json` 的 `origin_client`，空值记 `unknown` |
| `execution` | `execute_runtime` 里的 `UsageOrigin` = `daemon`；其余前端（含 Web 桥接子进程）= `in_process` | |
| `kind` | `main`：`Agent::run_inner` 主循环；`subagent`：子 Agent 的主循环（`UsageLedgerScope::for_subagent`）；`compression`：`agent/context.rs::summarize_once`；`auxiliary`：`LedgeredProvider` 包住的 Provider | 见下「挂接点」 |
| `session_id` / `turn_id` / `task_id` | harness 的会话 id；Runtime 任务的 `turn_id` 与 `RuntimeConnection::task_id()` | 回填取 `tasks.json` |
| `agent_id` | 子 Agent 的 UUID；根 Agent 实时为 `null`，回填取 `tasks.json.agent_id` | |
| `workspace` | harness 解析出的工作区绝对路径 | |
| `provider` / `model` / `local` | `Provider::ledger_identity()` → `ProviderIdentity::from_config`：some.im / anthropic / OpenAI 兼容端点的 host；`local` = 回环地址或显式配置的免鉴权辅助模型（Ollama 等） | 回填拿不到 Provider，`provider` 为 `null`；`model` 取任务 → Runtime 会话 → `agents.json`（子 Agent），都没有就 `null`，不拿当前配置猜历史；`local` 回填记 `false` |
| `input_tokens` / `cache_read_tokens` / `output_tokens` / `total_tokens` | 协议原样上报的 `Usage` | 没报就是 `null`，不估算；`input_tokens` 含缓存命中；回填的 `subagent_usage` 事件本来就不带 `cache_read_tokens` |
| `latency_ms` | `ModelCall`：`begin` 到 `settle` | 回填为 `null` |
| `outcome` | `ok` / `error`（含带着部分 usage 的流中断）/ `cancelled`（被抢占，或调用进行中整个 future 被丢弃，由 `ModelCall` 的 `Drop` 补记） | 回填全为 `ok` |
| `event_sequence` | daemon 回合：`RuntimeEventSink::emit_sequenced` 返回的那条 usage 事件在 `events.ndjson` 里的序号；子 Agent 经 `ChildEventSink::emit_sequenced` 转发拿到；进程内为 `null` | 回填为源事件序号 |
| `backfilled` | 实时 `false`，回填 `true` | |

## 挂接点

- `crates/willdeep-core/src/usage_ledger.rs`（记录与格式）、`usage_ledger/sink.rs`（有界通道 + 单写线程，`shared_sink` 按目录共享，`flush_all` 在 `main()` 退出前排空）、`usage_ledger/scope.rs`（`UsageLedgerScope` / `ModelCall` / `LedgeredProvider`）。
- **主回合 / 子 Agent**：`Agent::run_inner` 在每次 `complete_or_preempt` 前 `begin_model_call`，在原来发 `AgentEvent::Usage` 的两处改为 `settle_model_call`——有 usage 时照旧发事件（经 `EventSink::emit_sequenced` 拿序号），没有 usage 也落一行。子 Agent 是同一个 `Agent`，由 `SubagentCatalog::with_usage_ledger` → `SubagentRun.usage_ledger` → `run_once` 挂上派生句柄。
- **压缩**：`agent/context.rs::summarize_once` 每个候选各记一行 `compression`（成功、空摘要、失败都记）。
- **辅助请求**：harness 在构造处用 `UsageLedgerScope::auxiliary` 包好：安全判官、看图兜底、路由分类器（包括复用的主 Provider——包的是新 `Arc`，主循环用的那份不受影响）、标题 / 下一句预测候选；子 Agent 大文件摘要（`subagent/brief.rs`）在 runner 里包；Web 端下一句预测与插件页 AI 请求用 `harness::standalone_usage_ledger`。**被 Agent 主循环使用的 Provider 不能再包**，否则一次调用记两行。
- `EventSink::emit_sequenced` 是新加的带默认实现的方法：宿主不记序号就返回 `None`。包装别的 sink 的实现必须转发它（目前是 `ChildEventSink`）。

## 历史回填

```bash
willdeep usage backfill --dry-run   # 按本机时区逐日列出调用数与 Token，不写任何文件
willdeep usage backfill             # 写入账本并更新标记
```

- 读 `runtime/events.ndjson` 里 `type` 为 `usage` / `subagent_usage` 的事件（`message` 去掉 `task_id=<uuid> ` 前缀后的 JSON），按 `task_id` 连 `runtime/tasks.json`。一行一行流式读，先按子串筛再解析。
- 去重三道：账本里已有同 `id`；某条实时行（`backfilled=false`）的 `event_sequence` 等于这条事件的序号；事件所属任务还没结束（排队 / 执行中 / 等人）——那一轮正由运行中的 daemon 实时记账，它的行可能还在写线程队列里。反复跑结果逐字节不变（单测 `backfill_joins_tasks_and_is_byte_identical_when_repeated`）。
- daemon 启动时、开始接任务之前自动跑一次（`daemon.rs::run` → `usage_cmd::backfill_on_daemon_start`）。标记 `usage/.backfill-v1.done` 存在就整段跳过，内容是 `{"schema":"willdeep.usage-backfill.v1","max_sequence":N}`。选择「有标记即跳过」而不是每次从标记序号往后补：0.81 起所有 daemon 回合都实时记账，标记之后不会再有需要补的事件；降级到旧版跑过一段再升回来的空档，用手动 `willdeep usage backfill` 补（它不看标记，靠上面三道去重）。失败只警告，下次启动再试。
- 进程内回合在账本上线前的历史无法回填（只有运行级累计、没有时间），如实缺失。

## 测试

- `crates/willdeep-core/src/usage_ledger/tests.rs`：键集与禁用字段、线格式、16 线程 × 1000 行并发追加到同一文件（16000 行全可解析、id 不重复、权限 0600）、共享写线程并发提交、账本不可写时只计失败、账本不可写时回合照常完成、Agent 一次请求一行（`in_process` 与 `daemon` 两种 `execution`，`event_sequence` 对上事件宿主序号）、辅助请求含失败、压缩记 `compression`、被丢弃的调用记 `cancelled`。
- `crates/willdeep-cli/src/usage_cmd/tests.rs`：夹具 `events.ndjson` + `tasks.json` 回填字段、两次回填逐字节相同、与实时行重叠的序号跳过、未结束任务跳过、dry-run 不写盘、daemon 启动只回填一次。
- `crates/willdeep-cli/src/daemon/tests.rs::daemon_turns_link_ledger_lines_to_their_usage_events`：Agent 经真实 `RuntimeEventSink` 跑一轮，账本行的 `event_sequence` 与 `events.ndjson` 中 usage 事件序号逐一对应，随后回填一条不补。
