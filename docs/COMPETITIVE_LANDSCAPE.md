# 竞争力分析：WillDeep vs pi / Claude Code / Codex / deepseek-harness

> 分析日期：2026-08-21 | 基于 develop @ 0.39.0-rc1（代码实况盘点）+ 当日公开情报调研。
> 竞品数据时效标注在各节；标注「自报」的分数未经第三方榜单验证。
> 结论供产品与路线图决策参考；路线图落地仍以 `CLI_TUI_RUNTIME_ROADMAP.md` 为准。

## 一句话结论

在「大众终端 coding agent」赛道 WillDeep 没有位置，也不应去抢；在「私有化 / 主权 /
异构小模型编排」利基上，WillDeep 有四家竞品都没有的结构性差异化。关于业界的
「短系统提示词」趋势：WillDeep 已经在短提示词阵营（核心 ~1K token，实测总
~2.6K token），无需再为此改造。

## 自身现状（代码实况，非文档愿景）

- 60,097 行 Rust / 4 crate（core 16.4K、cli 40.8K、runtime-protocol 1.8K、
  runtime-client 1.1K），另有 React Web 前端 ~1.2K 行 TS/TSX。
- 常驻 21 个工具 + MCP 按需 2 个（`list_mcp_tools` / `call_mcp_tool`）；
  CLI / TUI / daemon / Web / mobile 五形态真实存在，daemon 统一 API 50+ 操作、
  SSE 断点续传、Unix socket / Named Pipe、幂等去重。
- 6 个公开子代理工种（旧专门 ID 内部兼容，32K–1M 多档窗口）；写入型默认专属 worktree + 前置写集审批；
  verifier 由 Runtime 亲自执行、退出码裁决，worker 不自证。
- small-model-first 路由自 0.37.0-rc1 起是 Runtime 强制策略（`routing.rs`），
  deep 档申请制升级票据 + 运行时观测交叉验证 + 每 Harness 调用预算。
- 上下文压缩下沉网关托管 `someim-32b-compressor`（0.39.0-rc1），本地保留两层
  fallback；固定上下文税已治理（技能索引 4KB 封顶、MCP schema 按需）。
- 命令安全两级：静态规则（`safety.rs`，危险形状永不送 judge）→ AI judge
  （some.im 托管 `someim-security-guard`）；审批四档 + 持久 Always Allow。
- 系统提示词：`STABLE_CONTRACT`（`prompt.rs:5`）4,096 字符 ≈ 1K token；
  叠加规则文件与技能索引后本仓库实测约 2.6K token。
- 测试 444 个，绝大多数为桩 Provider 的编排层测试；真模型「实弹靶场」
  （`livefire.rs`）默认 ignore，不在 CI。

## 竞品横向对比（信息时效 2026-08-21）

| 维度 | WillDeep | pi | Claude Code | Codex CLI | deepseek-harness (dsh) |
|---|---|---|---|---|---|
| 系统提示词量级 | 核心 ~1K token，实测总 ~2.6K | <1K token | 核心 ~10.5K，起步实发 ~33K | ~2–4K，实发 ~13K | minimal 模式一句话 |
| 工具数 | 21+2 | 4（read/write/edit/bash） | 27+ | 中等 | minimal 仅 2 |
| 模型路由 | S/M/L 三档 Runtime 强制 + deep 申请制 | 手动换模型（15+ 供应商） | 手选 + 子代理可指定 | 手选 | 手选 |
| 子代理 | 6 个公开工种 + 内部专门路由 + verifier 闭环 + worktree 隔离 | 拒绝黑盒，bash 里 spawn 子进程 | agent teams、后台 agents | 较弱 | 插件化提供（含 Codex/CC 子代理插件） |
| OS 级沙箱 | 无（审批 + 静态规则 + AI judge） | 无（外包 Docker/micro-VM） | 有 | 有（Seatbelt/Landlock 三档） | 有（bwrap/Landlock/Seatbelt） |
| Hooks / Checkpoint | 无 / 无（diff review + revert 替代） | 扩展系统 / 无 | 有 / 有（/rewind） | 部分 | 插件化 |
| MCP | stdio client，schema 按需 | 明确拒绝 | 全套 | 支持 | 依赖在、默认不启用 |
| 开源/生态 | 个人项目 | MIT，~94.8k stars | 闭源，商业功能面最全 | Apache-2.0，~110k stars | MIT，2026-08-13 发布，~178k stars |

行业背景两条：

1. **Harness 效应**：同一模型换 harness 在 Terminal-Bench 上可差约 16 个百分点，
   业界共识 harness 贡献不小于换模型。
2. **dsh 是最大的新变量**：发布 8 天 178k stars，「everything is a plugin」
   （Cordis 组件系统），minimal 模式（一句话提示词 + 2 工具）即 DeepSeek 官方
   评测配置，配 V4-Pro 自报 TB 2.1 = 87.9（自报）。企业可能拿它自建私有 agent。

## 护城河（四家都没有的）

1. **Runtime 级三档路由 + deep 申请制**。竞品全部是单模型哲学或用户手选；
   WillDeep 的「默认 M、向下派工优先、向上要票据」围绕「模型不出国、机房只有
   S+M」设计（见 `MODEL_TIERS.md`），这是 air-gapped 客户付费的那个场景。
2. **Verifier 闭环纪律**。退出码唯一裁决、worker 不自证、失败输出消化回灌——
   对小模型幻觉的结构性防御，无一家做成硬约束。
3. **托管职能提示词**。`someim-32b-<trade>` / compressor 的提示词在服务端注入，
   客户端只发裸转录——比 pi 更激进的短提示词实践（提示词不走客户端），
   并吃满服务端缓存。
4. **双端体系**：与 macOS Swift 版共用会话目录、webhook schema、工种模型映射。

## 短板（按对竞争力的伤害排序）

1. **无 OS 级沙箱**。对私有化企业客户（金融、政务）是准入项缺失而非加分项
   缺失；Codex（Seatbelt/Landlock）与 dsh（bwrap）均已内置。→ 第一优先补课。
2. **无 hooks**。企业接审计 / 合规 / CI 门禁的标准集成点；当前只有通知 webhook。
3. **MCP 仅 stdio**，无 Streamable HTTP / OAuth（已在已知问题清单）。
4. **无 plan/todo 工具、无 checkpoint**。diff review + revert 是合格替代，
   但长任务可观测性弱于 Claude Code 的 `/rewind` + todo。
5. **路由分类是中英关键词表**（`routing.rs`）：日语等语言直接落 standard 兜底，
   安全但浪费三档体系；分类规则仅 5 个测试。
6. 测试厚在编排层；实弹靶场不在 CI，模型行为层回归缺位。

## 「短提示词路线」判定

- 支持证据（强）：pi（<1K token + 4 工具）、dsh minimal（一句话 + 2 工具）、
  Terminal-Bench 官方 Terminus 2（tmux-only）三方独立收敛；机制解释是前沿模型
  已被 RL 后训练内化 agent 行为，长提示词在与真实工作内容抢上下文。
- 关键限定：**短提示词成立的前提是前沿模型**。pi 的成绩配 Opus 4.8，dsh minimal
  配 V4-Pro。小模型未被灌够 agent 行为，裸奔提示词可能塌。
- 对 WillDeep 的映射：体系建立在 32B 小模型上，而现有解法已经答对——小模型要的
  不是长提示词，而是**窄任务 + 托管职能提示词 + 硬验证**。这正是业界尚无系统
  实验的「L 档极简、S 档脚手架」分层，WillDeep 是少数已落进代码的。
- 决策：**不再压缩** `STABLE_CONTRACT`。其中派工契约（Task Packet 教学）服务的
  Root 是 GLM-5 而非前沿模型，且派工是核心差异化行为；2.6K token 固定税不构成
  成本问题（33K 才构成）。不为短而短。

## 总评与优先级建议

竞争力评级：大众市场 ★★☆（不参战）；私有化/主权利基 ★★★★（真差异化，
但沙箱与 hooks 不补齐，企业安全评审过不去）。

建议优先级（只排序，不承诺排期）：

1. OS 级沙箱（macOS Seatbelt / Linux Landlock，对齐 Codex 三档模型）；
2. Hooks（审计/合规/CI 集成点，复用现有 webhook schema 经验）;
3. MCP Streamable HTTP + OAuth；
4. plan/todo 工具与长任务可观测性；
5. 路由分类多语言化与测试加厚；实弹靶场进定期 CI。

对 dsh 的反制点：它没有 S/M/L 派工体系——那是 air-gapped 场景的地基，
继续把三档路由的验证数据（`agent-metrics` 的 Deep Share、Worker Verified
Success）做成可对外讲的故事。

## 主要信息来源

- 代码实况：本仓库 develop @ 7f46c05 全量盘点（2026-08-21）。
- pi：mariozechner.at 2025-11-30 博文、github.com/badlogic/pi-mono、pi.dev。
- Claude Code：code.claude.com/docs、Piebald-AI/claude-code-system-prompts
  （v2.1.238 提取，2026-08-20）。
- Codex CLI：github.com/openai/codex（含 issue #19212）。
- dsh：github.com/deepseek-ai/deepseek-harness（2026-08-13 发布）、
  DeepSeek-V4-Flash-0731 模型卡。
- 基准与讨论：tbench.ai、codex.danielvaughan.com（Harness 效应）、
  elsolitario.org（Databricks 基准转述，二手）。
