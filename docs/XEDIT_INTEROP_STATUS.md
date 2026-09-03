# Xedit ↔ willdeep-rs 联动现状与路径

> 初次勘察：2026-08-21；最近同步复核：2026-08-31（Worker 三档）。|
> rs：0.52.0-rc2 | Xedit：1.312.0-rc2。
> 本文是**现状盘点与路径建议**；工具能力清单见 `XEDIT_TOOL_PARITY.md`（工具维度），
> 双端逐项对照表见 `SKILL_WORKERS.md` 对照一节，战略基调见 Xedit 仓库
> `docs/CROSS_PLATFORM_CLI_STRATEGY.md`（决策 3：rs 先独立发展，协议先行，
> 不预设融合时间表）。
> 文中 Xedit 侧路径均为 Xedit 仓库内相对路径。

## 一句话结论

两端是**「文件层共享 + 契约层对齐 + 运行时层只读打通」**的三段式：前两层实打实
在运转且有单测/fixture 锁定，2026-08-21 起审批规则也进入共享面（用户可感知
的第一个联动）；第三层同日完成四阶段替换计划的阶段一——Swift 已能经 Unix
socket 连上本机 Runtime 并读出结构化状态，写方向与事件流仍未接。

## 双端画像（2026-08-21）

| 项 | willdeep-rs | Xedit |
|---|---|---|
| 版本 | 0.52.0-rc2 | 1.312.0-rc2 |
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
| **Worker 三档**（基础/进阶/专家）与默认模型 | 双端镜像，同一张表 | `worker_tier.rs`（`default_hosted_model` + 契约测试） | `AgentWorkerTierModels.defaultBinding`（Xedit 1.312.0-rc1） |
| **五个公开职责**（调查/实现/验证/审查/运维） | 同名同义 | `PUBLIC_SUBAGENT_IDS` + `public_profile_id` 别名表 | `AgentWorkerRole` |
| 工种→模型映射（`someim-32b-<trade>`） | **已退役**（0.50.0-rc1 / 1.311.0-rc2）：七个别名在请求边界归一到 `someim-32b`，职责提示词由客户端持有 | `worker_tier.rs` 的 `normalize_hosted_model` + `hosted_worker_model` | `AgentSubagentModelCompatibility` |
| Task Packet 字段 | 近乎字段级同构 | `subagent/types.rs:139-174` | `AgentSubagentTaskPacket.swift:19-66` |
| 会话文件 | **单向**：rs 读 Swift + `pinnedAt` 就地回写；续聊写 rs 副本不覆盖原文件。0.43.0-rc1 起桥接会话进入 rs 历史面板并标 `[Xedit]` | `session.rs` 的 `swift_digest` / `swift_session`；`session_store.rs` 的 `extend_with_unmanaged` | Xedit 不读 `~/.willdeep/sessions` |
| 会话标题两级生成 | 同一套语义，各自实现 | `session_title.rs`（占位符名单含 Xedit 的中英文默认名） | `AppStateAgentTitleSummarizer.swift`、`AgentSessionStore.isPlaceholderTitle` |
| 本地辅助模型 | 语义对齐、配置存储暂不共享：复用单模型，本地优先后远端回退，低置信度才做模型路由 | `config.rs` `[local_model]`、`harness.rs`、`routing.rs` | `AgentLocalModelSupport.swift`、`AppStateAgentWorkerRouting.swift` |
| `projects.json` | rs 读 Xedit | `crates/willdeep-cli/src/projects.rs` | Application Support |
| **插件包** `~/.willdeep/plugins/<id>/<version>/` | **双向共享包内容，状态各存各的**（0.50.0-rc1 起） | `crates/willdeep-core/src/plugin/`、`plugin_web.rs`、`plugin_bridge.js`；见 [PLUGINS.md](PLUGINS.md) | `AgentPluginRegistry.swift` 等 11 个文件；`docs/WILLDEEP_PLUGIN_SYSTEM_DESIGN.md` |
| 插件清单 schema | 同一份契约，两端各自实现校验 | `plugin/manifest.rs`（含菜单位置白名单往返测试） | `docs/plugin-schema/willdeep-plugin.schema.json` |
| 插件页面桥 `window.willdeep.*` | 逐方法对齐，传输层各异 | `plugin_bridge.js`（postMessage） | `AgentPluginPageHost.swift`（WKWebView messageHandlers） |
| `always-allow.json` 审批规则 | 双向读写（2026-08-21 起），共享精确命令 | `tools.rs` `with_always_allow_store` + 跨语言契约测试 | `AgentSharedAlwaysAllowStore.swift` + 8 项契约测试 |
| `model-catalog.v1.json` 模型目录 | **canonical 契约已定，代码未接入** | `docs/SHARED_MODEL_CATALOG.md` + JSON Schema/示例 | 计划由 `AgentProviderLibrary` / some.im public model catalog 迁移；真实凭据只存 `credential_ref` |
| 命令安全分类器 | rs 移植自 Xedit | `safety.rs:1-19` 头注释 | — |
| 子 Worker 命令智能审核 | 双端同语义 | `reviewed_subagent_shell` + `target_command` 精确授权 | `CommandReviewer` + 父级 `ops_runner(target_command)` 人审兜底 |
| 判官模型 `someim-security-guard` | 同名 | `harness.rs:26` | `AgentModels.swift` |
| `mobile-gateway.v1` | 协议版本共用，room/token 独立 | `mobile.rs`、`AUTHENTICATION.md:133` | `MobileGatewayCoordinator.swift` |
| CLI 命名权 | `willdeep` 归 rs | — | `CommandLineToolInstaller.swift`（App 装成 `willdeep-app`） |

插件那一行是目前**用户感知最强**的一条共享面：同一个插件包在两个宿主里都能跑，
包不用改一行。刻意不共享的是启用状态与权限审批——两个宿主的沙箱边界不是一回事
（rs 是 opaque-origin iframe + CSP，Xedit 是非持久化 WKWebView + 自定义协议），
跨宿主复用审批等于替另一侧替用户点了头。这与 `always-allow.json` 上得到的教训同源：
共享货币必须是**内容**，不是**判断**。

**明确尚未共用**（见 Xedit `docs/SMALL_MODEL_FIRST_RUNTIME_PLAN.md` §6）：
`[subagents.<trade>]`（Xedit 存 UserDefaults）、`[providers.*]`（Xedit 走
Provider 库 + Keychain，不读明文 TOML）。两项的收敛契约现已定义在
[`SHARED_MODEL_CATALOG.md`](SHARED_MODEL_CATALOG.md)：先共享非敏感 Provider/模型目录，
再用 `credential_ref` 接系统凭据后端，绝不把 Key 搬进共享 JSON。

## 契约层对齐

- rs `SKILL_WORKERS.md` 有正式双端对照表，原则："能一致的必须一致；不一致的
  要写明理由，而不是让它自然漂移"。
- canonical 文档跨仓分工：`MODEL_TIERS.md` 在 rs；`GOAL_TEAMS_ROLES_DESIGN.md`
  与 `LONG_HORIZON_AUTONOMY.md` 在 Xedit，rs 侧为引用与落地映射。
- **内核事件信封 `agent-kernel-event.v1`：canonical 在 rs**（2026-09-03 定，
  Xedit 为 mirror）。内核语义本身由 Xedit 1.315.0-rc15 首发实现，但字段契约
  文本归 rs，移植计划见 [AGENT_RUNTIME_KERNEL.md](AGENT_RUNTIME_KERNEL.md)。
  分工边界：外部事件的**云端中继归 Xedit 加 Go relay**，rs 只做本机入站
  （daemon 端点、hooks、本机定时任务），因此 `collab-relay.v2` 不是双端共享面。
- 指标与纪律双向回流：Xedit 的 Citation Audit、实弹靶场自 rs 回流；rs 的
  安全分类器、审批语义自 Xedit 移植。

## 运行时层：阶段一（只读观察）已打通

2026-08-21 两步落地，Swift 侧现在能真的连上本机 Runtime 并读出结构化状态：

- **第零步（Xedit 1.283.0-rc1）**：`WillDeepRuntimeProtocol.swift` ——
  统一响应信封加十一类公共对象与四类控制结果的 Codable 解码器，配 4 项
  fixture 契约测试。只解码，不连传输。解码器放在 App target 而非测试
  target：契约测试若只跑测试专用结构体，证明的仅仅是测试能编译。
- **阶段一（Xedit 1.284.0-rc1）**：`WillDeepRuntimeTransport`（发现 + 传输）
  与 `WillDeepRuntimeClient`（只读操作），12 项测试，其中一条**真打本机
  daemon**（无 daemon 时跳过）。

四条设计取舍值得记下来，改这块之前先看：

1. **优先 Unix socket，回环 TCP 仅作回退。** 本文档 §2 明写本机客户端应优先
   连 Runtime 目录内 `0600` 的 socket，而 `URLSession` 拨不了 unix socket。
   Xedit 因此手写了一个极小的 HTTP/1.1 客户端（一个 POST、一个 JSON body、
   `Content-Length` 响应）。socket 的文件权限是真实访问边界，不是风格问题：
   为省事改用 TCP，等于该 App 成为唯一无视协议传输偏好的客户端。Runtime
   不会发的东西（chunked、keep-alive 复用、重定向）一律明确报错而非半懂。
2. **`daemon.json` 当不可信输入。** `address` 只接受回环地址——被篡改的状态
   文件不能把 App 的 token 引向另一台主机。
3. **「只读」是代码强制，不是约定。** Runtime 有 54 个操作，其中不乏
   `session.delete`、`turn.submit`、`agent.spawn`。客户端持一份**字面量**
   只读白名单并在发出任何字节前拒绝其余操作；靠"凡是 `.list` 结尾"推导的
   白名单，早晚会放进下一个长得像读、实则有副作用的操作。
4. **端到端测试必须证明自己没被跳过。** 那条真打 daemon 的测试写了跳过分支
   （开发机没 daemon 不算客户端缺陷），因此上线前用临时诊断版验证过它确实
   走通了发现与连接两步——一条永远在跳过的端到端测试比没有更糟。

**仍未接入**：写方向（`agent.spawn` 等请求编码）刻意留到真正发起调用的阶段，
现在加等于交付一个没跑过的写入器；事件流（SSE / NDJSON 游标续传）也未接。
Xedit 连自己 daemon 走的仍是 Go `willdeep-agent` 的 `/v1/fs/*` 远端工具后端，
与本 Runtime 控制面无关（Xedit `docs/GOAL_TEAMS_ROLES_DESIGN.md:35`）。

**契约演进备忘（rs 0.55.0-rc1）**：`RuntimeTask` 新增可选字段 `prompt_excerpt`
（打码 + 120 字符截断的提示词摘要，任务标识用），`public-api-v1.json` fixture
已带上。字段可空，Swift 解码器未同步也不会断（未知键忽略、Optional 缺省
nil）；Xedit 下次同步 fixture 时在 `WillDeepRuntimeProtocol.swift` 补上即可。

**契约演进备忘（rs 0.62.0-rc1）**：新增 `kernel.list` / `kernel.get` /
`kernel.ignore` 三个操作与 `PublicKernelEvent` 投影，`public-api-v1.json`
fixture 已带上。**事件正文不进公共 DTO**，只有打码截断的标题摘要。Swift 侧
未同步不会断（新操作不调用即可），但要注意命名：Xedit 那边的内核事件与
rs 控制面既有的 `event.*` 是两个方向，接的时候别把它们混成一个流。

## 网关实况（2026-08-21 逐档实弹探活）

`/v1/models` **不列出虚拟模型链**（连在用的 `someim-security-guard`、
`someim-32b-compressor` 都不在列表里），因此模型列表不能当清单用；下表来自
逐档最小 `chat/completions` 请求：

| 档 | 状态 | 实测上游 |
|---|---|---|
| `someim-32b` + 七个工种档（scout / reader / editor / test-fixer / build-fixer / log-inspector / git-detective） | ✅ 在线，但**两端已停止使用工种档**（0.50.0-rc1 / 1.311.0-rc2）：职责提示词改由客户端持有，七个别名在请求边界归一到 `someim-32b` | 多为 glm-5，`editor` 已落 poolside/laguna-xs-2.1 |
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
2. ~~**审批存储两套**~~ ✅ **已收敛（2026-08-21，Xedit 1.283.0-rc2 +
   rs 0.39.0-rc3）**：两端现在读写同一个 `~/.willdeep/always-allow.json`。
   语义见下节；`approvals.jsonl`（审计日志）仍是 rs 独有，不共享。
3. **`task.digest_oversized` 缺口**：rs 有（air-gapped 分片消化，0.28.0-rc1），
   Xedit 无——而这恰是三档战略在 S+M 机房的降级关键。
4. **canonical 文档互引用硬编码绝对路径**（如 `~/Sites/Xedit`）：换机器或
   开源后会断，应改为"Xedit 仓库 + 仓内相对路径"的引用写法。
5. **系统提示词无共享源**：30KB vs 4.5KB 两份独立文本。按
   `COMPETITIVE_LANDSCAPE.md` 的短提示词判定，回流方向应是 rs 的瘦身经验
   （托管职能提示词、4KB 技能索引、MCP 按需）流向 Xedit，而非反向。

**有意分歧（不处理，已有书面理由）**：冲突文件集 Xedit 排队 / rs 拒绝；
Swift 的 `terminal_operator` 驾驶 App 侧栏终端，Rust 以公开 `ops_runner` 承接同类
命令审核语义但不模拟 GUI 终端；Provider 凭据存储结构性差异；
`auto_dispatch_read_only` Xedit 仅解析不生效（先对名字后对行为）。

## 战略判断

「macOS 原生工作台 + 跨平台常驻内核」的组合是 pi / Claude Code / Codex / dsh
四家都没有的形态（Claude Code 的桌面 App 是同一内核的壳），与
`COMPETITIVE_LANDSCAPE.md` 列的护城河第 4 条互为表里。代价是**漂移税**：
164 vs 23 工具、两套 agent loop、两套审批、两份提示词，每个共享概念都靠
对照表人肉维持。当前"协议先行、不定融合时间表"的基调正确，但有保质期——
对照表规模再翻倍就管不住了。

## 共享 always-allow 的语义（2026-08-21 落地）

两端的规则模型本就不同，共享时**没有**把它们合并成一种，而是选了一个双方
都能安全表达的交集：

| | Xedit | rs |
|---|---|---|
| 本地规则模型 | 命令族（`git push` 覆盖整族） | 精确命令（字符串相等） |
| 写入共享文件 | 用户批准的**那条精确命令** | 同左（本来就是精确命令） |
| 读取共享文件 | 认 `command-exact:` 规则 | 同左 |

**共享货币是「人真正看过并批准的那条精确命令」。** 理由是方向性的：把宽的
（族）共享过去等于偷改另一侧的政策——rs 用户只被问过「记住这一条命令」，
不该因此自动放行整族；而精确命令是任何包含它的族的子集，两个方向都不放宽
任何一侧的权限模型。Xedit 点击「始终允许」时仍照旧存自己的族规则，**另外**
发布一条 `command-exact:`。

三条硬约束（任一违反都会伤到另一侧，不只是本特性失效）：

1. **文件必须 0600**。rs 的 `with_always_allow_store` 见到 group/other 位会
   拒绝整个存储并报错，弄坏的是每一次 CLI 运行。Xedit 走 0600 临时文件 +
   原子替换，并在替换后重新断言 mode。
2. **带凭据的命令永不写入**。rs 0.39.0-rc2 起不再铸这类规则并清理存量；
   若 Xedit 照写，泄漏只会换个进程重现。该闸门实现在共享存储自身，因为它
   是共享文件的不变量，不属于任一 App 的审批 UI。
3. **只铸对方也会铸的签名**，并且**写前重读合并、看不懂的规则原样保留**。
   两个进程共用一个文件，盲写会丢掉对方新增的规则。

双向契约测试各在一侧：Xedit `AgentSharedAlwaysAllowStoreTests`（8 项）、
rs `a_store_written_by_the_macos_app_loads_and_matches_here`（后者用的是
Foundation `JSONEncoder` 输出的逐字节捕获——两空格缩进、正斜杠转义成
`\/`，都是合法 JSON，但「合法」不等于「验过」）。

## 建议路径（按序）

1. ~~**修漂移 1**：Xedit 托管白名单对齐 7 项~~ ✅ 已完成（1.282.0-rc8）。
2. ~~**Swift 第零步**：用 rs 的 `public-api-v1.json` 建 decoder contract
   test~~ ✅ 已完成（1.283.0-rc1）。
3. ~~**审批存储收敛**~~ ✅ 已完成（1.283.0-rc2 + 0.39.0-rc3），见上节。
4. ~~**阶段一：Swift 只读观察 Runtime**~~ ✅ 已完成（1.284.0-rc1）。
5. **下一步候选**（按价值排序，未开工）：
   - **事件流**：Swift 接 SSE 或 NDJSON 并按 `sequence` 游标续传，观察面板
     才能实时跳动而不是靠轮询；
   - **UI 落地**：把只读快照接进 Xedit 界面（Agent 树 / 任务 / 待处理项），
     现在客户端有了但没人看得见；
   - **阶段二：会话与审批双读校验**（`CLI_TUI_RUNTIME_ROADMAP.md` §1.0），
     即同一份状态两侧各读一遍并比对，为后续写方向接管铺路。
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
