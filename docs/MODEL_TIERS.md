# 模型上下文三档切分（model-tiers.v1）

> 2026-08-16 定稿，2026-08-19 将路由与 Deep 准入从提示词升级为 Runtime 强制策略，2026-08-31 引入与职责正交的 Worker 三档（同步 Xedit 1.312.0-rc1）。与 macOS 版 Xedit 共用同一套档位语义；rs 侧实现状态见文末。
> 关联：`docs/SKILL_WORKERS.md`（S 档的完整落地）、Xedit `docs/SMALL_MODEL_SKILL_WORKERS_DESIGN.md`。

## 动机：不只是省钱，是主权

Skill Worker 体系立项时的叙事是成本：小模型便宜，能下放就下放。这个叙事没错，
但它不是最重要的那个。

**最重要的场景是：模型不出国、不上公网。** 很多公司（科研院所、金融、政务、
制造）的数据不允许离开机房，能部署的只有开源权重 + 自有 GPU。这个约束下的
现实是：

- **32K–64K 档**：单卡可跑，选择极多，私有化最容易。目标部署基线中，16GB 显存可承载 35B-A3B 级模型并覆盖 32K，按 KV cache 与量化配置可选择 48K/64K，因此不再保留 16K 档。
- **128K–256K 档**：开源权重的主流窗口（DeepSeek、Qwen、GLM、Kimi 开源版
  都在这个区间），多卡可部署，选择很多。
- **1M 档**：私有化选择极少。长上下文推理的 KV cache 内存开销巨大，就算权重
  开源，多数机房也供不起。

所以结论倒过来了：**体系必须在「只有 S+M」的机房里完整可用；L 档是加速器，
不是地基。** 一个默认假设 1M 窗口的工作流，在私有化那天就会停摆——而那正是
客户付钱的那天。省钱是这套切分的副产品，可部署性才是设计目标。

## 两个「档」，别混

2026-08-31 起本文里有两套三档，它们回答的不是同一个问题：

| | 部署基线（S/M/L） | Worker 档位（基础/进阶/专家） |
|---|---|---|
| 回答什么 | **这套体系在什么机房跑得起来** | **这次派工给多贵的模型、多少预算** |
| 维度 | 上下文窗口段 | 模型能力 + 预算配额 |
| 定义在 | 本文下一节 | `crates/willdeep-core/src/worker_tier.rs` |
| 谁在用 | 私有化评估、技能 `tier:` 标注 | `spawn_agent.worker_tier`、路由准入 |

两者会对不上是正常的：进阶档给 256K 预算，不代表它背后的模型只有 256K 物理
窗口（`deepseek-v4-flash` 就是 1M 窗口按 256K 预算用）。**预算是我们给它的额度，
窗口是它能吃下的极限。**

### Worker 档位（与 macOS 版 Xedit `AgentWorkerTier` 同一份契约）

| 档 | 预算 | 默认 some.im 模型 | 准入 |
|---|---|---|---|
| **基础 standard** | 128K | `someim-32b` | 无，默认档 |
| **进阶 advanced** | 256K | `deepseek-v4-flash` | 无 |
| **专家 expert** | 会话窗口 | `gpt-5.6-sol` | **票据 + 每 Harness 预算** |

「默认」两个字是字面意思：这张表是**没写配置时**的解析结果，用
`[worker_tiers.<档>]` 可以逐档换模型甚至换端点（专家档走 Anthropic 的
`opus-5`、其余仍走网关，是最常见的一种配法）。表的价值在于两个客户端默认
一致——同一个人换个 App 打开不该换个 Worker——而不在于锁死。配法见
[子 Agent 与后台任务](SUBAGENTS.md#换掉某一档的模型)。

**职责与档位正交**。以前一个工种既定职责又定模型（`someim-32b-<工种>` 一个萝卜
一个坑），于是加一种职责就要在网关上多铺一条链，换个模型又得改工种。现在职责
只管提示词、工具和写入边界，模型档位单独选：五个公开职责 × 三档。

`deep` 这个名字当年同时表示「复杂调查」这个职责和「用最贵的模型」这个档位。
拆开之后，职责归 `generalist`，价格归 `expert`；两处都保留 `deep` 作为别名。

### 部署基线（S/M/L）

| 档 | 窗口 | 角色 | 私有化难度 |
|---|---|---|---|
| **S（worker）** | 32K / 48K / 64K | 有界任务：修测试、修编译、定位、日志解释、单文件编辑、模板生成 | 单卡，容易 |
| **M（standard）** | 256K | **日常编码主力**：`implementer` 承担最多 16 个声明文件的功能、重构、新文件、评审与文档 | 多卡，可行 |
| **L（deep）** | 1M | 跨模块重构、全库理解、超长材料（大日志全量、多文档综述、长篇规划） | 极难，多数场景没有 |

> **一处必须说明的张力**：上面「主权」那一节论证 S 档存在的理由是「16GB 显存
> 单卡可承载 35B-A3B 级模型并覆盖 32K」。而 Worker 的基础档预算是 128K——
> 128K 的 KV cache 比 32K 大一个量级，同一张卡装不下。
>
> 这两件事并不矛盾，但**必须分开读**：S/M/L 是**私有化评估用的窗口段**，回答
> 「这台机器能不能跑」；基础/进阶/专家是**托管环境下的默认配置**，回答「不特别
> 指定时给多少」。真正进 air-gapped 机房时，Worker 档位的预算要按那台机器的
> S 档重新配（`[subagents.*] context_window` 显式覆盖），不能照抄托管默认值。
> 谁把 128K 当成私有化基线，谁就会在采购单上少算一位数。

**不建新的档位虚拟模型**。网关现有模型已经覆盖三档——专家档直接用
`gpt-5.6-sol`，进阶用 `deepseek-v4-flash`，基础用 `someim-32b`——再造一层
`someim-1m` 只会多一个要维护的名字。

**七个 `someim-32b-<工种>` 已经退役**（0.50.0-rc1）。它们存在的理由是
**服务端托管的职能提示词**，而职责提示词现在由客户端随请求发送：两边都做就是
双重注入，而且每加一种职责都得先在网关上铺一条链才能发版。旧名在请求边界归一
到 `someim-32b`。`someim-32b-compressor`（上下文压缩）与
`someim-32b-security-guard`（命令安全裁判）不在此列——它们不是工种别名，是各自
独立的职能模型。

两条派工维度，先后有序：

1. **可验证性**（能不能不信模型自述）——决定能否下放到 S。这是
   `SKILL_WORKERS.md` 的第一纪律，靶场已用数字验证（glm-5 十发十中）。
2. **材料量**（真实需要多少上下文）——决定要不要上 L。注意是**真实需要**：
   一条 500 行的失败日志经确定性消化后 6 KB 就够，不构成上 L 的理由。

## 路由纪律：默认 M，向下优先，向上申请

以前的默认是「主模型全包，偶尔下沉」。现在倒转：

```text
任务进来
  ↓ 能拆成 32K/48K/64K 的窄任务吗？ → 是：S 档 worker
  ↓ 能声明目标、事实和不超过 16 个写入路径吗？ → 是：256K implementer
  ↓ 先消化或分片，仍无法装进 256K 吗？ → 是：L/deep，且要说明理由
```

**L 是稀缺资源，用它要有理由**（像内存分配一样）。三种真实理由：
全库级重构的一致性推理、无法分片的超长单体材料、跨几十个文档的综述合成。
「懒得拆」不是理由。

### L 档缺席时的降级（air-gapped 模式）

私有化机房大概率没有 L。三条降级路径，按序尝试：

1. **消化**：确定性预处理（失败聚焦、尾部截取、结构抽取）把材料压进 M——
   Verifier 闭环的失败消化就是现成先例。
2. **分片 + 归纳**：S/M worker 并行读片段出结构化摘要，M 档合成
   （map-reduce）。`scout`/`reader`/`log_inspector` 就是为此存在的。
3. **降精度声明**：实在装不下就明说「本结论基于抽样/分片，未见全量」——
   诚实的部分答案好过装作看过全量的完整答案。

## 技能库审计（2026-08-16，29 个技能逐个过）

标注已写进各技能 `SKILL.md` 的 `tier:` 字段，`list_skills` 会带出来。
判定依据永远是上面两条维度，不是拍脑袋。

### S 档（worker，11 个）——可验证或强模板化，起手材料小

| 技能 | 验证方式 |
|---|---|
| `image-processing` | 命令退出码 + 产物文件存在/尺寸 |
| `pdf-design` | 命令退出码 + 产物页数/存在 |
| `cdnproxy-niuwoai` | URL 改写规则机械，curl 探活可验证 |
| `harbor-prebuilt-image` | 构建退出码 + 远端 digest 比对 |
| `willdeep-debug-package` | 构建退出码 + bundle 校验步骤内置 |
| `k8s-manifest-generator` | `kubectl apply --dry-run` / kubeconform |
| `helm-chart-scaffolding` | `helm lint` / `helm template` |
| `k8s-tokenhub-deploy-migration-check` | 确定性比对，输出就是清单 |
| `pinchtab` | 每步浏览器状态回读即验证 |
| `musemail-light-palettes` | 查表式设计 token 参考 |
| `someim-macos-auto-update` | appcast 校验命令可验证 |

### M 档（standard，14 个）——需要判断或中等材料，装得进 256K

`build-test-acceleration`、`swiftui-hang-diagnose`（采样判读要经验，但日志可先
经 S 档消化）、`mac-app-docs-site`、`ppt-story-architect`、`anti-ai-fiction-voice`
（局部改写）、`hongguo-drama`（单集剧本）、`k8s-security-policies`（生成可模板、
安全审查要判断）、以及六个部署编排类：`tokenhub-k8s-rollout`、`tokenhub-release`、
`muchtoken-deploy`、`release-macos-app`、`go-react-embed-deploy`、`gitops-workflow`、
`singbox-clash-rotation`。

部署类特别说明：**执行段可下放、决策段不下放**。「重试并盯梢」可以交给
worker（Xedit 的 `target_command` 单命令授权就是这条通路），失败诊断、回滚、
一切不可逆决策留在 M 档主会话。这与 Xedit 设计 §10.5 的边界一致。

### L 档（deep，4 个）——长材料或全局一致性

`female-web-novel`、`male-web-novel`、`sci-fi-novelist`（长篇规划：几十万字的
伏笔一致性是真实的长上下文需求，且**无 verifier 可依，不满足下放第一纪律**）、
`goal-teams`（多代理编排本身）、以及未来的全库审计/跨模块重构类技能。

创作类补充：单章续写实际是 M 档任务（前文摘要 + 卷纲装得进 256K）。标 L 是
按「完整使用该技能的最重场景」标的；派工时如果只做单章，主会话可自行降档。

## 与 Skill Worker 体系的关系

S 档不是新东西——`SKILL_WORKERS.md` 的窄工种就是 S 档执行器；新增的
`implementer` 是 M 档执行器。本文档补的是上面两层：M 档作为日常编码默认、L 档作为申请制稀缺资源，以及 **skill → tier 的显式
关联**（`tier:` frontmatter），让派工决策在读技能正文之前就能做。

worker-tier 技能的标准执行路径：主会话从 4KB 路由索引或 `list_skills` 读到
`tier=worker` → 编 Task Packet（goal / read_files / write_files / verifier=技能自带的
验证命令）→ `spawn_agent` 派给对应工种 → Runtime 将技能正文内联并跑 verifier
判定。完整技能库和正文都不再常驻主会话窗口。

## 实现状态（rs 侧）

| 项 | 状态 |
|---|---|
| `tier:` frontmatter 解析（worker/standard/deep，容错别名 small/medium/large） | ✅ 0.27.0-rc1 |
| `list_skills` / 技能摘要带 tier | ✅ 0.27.0-rc1 |
| 系统提示词教会主会话 tier 路由 | ✅ 0.27.0-rc1；0.37.0-rc1 起只是补充说明，真实准入由 Runtime 决定 |
| 技能库 29 个逐个标注 | ✅ 2026-08-16（写入 `~/.willdeep/skills`） |
| 16K 档退出 | ✅ 0.33.0-rc1——最低档提升到 32K，reader/editor/build_fixer 使用 48K，test_fixer 使用 64K |
| 256K 通用编码执行器 | ✅ 0.33.0-rc1——`implementer` 支持有界多文件创建/编辑、可选 verifier、最多 16 个声明路径和独立 Worktree |
| 写入权限继承 | ✅ 0.33.0-rc1——`smart/workspace-write` 的 Worker 继承工作区写权限免额外审批；`strict` 仍审批，`read-only` 仍拒绝 |
| L / M 档模型绑定 | ✅ 0.37.0-rc1 默认 Root=`glm-5`，七个窄工种自动命中 `someim-32b-<trade>`，L=`deepseek-v4-flash`；显式配置仍可覆盖 |
| Worker 三档与职责正交 | ✅ 0.50.0-rc1——`WorkerTier` 基础/进阶/专家独立于五个公开职责；`spawn_agent.worker_tier` 单独选档，专家档沿用原 Deep 的票据与预算；七个 `someim-32b-<trade>` 收敛为请求边界兼容映射 |
| 档位模型可配置 | ✅ 0.52.0-rc1——`[worker_tiers.<档>]` 逐档绑定 provider/model/窗口；在此之前上表只是文档，运行时并未查过它（进阶档不换模型、专家档默默用会话自己的模型）。同一改动修好换模型后职责提示词两边都不发的空壳 Worker |
| 档位进入路由设置界面 | ✅ 0.53.0-rc1——TUI `/routing` 与 Web 设置矩阵在职责表下方增加档位表，读写同一份 `[worker_tiers.*]`，专家档标注「需票据」。顺带修好 Web 面板自 0.51 改名后把 `generalist`/`reviewer` 显示成「—」 |
| macOS 端读同一份配置 | ✅ Xedit 1.314.0-rc1——App 的三档绑定改为优先读 `[worker_tiers.*]`，只读不回写；文件指定了本机没有的 Provider 时该档标记不可用，而不是回落父端点 |
| 确定性请求路由 | ✅ 0.37.0-rc1——定位/阅读/日志/Git 追溯自动 S 档预检，测试/构建/实现写入结构化路由事件，Goal 信封不污染当前请求分类 |
| L 档升级票据与预算 | ✅ 0.37.0-rc1——必须携带低档尝试、上下文证据与不可拆分原因；Runtime 交叉验证本 Harness 观测并限制调用次数 |
| 固定上下文税 | ✅ 0.37.0-rc1——技能路由摘要上限 4KB，MCP Schema 改为搜索后按需加载 |
| 模型路由设置界面 | ✅ 0.38.0-rc1——TUI `/routing` 与 Web 设置矩阵共同原子更新 `config.toml`，支持推荐默认、显式覆盖、上下文档位、Deep 预算和并发修改冲突检测 |
| worker-tier 技能的确定性派工触发 | ✅ 0.28.0-rc1——`list_skills` 命中 worker 技能时结果尾部附 `<delegation-hint>` 派工配方（仅主 Agent；子 Agent 不能派生，给它提示只是噪音）。配套 `task.skill`：Runtime 把技能正文内联进 Worker 首条消息，Worker 不再需要自己取指令 |
| air-gapped 降级的自动化（分片 + 归纳） | ✅ 0.28.0-rc1——`task.digest_oversized`：超过内联预算的材料由 Worker 自己的廉价模型分片消化（标识符/断言原文保留，逐块标注 digested，失败的块点名不静默），默认关闭因为它花模型调用，开关在派工者手里 |
