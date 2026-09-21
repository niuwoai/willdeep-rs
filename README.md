# WillDeep

**敢让它自己跑，因为跑完你能查账。**

自托管的 AI Coding Agent。模型可以不出国，每一行改动能追溯到是哪个 Agent、哪次工具调用改的，小模型交上来的活由退出码裁决而不是自我声明。一个二进制，终端 / 浏览器 / 手机三种界面，关掉窗口任务照跑。

> **WillDeep** is a self-hosted AI coding agent for people who have to answer for what the agent did: every changed line is attributed to the agent and tool call that wrote it, dangerous commands never reach the model for a verdict, sub-agent work is judged by exit codes, and sessions outlive your terminal. Works with any OpenAI-compatible or Anthropic-style endpoint, including models that never leave your datacenter.

[![CI](https://github.com/niuwoai/willdeep-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/niuwoai/willdeep-rs/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/niuwoai/willdeep-rs)](https://github.com/niuwoai/willdeep-rs/releases/latest)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/rust-1.94%2B-orange.svg)
![Platforms](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey.svg)

```bash
willdeep --workspace . "检查当前仓库并修复测试"
```

---

## 30 秒上手

**1. 装二进制。** [Releases](https://github.com/niuwoai/willdeep-rs/releases/latest) 提供四个平台的发行包，解开就是一个 `willdeep`：

```bash
# macOS（Universal，Apple Silicon 与 Intel 通用）
curl -fsSL https://github.com/niuwoai/willdeep-rs/releases/latest/download/willdeep-macos-universal.tar.gz | tar -xz
sudo install -m 0755 willdeep /usr/local/bin/willdeep
willdeep --version
```

| 平台 | 发行包 |
|---|---|
| macOS | `willdeep-macos-universal.tar.gz` |
| Linux amd64 / arm64 | `willdeep-linux-amd64.tar.gz` · `willdeep-linux-arm64.tar.gz` |
| Windows x64 | `willdeep-windows-x64.zip` |

想从源码构建，见下文[参与开发](#参与开发)。

**2. 给一把钥匙。** 没有配置文件也能跑，环境里有认得出的 Key 就直接开工：

```bash
export SOMEIM_API_KEY=...        # 或 ANTHROPIC_API_KEY；OpenAI 兼容端点的写法见配置指南
willdeep --onboarding            # 或者走一遍交互式设置，把 Provider 写进 ~/.willdeep/config.toml
```

**3. 开干。** 同一个二进制，三种用法：

```bash
willdeep --workspace .                          # 终端界面（TUI）
willdeep --web --workspace .                    # 浏览器界面，默认 127.0.0.1:9847
willdeep run --output json "总结当前风险"        # 无头自动化：稳定退出码，可进 CI
```

`willdeep doctor` 不联系任何 Provider 就能检查配置、围栏和 Runtime 状态。更细的步骤见 [安装与构建](docs/INSTALL.md)、[配置指南](docs/CONFIGURATION.md)、[放进 CI](docs/CI_INTEGRATION.md)。

---

## 适合谁

| 你的处境 | WillDeep 给你的 |
|---|---|
| 数据不能出机房，只能跑开源权重 + 自有 GPU | 三种线格式接任何端点；三档路由专为「机房里只有 32K 和 128K 两档模型」设计，1M 档是加速器，不是地基 |
| 安全评审要问「Agent 到底干了什么」 | `willdeep audit export` 出一份报告：审批放行、人工裁决、hook 拦截、验证证据、每处改动的归属 |
| 想让小模型干活，又怕它糊弄 | 子 Agent 的验证命令由 Runtime 亲自跑，退出码是唯一裁决；删掉测试变绿的算作弊 |
| 任务动辄半小时，不想守着终端 | 常驻 Daemon 在自己进程里跑，关 CLI、断 SSH、刷新浏览器都不掉；手机上能批审批 |

**不适合：** 追求大众终端 Agent 的极致手感、需要 Computer Use / Browser Use、要多用户的 Web 服务。这些赛道我们不抢，细节见[边界](#边界)。

---

## 它和别的 Coding Agent 有什么不一样

四家主流竞品都是「一个进程里跑一个 Agent」。WillDeep 把 Runtime 做成常驻控制面，下面六件事都长在这上面：

```
 终端 TUI ─┐
 浏览器   ─┼─→ Runtime Daemon（常驻）─→ 主 Agent ─→ 子 Agent（专属 Worktree）─→ Verifier（退出码裁决）
 手机中继 ─┘         │
                     └─ 闸门：工作区策略 → 静态规则 → AI 判官 → OS 围栏 → 你的 hooks
```

### 查得清 · 每一行改动都有出处

- Diff 快照精确到**哪个 Turn、哪个 Agent、哪次工具调用**改了这一行。不是「AI 改了 37 个文件」，是「第 4 轮里 `test_fixer` 的第 2 次 `edit_file` 动了这 6 行」。
- 审查面板逐文件通过 / 拒绝 / 要求修改；提交前看 Commit Preview（只预览，不执行）；撤销走回收区，不是 `git checkout` 一把梭。
- 写入型子 Agent 在专属 Git Worktree 里干活，审查通过才允许合并；主干动过同一个文件就直接判定不可合并。
- `/rewind` 回到第 N 步，对话与文件一起回，回退本身也可回退。→ [检查点回退](docs/CHECKPOINT_REWIND.md)

### 拦得住 · 五道闸门，一道比一道靠外

1. **工作区策略。** 只读 / 智能审批 / 可写三档，由服务端强制，**客户端无法自报可写**。
2. **静态规则。** `rm` / `sudo` / `mkfs` / `git push --force` / fork 炸弹这类危险形状**永不送给模型裁决**，直接交给你。分类器故意保守：解析不了的一律降级为「要判断」，一个 bug 的代价是多弹一张审批卡，不是一次没人过目的 `rm -rf`。反过来也拿捏得住：`grep "rm -rf" log` 是搜索，引号里的危险词是数据。
3. **AI 判官。** 剩下的模糊地带交给一次有界调用。判官看到的是不可信文本，所以凭据先脱敏、不可信字段用 XML 包起来、回复只认单个 `<verdict>` 标签，一条把 `YES` 回显出来的命令冒充不了模型的判断。「始终允许」只记规范化后的完整命令，不记前缀。
4. **OS 级围栏。** 前三道判的是「模型请求做什么」，不是「进程实际能做什么」。macOS Seatbelt / Linux bubblewrap 把写入范围和网络交给内核裁决，有后端的机器默认开。→ [OS 级围栏](docs/SANDBOX.md)
5. **你的规矩。** `[[hooks]]` 在工具执行前把事件 JSON 喂给你的命令，非零退出就拦，stderr 成为拒绝理由。阻塞式 hook 超时默认**拦**而不是放：一个坏了就自动放行的门禁，恰好会在出事的时候失效。→ [生命周期挂钩](docs/HOOKS.md)

```toml
[[hooks]]
name = "change-ticket"
event = "pre_tool"
command = "/usr/local/bin/check-change-ticket"   # 去问公司的变更单系统
blocking = true
timeout_seconds = 10
on_error = "deny"
```

### 说得准 · 谁干的活谁不判

- 子 Agent 的验证命令由 Runtime 亲自执行，**退出码是唯一裁决**，worker 不自证。
- 绿了还不够：靶场逐字比对测试块。把测试删掉也能变绿，那是最省力的通关方式，作弊的不算通过。
- 没有退出码的只读工种（定位符号、查日志、追 Git 真凶）也不放它们隐身：Runtime 抽查报告里点名的路径、行号、commit 是否真实存在。它只证伪「地名是编的」，不证明「答对了」，所以引用准确率和答对率**分开算**。
- **「未验证」是独立于通过和失败的第三种答案。** 分母为 0 时打 `-` 而不是 `0%`。一个分不清「什么都没验证」和「什么都没通过」的指标，比没有指标更糟。→ [任务验证](docs/TASK_VERIFICATION.md)

### 派得动 · 小模型先干，主上下文保持干净

- 六个公开工种：Reader、Implementer、Tester、Ops Runner、Judge、Deep。定位、阅读、日志、Git 追溯自动派给小模型，主上下文不被塞满。
- 三档路由是 Runtime 强制策略，不是建议；1M 的 `deep` 档**申请制**，必须提交带低档尝试证据的升级票据。
- 路由不焊死：TUI `/routing` 与 Web 面板都能改 Root / Worker / Deep 的 Provider、模型、窗口与预算，也能一键恢复推荐映射。手改 `config.toml` 仍是一等路径，并发保存会检测冲突。→ [子 Agent](docs/SUBAGENTS.md)、[模型三档](docs/MODEL_TIERS.md)

### 跑得住 · 任务活得比你的终端久

- 常驻 Runtime Daemon 在自己进程内跑任务。关掉 CLI、断开 SSH、刷新浏览器，活儿照跑；回来按事件游标续上，一条消息都不丢。
- `daemon upgrade` 排空在途工具后无损接管，升级二进制不用杀任务。这条路径有真 Daemon 的端到端测试盯着。
- 终端、浏览器、手机共用同一套 Session、审批和 Agent 状态：在终端起个头，通勤路上用手机批一条审批，回家在浏览器里看结果。
- 轮次运行中可以插话：提示词连同附件排队，`Esc` 随时中断，队列立刻续上。→ [Runtime Daemon](docs/RUNTIME_DAEMON.md)

### 藏得住 · 看得见的进度，看不见的秘密

- 工具调用、Token、耗时、Diff 归属全部结构化上报；Prompt、命令、工具参数、工具输出、本地路径**不下发到 Web 前端，也不进日志**。
- 写入持久日志前，参数里的凭据按命令审批同一套规则打码，超长输出截断成有界摘要。
- **自带模型，不锁厂商。** OpenAI Chat Completions、OpenAI Responses、Anthropic Messages 三种线格式，加上 some.im 一键登录。Provider 身份和线格式是两个独立维度，可以自由组合。→ [认证与凭据](docs/AUTHENTICATION.md)

---

## 能力一览

| | |
|---|---|
| **模型** | Chat Completions · Responses · Anthropic Messages · some.im |
| **工具** | 文件搜索/读写/精确编辑 · Git 状态/Diff/Blame · Shell · 后台 Job · Web 搜索与抓取 |
| **界面** | Ratatui TUI · React Web · 手机中继 · NDJSON 自动化输出 |
| **扩展** | `SKILL.md` 技能 · MCP（stdio 与 Streamable HTTP，远程服务 OAuth 登录） · 项目上下文文件 · 插件（与 macOS 版共享插件包） |
| **协作** | 持久 Session/Turn · 历史会话检索 · Fork 与归档 · 多工作区 · 子 Agent 树 · 接住 macOS 版交接的会话 |
| **审查** | Diff 快照与归属 · Worktree 审查合并 · Commit Preview · 安全撤销 · `/rewind` 检查点回退 |
| **闸门** | 三档工作区策略 · 静态规则 + AI 判官两级命令审批 · 持久 Always Allow · OS 级围栏（写入 + 网络） · 门禁 Hooks · `willdeep audit export` 审计报告 |
| **遥测** | 子 Agent 判定落盘 · Skill Coverage / Verified Success / Escalation Rate · 实弹靶场 |
| **语言** | 简体中文 · English · 日本語 |

---

## 数据说话

小模型派工到底修不修得动？这里不放形容词，放靶场跑出来的数：真 Provider、真缺陷、真 `cargo` 退出码，**verifier 通过且测试块逐字未改**才算成功。口径与样本见 [小上下文 Skill Worker](docs/SKILL_WORKERS.md)，原始数据在 [`bench/skill-worker-range/`](bench/skill-worker-range/)。下面这段由 `ruby scripts/range_trend.rb --inject` 生成，别手改。

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

靶场是实验室，线上是另一回事：日常使用中 Runtime 真实派出去的子 Agent，每周一拍一张快照，只有计数和比率，没有 prompt、路径、agent id。刚起步，样本很少，口径见 [线上派工指标](docs/AGENT_METRICS.md)。

<details><summary>线上派工每周快照（由 <code>ruby scripts/agent_metrics_trend.rb --inject</code> 生成，别手改）</summary>

<!-- agent-metrics:begin -->
最近快照：**2026-09-20T17:04:54Z** · 窗口 7d · 代码 `b5d1fcc` · 版本 `0.78.0-rc26`

| 指标 | 近 7d | 对比上次 | 目标 | 趋势 |
|---|---|---|---|---|
| **Deep Share** | 0% | — | ≤ 5% | `▄` |
| Skill Coverage | 60% | — | ≥ 50% | `▄` |
| **Worker Verified Success** | - | — | ≥ 85% | `·` |
| Escalation Rate | - | — | ≤ 15% | `·` |
| 只读工种引用准确率 | - | — | — | `·` |

近 7d：子 Agent 运行 5（窄工种 3 · 标准 2 · deep 0）· 有 verifier 0 · 未验证 5 · 平均尝试 -
累计：子 Agent 运行 11 · Deep Share 0% · Worker Verified Success 0%（0/2）· 未验证 9

<details><summary>历史 1 次快照</summary>

| 时间 | 代码 | 窗口 | 子运行 | Deep Share | Skill Coverage | Verified Success | Escalation | 引用准确率 | 平均尝试 |
|---|---|---|---:|---|---|---|---|---|---|
| 2026-09-20T17:04:54Z | `b5d1fcc` | 7d | 5 | 0% | 60% | - | - | - | - |

</details>
<!-- agent-metrics:end -->

</details>

---

## 边界

同一份诚实：这些是**现在没有**的，别在评审会上被它们绊倒。

- **OS 级围栏不限制读取，网络没有中间档。** 进程读什么不管（`~/.aws/credentials` 读得到），网络只有通 / 断两档，不能只放某个域名。`[[hooks]]` 命令、MCP stdio 子进程和宿主自己的 `git` / `rg` 调用不在围栏里；没装 bubblewrap 的 Linux 机器没有围栏。
- **Hooks 只有三个触发点。** `pre_tool` / `post_tool` / `approval_resolved`，其中 `approval_resolved` 还没接线；hook 只能放行或拦截，改不了参数。
- **MCP 不接服务端主动请求。** `sampling/*`、`roots/*` 和 GET 事件流一律不处理：宿主没有替远端代发模型请求的授权。
- **检查点只到轮次粒度，只认 git 仓库。** 一轮之内逐个工具的改动仍靠 `/diff` 的归属记录；不是 git 仓库的工作区只能回对话。
- **不含 Computer Use 与 Browser Use。**
- **Web 模式是单用户模式，没有应用层鉴权。** 见下方安全须知。

大众终端 Agent 那条赛道我们不抢。要的是私有化、主权、异构小模型编排这一段。

---

## 文档

完整文档在 **[docs/](docs/README.md)**，按「先会用、再懂原理」排。常用入口：

| 想做什么 | 看这里 |
|---|---|
| 在终端里用顺手 | [TUI 使用指南](docs/TUI_GUIDE.md) · [CLI 参考](docs/CLI_REFERENCE.md) |
| 在浏览器 / 手机上用 | [Web 端使用指南](docs/WEB_GUIDE.md) · [手机中继](docs/MOBILE.md) |
| 接自己的模型 | [配置指南](docs/CONFIGURATION.md) · [认证与凭据](docs/AUTHENTICATION.md) · [some.im 集成](docs/SOMEIM_INTEGRATION.md) |
| 管住它 | [审批与自动化](docs/APPROVALS.md) · [OS 级围栏](docs/SANDBOX.md) · [生命周期挂钩](docs/HOOKS.md) · [审计导出](docs/AUDIT_EXPORT.md) |
| 让小模型干活 | [子 Agent](docs/SUBAGENTS.md) · [小上下文 Skill Worker](docs/SKILL_WORKERS.md) · [线上派工指标](docs/AGENT_METRICS.md) |
| 扩展它 | [Skills 与 MCP](docs/SKILLS_AND_MCP.md) · [插件系统](docs/PLUGINS.md) · [Runtime 控制 API](docs/RUNTIME_CONTROL_API.md) |
| 出问题了 | [故障排查](docs/TROUBLESHOOTING.md) |

---

## 安全须知

- API Key 优先用 `api_key_env`；配置里出现明文 `api_key` 时，Unix 下文件权限必须是 `0600`，否则拒绝启动。
- Runtime 控制面只监听回环，认证 Token 不会传给 Shell 或 MCP 子进程。
- **Web 模式是单用户模式，没有应用层鉴权。** 跨机器访问必须由 Nginx、VPN 或 SSH Tunnel 提供认证与 HTTPS，不要把端口直接暴露公网。
- 手机配对二维码明文携带中继 Token，只对自己的设备扫码。

---

## 参与开发

从源码构建需要 Rust 1.94、Node.js 22 与 Yarn。Web 前端会嵌进二进制，所以先构建前端：

```bash
cd web && yarn install --frozen-lockfile && yarn build && cd ..
cargo build --release          # 产物在 target/release/willdeep
```

提交前把这四行跑绿：

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
ruby scripts/test/range_report_test.rb && ruby scripts/test/range_trend_test.rb
```

三种 Provider 协议均有本地 Mock HTTP 契约测试，覆盖完整工具往返，不调用真实 API，也不消耗 Key。真模型的实弹靶场默认 `#[ignore]`，不在 CI：它要真凭据、要网络、每轮都花钱，跑法见 [`bench/skill-worker-range/`](bench/skill-worker-range/)。

架构见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)，功能台账见 [PRODUCT_OVERVIEW.md](PRODUCT_OVERVIEW.md)，每个版本改了什么见 [CHANGELOG.md](CHANGELOG.md)。Issue 与 PR 都欢迎；涉及安全边界的改动，请同步更新对应文档，宁可写「当前不支持」，也不写模糊的承诺。

---

## 许可证

Apache License 2.0。`WillDeep` 名称和商标不随源代码许可证授权。

如果「能查账的自主」正是你在等的那种 Agent，点个 ⭐ 让更多人看到它。
