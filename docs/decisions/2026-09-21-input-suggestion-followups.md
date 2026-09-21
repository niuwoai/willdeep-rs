# 需求单：rc29 收尾——发布、Web 端下一句预测、README 演示、预测质量实弹（2026-09-21）

> 状态：待实现 | 基于 develop @ 0.78.0-rc29（**工作树未提交**） | 关联：`docs/EXPERIENCE_BASELINE.md` 第 19 项、`docs/TUI_GUIDE.md`「轮次结束后的下一句预测」、`CHANGELOG.md` `[0.78.0-rc29]`
> 这份文件是给新会话直接开工用的：每一项都有背景、目标、验收、落点与边界。按顺序做，A 不做完其余都挂在未提交的状态上。

## 现状（新会话开工前先核对）

2026-09-21 在 develop 工作树上完成但**没有 commit** 的三件事：

1. README 按「第一屏法则」重排（结构变了，口径没变；`range` / `agent-metrics` 两对注入标记原样保留）。
2. TUI「轮次结束后预测下一句」：`crates/willdeep-core/src/input_suggestion.rs`（新文件）、`Agent::with_input_suggesters` / `suggest_next_input`、`crates/willdeep-cli/src/tui/*` 的世代号与 Tab / Esc、`[agent] input_suggestions` 开关。与 macOS 版 Xedit 1.385.0-rc1 `AgentInputSuggestion.swift` 同一契约（独立一次小请求、标题那一档模型、最近 2 条用户原话各 400 字 + 助手尾部 1500 字、Tab 只填入不发送、晚到丢弃、不落盘不进协议）。
3. 版本 0.78.0-rc29：`Cargo.toml`、`Cargo.lock`、`web/package.json`、`PRODUCT_OVERVIEW.md`、`docs/EXPERIENCE_BASELINE.md`、`CHANGELOG.md`。

已验证：`cargo fmt --check`、`cargo clippy --workspace --all-targets -D warnings`、`cargo test --workspace`（1123 通过 0 失败）、Ruby 脚本测试、README 链接与徽章可达、注入脚本幂等。`~/.cargo/bin/willdeep` 已是 rc29，rc28 备份在 `~/.willdeep/bin-backup/willdeep-0.78.0-rc28-before-input-suggestion-20260921113144`。

先跑一遍确认工作树还是这个状态：

```bash
git status --short && git diff --stat | tail -1 && ~/.cargo/bin/willdeep --version
```

---

## A. 提交并发布 rc29（必做，最先做）

**背景**：规则不允许直接在 develop 上 commit；一个 PR 不混 feat / fix / refactor；发布 tag 只能指向已合入默认分支的提交，且 tag、CHANGELOG、`PRODUCT_OVERVIEW.md` 版本一致。`.github/workflows/release.yml` 由 `v*` tag 触发，产四个平台发行包并 `gh release create`；**流水线里没有版本一致性校验**，一致性靠人核。

**目标**：把未提交的改动以可审的粒度进 develop，打 `v0.78.0-rc29`，Release 产物可下载。

**做法**：

1. 从 develop 开分支 `feat/input-suggestion-rc29`。
2. 两个 commit，边界按文件分：
   - `docs(readme): 按第一屏法则重排，发行包安装前置（0.78.0-rc29）` — `README.md` 以及 CHANGELOG 里 README 那一条。
   - `feat(tui): 轮次结束后预测下一句，Tab 采用（0.78.0-rc29）` — 其余全部（core / cli / docs / 版本号文件 / `config.example.toml`）。
   - 提交信息末尾带 `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`。
3. 开 PR 到 develop，描述里列验证结果（上面那段）；PR 描述末尾 `🤖 Generated with [Claude Code](https://claude.com/claude-code)`。
4. 合并后在 develop 的合并提交上打 `v0.78.0-rc29` 并推送 tag；等 Release 工作流跑完。

**验收**：
- `git log --oneline -3 develop` 能看到两个 commit；`git tag --points-at HEAD` 含 `v0.78.0-rc29`。
- `gh release view v0.78.0-rc29 --json assets --jq '.assets[].name'` 列出 `willdeep-linux-amd64.tar.gz`、`willdeep-linux-arm64.tar.gz`、`willdeep-macos-universal.tar.gz`、`willdeep-windows-x64.zip`。
- `curl -fsSL https://github.com/niuwoai/willdeep-rs/releases/latest/download/willdeep-macos-universal.tar.gz | tar -tz` 输出 `willdeep`（README 第一屏的安装命令靠这个）。
- `grep -c "0.78.0-rc29" Cargo.toml web/package.json PRODUCT_OVERVIEW.md CHANGELOG.md` 每个 ≥ 1。

---

## B. 常驻 Runtime 升级到 rc29（一条命令，人来敲）

**背景**：本机 Runtime Daemon 仍是 rc28（安装时 `daemon tasks` 里有一条 `WaitingApproval`，在 `~/Sites/sso/sdk/python`，所以没有替它做决定）。rc29 的 TUI 对着 rc28 的 Daemon 会在开屏报版本不一致。

**做法**：处理完那条待审批任务后：

```bash
willdeep daemon upgrade
```

**验收**：`willdeep daemon status` 显示 `version=0.78.0-rc29`；`willdeep daemon tasks` 里升级前存在的任务一条不少；TUI 开屏不再有版本不一致提示。

**回滚**（如果 rc29 有问题）：

```bash
install -m 0755 ~/.willdeep/bin-backup/willdeep-0.78.0-rc28-before-input-suggestion-20260921113144 ~/.cargo/bin/willdeep && willdeep daemon upgrade
```

---

## C. Web 端下一句预测（feature，进 0.78.0-rc30）

**背景**：TUI 已有，Web 没有；`docs/EXPERIENCE_BASELINE.md` 第 19 项 Web 列为「未做」。Web 的形态与 TUI 不同：聊天是 `POST /api/chat/stream` 返回 SSE（`crates/willdeep-cli/src/web.rs` 约 1600 行处把 Runtime 的 `turn.completed` / `turn.partial` 转成 `{"type":"completed"|"partial","text",...}` 后流就结束了），Web 服务端**不持有**带标题 Provider 的 `Agent`（`web.rs` 里没有 `titling::` / `with_titlers`），只是 Runtime 的一个客户端。

**目标**：与 TUI 行为一致——轮次正常收尾后，空 Composer 里灰字显示一句预测，`Tab` 填入不发送，打字 / `Esc` / 新一轮开始即清；受同一个 `[agent] input_suggestions` 开关管；三种语言；不落盘、不进 Runtime 协议。

**推荐设计**（实现者可改，但改了要在 ADR 里写为什么）：

- **服务端**：新增 `POST /api/sessions/{id}/input-suggestion`，请求体 `{ "turn_id": "<uuid>" }`，响应 `{ "suggestion": string | null }`。服务端从 `$WILLDEEP_HOME` 读该会话最新消息（`latest_assistant_text` 已经这么做过，照抄它的读法），用 `willdeep_core::input_suggestion::payload` 组正文，按 `harness.rs` 里装配标题 Provider 的同一段逻辑（本地模型 `prefer_for_titles` 优先，`title_model` 兜底）建 Provider 调 `predict`。开关关着直接回 `null`；会话正在跑（Runtime 里该会话有活动 Turn）直接回 `null`。**不要**把预测挂进 chat 的 SSE 流：流在 `completed` 后就关了，而且刷新页面后拿不回来；独立端点两者都解决。
- **前端**（`web/src/App.tsx` Composer 附近，`placeholder={busy ? t.steerPlaceholder : t.promptPlaceholder}` 那一行）：收到 `completed` / `partial` 且 `prompt` 为空、无附件、无排队时，调一次端点；请求带上发起时的 `turn_id` 与本地世代号，返回时若 `prompt` 非空、`busy` 为真、或世代号已变则丢弃。显示用灰字 ghost 层（不要拿 `placeholder` 硬塞，「Tab 采用」提示要比正文小一号、颜色 `var(--text-ghost)`），`onKeyDown` 里 `Tab` 且 `prompt === ""` 且有建议 → `preventDefault` + `setPrompt(suggestion)`；`Esc` 清；`onChange` 变非空清；新一轮开始清。
- **i18n**：`web/src/i18n.ts` 三种语言各加 `inputSuggestionAccept`（`Tab 采用` / `Tab to accept` / `Tab で採用`）。
- **文档**：`docs/WEB_GUIDE.md`「Composer」与「JSON API」加一段；`docs/EXPERIENCE_BASELINE.md` 第 19 项 Web 列改为已到位；`PRODUCT_OVERVIEW.md` 那条去掉「Web 端未做」；CHANGELOG `[0.78.0-rc30]`。

**验收**：
- Rust：端点单测——开关关回 `null`、会话在跑回 `null`、`payload` 为空回 `null`、Provider 失败回 `null` 且 HTTP 200（预测是装饰，不许 5xx）；不写任何文件（对比请求前后 `$WILLDEEP_HOME` 的 mtime）。
- 浏览器回归：照 `docs/WEB_RUNTIME_RETRY_QA.md` 的模式，给 `scripts/web_runtime_retry_fixture.mjs` 加该端点的假响应，新增 `scripts/web_input_suggestion_test.cjs`：完成后灰字出现；`Tab` 后输入框等于建议且**没有**发出 `/api/chat/stream` 请求；打字后灰字消失；刷新页面后不复现（不持久化）。
- 三种语言切换后提示文案正确。
- `cargo test --workspace`、`cd web && yarn lint && yarn build` 全绿；版本号四处同步到 rc30。

**边界**：不做多候选、不做轮换、不做手机中继端；Xedit 侧不改（它自己有实现）。

---

## D. README 首屏加一张真实终端 GIF（docs）

**背景**：README 重排后最缺的是一张真实画面。仓库里唯一的图 `WillDeep-2026-08-19.png` 是「每日记录」分享卡，不代表产品主体。本机没有 `vhs` / `asciinema` / `agg`。

**目标**：首屏命令块下方一张 ≤ 3 MB 的 GIF，展示一次真实的 TUI 轮次：提交提示词 → 工具行逐条出现并折叠 → 回复 → 分隔线 → 空输入框里出现灰字预测。

**做法**：
1. `brew install vhs`（依赖 ttyd 与 ffmpeg，brew 会带上）。
2. 录制脚本入库：`docs/media/readme-demo.tape`，输出 `docs/media/readme-demo.gif`。参数：`Set Width 1000`、`Set Height 600`、`Set FontSize 14`、`Set Theme "Catppuccin Mocha"` 或等价深色主题，总时长 ≤ 25 秒，`Set TypingSpeed 40ms`。
3. 演示工作区用 `examples/` 下一个小仓库或临时 clone 一个几十行的样例，提示词用 README 第一屏那句「检查当前仓库并修复测试」。**真跑模型**（`SOMEIM_API_KEY` 走环境变量，`WILLDEEP_HOME` 指到临时目录，录完删），画面里不能出现任何 key、真实路径里的用户名、私有仓库名。
4. README 第一屏命令块后插入 `![WillDeep TUI 演示](docs/media/readme-demo.gif)`；`docs/README.md`「文档约定」加一句「GIF 由 `docs/media/readme-demo.tape` 生成，改了 TUI 手感就重录」。

**验收**：GIF 文件 ≤ 3 MB；`vhs docs/media/readme-demo.tape` 可重复生成；README 在 GitHub 上首屏（前 35 行）能看到它；`grep -rn "sk-\|SOMEIM_API_KEY=" docs/media/` 为空；画面里出现「Tab 采用」灰字（证明录的是 rc29+）。

**备选**：`asciinema rec` + `agg` 转 GIF，同样把 `.cast` 入库。

---

## E. 预测质量的实弹验证（测试 / bench）

**背景**：rc29 的预测逻辑只有单元测试盯着清洗规则，没跑过真模型。提示词是英文硬编码的，输出语言靠「跟用户走」这一条规则，没人验过它在 glm-5 / deepseek / 本地 gemma 上到底守不守。一个不知道好不好的功能默认开着，等于把口碑押在没测过的东西上。

**目标**：一套可重复的实弹样本 + 归档，回答三个问题：预测有多少是合理的、助手口吻和凭据漏网了没有、该 `NONE` 的时候是不是 `NONE`。

**做法**：
1. 样本集 `bench/input-suggestion/samples/*.json`，≥ 15 条，每条是一段裁好的对话（user / assistant 交替，最后一条是 assistant）加 `expect` 字段：`"suggest"`（应该给出一句）、`"none"`（任务已收口，应该 NONE）、`"reject"`（助手尾部埋了一个假 key `sk-test-…`，清洗必须拒）。覆盖中 / 英 / 日，覆盖「要不要我继续？」「是否合回 develop？」这类 yes/no 收尾，覆盖纯陈述式收尾。
2. 跑法照靶场：`willdeep-core` 里一个 `#[ignore]` 的实弹测试（参考 `crates/willdeep-core/src/livefire.rs` 的组织方式）逐样本调 `input_suggestion::predict`，打印 `样本 / 模型 / 原始输出 / 清洗后 / 耗时 / token`；`scripts/input_suggestion_eval.rb` 收集输出写 `bench/input-suggestion/runs/<时间戳>-<模型>.json` 并向 `bench/input-suggestion/history.jsonl` 追加一行摘要（含 `commit`、`dirty`，规则同 `bench/skill-worker-range/README.md`）。
3. 人工判定一列 `judged`（`plausible` / `wrong-voice` / `off-topic` / `wrong-language`），先手填；自动指标只算能自动算的：`expect=reject` 命中率、`expect=none` 命中率、清洗拒绝率、平均耗时与 token。
4. 结论写回 `docs/TUI_GUIDE.md` 那一节末尾（一句话 + 指向 `bench/input-suggestion/`），像 README 里靶场那样**分母为 0 打 `-`**。

**验收**：
- `expect=reject` 命中率 **100%**（凭据漏一个就是事故，不是指标）。
- `expect=none` 命中率 ≥ 80%；`expect=suggest` 中人工判 `plausible` ≥ 70%，`wrong-voice` = 0。
- 达不到就改提示词或清洗规则，再跑一轮，两轮都归档；历史里能看出改动前后的涨跌。
- 实弹测试默认 `#[ignore]`，不进 CI。

---

## F.（可选）英文 README

**背景**：仓库公开在 GitHub，README 中文；首屏已有一段英文摘要。星和 fork 的另一半来自英文读者。

**做法**：`README.en.md`，按 `~/.willdeep/skills/github-readme/references/structure.md` 的英文骨架，内容以中文版为准翻译，不另起炉灶；两份 README 标题下互链（`简体中文 | English`）。注入标记区块（靶场数据）英文版不放，改为一句话链接到中文版对应小节，免得两处都要注入。

**验收**：`README.en.md` 链接全部可达；中文版改结构时能一眼看出英文版哪段要跟；`docs/README.md` 文档约定里注明「英文版跟随中文版，不单独维护口径」。

---

## G.（可选，顺手）核实 `cargo install --git` 这条安装路径

**背景**：`docs/CI_INTEGRATION.md`「安装」写了 `cargo install --git … willdeep`。但 `web/dist` 不入库，`crates/willdeep-cli/src/web.rs` 用 `#[derive(RustEmbed)] #[folder = "../../web/dist"]`，rust-embed 在 release 构建时目录不存在会直接编译失败——这条命令大概率在干净机器上装不上。README 重排时有意没写它。

**做法**：在一个没有 `web/dist` 的临时 clone 里跑一次；装不上就把文档改成「先 `yarn build` 再 `cargo install --path crates/willdeep-cli`」或给 `build.rs` 加缺目录时的明确报错。

**验收**：文档里写的每条安装命令在干净环境跑过一次；结果记进 CHANGELOG。

---

## 开工顺序与版本

A → B（人敲一条命令）→ C（rc30）→ E（可与 C 并行，不改版本，只加 bench 与 `#[ignore]` 测试）→ D（docs，不改版本或随 rc30）→ F / G 可选。

版本规则按本仓惯例：0.78.0 这条 rc 线上功能与修复都只递增 rc 号；C 是功能，进 `[0.78.0-rc30]`；D / E / G 单独提交时不改版本，随最近的 rc 一起记 CHANGELOG。

## 相关文件速查

| 要改什么 | 看哪里 |
|---|---|
| 预测的核心与清洗 | `crates/willdeep-core/src/input_suggestion.rs` |
| Agent 的预测 Provider 装配 | `crates/willdeep-core/src/agent.rs`（`with_input_suggesters`）、`crates/willdeep-cli/src/harness.rs`（标题 / 预测共用装配段） |
| TUI 的世代号与按键 | `crates/willdeep-cli/src/tui/app_state.rs`（`adopt_` / `accept_` / `dismiss_input_suggestion`）、`tui/dispatch.rs`（`dispatch_input_suggestion`）、`tui/event_loop.rs`、`tui/runtime_ui.rs`（`runtime_turn_settled`） |
| Web 服务端 | `crates/willdeep-cli/src/web.rs`（`latest_assistant_text`、`/api/sessions/{id}/rewind` 的路由写法） |
| Web 前端 | `web/src/App.tsx`（Composer）、`web/src/i18n.ts` |
| 浏览器回归 | `docs/WEB_RUNTIME_RETRY_QA.md`、`scripts/web_runtime_retry_fixture.mjs`、`scripts/web_*_test.cjs` |
| 靶场归档惯例 | `bench/skill-worker-range/README.md`、`scripts/range_report.rb` |
| README 方法论 | `~/.willdeep/skills/github-readme/SKILL.md`（Codex 里 `$github-readme`） |
