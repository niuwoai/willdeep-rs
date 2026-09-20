# ADR：体验基线对齐与模型行为评测（2026-09-20）

> 状态：已采纳 | 基于 develop @ v0.78.0-rc12 | 关联：`docs/COMPETITIVE_LANDSCAPE.md`、`docs/EXPERIENCE_BASELINE.md`、`docs/CLI_TUI_RUNTIME_ROADMAP.md` 阶段 13

## 背景

0.78 这一轮连发十二个 rc，修的全是终端「手感」：模型中途一句话不说、轮次结束没有信号、思维链制表符把屏幕画花、插话只能排队。这些 Claude Code 两年前就有。架构上的护城河（常驻 Runtime、多端同会话、可审计的自主、验证闭环、小模型路由）用户第一眼感受不到，第一眼感受到的是手感。

同时暴露出另一件事：提示词改一句（rc6 要求模型汇报进度）效果只能靠肉眼看。线上一个 deepseek-v4-pro 会话 397 条 assistant 消息只有 18 条有正文，这个数字是事后用 ruby 数出来的，没有任何回归测试会因为它变差而报警。

## 决策

按下面的顺序推进，每一项都有可验收的产物；先把地基上的房子盖到能住，再对外讲地基多好。

1. **体验基线对齐 Claude Code**。以 `docs/EXPERIENCE_BASELINE.md` 为清单逐项过，每项标「已到位 / 待做」，TUI 与 Web 分别打勾。验收方式是脚本化的 TUI 会话回放（`crates/willdeep-cli/src/tui/test_suite/` 已有事件级测试，补齐整轮回放）。
2. **模型行为评测进定期流水线**。第一步是离线指标：`scripts/session_metrics.rb` 从会话记录算出「人话率」（有正文的 assistant 消息占比）、工具调用数与失败数、思维链占比、平均轮次，输出 JSON + Markdown。第二步是固定任务集每晚对 DeepSeek V4、GLM、Claude 各跑一遍（`willdeep run` 无头），把同一套指标画成趋势。没有这个，提示词调优就是碰运气。（第二步 v0.78.0-rc21 落地：`bench/model-eval/` 20 个任务、`scripts/model_eval.rb`、趋势与报警脚本、每天 launchd；见 `docs/MODEL_EVAL.md`。）
3. **审计做成能演示的东西**：`willdeep audit export`，把 `approvals.jsonl`、hooks 拦截、verifier 裁决、diff 归属汇成一份报告。企业客户看的是这个。（v0.78.0-rc22 落地，见 `docs/AUDIT_EXPORT.md`；`approvals.jsonl` 同时补上 `session_id`。）
4. **Runtime 作为平台对外**：控制面 60 个操作已稳定，`willdeep-runtime-client` 作为 SDK 发布；`willdeep run` 进 CI 流水线的文档与样例。（v0.78.0-rc23 落地：两个 crate 发布就绪并通过 `cargo publish --dry-run`，README 与示例见 `crates/willdeep-runtime-client/`；CI 文档 `docs/CI_INTEGRATION.md`，样例 `examples/ci/`。实际发布到 crates.io 需要 token，由维护者执行。）
5. **MCP Streamable HTTP + OAuth**；写入围栏默认开并补网络围栏。（前半段 v0.78.0-rc24 落地：`url` 服务、会话与协议版本头、`willdeep mcp login` 的授权码 + PKCE 登录与自动刷新，见 `docs/SKILLS_AND_MCP.md`；围栏两项另起。）
6. **小模型路线做成数据**：`agent-metrics` 的 Deep Share、Worker Verified Success 定期对外发布。

主 Agent 的 token 预算闸门（`[agent] token_budget`）作为第 1 项的附带项一起补：轮次上限放到 200 之后，它是唯一的自动闸门。

## 不做的

- 不和前沿模型客户端比「聪明」；不为了跑分调模型。
- 不把 `STABLE_CONTRACT` 做长（rc6 加的三行是行为规则，不是脚手架）。
- 不做 IDE 插件全家桶；多端的重心仍是 Runtime 控制面与现有四种界面。

## 后果

- 每个 rc 的 CHANGELOG 要能对应到清单上的一项；对应不上的改动先问自己该不该做。
- 评测指标一旦进入流水线，提示词与路由的改动必须附带指标前后对比。
- 本 ADR 的「状态」栏在第 1、2 项验收后改为「已落地」，并回写 `docs/COMPETITIVE_LANDSCAPE.md` 的短板节。
