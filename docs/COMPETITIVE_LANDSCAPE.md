# 竞争力分析：WillDeep vs pi / Claude Code / Codex / deepseek-harness

> 分析日期：2026-09-20 | 基于 develop @ v0.78.0-rc12（代码实况盘点）。
> 上一版 2026-08-21 @ 0.39.0-rc1；两版之间发了 39 个 rc，本版按代码实况重写，竞品栏未做新一轮外部调研，时效仍标 2026-08-21。
> 结论供产品与路线图决策参考；落地顺序以 `docs/decisions/2026-09-20-experience-baseline-and-model-eval.md` 为准。

## 一句话结论

结构性差异化没有变：四家竞品都是「一个进程里跑一个 agent」，WillDeep 把 Runtime 做成了常驻控制面，多端同会话、可审计的自主、验证闭环、小模型路由都长在这上面，私有化 / 主权 / 异构小模型编排的利基仍是真差异化。变了的是短板的排序：8 月排第一的 OS 级沙箱和 hooks 已经补上，现在排第一的是**终端手感**——0.78 连发十二个 rc 修的全是 Claude Code 两年前就有的东西，用户第一眼感受到的是这个，不是架构。

## 自身现状（代码实况，非文档愿景）

- 约 108K 行 Rust / 4 crate（core 43.6K、cli 60.0K、runtime-protocol 3.5K、runtime-client 1.1K），React Web 前端约 3.7K 行 TS/TSX。CHANGELOG 累计 177 个版本条目。
- 五形态真实存在：CLI（含无头 `willdeep run`）、TUI、Runtime Daemon、Web、手机中继；另有 macOS Swift 版（Xedit）共用会话目录与协议，`willdeep handoff` 可接住桌面会话。
- Runtime 控制面 60 个操作（`SUPPORTED_OPERATIONS`），SSE 断点续传、Unix socket / Named Pipe、幂等去重；本轮新增 `turn.steer`（本轮进行中的用户插话直接送达）。
- 常驻工具 20 余个 + MCP 按需 2 个（`list_mcp_tools` / `call_mcp_tool`）；MCP 仍只有 stdio 传输。
- 审批四档（strict / smart / workspace-write / full-access）+ 静态命令分类 + AI 判官 + Always Allow 持久化；**OS 级写入围栏**（macOS Seatbelt / Linux bubblewrap）已落地，预览态默认关；**`[[hooks]]` 生命周期挂钩**（pre_tool / post_tool / approval_resolved，可阻断）已落地；`approvals.jsonl` 记每一次审批来源。
- 子 Agent：6 个公开工种，写入型默认专属 worktree + 前置写集审批，verifier 由 Runtime 执行、退出码裁决；主 Agent 可对后台子 Agent `send_agent_message` / `stop_agent`，`monitor` 工具盯进行中的输出。
- small-model-first 三档路由是 Runtime 强制策略，deep 档申请制；上下文压缩改为请求期增量摘要。
- 思考型模型（DeepSeek V4、GLM）的完整支持：`reasoning_content` 回传与补空、思维链流式显示、制表符与控制字符清洗。
- 单元测试：core 522、cli 443、protocol 10、client 31；daemon 端到端 `headless_runtime` 26 条。真模型「实弹靶场」仍默认 ignore，不在 CI。
- 系统提示词 `STABLE_CONTRACT` 约 1K token，rc6 加了三行进度汇报规则；总固定税仍约 2.6K token。

## 8 月以来补齐了什么

| 8 月短板 | 现状 |
|---|---|
| 无 OS 级沙箱 | 写入围栏已落地（Seatbelt / bwrap），默认关；网络围栏未做 |
| 无 hooks | `[[hooks]]` 已落地，审计留痕与门禁拦截 |
| 无 plan / checkpoint | `/plan` 计划卡片 + 宿主持久化步骤状态；流式检查点用于断线恢复；仍无「回到第 N 步」 |
| MCP 仅 stdio | 未变 |
| 路由分类中英关键词表 | 未变 |
| 实弹靶场不在 CI | 未变 |

另外新增且竞品没有的：会话跨端交接（`handoff`）、审批跨端回弹到发起端、后台命令脱离父进程落盘、`turn.steer` 插话送达。

## 竞品横向对比

WillDeep 列为 2026-09-20 代码实况；竞品列沿用 2026-08-21 调研，未重新核实。

| 维度 | WillDeep | pi | Claude Code | Codex CLI | deepseek-harness (dsh) |
|---|---|---|---|---|---|
| 运行形态 | 常驻 Runtime 控制面 + CLI/TUI/Web/手机/macOS 五端同会话 | 单进程 TUI | 单进程 TUI + IDE 插件 | 单进程 TUI | 单进程，插件化 |
| 系统提示词量级 | 核心 ~1K token，实测总 ~2.6K | <1K token | 核心 ~10.5K，实发 ~33K | ~2–4K，实发 ~13K | minimal 一句话 |
| 工具数 | 20 余 + MCP 按需 2 | 4 | 27+ | 中等 | minimal 仅 2 |
| 模型路由 | S/M/L 三档 Runtime 强制 + deep 申请制 | 手动换模型 | 手选 + 子代理可指定 | 手选 | 手选 |
| 子代理 | 6 工种 + verifier 闭环 + worktree 隔离 + 可指挥可叫停 | bash 里 spawn 子进程 | agent teams、后台 agents | 较弱 | 插件化提供 |
| OS 级沙箱 | 写入围栏（Seatbelt / bwrap，默认关） | 无 | 有 | 有（三档） | 有 |
| Hooks / 审计 | `[[hooks]]` 可阻断 + `approvals.jsonl` | 扩展系统 | 有 | 部分 | 插件化 |
| 检查点回退 | 无（diff review + revert） | 无 | 有（/rewind） | 部分 | 插件化 |
| 插话送达 | `turn.steer`，下一次调模型前注入 | 有 | 有 | 有 | 有 |
| 思考型模型 | 一等公民（回传、显示、清洗） | 依赖模型 | 原生 thinking | 原生 | 原生 |
| MCP | stdio client，schema 按需 | 拒绝 | 全套 | 支持 | 默认不启用 |
| 开源 / 生态 | 个人项目 | MIT，~95k stars | 闭源 | Apache-2.0，~110k stars | MIT，~178k stars |

行业背景两条不变：Harness 效应（同一模型换 harness 在 Terminal-Bench 上可差约 16 个百分点）；dsh 是最大的新变量，企业可能拿它自建私有 agent。

## 护城河（四家都没有的）

1. **常驻 Runtime 控制面**。关掉终端任务还在，审批弹回发起端，桌面会话能交接到终端，60 个操作可被任何客户端调用。这是其余四条的地基。
2. **可审计的自主**。四档审批 + 静态规则 + AI 判官 + 写入围栏 + hooks 门禁 + 审批留痕，私有化客户安全评审要的整条链都在。
3. **验证闭环纪律**。退出码唯一裁决、worker 不自证、失败输出消化回灌。
4. **Runtime 级三档路由 + deep 申请制**，为「模型不出国、机房只有 S+M」设计。
5. **双端体系**：与 macOS Swift 版共用会话目录、后台任务合同、工种模型映射。

## 短板（按对竞争力的伤害排序）

1. **终端手感刚追平，Web 还没跟上**。中途汇报、工具行折叠、思维链、结束信号、插话送达都只在 TUI 做了，Web 端的 `reasoning_delta` 直接丢弃。清单见 `docs/EXPERIENCE_BASELINE.md`。
2. **模型行为层没有回归测试**。提示词改一句效果只能肉眼看；「人话率」这种数字要事后用脚本数。→ ADR 第 2 项。
3. ~~**MCP 仅 stdio**，无 Streamable HTTP / OAuth。~~ rc24 补齐：Streamable HTTP 传输与 OAuth 登录（`willdeep mcp login`）；未接服务端主动请求。
4. **无检查点回退**；首次使用体验差（`config.toml` 手写，旧值静默改变行为）。
5. 路由分类仍是中英关键词表；实弹靶场不在 CI。
6. 主 Agent 直到 rc12 没有 token 预算闸门（rc13 补）。

## 总评与优先级建议

竞争力评级：大众市场 ★★☆（不参战）；私有化 / 主权利基 ★★★★（真差异化，沙箱与 hooks 已补，现在缺的是手感与证据）。

优先级与产物见 `docs/decisions/2026-09-20-experience-baseline-and-model-eval.md`：体验基线对齐 → 模型行为评测进流水线 → 审计报告 → Runtime 作为平台 → MCP 传输与网络围栏 → 小模型路线数据化。

## 主要信息来源

- 代码实况：本仓库 develop @ v0.78.0-rc12 全量盘点（2026-09-20）；线上会话记录统计（`~/.willdeep/sessions`，仅计数）。
- 竞品：沿用 2026-08-21 版的来源（pi.dev 与 badlogic/pi-mono；code.claude.com/docs；openai/codex；deepseek-ai/deepseek-harness；tbench.ai）。
