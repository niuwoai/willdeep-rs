# 模型上下文三档切分（model-tiers.v1）

> 2026-08-16 定稿，2026-08-17 按企业私有部署目标取消 16K 并引入 256K `implementer`。与 macOS 版 Xedit 共用同一套档位语义；rs 侧实现状态见文末。
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

## 三档定义

| 档 | 窗口 | 角色 | 私有化难度 | some.im 模型（2026-08-16 全部实测在线） |
|---|---|---|---|---|
| **S（worker）** | 32K / 48K / 64K | 有界任务：修测试、修编译、定位、日志解释、单文件编辑、模板生成 | 单卡，容易 | `someim-32b-<工种>`（已建成） |
| **M（standard）** | 256K | **日常编码主力**：`implementer` 承担最多 16 个声明文件的功能、重构、新文件、评审与文档 | 多卡，可行 | 默认 `glm-5`，企业部署绑定本地/内网 256K Provider |
| **L（deep）** | 1M | 跨模块重构、全库理解、超长材料（大日志全量、多文档综述、长篇规划） | 极难，多数场景没有 | `deepseek-v4-flash` |

**不建新的档位虚拟模型**。网关现有模型已经覆盖三档——L 直接用
`deepseek-v4-flash`（1M 窗口），M 用 `glm-5.x` / `kimi-k3` 这一窗口段——
再造一层 `someim-1m` 只会多一个要维护的名字。`someim-32b-<工种>` 是例外，
它存在的理由不是窗口而是**服务端托管的职能提示词**；档位本身不需要这层。

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

worker-tier 技能的标准执行路径：主会话读到 `tier=worker` → 编 Task Packet
（goal / 相关文件 / verifier=技能自带的验证命令）→ `spawn_agent` 派给对应工种
→ Runtime 跑 verifier 判定。技能正文由 worker 用 `read_skill` 自取，不占主会话
窗口。

## 实现状态（rs 侧）

| 项 | 状态 |
|---|---|
| `tier:` frontmatter 解析（worker/standard/deep，容错别名 small/medium/large） | ✅ 0.27.0-rc1 |
| `list_skills` / 技能摘要带 tier | ✅ 0.27.0-rc1 |
| 系统提示词教会主会话 tier 路由 | ✅ 0.27.0-rc1 |
| 技能库 29 个逐个标注 | ✅ 2026-08-16（写入 `~/.willdeep/skills`） |
| 16K 档退出 | ✅ 0.33.0-rc1——最低档提升到 32K，reader/editor/build_fixer 使用 48K，test_fixer 使用 64K |
| 256K 通用编码执行器 | ✅ 0.33.0-rc1——`implementer` 支持有界多文件创建/编辑、可选 verifier、最多 16 个声明路径和独立 Worktree |
| 写入权限继承 | ✅ 0.33.0-rc1——`smart/workspace-write` 的 Worker 继承工作区写权限免额外审批；`strict` 仍审批，`read-only` 仍拒绝 |
| L / M 档模型绑定 | ✅ 用现有网关模型：L=`deepseek-v4-flash`、M=`glm-5.x`/`kimi-k3`（均实测在线），不建新虚拟模型。派 L 档任务即 `spawn_agent` 时经 `[subagents.deep] model = "deepseek-v4-flash"` 绑定，或会话内直接换模型 |
| worker-tier 技能的确定性派工触发 | ✅ 0.28.0-rc1——`list_skills` 命中 worker 技能时结果尾部附 `<delegation-hint>` 派工配方（仅主 Agent；子 Agent 不能派生，给它提示只是噪音）。配套 `task.skill`：Runtime 把技能正文内联进 Worker 首条消息，Worker 不再需要自己取指令 |
| air-gapped 降级的自动化（分片 + 归纳） | ✅ 0.28.0-rc1——`task.digest_oversized`：超过内联预算的材料由 Worker 自己的廉价模型分片消化（标识符/断言原文保留，逐块标注 digested，失败的块点名不静默），默认关闭因为它花模型调用，开关在派工者手里 |
