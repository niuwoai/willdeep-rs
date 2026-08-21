# Xedit ↔ willdeep-rs 联动现状与路径

> 勘察日期：2026-08-21（同日完成前两步并回写）| rs：0.39.0-rc2 | Xedit：1.283.0-rc1。
> 本文是**现状盘点与路径建议**；工具能力清单见 `XEDIT_TOOL_PARITY.md`（工具维度），
> 双端逐项对照表见 `SKILL_WORKERS.md` 对照一节，战略基调见 Xedit 仓库
> `docs/CROSS_PLATFORM_CLI_STRATEGY.md`（决策 3：rs 先独立发展，协议先行，
> 不预设融合时间表）。
> 文中 Xedit 侧路径均为 Xedit 仓库内相对路径。

## 一句话结论

两端当前是**「文件层共享 + 契约层对齐 + 运行时层零耦合」**的三段式：前两层
实打实在运转且有单测/fixture 锁定；第三层（Swift 接入 rs Runtime 控制面）
协议、fixture、四阶段路线图全部就绪，Swift 侧接入代码为零——这是准备最充分、
价值最大的联动潜力点。

## 双端画像（2026-08-21）

| 项 | willdeep-rs | Xedit |
|---|---|---|
| 版本 | 0.39.0-rc1 | 1.282.0-rc6 |
| 规模 | ~60K 行 Rust / 4 crate | ~252K 行 Swift（主应用 481 文件） |
| 工具数 | 21+2 | 164（`Xedit/AgentToolRegistry.swift`） |
| 系统提示词 | `STABLE_CONTRACT` ~4.5KB | 稳定前缀 ~30KB（v29，`AgentContextBuilder.swift`） |
| 平台 | macOS / Linux / Windows | macOS 专属 |
| 定位 | 跨平台内核 + daemon/TUI/Web/自动化 | 原生 Agent 工作台（编辑器、Computer Use、插件） |

## 文件层共享（已在运转的，逐条有证据）

| 共享面 | 方向 | rs 侧证据 | Xedit 侧证据 |
|---|---|---|---|
| `config.toml` `[notifications]` | 双向读写 | `crates/willdeep-cli/src/config.rs`、`notify.rs` | `Xedit/AgentAttentionSettings.swift`（段级替换，保留注释） |
| `config.toml` `[agent]` 路由三键 | CLI 拥有，App 只读 | 0.37.0-rc1 引入 | `Xedit/AgentSharedRuntimeConfig.swift`（只读理由在头注释） |
| `~/.willdeep/skills/` + `tier:` frontmatter | 双向 | `crates/willdeep-core/src/skills.rs:7` | `Xedit/AgentSkillDirectory.swift:121`（注释逐字同源） |
| webhook `willdeep.webhook.v1` | Xedit 定义、rs 遵从 | `notify.rs:8-14`（"wire format is not ours to invent"），状态词表对齐 Swift enum raw value，回归测试锁定 | `AgentAttentionSettings.swift:472-511` |
| 工种→模型映射（`someim-32b-<trade>`） | 双端镜像 | `subagent.rs:1618`（注释点名镜像对象）+ 单测 | `AgentSubagentJobPrompts.hostedModel(for:)` |
| Task Packet 字段 | 近乎字段级同构 | `subagent.rs:143-178` | `AgentSubagentTaskPacket.swift:19-66` |
| 会话文件 | **单向**：rs 读 Swift + `pinnedAt` 就地回写；续聊写 rs 副本不覆盖原文件 | `session.rs:472-572` | Xedit 不读 `~/.willdeep/sessions` |
| `projects.json` | rs 读 Xedit | `crates/willdeep-cli/src/projects.rs` | Application Support |
| 命令安全分类器 | rs 移植自 Xedit | `safety.rs:1-19` 头注释 | — |
| 判官模型 `someim-security-guard` | 同名 | `harness.rs:26` | `AgentModels.swift` |
| `mobile-gateway.v1` | 协议版本共用，room/token 独立 | `mobile.rs`、`AUTHENTICATION.md:133` | `MobileGatewayCoordinator.swift` |
| CLI 命名权 | `willdeep` 归 rs | — | `CommandLineToolInstaller.swift`（App 装成 `willdeep-app`） |

**明确尚未共用**（见 Xedit `docs/SMALL_MODEL_FIRST_RUNTIME_PLAN.md` §6）：
`[subagents.<trade>]`（Xedit 存 UserDefaults）、`[providers.*]`（Xedit 走
Provider 库 + Keychain，不读明文 TOML）。

## 契约层对齐

- rs `SKILL_WORKERS.md` 有正式双端对照表，原则："能一致的必须一致；不一致的
  要写明理由，而不是让它自然漂移"。
- canonical 文档跨仓分工：`MODEL_TIERS.md` 在 rs；`GOAL_TEAMS_ROLES_DESIGN.md`
  与 `LONG_HORIZON_AUTONOMY.md` 在 Xedit，rs 侧为引用与落地映射。
- 指标与纪律双向回流：Xedit 的 Citation Audit、实弹靶场自 rs 回流；rs 的
  安全分类器、审批语义自 Xedit 移植。

## 运行时层：第零步已迈出

- **2026-08-21 进展**：Xedit 1.283.0-rc1 新增 `WillDeepRuntimeProtocol.swift`
  ——统一响应信封加十一类公共对象与四类控制结果的 Codable 解码器，配
  `WillDeepRuntimeProtocolTests`（4 项）。**只解码，不连传输、不改行为、不
  依赖运行中的 daemon**，四阶段计划的「阶段一：只读观察」现在可以直接建在
  这批类型上。契约夹具逐字复制进 `XeditTests/Fixtures/`，并有一条与本仓
  源文件的字节比对（两仓同时存在时比对，缺失时跳过）。
  解码器放在 App target 而非测试 target：契约测试若只跑测试专用结构体，
  证明的仅仅是测试能编译。
- **仍未接入**：传输层。Xedit 连 daemon 走的仍是 Go `willdeep-agent` 的
  `/v1/fs/*` 远端工具后端，与 rs 控制面（`/v1/api`、`control.sock`）无关
  （Xedit `docs/GOAL_TEAMS_ROLES_DESIGN.md:35` 有权威说明）。请求编码
  （`agent.spawn` 等写方向）也刻意留到真正发起调用的那一阶段——现在加等于
  交付一个没跑过的写入器。
- **已就绪未用**：类型化 `willdeep-runtime-client`、四阶段"Swift Harness
  替换"计划（`CLI_TUI_RUNTIME_ROADMAP.md` §1.0 迁移门槛、§7.14 验收）。

## 网关实况（2026-08-21 逐档实弹探活）

`/v1/models` **不列出虚拟模型链**（连在用的 `someim-security-guard`、
`someim-32b-compressor` 都不在列表里），因此模型列表不能当清单用；下表来自
逐档最小 `chat/completions` 请求：

| 档 | 状态 | 实测上游 |
|---|---|---|
| `someim-32b` + 七个工种档（scout / reader / editor / test-fixer / build-fixer / log-inspector / git-detective） | ✅ 在线 | 多为 glm-5，`editor` 已落 poolside/laguna-xs-2.1 |
| `someim-32b-compressor` | ✅ 在线 | inclusionai/ling-3.0-flash |
| `someim-security-guard`、`someim-judge`（主循环档） | ✅ 在线 | stealth/ox-alpha |
| `someim-32b-security-guard` / `-judge` / `-reviewer` / `-ops-runner` | ❌ `model_not_configured` | 未建 |

两点须记住：**上游会变**（compressor 与 security-guard 的实测上游都已与文档
初版记载不同），客户端不应依赖具体上游——那正是虚拟模型的意义；而
**主循环档与 Worker 档同名不同物**，`someim-security-guard`（replace 模式、
承担自动审批信任边界）与 `someim-32b-security-guard`（prepend 模式的 Worker
工种）是 Xedit 有意并存的两个模型，其文档写明"不要合并"。

## 当前漂移清单（按处理优先级）

1. ~~**托管工种白名单不一致**~~ ✅ **已修（2026-08-21）**：Xedit 1.282.0-rc8
   把 `security_guard` / `judge` 移出 `hostedTradeIDs`，两端同为七项，并加了
   字面量名单测试（原测试从 `hostedTradeIDs` 反推 `hostedVirtualModel`，两者
   永远自洽、同时错也发现不了）。
   *更正*：本文初版称 Xedit"同仓两名、命名冲突"——**该判断有误**。
   `someim-security-guard` 与 `someim-32b-security-guard` 是有意并存的两个
   模型，信任边界不同（见上一节），不应合并。
2. **审批存储两套**：Xedit Always Allow 存 UserDefaults，rs 写
   `~/.willdeep/always-allow.json` + `approvals.jsonl`，互不可见——App 里
   点过"始终允许"的命令 CLI 还会再问。rs `ARCHITECTURE.md` 已预留"替换规则
   存储适配器"收敛路径。
   **前置条件已补齐**：rs 0.39.0-rc2 起，带凭据的命令不再能记成规则，加载时
   还会清理存量泄漏规则。共享这个文件之前必须先有这道闸——否则等于把凭据
   扩散到第二个 App 的存储与同步面。
3. **`task.digest_oversized` 缺口**：rs 有（air-gapped 分片消化，0.28.0-rc1），
   Xedit 无——而这恰是三档战略在 S+M 机房的降级关键。
4. **canonical 文档互引用硬编码绝对路径**（如 `~/Sites/Xedit`）：换机器或
   开源后会断，应改为"Xedit 仓库 + 仓内相对路径"的引用写法。
5. **系统提示词无共享源**：30KB vs 4.5KB 两份独立文本。按
   `COMPETITIVE_LANDSCAPE.md` 的短提示词判定，回流方向应是 rs 的瘦身经验
   （托管职能提示词、4KB 技能索引、MCP 按需）流向 Xedit，而非反向。

**有意分歧（不处理，已有书面理由）**：冲突文件集 Xedit 排队 / rs 拒绝；
`terminal_operator` rs 明确不做；Provider 凭据存储结构性差异；
`auto_dispatch_read_only` Xedit 仅解析不生效（先对名字后对行为）。

## 战略判断

「macOS 原生工作台 + 跨平台常驻内核」的组合是 pi / Claude Code / Codex / dsh
四家都没有的形态（Claude Code 的桌面 App 是同一内核的壳），与
`COMPETITIVE_LANDSCAPE.md` 列的护城河第 4 条互为表里。代价是**漂移税**：
164 vs 23 工具、两套 agent loop、两套审批、两份提示词，每个共享概念都靠
对照表人肉维持。当前"协议先行、不定融合时间表"的基调正确，但有保质期——
对照表规模再翻倍就管不住了。

## 建议路径（按序）

1. ~~**修漂移 1**：Xedit 托管白名单对齐 7 项~~ ✅ 已完成（1.282.0-rc8）。
2. ~~**Swift 第零步**：用 rs 的 `public-api-v1.json` 建 decoder contract
   test~~ ✅ 已完成（1.283.0-rc1）。
3. **审批存储收敛**（下一步）：Xedit 换成读写 `~/.willdeep/always-allow.json`
   的适配器，双端"始终允许"互认——用户可感知的第一个联动体验。凭据闸门
   已由 rs 0.39.0-rc2 补齐，可以动了。
4. **会话单向变双向**：rs 已读 Swift 会话；反向为空白。Swift/Rust 共享
   会话 schema 稳定后开双向原地写入（rs 已知问题清单既有条目）。
5. **中期锚点不变**：按路线图让 rs 内核成为唯一 Harness，Xedit 退成
   "最好的客户端"，漂移税归零；Computer Use 经共享 Helper 协议接入
   （边界要求见 `XEDIT_TOOL_PARITY.md`，完整边界具备前 rs 不注册可写
   Computer Use 工具）。

## 复核口径

本文所有"已共享/未接入"判断基于 2026-08-21 对两仓的全量代码勘察（含 grep
证据），非文档愿景转述。后续任一侧改动共享面（webhook、工种映射、Task
Packet、config 键、skills tier）时，应同步更新本文与 `SKILL_WORKERS.md`
对照表。
