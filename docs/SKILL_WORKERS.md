# 小上下文 Skill Worker

> 与 macOS 版 Xedit 的 `docs/SMALL_MODEL_SKILL_WORKERS_DESIGN.md`（skill-workers.v1）同源。本文是 willdeep-rs 侧的落地说明与差异记录。

一句话：**优先把编码工作交给可私有部署的 32K / 48K / 64K / 256K 模型：窄工种用 Task Packet 喂料、用 Verifier 判结果，日常多文件实现交给 256K `implementer`；模型不自证，程序来证。**

## 为什么

三条纪律，顺序不能颠倒：

1. **越容易自动验证的任务，越可以大胆用弱模型**。`cargo test` 的退出码不会撒谎，模型的「我修好了」会。
2. **平均上下文要小**。max 64K 不等于 avg 64K；给小模型配一个 128K 的窗口，它照样会用 25 次 grep 把窗口烧穿。真正省钱的是**平均**上下文。
3. **相关文件由主 Agent 内联，Worker 不自己找**。Worker 每一轮搜索都在烧窗口；Task Packet 直接带文件内容，Worker 起手就是热的。

## 公开工种清单

| 工种 | 职能 | 工具 | 窗口 | Payload 上限 | 写通道 | Verifier |
|---|---|---|---:|---:|---|---|
| `reader` | 阅读并总结长文件 | read / list / search | 48K | 5 KB | 无 | — |
| `implementer` | 有界多文件功能、重构、新文件 | search / grep / list / read / create / edit / reviewed shell | 256K | 16 KB | **文件集锁** | 可选 |
| `tester` | 测试与行为审核 | search / grep / read / git / reviewed shell | 64K | 6 KB | 无 | 可选 |
| `ops_runner` | 有界运维与命令执行 | read / git status / reviewed shell | 48K | 5 KB | 无 | 可选 |
| `judge` | 独立正确性/安全审核 | search / grep / read / git diff，无 Shell | 48K | 5 KB | 无 | — |
| `deep` | 开放式跨文件调查 | search / grep / read / list / git status / reviewed shell | **继承会话窗口** | 默认 | 无 | — |

`scout`、`editor`、`test_fixer`、`build_fixer`、`log_inspector`、`git_detective`
等旧专门工种继续存在，并按旧 ID 参与自动路由、some.im 托管链、已保存工作流和
历史展示；只是它们不再挤在公开选择器里。公开展示归并关系由代码中的
`public_profile_id` 固定，运行时解析仍优先使用精确旧 ID。

Reviewed shell 的边界是“静态安全 → AI 判官 → 精确命令人类确认”，不是把 Shell
权限整包交给 Worker。危险形状、凭据和敏感路径不进入 AI；AI 拒绝/不可用时返回
原命令，父 Agent 只能用 `ops_runner + target_command` 对同一字符串申请一次性授权。

`deep` 是**刻意的例外**：它跑父模型，因为它的活本来就装不进小窗口。跨模块重构、架构设计、语义模糊的任务继续走 `deep` 或主 Agent，不要硬拆。

### 模型绑定：托管工种档（与 macOS 版 Xedit 同一张表）

Provider 是 some.im 时，七个工种各自跑网关托管的虚拟模型 `someim-32b-<工种>`
（连字符：`test_fixer` → `someim-32b-test-fixer`），职能提示词由网关 prepend，
**客户端不再发自己那份**——同一个工种有两份职能描述，等它们漂移的那天，
由模型来挑该听谁的。客户端仍然发边界段（看不到父会话、不能问用户、不能派生、
报告即返回值）：**服务端管职能，客户端管边界**。

| 层 | 归属 | 内容 |
|---|---|---|
| 职能段 | 服务端（`someim-32b-<工种>`） | 角色、方法、报告契约、诚实要求 |
| 边界段 | 客户端 | 无用户、无嵌套、写通道、工具协议 |
| Task Packet | 客户端 | 目标、事实、约束、相关文件、verifier |

`deep` 例外（跑父模型）。网关没托管的工种回落到基础廉价档（`glm-5`）并由客户端
发职能段——**一个工种绝不能落到「一份职能描述都没有」**。非 some.im 的 provider
行为不变。`[subagents.<工种>] model` 显式指定时以用户为准，并自动切回客户端职能段。

> 与 Xedit 的一致性由 `hosted_worker_model()` 的单元测试锁定：同一个操作者从
> CLI 和从 App 派同一个工种，必须落到同一个模型上。

## Task Packet

`spawn_agent` 的可选参数 `task`。**不传 `task` 时行为与以前逐字节相同**——自由文本 prompt 仍然有效。

```jsonc
{
  "profile": "test_fixer",
  "prompt": "修到绿，别动公开接口",
  "task": {
    "goal": "修复 subagent::tests::verifier_loop 失败",
    "read_files": ["crates/willdeep-core/src/subagent/runner.rs", "crates/willdeep-core/src/agent.rs"],
    "write_files": ["crates/willdeep-core/src/subagent/runner.rs"],
    "known_facts": ["失败始于 f936618", "断言是 attempts=3 实得 1"],
    "constraints": ["不改 SubagentProfile 的公开字段"],
    "verifier": { "command": "cargo test -p willdeep-core subagent", "expected_exit_code": 0 },
    "max_attempts": 3
  }
}
```

运行时处理：

1. `read_files` 与 `write_files` 都由 Runtime 读出并内联进 Worker 的第一条消息。内联预算 = 窗口 × 3/4 字节（32K 档 ≈ 24 KB，64K 档 ≈ 48 KB），超预算的文件**明确标注被省略**，不静默丢弃；读不到的文件标 `unreadable`，也不静默丢弃。
2. `known_facts` / `constraints` 作为独立段落进入首条消息。
3. **对写入型工种，只有 `write_files` 是它能改的文件集**——`read_files` 只给上下文，不扩大权限。审批和写通道使用同一份写清单，不存在「批准了 A 却能改 B」。旧 `relevant_files` 仅作兼容，并继续保留旧的读写合并语义。
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

`implementer` / `test_fixer` / `build_fixer` 必须能同时改多个实现与测试文件，`editor` 的单文件锁不够。做法是把「单文件」推广成「声明式文件集」，**同一条通路，不开第二套**：

1. **继承 Workspace 策略**：`smart/workspace-write` 下，声明文件集和主 Agent 的普通工作区编辑一样免除额外审批；`strict` 用一张审批卡列出全集合；`read-only` 一律拒绝。上限 16 个文件，覆盖一个有界功能切片，更多文件由父 Agent 拆分。
2. **逐条落点校验**：`edit_file` / `create_file` 的路径必须在集合内，越界拒绝，并明确告诉 Worker「要扩大范围就报告父 Agent 重新派工」——给出路，不是给闭门羹。
3. **文件集互斥**：同一 Catalog 内两个运行中 Worker 的文件集有交集时，后者被拒绝并点名冲突文件。锁在运行结束、超时、取消、panic 时都会释放（`Drop`）。

`editor` 是集合大小为 1 的特例，走的是同一段代码。

`read_files` 只承担「内联给你看」，`write_files` 承担「允许你改/创建」；不存在的写路径会作为待创建文件保留在白名单里。两者分开后，父 Agent 可以把接口、测试和相邻实现作为只读上下文，同时只授权真正需要修改的文件。

> **与 Xedit 设计的差异**：Xedit 设计里冲突的后来者是「排队」，rs 侧是「拒绝并点名冲突文件」。理由是并发上限本来只有 3，排队会引入等待与死锁面，而拒绝把决定权交回父 Agent——它比锁更清楚该等还是该重新切分。

## 只读工种的弱验证：引用抽查

`test_fixer` 有退出码，`scout` 没有。此前只读工种（`scout` / `reader` /
`log_inspector` / `git_detective`）的判定字段永远是 `None`——**永远「未验证」，
于是在指标里永远隐身，而隐身读起来就像没问题**。

它们的答案并非不可证伪：一个位置要么存在，要么不存在。运行结束后 Runtime 对
报告做一次确定性抽查（纯代码，不花模型、不占轮次）：

| 引用形态 | 核对方式 |
|---|---|
| `src/foo.rs` | 文件在不在 |
| `src/foo.rs:42` | 文件在不在 + 行号是否越过文件末尾 |
| 7–40 位十六进制 | `git cat-file -e <sha>^{commit}` 能不能解析 |

结果两处落地：报告尾部追加 `<citation-check checked="N" unverifiable="M">`
（点名对不上的那几条，父 Agent 直接看得见），判定事件带上 `claims_checked` /
`claims_unverifiable` 两个字段。`willdeep daemon agent-metrics` 同时报告
Worker/Standard/Deep 实际运行数、Deep Share 与 `citation_accuracy`。

三条边界，写清楚免得把这项当成它不是的东西：

1. **它只证伪「地名是编的」，不证明「答对了」**。一个 Worker 可以引用十个真实
   文件，同时完全答非所问。靶场里因此把「引用准确率」和「答对率」分成两列算。
2. **认不出来的 token 一律不计**。把解析器读不懂的词算成坏引用，指标就变成在
   量解析器，不是在量 Worker。
3. **`checked = 0` 不是满分**。什么都没引用的报告不进分子也不进分母——和
   「引用全对」是两件事，正如「没验证」不是「通过」。

## 确定性派工触发

露脸次数不能只靠主模型自觉。主 Agent 的 `run_command` 返回非零退出码、且命令形状匹配测试 / 构建特征（`cargo test`、`pytest`、`go build`、`tsc`、`make`、`xcodebuild` 等）时，工具结果尾部自动追加一段：

```text
<delegation-hint profile="test_fixer">
This failure is a good fit for the `test_fixer` worker. Spawn it with a task packet: …
</delegation-hint>
```

不强制、不自动派工——**决定权仍在父 Agent**，只是把派工成本压到一句话。子 Agent 永远不会收到这个提示：它不能派生，给它提示只是给一条它执行不了的指令。

> Xedit 侧的实测（2026-08-16，358 个会话侧车、25 340 条消息）：**只有 5 个会话
> 派过工，1.4%**。结论与 rs 侧的 0 派工记录一致——**光有提示不够，模型就是不派**。
> Xedit 的下一刀切在 workflow 引擎（把 fan-out 步骤直接绑工种）；rs 侧没有
> workflow 引擎，对应的抓手是 Goal Teams 与调度，尚未动。

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

配置可使用六个公开 ID 或仍受支持的内部兼容 ID；未知名字会在启动时报错而不是静默忽略。

## 与 macOS 版 Xedit 的对照（2026-08-16 盘点）

两边同源、同一个网关、同一批账号，因此**能一致的必须一致**；不一致的地方要写明
理由，而不是让它自然漂移。

| 项 | Xedit (1.263.0-rc1) | willdeep-rs (0.26.0-rc1) | 状态 |
|---|---|---|---|
| 托管工种模型 `someim-32b-<工种>` | ✅ 7 个 | ✅ 同一张表，单测锁定 | **已对齐** |
| 服务端职能段 / 客户端边界段分工 | ✅ | ✅ | **已对齐** |
| Task Packet / Verifier 闭环 / 文件集锁 | ✅ | ✅ | 已对齐 |
| `X-Playground-Session-ID` | ✅ | ✅ | **已对齐** |
| 命令型 Worker 的智能审核 | ✅ 静态规则 + AI + 父级精确人审 | ✅ 同语义 | **已对齐** |
| 确定性派工触发（测试/构建失败） | ✅ | ✅ | 已对齐 |
| 冲突文件集 | 排队 | 拒绝并点名 | 有意分歧（见上文） |
| 只读工种引用抽查 | ✅ 1.264.0-rc1 回流 | ✅ 本版新增 | **已对齐**（Xedit 侧第四个指标 Citation Accuracy） |
| 实弹靶场 | ✅ 1.264.0-rc1 回流（5 样本） | ✅ 12 样本 | **已对齐**（样本各按语言，判定纪律逐字一致） |
| 五个公开职责 | ✅ 调查 generalist / 实现 implementer / 验证 tester / 审查 reviewer / 运维 ops_runner | ✅ 同名单（`AgentWorkerRole`）；旧 ID 内部兼容 | **已对齐**（0.50.0-rc1 ↔ 1.312.0-rc1） |
| Worker 三档（基础/进阶/专家） | ✅ `WorkerTier`，与职责正交；专家档沿用 Deep 的票据与预算 | ✅ `AgentWorkerTier`，档位在设置里配 | **已对齐**；准入控制是 rs 独有 |
| Workflow 步骤绑工种 | ✅ 派工率第一刀 | ❌ 无 workflow 引擎 | 结构性差异，rs 侧对应抓手是 Goal Teams |
| 沙箱档（seatbelt / 凭证档） | ✅ | ❌ 无 seatbelt | 平台差异 |

## 分期与现状

| 阶段 | 内容 | 状态 |
|---|---|---|
| P0 观测/纪律 | 窗口分档、per-profile payload 上限 | ✅ 已落地 |
| P1 闭环 | Task Packet、Verifier 循环、文件集锁、`implementer` / `test_fixer` / `build_fixer` | ✅ 已落地 |
| P2 露脸 | 确定性派工触发、`log_inspector` / `git_detective` | ✅ 已落地 |
| P3 遥测 | 判定结构化落盘、三个北极星指标、Replay 锚点 | ✅ 已落地 |
| 后续 | Replay 回放工具、per-skill 路由表、verdict 回执上报 some.im | ⏳ 未做 |

## 实弹靶场

遥测只能算「已经派出去的工」的成绩。**「小模型到底修不修得动」这个问题，靶场
才能回答**——它现建十个带真实缺陷的 Cargo 仓库，用真 Provider 派工，用真
`cargo` 退出码判定。

```bash
ruby scripts/skill_worker_range.rb
ruby scripts/skill_worker_range.rb --model glm-5 --cases test_off_by_one,build_missing_mut
```

- 驱动器：`scripts/skill_worker_range.rb`，从 `~/.willdeep/config.toml` 取默认
  provider 的凭据，产出 `target/skill-worker-range/range.json` 与 `range.md`
  两份报告，外加每个样本的逐字记录 `traces/<样本>.log`。
- 靶子：`crates/willdeep-core/src/livefire.rs`，五个 `test_fixer` 样本（逻辑
  缺陷）＋五个 `build_fixer` 样本（编译错误）＋两个只读样本（`scout` 定位符号、
  `git_detective` 定位真凶），默认 `#[ignore]`，**不进 CI**：它要真凭据、
  要网络、要花钱。
- 每个可验证样本派工前先跑一遍 verifier 确认**是红的**。绿着的靶子测不出任何
  东西，报告里会点名 `seeded_red = false` 的无效样本。只读样本没有 verifier，
  衡量的是引用准确率与答对率两列——**分开算**，因为引用真实不等于答对。

两条判定纪律：

1. **成功 = verifier 通过 且 测试块逐字未改**。退出码只知道绿了，不知道绿得
   干不干净；把测试删掉也能变绿，而那是最省力的通关方式。靶场单独比对测试块
   原文，作弊的不进分子。
2. **逐字记录默认留存**。判定告诉你成没成，记录告诉你为什么——第一轮实弹里
   「跑满 8 轮、一个字没改」的失败，就是靠记录才看出来是 Runtime 拒了 Worker
   的正确补丁，而不是模型不会修。

### 常态化

一次快照回答「小模型行不行」，回答不了**「我这次改动让它变好了还是变坏了」**，
而后者才是回归。所以靶场跑完会自动把成绩归档进 `bench/skill-worker-range/`
（git 跟踪）：`history.jsonl` 每轮一行摘要，`runs/<时间戳>-<模型>.json` 存完整报告。
在此之前产出全落在 `target/` 里，被 `.gitignore` 第一行拦着，2026-08-16 那轮的
原始数据就是这么没的。

```bash
ruby scripts/skill_worker_range.rb        # 跑一轮，自动归档
ruby scripts/range_trend.rb               # 看趋势
ruby scripts/range_trend.rb --inject      # 把趋势写回 README 与本文档
```

摘要里最重要的字段是 `commit`：没有它，一行成绩就是无主的，涨跌归因不到任何
改动上。工作区不干净时脚本会警告——那轮成绩挂在一个没提交的状态上，回放不了。

**它仍然不进 PR 的 CI**：要真凭据、要网络、每轮都花钱。定时跑用 launchd
（模板见 [`scripts/launchd/`](../scripts/launchd/README.md)）或 cron，建议每周一轮，
并在跑之前 `git pull` 到要测的那版代码上。

三条比率的分母为 0 时存 `null` 不存 `0`，渲染成 `-` 与 `·` 而不是谷底——
这条纪律与 `agent-metrics` 的「分母为 0 打 `-`」同源，见下文「遥测与指标」。

### 趋势

下面这段由 `ruby scripts/range_trend.rb --inject` 生成，别手改。

<!-- range:begin -->
最近一轮：**2026-08-24T03:21:24Z** · 模型 `glm-5` · 代码 `7e4bb4e`

| 指标 | 最近一轮 | 对比上轮 | 趋势 |
|---|---|---|---|
| **Worker Verified Success** | 100% | ±0 | `▄▄` |
| 只读工种引用准确率 | 100% | ±0 | `▄▄` |
| 只读工种答对率 | 100% | ±0 | `▄▄` |

样本 16（可验证 13 · 只读 3） · 平均 5318 token/样本 · 12 秒/样本 · 作弊 0

<details><summary>历史 2 轮</summary>

| 时间 | 代码 | 模型 | 样本 | Verified Success | 作弊 | 引用准确率 | 答对率 | 平均尝试 |
|---|---|---|---:|---|---:|---|---|---|
| 2026-08-24T03:21:24Z | `7e4bb4e` | `glm-5` | 16 | 100% | 0 | 100% | 100% | 1.00 |
| 2026-08-24T03:07:01Z | `2af4f76` | `glm-5` | 12 | 100% | 0 | 100% | 100% | 1.00 |

</details>
<!-- range:end -->

### 首轮实测（2026-08-16，`glm-5`，12 样本 · 手抄留档）

> 这张表是归档机制上线之前手抄进文档的，原始数据已随 `target/` 丢失，**不进
> 趋势曲线**。留着是因为它是这条谱系上第一个真实数据点；下一轮跑完，上面的
> 「趋势」会自动接管。

| 指标 | 结果 |
|---|---|
| Worker Verified Success（10 个可验证样本） | **10/10（100%）** |
| 平均尝试次数 | 1.00（没有一个样本用到第二次） |
| 作弊（verifier 绿但改了测试） | 0 |
| 只读工种引用准确率 | **4/4（100%）** |
| 只读工种答对率 | **2/2** |
| 平均单样本 token | ≈ 5 100（总 61 732） |
| 平均单样本耗时 | ≈ 19 秒（总 227 秒） |

**这组数字支持的结论只有一条**：形状有界、失败可由退出码判定的局部修复
（少写 `mut`、类型不符、非穷尽 match、move 后再用、漏 import、off-by-one、
比较符写反、空输入 panic、漏闰年规则、空白折叠），以及有界的定位类问题
（多模块里找符号、三个 commit 的历史里找真凶），交给 32K/64K 窗口的 `glm-5`
是够用的，且**一次就过**。

它**不支持**的结论：小模型能干跨文件重构、能定语义模糊的需求、能替代 `deep`
或主 Agent。样本是十几个几十行的独立小仓库，是这条谱系上最容易的一端。要把
结论往难处推，先往靶场里加难样本，别往 PPT 里加形容词。

靶场至今打出的三个真问题（都已修）：

- 审批过的写入目标没做规范化：workspace 路径里只要有一层符号链接
  （macOS 的 `/var`、`/tmp`、软链的检出目录），Worker 发出的正确补丁会被判成
  「越界写入」，且拒绝信息里点的就是它刚请求的那个路径。修复前 `glm-5` 在
  `build_missing_mut` 上跑满 8 轮、烧 1.4 万 token、一个字没改；修复后同一样本
  1 次尝试通过、4.7k token、13.9 秒。
- verifier allowlist 的拒绝信息不说「那什么才行」。Worker 的典型近失是给验证
  命令加装饰（`cargo build 2>&1`），每猜一次就是一轮。现在拒绝信息直接引用
  允许的原文。
- `git_detective` 用手上的工具**答不了它得名的那个问题**：`git_diff` 不接受
  revision 参数，四个固定 git 工具没法比较两个 commit。实测跑满 8 轮、9.4k
  token、零结论。现在它拿到只读 git shell（形状门禁），同一样本 1 轮答对。

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
