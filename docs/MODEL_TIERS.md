# 模型上下文三档切分（model-tiers.v1）

> 2026-08-16 定稿。与 macOS 版 Xedit 共用同一套档位语义；rs 侧实现状态见文末。
> 关联：`docs/SKILL_WORKERS.md`（S 档的完整落地）、Xedit `docs/SMALL_MODEL_SKILL_WORKERS_DESIGN.md`。

## 动机：不只是省钱，是主权

Skill Worker 体系立项时的叙事是成本：小模型便宜，能下放就下放。这个叙事没错，
但它不是最重要的那个。

**最重要的场景是：模型不出国、不上公网。** 很多公司（科研院所、金融、政务、
制造）的数据不允许离开机房，能部署的只有开源权重 + 自有 GPU。这个约束下的
现实是：

- **32K–64K 档**：单卡可跑，选择极多，私有化最容易。
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
| **S（worker）** | 32K–64K | 可验证的有界任务：修测试、修编译、定位、日志解释、模板生成、命令编排 | 单卡，容易 | `someim-32b-<工种>`（已建成） |
| **M（standard）** | ~256K | **会话默认档**：日常编码循环、单模块开发、评审、文档、单章创作 | 多卡，可行 | 会话主模型（`someim-auto-flash` 托管路由）；显式选 M 用 `glm-5.x` / `kimi-k3` |
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
  ↓ 能拆出可验证的有界子任务吗？ → 是：S 档 worker（spawn_agent + task packet）
  ↓ 剩下的部分材料装得进 256K 吗？ → 是：M 档（会话默认）
  ↓ 只有材料真实超过 M 且不可消化/分片时 → L 档，且要说明理由
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

S 档不是新东西——`SKILL_WORKERS.md` 的八个工种就是 S 档的执行器。本文档补的
是上面两层：M 档作为默认、L 档作为申请制稀缺资源，以及 **skill → tier 的显式
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
| L / M 档模型绑定 | ✅ 用现有网关模型：L=`deepseek-v4-flash`、M=`glm-5.x`/`kimi-k3`（均实测在线），不建新虚拟模型。派 L 档任务即 `spawn_agent` 时经 `[subagents.deep] model = "deepseek-v4-flash"` 绑定，或会话内直接换模型 |
| worker-tier 技能的自动派工触发 | ⏳ 未做——现阶段靠提示词；确定性触发（如 list_skills 命中 worker 技能时附派工配方）是下一步 |
| air-gapped 降级的自动化（分片 + 归纳） | ⏳ 未做，先靠 scout/reader 手动编排 |
