# WillDeep CLI

**敢让它自己跑，因为跑完你能查账。**

自托管的 AI Coding Agent。模型可以不出国，每一行改动能追溯到是哪个 Agent、哪次工具调用改的，弱模型交上来的活由退出码裁决而不是自我声明。一个二进制，三种界面，任务不会因为你关掉窗口就死掉。

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

```bash
willdeep --workspace . "检查当前仓库并修复测试"
```

---

## 为什么是 WillDeep

自主性不稀缺，**敢用的自主性**才稀缺。下面六件事是同一件事的六个面：让机器自己动手，同时让你事后查得清、当场拦得住。

### 查得清 · 每一行改动都有出处

Diff 快照精确到**是哪个 Turn、哪个 Agent、哪次工具调用**改的这一行。不是"AI 改了 37 个文件"，是"第 4 轮里 `test_fixer` 的第 2 次 `edit_file` 动了这 6 行"。

审查面板里能逐文件通过 / 拒绝 / 要求修改，能在提交前看 Commit Preview（只预览，不执行），能对单个文件安全撤销——撤销走回收区，不是 `git checkout` 一把梭。写入型子 Agent 在专属 Git Worktree 里干活，审查通过才允许合并；主干动过同一个文件就直接判定不可合并。

### 拦得住 · 改你的代码这件事，得先过你这关

三档工作区策略（只读 / 智能审批 / 可写），写文件、跑 Shell、调 MCP、访问网络各有独立闸门。策略由服务端强制，**客户端无法自报可写**。

命令闸门是两级的：静态规则先判，危险形状（`rm` / `shred` / `sudo` / `mkfs` / `reboot`、`git push --force` 与 `reset --hard`、fork 炸弹、带标志的 `cp` 与 `ln`）**永不送给模型裁决**，直接交给你。分类器故意保守：解析不了的一律降级为"要判断"，一个 bug 的代价是多弹一张审批卡，不是一次没人过目的 `rm -rf`。反过来也拿捏得住——`grep "rm -rf" log` 是搜索，不是删除，引号里的危险词是数据。

剩下的模糊地带交给一次有界的 AI judge 调用。judge 看到的是不可信文本，所以凭据在本地先脱敏、不可信字段用 XML 包起来并打断闭合标签、回复只认单个 `<verdict>` 标签——一条把 "YES" 回显出来的命令没法冒充模型的判断。

"始终允许"只记住**规范化后的完整命令**，不是命令前缀。给自己留前缀后门，等于没有闸门。

### 说得准 · 谁干的活谁不判

子 Agent 的验证命令由 Runtime 亲自执行，**退出码是唯一裁决**，worker 不自证。绿了还不够：靶场会逐字比对测试块，把测试删掉也能变绿，而那是最省力的通关方式——作弊的不算通过。

没有退出码的只读工种（定位符号、查日志、追 Git 真凶）也不放它们隐身：Runtime 纯代码抽查报告里点名的路径、行号、commit 是否真实存在。它只证伪"地名是编的"，不证明"答对了"，所以引用准确率和答对率**分开算**。

**「未验证」是独立于通过和失败的第三种答案。** 把它并进任何一边，指标就开始自我恭维。分母为 0 时打 `-` 而不是 `0%`——一个分不清"什么都没验证"和"什么都没通过"的指标，比没有指标更糟。

### 派得动 · 小模型先干，主上下文保持干净

定位、阅读、日志、Git 追溯自动派给 `someim-32b-<工种>`，普通编码由 GLM-5 控制；1M 的 `deep` 档**申请制**，必须提交带低档尝试证据的升级票据。九种子 Agent 各自绑定上下文窗口、Token 预算和熔断阈值。

这套三档路由是 Runtime 强制策略，不是建议——它是为"模型不出国、机房里只有 S 和 M 两档"那个场景设计的。上下文压缩下沉到网关托管，本地保留两层兜底。

路由不焊死：TUI `/routing` 与 Web 面板都能持久修改 Root / Worker / Deep 的 Provider、模型、窗口与预算，也能一键恢复推荐映射。手改 `config.toml` 仍是一等路径，并发保存会检测冲突，不拿旧页面盖掉新配置。

### 跑得住 · 任务活得比你的终端久

常驻 Runtime Daemon 在自己进程内跑任务。关掉 CLI、断开 SSH、刷新浏览器，活儿照跑；回来按事件游标续上，一条消息都不丢。升级二进制不用杀任务——`daemon upgrade` 排空在途工具后无损接管，这条路径有真 Daemon 的端到端测试盯着。

终端、浏览器、手机共用同一套 Session、审批和 Agent 状态。在终端起个头，通勤路上用手机批一条审批、补一句指令，回家在浏览器里看结果。轮次运行中输入不再被封死：只改本地显示的立即执行，提示词连同附件排队，其余给明确原因而不是沉默。`Esc` 随时中断当前轮次，队列立刻续上。

### 藏得住 · 看得见的进度，看不见的秘密

工具调用、Token、耗时、Diff 归属全部结构化上报。而 Prompt、命令、工具参数、工具输出、本地路径**不下发到 Web 前端，也不进日志**。

失败详情走本机授权接口按未脱敏口径单独取回——调用方本来就持有 Runtime Token，能导出整段转录，这条边界是清醒划的，不是漏的。写入持久日志前，参数里的凭据按命令审批同一套规则打码，超长输出截断成有界摘要。

**自带模型，不锁厂商。** OpenAI Chat Completions、OpenAI Responses、Anthropic Messages 三种线格式，加上 some.im 一键登录。Provider 身份和线格式是两个独立维度，可以自由组合——比如用 some.im 的鉴权跑 Anthropic 格式。

---

## 实测数字

小模型派工到底修不修得动？这里不放形容词，放靶场跑出来的数——真 Provider、真缺陷、真 `cargo` 退出码，**verifier 通过且测试块逐字未改**才算成功。口径与样本见 [小上下文 Skill Worker](docs/SKILL_WORKERS.md)，历次原始数据在 [`bench/skill-worker-range/`](bench/skill-worker-range/)。

下面这段由 `ruby scripts/range_trend.rb --inject` 生成，别手改。

<!-- range:begin -->
靶场还没有跑过（`bench/skill-worker-range/history.jsonl` 为空）。

```bash
ruby scripts/skill_worker_range.rb
```

它会真的调用 Provider、真的花钱，跑完自动归档并在这里长出趋势。
<!-- range:end -->

---

## 30 秒上手

```bash
# 1. 构建（前端会嵌入二进制）
cd web && yarn install --frozen-lockfile && yarn build && cd ..
cargo build --release

# 2. 登录
willdeep --onboarding

# 3. 开干
willdeep --workspace .
```

不带 Prompt 启动进入终端界面；想用浏览器就加 `--web`；想跑自动化就用 `willdeep run`。

```bash
willdeep --web --workspace /path/to/project     # 浏览器界面，默认 127.0.0.1:9847
willdeep run --output json "总结当前风险"        # 自动化，稳定退出码
```

详细步骤见 [安装与构建](docs/INSTALL.md) 与 [配置指南](docs/CONFIGURATION.md)。

---

## 能力一览

| | |
|---|---|
| **模型** | Chat Completions · Responses · Anthropic Messages · some.im |
| **工具** | 文件搜索/读写/精确编辑 · Git 状态/Diff/Blame · Shell · 后台 Job · Web 搜索与抓取 |
| **界面** | Ratatui TUI · React Web · 手机中继 · NDJSON 自动化输出 |
| **扩展** | `SKILL.md` 技能 · MCP stdio server · 项目上下文文件 |
| **协作** | 持久 Session/Turn · 历史会话检索 · Fork 与归档 · 多工作区 · 子 Agent 树 |
| **审查** | Diff 快照与归属 · Worktree 审查合并 · Commit Preview · 安全撤销 |
| **闸门** | 三档工作区策略 · 静态规则 + AI judge 两级命令审批 · 持久 Always Allow |
| **遥测** | 子 Agent 判定落盘 · Skill Coverage / Verified Success / Escalation Rate · 实弹靶场 |
| **语言** | 简体中文 · English · 日本語 |

---

## 边界

同一份诚实：这些是**现在没有**的，别在评审会上被它们绊倒。

- **无 OS 级沙箱。** 隔离靠审批闸门 + 静态规则 + AI judge，不靠 Seatbelt / Landlock。对金融、政务这类客户，这是准入项缺失。
- **无 hooks。** 目前只有通知 webhook，接审计 / 合规 / CI 门禁还缺标准挂载点。
- **MCP 只有 stdio。** 没有 Streamable HTTP，没有 OAuth。
- **无 checkpoint / rewind。** 回退靠 Diff 审查 + 安全撤销，是合格替代，但长任务的可观测性弱于逐轮快照。
- **不含 Computer Use 与 Browser Use。**
- **Web 模式是单用户模式，没有应用层鉴权**（详见下方安全须知）。

大众终端 Agent 那条赛道我们不抢。要的是私有化、主权、异构小模型编排这一段。

---

## 文档

完整文档在 **[docs/](docs/README.md)**。常用入口：

- [TUI 使用指南](docs/TUI_GUIDE.md) — 快捷键、鼠标（含 SSH / tmux）、斜杠命令
- [Web 端使用指南](docs/WEB_GUIDE.md) — 界面功能、JSON API、安全模型
- [CLI 参考](docs/CLI_REFERENCE.md) — 完整命令树与退出码
- [配置指南](docs/CONFIGURATION.md) — TOML、Provider 与 API 两个维度
- [认证与凭据](docs/AUTHENTICATION.md) — Key 解析链、登录、Token 边界
- [some.im 集成](docs/SOMEIM_INTEGRATION.md) — 登录、视觉回退、联网搜索
- [Runtime Daemon 与工作区](docs/RUNTIME_DAEMON.md) — 常驻控制面
- [子 Agent 与后台任务](docs/SUBAGENTS.md) — 九种工种、模型路由与 Worktree
- [小上下文 Skill Worker](docs/SKILL_WORKERS.md) — 派工纪律、Verifier 闭环、实弹靶场
- [审批与自动化](docs/APPROVALS.md) — 三档模式与 CI 用法
- [故障排查](docs/TROUBLESHOOTING.md) — 出问题先看这里

---

## 安全须知

- API Key 优先用 `api_key_env`；配置里出现明文 `api_key` 时，Unix 下文件权限必须是 `0600`，否则拒绝启动。
- Runtime 控制面只监听回环，认证 Token 不会传给 Shell 或 MCP 子进程。
- **Web 模式是单用户模式，没有应用层鉴权。** 跨机器访问必须由 Nginx、VPN 或 SSH Tunnel 提供认证与 HTTPS，不要把端口直接暴露公网。
- 手机配对二维码明文携带中继 Token，只对自己的设备扫码。

---

## 参与开发

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
ruby scripts/test/range_report_test.rb && ruby scripts/test/range_trend_test.rb
```

三种 Provider 协议均有本地 Mock HTTP 契约测试，覆盖完整工具往返，不调用真实 API，也不消耗 Key。真模型的实弹靶场默认 `#[ignore]`，不在 CI——它要真凭据、要网络、每轮都花钱，跑法见 [`bench/skill-worker-range/`](bench/skill-worker-range/)。

架构见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)，功能台账见 [PRODUCT_OVERVIEW.md](PRODUCT_OVERVIEW.md)。

---

## 许可证

Apache License 2.0。`WillDeep` 名称和商标不随源代码许可证授权。
