# 小上下文 Skill Worker

> 与 macOS 版 Xedit 的 `docs/SMALL_MODEL_SKILL_WORKERS_DESIGN.md`（skill-workers.v1）同源。本文是 willdeep-rs 侧的落地说明与差异记录。

一句话：**把「可自动验证」的编程小任务拆给 16K～64K 的小模型工种，用 Task Packet 喂料、用 Verifier 判结果——模型不自证，程序来证。**

## 为什么

三条纪律，顺序不能颠倒：

1. **越容易自动验证的任务，越可以大胆用弱模型**。`cargo test` 的退出码不会撒谎，模型的「我修好了」会。
2. **平均上下文要小**。max 64K 不等于 avg 64K；给小模型配一个 128K 的窗口，它照样会用 25 次 grep 把窗口烧穿。真正省钱的是**平均**上下文。
3. **相关文件由主 Agent 内联，Worker 不自己找**。Worker 每一轮搜索都在烧窗口；Task Packet 直接带文件内容，Worker 起手就是热的。

## 工种清单

| 工种 | 职能 | 工具 | 窗口 | Payload 上限 | 写通道 | Verifier |
|---|---|---|---:|---:|---|---|
| `scout` | 定位文件、符号、调用点 | search / grep / list / read | 32K | 4 KB | 无 | — |
| `reader` | 阅读并总结长文件 | read / list / search | 32K | 4 KB | 无 | — |
| `log_inspector` | 解释失败日志、归类错误 | read | 16K | 3 KB | 无 | — |
| `git_detective` | 回归定位、commit 考古 | git log / diff / blame / status / read | 32K | 4 KB | 无 | — |
| `editor` | 修改一个单独批准的文件 | read / edit | 32K | 4 KB | 单文件锁 | 可选 |
| `test_fixer` | 把失败测试修到绿 | read / edit / run_command（限 verifier） | 64K | 6 KB | **文件集锁** | 必需 |
| `build_fixer` | 修编译 / 类型 / lint 错误 | read / edit / run_command（限 verifier） | 32K | 4 KB | **文件集锁** | 必需 |
| `deep` | 开放式跨文件调查 | search / grep / read / list / git status | **继承会话窗口** | 默认 | 无 | — |

`deep` 是**刻意的例外**：它跑父模型，因为它的活本来就装不进小窗口。跨模块重构、架构设计、语义模糊的任务继续走 `deep` 或主 Agent，不要硬拆。

除 `deep` 外全部绑定廉价模型（some.im 下默认 `glm-5`），可在 `[subagents.<工种>]` 里逐个改绑。

## Task Packet

`spawn_agent` 的可选参数 `task`。**不传 `task` 时行为与以前逐字节相同**——自由文本 prompt 仍然有效。

```jsonc
{
  "profile": "test_fixer",
  "prompt": "修到绿，别动公开接口",
  "task": {
    "goal": "修复 subagent::tests::verifier_loop 失败",
    "relevant_files": ["crates/willdeep-core/src/subagent.rs"],
    "known_facts": ["失败始于 f936618", "断言是 attempts=3 实得 1"],
    "constraints": ["不改 SubagentProfile 的公开字段"],
    "verifier": { "command": "cargo test -p willdeep-core subagent", "expected_exit_code": 0 },
    "max_attempts": 3
  }
}
```

运行时处理：

1. `relevant_files` 由 Runtime 读出内联进 Worker 的第一条消息。内联预算 = 窗口 × 3/4 字节（32K 档 ≈ 24 KB，64K 档 ≈ 48 KB），超预算的文件**明确标注被省略**，不静默丢弃；读不到的文件标 `unreadable`，也不静默丢弃。
2. `known_facts` / `constraints` 作为独立段落进入首条消息。
3. **对写入型工种，`relevant_files` 同时就是它能改的文件集**——审批和写通道是同一份清单，不存在「批准了 A 却能改 B」。
4. `verifier.command` 先过门禁（见下），再作为该 Worker `run_command` 的**唯一**可执行命令。
5. `max_attempts` 覆盖工种默认值（默认 3，上限 6）。

**Packet 的编译质量决定 Worker 成功率。** 主 Agent 的 `spawn_agent` 工具描述已写明：能给的事实先给，别让 Worker 自己找。

## Verifier 闭环

```text
Worker 干活 → 声称完成
  ↓
Runtime 直接执行 verifier 命令（不占模型轮次、不弹审批卡）
  ↓
退出码 == expected → 成功，报告带 <verifier … verdict="passed" />
退出码 != expected → 失败输出经确定性消化后回灌，attempt + 1
  ↓
attempt 打满 → 整个运行判失败，报告要求升档
```

四个要点：

- **「没跑过 verifier 的 done 不算 done」**。Worker 说完成，Runtime 照样跑一遍再判。这不是不信任模型，这是不把「自述」当证据。
- **每次 attempt 起一个干净的 Agent**。上一轮的死胡同不带进下一轮，带过去的只有消化后的失败输出——那是唯一值钱的部分。
- **失败输出的消化是纯 Rust 代码，不花模型**：抓含 error / failed / panicked / assertion 等标记的行及其上下文（前 1 行、后 3 行），再加尾部 20 行，整体截到 6 KB。**断言原文逐字保留，不许转述**。
- **打满 attempt = 运行失败**，不是「带着遗憾的成功」。父 Agent 会看到失败并被明确要求升档：`willdeep daemon retry-agent <id> --model <父模型>`，或者补齐 Packet 重新派工。

### Verifier 的门禁

诚实的 verifier 本来就会写：build 产出目标文件，测试写 fixture。所以门禁**不是「必须只读」**——一刀切只读会把用户推回「相信模型自称通过」，那比放行更危险。

两段门禁，与主 Agent 的 `run_command` 同一条链：

| 静态分类 | 处理 |
|---|---|
| `AlwaysSafe`（只读或有界工作区命令） | 直接放行 |
| `AlwaysDangerous`（破坏性形状） | **直接拒绝，不给判官**——模型不能靠改叫「verifier」把 `rm -rf` 说进来 |
| `NeedsJudgment` | 交 AI 判官（some.im 下是网关托管的 `someim-security-guard`）裁决 |

**唯一不可退让的是「不能没有门禁」**：子 Agent 没有审批 UI，判官没配或掉线时，答案是拒绝，不是放行。

## 写通道：文件集锁

`test_fixer` / `build_fixer` 必须能同时改测试和实现，`editor` 的单文件锁不够。做法是把「单文件」推广成「声明式文件集」，**同一条通路，不开第二套**：

1. **spawn 即审批**：审批对象是「允许子 Agent 修改这 N 个文件」，审批卡逐行列出全集合，一次批完。上限 8 个文件——想动一打文件的活不是有界局部修复，该由父 Agent 拆开。
2. **逐条落点校验**：`edit_file` / `create_file` 的路径必须在集合内，越界拒绝，并明确告诉 Worker「要扩大范围就报告父 Agent 重新派工」——给出路，不是给闭门羹。
3. **文件集互斥**：同一 Catalog 内两个运行中 Worker 的文件集有交集时，后者被拒绝并点名冲突文件。锁在运行结束、超时、取消、panic 时都会释放（`Drop`）。

`editor` 是集合大小为 1 的特例，走的是同一段代码。

两条限制值得先知道，免得派工时踩空：

- 集合里的路径必须是**已存在的文件**——Worker 不能凭空建新文件。要新建文件，由主 Agent 先建好空文件再派工，或者干脆自己写。
- `relevant_files` 同时承担「内联给你看」和「允许你改」两个角色。只想让 Worker 读、不想让它改的文件，别放进写入型工种的 `relevant_files`——放进去就是授权。

> **与 Xedit 设计的差异**：Xedit 设计里冲突的后来者是「排队」，rs 侧是「拒绝并点名冲突文件」。理由是并发上限本来只有 3，排队会引入等待与死锁面，而拒绝把决定权交回父 Agent——它比锁更清楚该等还是该重新切分。

## 确定性派工触发

露脸次数不能只靠主模型自觉。主 Agent 的 `run_command` 返回非零退出码、且命令形状匹配测试 / 构建特征（`cargo test`、`pytest`、`go build`、`tsc`、`make`、`xcodebuild` 等）时，工具结果尾部自动追加一段：

```text
<delegation-hint profile="test_fixer">
This failure is a good fit for the `test_fixer` worker. Spawn it with a task packet: …
</delegation-hint>
```

不强制、不自动派工——**决定权仍在父 Agent**，只是把派工成本压到一句话。子 Agent 永远不会收到这个提示：它不能派生，给它提示只是给一条它执行不了的指令。

## 配置

```toml
[subagents.test_fixer]
provider_profile = "some-im"
model = "glm-5"
max_turns = 8
context_window = 65536      # 4000 – 1000000
tool_output_limit = 6144    # 1024 – 131072 字节
max_attempts = 3            # 1 – 6
timeout_seconds = 900
worktree = "dedicated"      # 写入型工种默认专属 Worktree
```

工种名必须是上表之一，写错会在启动时报错而不是静默忽略。

## 分期与现状

| 阶段 | 内容 | 状态 |
|---|---|---|
| P0 观测/纪律 | 窗口分档、per-profile payload 上限 | ✅ 已落地 |
| P1 闭环 | Task Packet、Verifier 循环、文件集锁、`test_fixer` / `build_fixer` | ✅ 已落地 |
| P2 露脸 | 确定性派工触发、`log_inspector` / `git_detective` | ✅ 已落地 |
| P3 遥测 | 判定结构化落盘、三个北极星指标、Replay 锚点 | ✅ 已落地 |
| 后续 | Replay 回放工具、per-skill 路由表、verdict 回执上报 some.im | ⏳ 未做 |

## 遥测与指标

每次子 Agent 运行结束都发一条判定事件并落进 Runtime 的 agent 记录：

| 字段 | 含义 | 公开 API |
|---|---|---|
| `verifier_passed` | `true` 通过、`false` 未通过、**`None` 压根没有 verifier** | ✅ |
| `attempts` | 拿到判定前跑了几次 | ✅ |
| `repo_commit` | 运行开始时的 HEAD——有了它，一条记录就是一个可回放的 case | ✅ |
| `verifier_command` | 用哪条命令判的 | ❌ 只留在 Runtime 本地状态文件 |

**「没验证」是独立的第三种答案**，不是通过也不是失败。把它并进任何一边，都会让指标开始自我恭维——每份没验证过的报告都会凭空变成一次成功（或一次失败），而它两样都没挣到。命令原文不进公开记录：命令行会带路径和参数，而算指标根本不需要它。

```bash
willdeep daemon agent-metrics
```

```text
agents                     children=5   workers=4
skill_coverage             80.0%    (窄工种运行数 / 全部子 Agent 运行数；目标 ≥ 50%)
worker_verified_success    66.7%    (通过数 / 有 verifier 的运行数：2/3；目标 ≥ 85%)
escalation_rate            33.3%    (尝试打满、需要更大模型的比例；目标 ≤ 15%)
attempts_per_verified_run  2.00
unverified_runs            2        (没给 verifier，所以两边都没证明)
```

三条口径说明：

- **Skill Coverage** 的分母是全部子 Agent 运行数，`deep` 不算窄工种——它按设计就跑父模型，把它算成派工只会让这个数字自我美化。rs 侧没有「主模型内联轮次」的计数，所以这个口径比 Xedit 设计里的略宽，读的时候记住这一点。
- **Escalation Rate** 在 rs 侧的口径是「有 verifier 且尝试打满的运行占比」——即需要换更大模型才可能过的比例。rs 的升档是人工 `retry-agent --model`，没有自动升档记录可统计。
- **分母为 0 时打印 `-`，不打印 0%**。「什么都没验证」和「什么都没通过」是两件事，一个分不清它们的指标比没有指标更糟。

`willdeep daemon agent <id>` 也会多打一行 `verdict`，TUI 在子 Agent 结束时提示验证结果与尝试次数。

## 相关文档

- [子 Agent 与后台任务](SUBAGENTS.md)
- [审批与自动化](APPROVALS.md)
- [配置指南](CONFIGURATION.md)
