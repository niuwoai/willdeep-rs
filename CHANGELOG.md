# Changelog

## [0.27.0-rc1] - 2026-08-16

> 本版主题：**模型上下文三档切分（model-tiers.v1）与 skill→档位关联**。完整设计见新增的 `docs/MODEL_TIERS.md`。

### Added
- **`docs/MODEL_TIERS.md`：三档切分设计**。S（32–64K，worker）/ M（~256K，会话默认）/ L（1M，申请制稀缺资源）。核心转向：**动机从省钱改为主权**——很多公司的数据不出机房，能私有化的开源权重主流在 32K–256K，1M 档几乎没有；所以体系必须在「只有 S+M」时完整可用，L 是加速器不是地基。路由纪律倒转为「默认 M、向下派工优先、向上用 L 要申请理由」，并给出 L 缺席时的三条降级路径（确定性消化、分片+归纳、降精度声明）。派工的两条维度先后有序：先问可验证性（决定能否下 S），再问真实材料量（决定要不要上 L）——500 行失败日志消化后 6 KB，不构成上 L 的理由。
- **`tier:` frontmatter：skill 显式声明自己的档位**。`SKILL.md` 里写 `tier: worker|standard|deep`（容错别名 small/medium/large，认不出的拼写静默忽略——技能是用户数据不是配置文件，别的工具写的技能不能因此坏掉）。`list_skills` 与技能摘要带出 `tier=…`，**派工决策在读技能正文之前就能做**：worker 技能走 spawn_agent + Task Packet，正文由 worker 自己 `read_skill` 拿，不占主会话窗口。系统提示词同步教会主会话这条路由。
- **技能库审计：29 个技能逐个过、逐个标**（已写入 `~/.willdeep/skills`）。S 档 11 个（image-processing、pdf-design、k8s-manifest-generator、harbor-prebuilt-image、pinchtab 等——可验证或强模板化）；M 档 14 个（诊断类、部署编排类——**执行段可下放、决策段不下放**，回滚等不可逆决策留主会话）；L 档 4 个（三个长篇创作 + goal-teams——几十万字伏笔一致性是真实的长上下文需求，且无 verifier 可依，不满足下放第一纪律）。每个 S 档技能都写明了验证方式，判定依据是两条维度，不是拍脑袋。
- 网关实测：`someim-1m` / `someim-256k` 目前均未配置（`model_not_configured`），已列入待建清单——建法同 `someim-32b` 虚拟模型，客户端零改动。

### Tests
- `tier:` 解析双向锁定：声明的档位必须带出，未知拼写与无声明必须沉默（不报错、不出现在摘要里）。

## [0.26.0-rc1] - 2026-08-16

> 本版主题：**与 macOS 版 Xedit（1.263.0-rc1）对齐**，外加只读工种第一次有了可统计的判定。完整对照表见 `docs/SKILL_WORKERS.md`「与 macOS 版 Xedit 的对照」。

### Added
- **托管工种模型：some.im 下每个工种跑自己的 `someim-32b-<工种>`**，与 Xedit 用同一张表（`test_fixer` → `someim-32b-test-fixer`，下划线转连字符）。此前 rs 侧七个工种共用一个裸 `glm-5`，同一个操作者从 CLI 和从 App 派同一个工种，落到的是两个不同的东西——而两边连的是同一个网关、同一批账号。职能提示词由网关 prepend，**客户端不再发自己那份**：一个工种有两份职能描述，等它们漂移的那天由模型挑该听谁的。客户端仍然发边界段（看不到父会话、不能问用户、不能派生、报告即返回值）——**服务端管职能，客户端管边界**。网关未托管的工种回落基础廉价档并由客户端发职能段：一个工种绝不能落到「一份职能描述都没有」。上线前逐个实测过网关：七个工种档全部可用，`security-guard` / `judge` / `reviewer` / `ops-runner` 返回 `model_not_configured`，故本版不引入这四个工种。
- **只读工种的弱验证：报告引用抽查。** `scout` / `reader` / `log_inspector` / `git_detective` 没有退出码，判定字段此前永远是 `None`——**永远「未验证」，在指标里永远隐身，而隐身读起来就像没问题**。它们的答案并非不可证伪：位置要么存在要么不存在。运行结束后 Runtime 纯代码抽查报告里的 `路径`、`路径:行号` 与 commit 哈希（`git cat-file`），结果写进报告尾部的 `<citation-check …>`（点名对不上的条目）与判定事件的 `claims_checked` / `claims_unverifiable` 两个字段，`willdeep daemon agent-metrics` 增加 `citation_accuracy` 一行，`daemon agent <id>` 增加 `citations` 一行。**它只证伪「地名是编的」，不证明「答对了」**；认不出来的 token 一律不计（否则量的是解析器不是 Worker）；`checked = 0` 不进分子也不进分母。
- **`git_detective` 拿到只读 git shell**，与 Xedit 的同名工种对齐。靶场实测暴露的问题：rs 给它的是四个固定 git 工具，而 `git_diff` 压根不接受 revision 参数——**「哪个 commit 改的」这个它得名的问题，用它手上的工具无法回答**，实测跑满 8 轮、9.4k token、一个结论都没出。现在放行 `run_command`，门禁是**形状**而非字面量：命令头必须是 `git`，且必须通过与主 Agent 同一套静态只读分类。修复后同一样本 1 轮答对、4 条引用全部核实通过。
- **靶场扩到 12 个样本**，新增两个只读样本（`scout` 在多模块里定位符号、`git_detective` 在三个 commit 的历史里定位真凶），报告新增「引用准确率」与「答对率」两列——**引用真实 ≠ 答对**，混在一起算的指标会自我恭维。`scripts/skill_worker_range.rb` 新增 `--report-only`（不花钱重渲染）。本版实测（`glm-5`，12 样本）：可验证样本 10/10 通过、平均 1.00 次尝试、0 作弊；只读样本引用准确率 4/4、答对率 2/2；单样本平均约 5 100 token / 19 秒。

### Fixed
- **补发 `X-Playground-Session-ID`**，与 Xedit 1.260.0-rc1 的同一处修复对齐。网关的用量账本只读这个头、不读我们自家的 `x-willdeep-session-id`，于是**全部流量的 `session_id` 都是空的**，Worker 的花费无法归到派它出去的那次会话——而这正是 Skill Worker 经济账赖以成立的那个数字。同一个不透明 UUID，随另一个头一起发。

### Tests
- 工种→模型映射与 Xedit 逐条对齐（`hosted_worker_model`），并锁定「托管时只发边界段、未托管时仍发客户端职能段」。
- 引用抽查双向锁定：编造的路径与越界的行号必须被抓，真实路径与真实行号不得误伤，散文不计数；解析不出的 commit 哈希被点名，真哈希放行。
- 只读 git shell 双向锁定：`git log -p` / `git show` / `git diff a b` 放行，`ls`、`git push`、`git commit` 拒绝。
- `X-Playground-Session-ID` 进入 provider 请求头回归测试。

### Docs
- 对照表随 Xedit 1.264.0-rc1 更新：引用抽查与实弹靶场两项已回流到 macOS 侧，两边判定纪律逐字一致（样本各按语言：rs 用 Cargo，Xedit 用 SwiftPM）。
- `docs/SKILL_WORKERS.md`：新增「模型绑定：托管工种档」「只读工种的弱验证」「与 macOS 版 Xedit 的对照」三节，工种表补 `git_detective` 的 shell 列，并记下 Xedit 侧派工率实测（358 个会话仅 5 个派过工，1.4%）——与 rs 侧 0 派工记录同向，**光有提示不够**。

## [0.25.0-rc2] - 2026-08-16

### Fixed
- **审批过的写入目标没做路径规范化，Worker 的正确补丁被判成越界写入**。`require_write_target` 把 `edit_file` 解析出的**规范化**路径，去比对**未规范化**的审批集合；workspace 上方只要有一层符号链接（macOS 的 `/var`、`/tmp`、软链的检出目录），两种写法永远对不上：Worker 每次编辑都被拒，而拒绝信息里点名的「你只能改这个文件」正是它刚刚请求的那个文件。**它没有任何办法自证清白，只能一轮一轮重发同一份正确补丁直到烧完轮次**。实测（`glm-5`、`build_missing_mut` 样本）：修复前跑满 8 轮、1.4 万 token、一个字没改；修复后 1 次尝试通过、4.7k token、13.9 秒。修法是在 `with_write_targets` 里把集合一并规范化——比较的两端必须是同一种写法。
- **verifier allowlist 的拒绝信息不告诉 Worker 什么才是对的**。典型近失是给验证命令加装饰（`cargo build 2>&1` 而不是 `cargo build`），而「不是这条」这种回答等于请它继续猜，每猜一次一轮。现在拒绝信息逐字引用允许的命令原文。

### Added
- **实弹靶场 `scripts/skill_worker_range.rb` + `crates/willdeep-core/src/livefire.rs`**。此前仓库里没有任何一处能证伪「可自动验证的小任务可以交给弱模型」这句话——单元测试用的是桩 Provider，而**桩 Provider 永远修得好**。靶场现建十个带真实缺陷的 Cargo 仓库（5 个逻辑缺陷派 `test_fixer`、5 个编译错误派 `build_fixer`），用真 Provider 派工、真 `cargo` 退出码判定，产出 JSON + Markdown 两份报告和每个样本的逐字记录。默认 `#[ignore]`、不进 CI：它要真凭据、真网络、真花钱。
  - **成功的定义是「verifier 通过**且**测试块逐字未改」**。退出码只知道绿了，不知道绿得干不干净；删掉测试是最省力的通关方式，而作弊不进分子。
  - **派工前先跑一遍 verifier 确认靶子是红的**。绿着的靶子测不出任何东西，报告会点名这类无效样本。
  - 逐字记录（Worker 发的每个工具调用参数 + Runtime 的每条回执）默认留存——上面那两个 Fixed 就是靠它看出来的，判定本身只会说「没通过」。

### Docs
- `docs/SKILL_WORKERS.md` 新增「实弹靶场」章节与首轮实测结果（`glm-5`，10/10 样本 100% 通过、平均 1.00 次尝试、0 作弊、单样本约 5 000 token / 17 秒），并写明这组数字**支持**什么结论、**不支持**什么结论：样本是十个几十行的独立小仓库，是难度谱系最容易的一端。

## [0.25.0-rc1] - 2026-08-15

### Added
- **子 Agent 判定遥测（Skill Worker 分期的 P3）**。上一版把 verifier 的结果写在报告文本里，人看得见、程序算不出——于是「Worker 到底靠不靠谱」这个问题只能凭印象回答，而凭印象回答的下一步通常是「感觉还行」。现在每次子 Agent 运行结束都发一条判定事件并落进 Runtime 的 agent 记录：

  | 字段 | 含义 | 公开 API |
  |---|---|---|
  | `verifier_passed` | `true` 通过 / `false` 未通过 / **`None` 压根没有 verifier** | ✅ |
  | `attempts` | 拿到判定前跑了几次 | ✅ |
  | `repo_commit` | 运行开始时的 HEAD——有了它一条记录就是一个可回放的 case | ✅ |
  | `verifier_command` | 用哪条命令判的 | ❌ 只留在 Runtime 本地状态文件 |

  **「没验证」是独立的第三种答案**，不是通过也不是失败。把它并进任何一边，指标就开始自我恭维：每份没验证过的报告都会凭空变成一次成功（或一次失败），而它两样都没挣到。命令原文不进公开记录——命令行会带路径和参数，而算指标根本不需要它。

- **`willdeep daemon agent-metrics`**：从 Runtime 现有的 agent 记录直接算三个北极星指标（Skill Coverage、Worker Verified Success、Escalation Rate），外加平均尝试次数与未验证运行数。没有另一套计数器，也就没有第二份会跟现实对不上的账。**分母为 0 时打印 `-` 而不是 0%**——「什么都没验证」和「什么都没通过」是两件事，一个分不清它们的指标比没有指标更糟。三条口径（含与 Xedit 设计的偏差）写在 `docs/SKILL_WORKERS.md`。
- `willdeep daemon agent <id>` 增加 `verdict` 行；TUI 在子 Agent 结束时提示验证结果与尝试次数。

### Changed
- 子 Agent 记录被复用时（重试、Session 续跑）清空上一轮的判定字段。让重试继承上一次的「通过」，等于把遥测变成一台专门生产好消息的机器。

### Tests
- 三种结局各发一条判定且互不混淆：无 verifier 报 `None`、验证失败报 `Some(false)` 并带真实尝试次数、通过报 `Some(true)`。
- Runtime 侧：判定事件落盘到 agent 记录；重试后判定被清空。
- 手工验证：旧格式 `agents.json`（完全没有新字段）能正常读取，指标计算与 `-` 分母行为符合预期。

### Docs
- `docs/SKILL_WORKERS.md` 新增「遥测与指标」章节与分期状态更新；`SUBAGENTS.md` 补 `agent-metrics`、判定说明，并把过时的 `context_window = 128000` 示例改成工种档位。

## [0.24.0-rc1] - 2026-08-15

### Added
- **小上下文 Skill Worker 体系**（对齐 macOS 版 Xedit 的 `skill-workers.v1` 设计，见新增的 `docs/SKILL_WORKERS.md`）。四件事一起落地，缺任何一件另外三件都不成立：

  - **窗口分档与 payload 上限**。此前四个工种全部共用会话的 128K 窗口，工具输出上限是全局的 128 KB 常量——**给一个廉价模型配 128K 窗口不会让它变强，只会让它把窗口烧穿**：一条 `cargo test` 的失败日志就能把它的上下文吃光，而这正是最需要它清醒的时刻。现在除 `deep` 外每个工种跑在自己的档位（16K / 32K / 64K），并各自限制工具输出字节数（3–6 KB）；`read_file` 与 git 系工具同样按档位收窄。`deep` 是刻意的例外：它跑父模型，因为它的活本来就装不进小窗口。
  - **Task Packet**。`spawn_agent` 新增可选参数 `task`：目标、已知事实、约束、相关文件和验证命令。相关文件由 Runtime 按档位预算（窗口 × 3/4 字节）读出并**内联进 Worker 的第一条消息**——Worker 每一轮 grep 都在烧窗口，而父 Agent 早就知道该看哪个文件。超预算与读不到的文件都会显式标注，不静默丢弃。**不传 `task` 时行为与以前逐字节相同**。
  - **Verifier 闭环：Worker 不自证**。带 `verifier` 的运行由 Runtime 在每次尝试后**亲自执行**验证命令，退出码是唯一裁决；「Worker 声称完成但从没跑过验证」这种情况不存在，Runtime 照样跑一遍再判。失败输出经纯 Rust 的确定性消化（失败聚焦段 + 尾部 20 行，**断言原文逐字保留不许转述**）后作为下一次尝试的简报回灌，每次尝试起一个干净的 Agent，只带走消化后的失败，不带走上一轮的死胡同。尝试打满（默认 3、上限 6）**判整个运行失败**并明确要求升档，而不是交一份读起来像成功的报告。
  - **文件集写通道**。`test_fixer` / `build_fixer` 必须能同时改测试和实现，单文件锁不够。写通道从 `Option<PathBuf>` 泛化为集合：一次审批整个集合（审批卡逐行列出，上限 8 个文件），越界写入拒绝**并告诉 Worker「要扩权就报告父 Agent 重新派工」**，运行中的 Worker 文件集互斥、冲突时点名冲突文件，锁随 `Drop` 释放（超时、取消、panic 都不会留下死锁）。`editor` 成为集合大小为 1 的特例，走同一段代码——一条通路，不开第二套。

- **四个新工种**：`test_fixer`（把失败测试修到绿，64K）、`build_fixer`（修编译/类型/lint，32K）、`log_inspector`（解释失败日志，16K，只读）、`git_detective`（回归定位与 commit 考古，32K，只读）。后两个同时对外部只读 Spawn（Runtime API、TUI `/agent spawn`、Web 侧栏）开放。
- **确定性派工触发**。主 Agent 的 `run_command` 返回非零退出码且命令形状匹配测试/构建特征时，工具结果尾部追加一段 `<delegation-hint>`，给出现成的派工配方。不强制、不自动派工——**决定权仍在父 Agent**，只是把派工成本压到一句话。子 Agent 永远收不到这个提示：它不能派生，给它提示只是给一条它执行不了的指令。
- `[subagents.*]` 新增 `tool_output_limit` 与 `max_attempts`；`context_window` 补上范围校验（4000–1000000）。

### Changed
- **Verifier 命令的门禁不是「必须只读」**。诚实的 verifier 本来就会写：build 产出目标文件，测试写 fixture。一刀切只读会把用户推回「相信模型自称通过」，那比放行更危险。改成与主 Agent `run_command` 同一条链：静态判定只读的直接放行，破坏性形状**直接拒绝且不给判官**（模型不能靠把命令改叫 verifier 就把 `rm -rf` 说进来），其余交 AI 判官（some.im 下是网关托管的 `someim-security-guard`）。**唯一不可退让的是「不能没有门禁」**——子 Agent 没有审批 UI，判官没配或掉线时答案是拒绝。
- 带 verifier 的 Worker 的 `run_command` 被收窄到**只能执行它自己的验证命令**，连静态安全的 `ls` 都不放行。无人值守的上下文里，一个能跑任意命令的 shell 没有主人。

### Tests
- Verifier 闭环：一个每次都自称完成、实际第三次才真干活的 Worker，必须跑满三次尝试才算通过——两次「自称成功」不能结束运行。
- 尝试打满时运行判失败，报告带尝试次数、要求升档，并保留失败输出原文。
- 不可判定的 verifier 在没有判官时被拒绝并说明原因；破坏性 verifier 直接拒绝、不经判官。
- Task Packet 内联：目标、事实、约束、验证命令与**文件正文**都进首条消息，读不到的文件被点名而非静默丢弃。
- 文件集锁：交集冲突被拒绝并点名文件，持有者结束后释放。
- 工种档位回归：只有 `deep` 继承会话窗口，其余工种窗口不超过 64K 且 payload 上限小于窗口。
- 失败输出消化：500 行编译噪音里的断言原文逐字保留，整体不超预算。
- 写通道：集合内每个文件可写、集合外拒绝且提示扩权路径；带 allowlist 的 Worker 只能跑验证命令；派工提示只出现在主 Agent 的失败测试命令上。

### Docs
- 新增 `docs/SKILL_WORKERS.md`：工种表、Task Packet 格式、Verifier 门禁两段规则、文件集锁三道门、配置项、分期现状与**与 Xedit 设计的差异记录**（冲突文件集在 rs 侧是拒绝并点名，不是排队）。
- `docs/SUBAGENTS.md` 工种表补窗口列与 Task Packet 小节；`docs/ARCHITECTURE.md` 子 Agent 段重写；`config.example.toml` 补四个新工种示例并把 `scout` 的窗口从 128000 改成 32768（原值正是本次要治的病）。

### Known gaps
- P3 遥测未做：verifier 结果目前以 `<verifier … />` 标记写在报告文本里，父 Agent 和 Runtime 都看得到，但**还不是可统计的结构化字段**。Worker Verified Success、Skill Coverage 这类 KPI 要等字段落进 transcript 才能算。

## [0.23.0-rc4] - 2026-08-14

### Fixed
- 所有 Provider 请求统一新增 `X-Client-Name: WillDeep CLI` 与同版本的 `X-Client-Version`，some.im 网关客户端排行榜不再把 WillDeep CLI 请求归为空客户端，BYOK 第三方端点也能明确识别调用方；原有 User-Agent、会话与工作区标识保持不变。

### Tests
- 新增客户端请求头回归测试，覆盖 some.im、OpenAI Compatible 与 Anthropic 三类 Provider，锁定名称与版本均随请求发出。

## [0.23.0-rc3] - 2026-08-14

### Changed
- **some.im 会话的 AI 判官改用网关托管的 `someim-security-guard`**，与 macOS 版 Xedit 对齐：同一个操作者从 CLI 和从 App 得到同一套判决，安全策略在服务端收紧即可生效，不必发客户端。此前 rs 侧固定用 `glm-5`（那是子 Agent 的廉价模型约定，从来不是安全模型），整个仓库连 `someim-security-guard` 这个字符串都不存在。其它 provider 维持不变：仍沿用当前会话模型——没有第二个端点可用，换模型等于换一套凭据，而拿不到凭据的判官等于没有判官。`[agent] judge_model` 仍可覆盖两者。

### Fixed
- 判官回复被截断时不再报成笼统的「没有 `<verdict>` 标签」。`someim-security-guard` 是推理模型（实测后端 `Qwen/Qwen3.6-27B`），出裁决前先写 1800–3100 字符的私有推理，命令越复杂推理越长；输出上限太小就会把回复截断在 `<verdict>YES` 这种没有闭合标签的半截上，甚至只剩空串。现在按 `finish_reason=length` 单独识别并写明「回复被截断，请调高输出预算」——**这个失败模式的方向最坏：越是需要判官的复杂命令越容易掉线**，笼统的错误原因会把预算问题伪装成模型不听话。rs 侧的 chat-completions 请求本就不下发 `max_tokens`，实测五类命令全部拿到完整裁决，故本次无需改预算，只补可诊断性。

### Added
- `approvals.jsonl` 的判官记录带上实际使用的模型（`AI review (someim-security-guard): …`、`AI review (…) unavailable: …`）。此前只记 `judge` 这个来源，判官换了模型或悄悄掉线在审计里完全看不出来，也无法与 Xedit 的 `judge.jsonl` 做同口径对比。新增 `SafetyJudge::model()`，非模型驱动的判官（测试桩）用默认值。

### Tests
- 新增 `someim_sessions_judge_with_the_managed_security_guard` 与 `every_other_provider_reuses_the_session_model`：锁住两分支的模型选择契约。
- 新增 `a_truncated_reasoning_reply_is_reported_as_a_budget_problem`：截断、空回复、无标签三种不可解析回复各自给出可区分的原因。

### Docs
- `docs/APPROVALS.md` 新增「判官用哪个模型」小节（两分支对照表 + 推理模型为什么不能设紧输出上限），审计说明补模型字段；`config.example.toml` 的 `judge_model` 示例同步。

## [0.23.0-rc2] - 2026-08-14

### Fixed
- Diff 批量审批不再冻住界面。此前按 `Y` 后 `handle_diff_attention_action` 在按键处理里逐文件 `await`，每个文件一次独立 HTTP 且每次都重新 `ensure_running` 探活——实测 15 个文件耗时 **27 秒**，全程 `draw()` 无机会执行，屏幕彻底静止，看着就像按键被吞了（`diff-reviews.json` 里那批记录时间戳从 +0s 排到 +27s，是完整证据；审批其实每次都生效了）。两处改：新增 `remote_review_many` 一次探活、复用同一个客户端跑完所有路径；批量提交整体挪进后台任务，弹窗立刻关闭并先给「正在提交 Diff 审批 · N」，完成后再报结果。鼠标点击 `[Y 通过]` / `[N 拒绝]` 走的是同一函数，同一修复覆盖。

- Inbox 详情弹窗支持 `M` 忽略条目，标题也写明这个键。此前 `M`（标记已读）只在焦点落在侧栏且选中「需要关注」分组时才生效，而弹窗里只认 `Esc`——于是一条几天前被中断的 Runtime 任务顶着「需要你处理」常驻，用户打开详情却发现无事可做、也无路可走。忽略按 Session 持久保存；运行中的条目拒绝忽略并给出说明，那是用户对它的唯一抓手。

### Changed
- 失败的后台任务保留 24 小时后也从 Attention Inbox 回收。此前只有「顺利完成」的会在 60 秒后回收，失败/超时/被杀的永久留存——一天前失败的命令早已不是待办，只是把真正需要关注的条目挤出视野。运行中的任务永不回收；要复盘去任务详情与历史里找。
- 右侧状态栏改为**默认隐藏**，新增 `/sidebar [on|off]` 显示/隐藏（`Ctrl+B` 等效）。隐藏时焦点自动回到输入框，不会卡在看不见的面板上。`/help`、F1 帮助面板与命令补全同步。

### Tests
- 新增 `sidebar_starts_hidden_and_the_command_toggles_it`：默认隐藏、切换、`on`/`off` 显式、隐藏后焦点回落、坏参数报用法而非静默切换。
- 新增 `inbox_items_can_be_dismissed_but_running_ones_cannot`：已结束条目可忽略且详情弹窗随之关闭；运行中的条目拒绝忽略。
- 新增 `background_tasks_are_recycled_by_status_and_age`：运行中永不过期；完成 60 秒后消失；失败/超时/被杀六小时仍在、一天后回收。

### Docs
- `docs/TUI_GUIDE.md`：布局表标注状态栏默认隐藏，新增 Inbox 回收规则说明，命令表补 `/sidebar`，快捷键表标注 `Ctrl+B` 等价关系。

## [0.23.0-rc1] - 2026-08-13

### Added
- TUI 新增 `/daemon [status|start|stop|upgrade]`，在界面内管理真正执行命令的 Runtime，不必另开终端。`upgrade` 会排空在途工作再交接，可能耗时数分钟，因此在后台任务里跑并把进度逐条送回对话，**不阻塞界面**。
- `/webapp` 补上 `stop`（并给 `start` 一个显式别名）。此前只能启动和看状态，停不掉——「启停」缺了一半。`stop` 只对本 TUI 启动并记录在案的 pid 发 `SIGTERM`，且先确认其记录地址仍在应答；状态文件过期时只清理文件，不会误杀继承同一 pid 的无关进程。
- **Runtime 版本不一致告警**。TUI 只是前端，命令由 Runtime Daemon 执行；一个几天前启动的 Daemon 会继续按它自己那版的审批策略跑，而 `willdeep --version` 显示的是客户端版本，说明不了实际执行方——本次开发中就因此白测了一轮（客户端 0.22.0-rc5，Daemon 仍是 0.21.0-rc62）。现在 `RuntimeSnapshot` 带上 `runtime_version`（`probe` 本就返回 `health.version`，此前被丢弃），侧栏「运行状态」在不一致时置顶黄色警告并常驻，首次发现时在对话里写一行说明。重复快照不刷屏；换到同版本后消失；换到另一个旧版本重新告警。

### Changed
- `daemon.rs` 的 `status` / `stop` / `upgrade` 改为返回消息字符串，`start` / `upgrade` 接受进度回调；CLI 传打印回调，TUI 传送信回调。这样 TUI 复用与 CLI **完全相同**的排空与交接逻辑，不必复制一份会腐化的实现，也避免 `println!` 冲掉 TUI 画面。

### Tests
- `daemon_commands` 2 项：五种 `/daemon` 写法解析正确；未知参数报用法，且 `/daemonize the thing`、`restart the daemon` 这类不被误吞。
- `tui/test_suite.rs` 3 项：`/daemon`、`/webapp` 各形态都走主循环而非兜底（不误报未知命令）；两者都在补全目录里；Runtime 版本不一致只告警一次、状态常驻、同版本后清除、换旧版本再次告警。

### Docs
- `docs/TUI_GUIDE.md` 命令表补 `/daemon`、细化 `/webapp`，新增「Runtime 版本不一致」一节。
- `docs/WEB_GUIDE.md` 补 `/webapp stop|start` 与其误杀防护说明。

## [0.22.0-rc5] - 2026-08-13

### Added
- 高频参数补齐短选项：`-c`（`--config`）、`-p`（`--profile`）、`-m`（`--model`）、`-r`（`--resume`），与 rc4 的 `-w` 一起构成常用五个。前四个继承各自的 `global = true`，子命令前后都能写；`-r` 与 `--resume` 一样只在顶层。长选项写法全部不变。

### Tests
- 新增 `high_frequency_arguments_have_short_forms`：五个短选项与对应长选项解析到同一字段、可组合使用。
- 新增 `workspace_defaults_to_the_current_directory`：不带 `--workspace` 时 `resolve_workspace` 落在**当前工作目录**（canonicalize 后）。此前这条只是实现细节（`resolve_workspace` 兜底 `PathBuf::from(".")`、Web 模式兜底 `std::env::current_dir()`），没有任何断言保护，改动别处很容易悄悄改掉它。

### Docs
- `docs/CLI_REFERENCE.md` 参数表补上 `-c` / `-p` / `-m` / `-r`，加一行短选项一览，并写明 `--workspace` 缺省即当前目录。

## [0.22.0-rc4] - 2026-08-13

### Added
- `--workspace` 新增短选项 `-w`（`willdeep -w ~/Sites/project`）。它继承原有的 `global = true`，子命令前后都能写。此前整个 CLI 没有定义过任何短选项，`-w` 不与现存参数冲突，长选项写法完全不变。

### Tests
- 新增 `workspace_accepts_the_short_flag_everywhere`：锁定 `-w` 在裸 TUI、子命令前、子命令后三种位置都解析成同一路径，且长选项行为不变。

### Docs
- `docs/CLI_REFERENCE.md`、`docs/WEB_GUIDE.md` 的参数表补上 `-w`。

## [0.22.0-rc3] - 2026-08-13

### Fixed
- `ask_user` 提问也改为到达即弹（rc2 曾按「会抢输入行」保留手动打开，实测下来同样是让人干等，遂改）。本地和 Runtime 两条路径一致：一到就开弹窗、响铃、在活动流写一行 `等待你回答 · <问题>`。弹窗自带输入框，主输入框里已经打了一半的草稿**原样保留**，只是从弹出那刻起按键改由弹窗接管。
- 提问同样改为队列，修掉与审批相同的静默失败：此前第二个提问直接覆盖 `app.question`，被覆盖的 sender 一 drop，`ask_user` 收到 `None` 当作「用户没回答」，而用户根本没见过那个问题。现在先到先弹，回答完立刻顶上下一个，标题显示「还有 N」。三处回答出口（Esc、Enter、鼠标点选）都会接上队列。
- 切换会话时待回答提问显式作废并提示（`切换会话已放弃待回答的提问`），不再静默丢 channel。

### Tests
- 新增 2 项：提问排队且不吞主输入框草稿（并断言回答后立刻顶上下一个、Esc 出口同样接队列）、切换会话显式作废并提示。

## [0.22.0-rc2] - 2026-08-13

### Fixed
- 当前回合需要审批时立刻弹窗，不再只在侧栏留一条「等待审批」让人自己发现。Runtime 快照一旦带出待处理审批就自动打开确认框并响铃；此前只弹一条转瞬即逝的通知，任务在那儿干等，用户以为是卡死。
- 审批不再互相覆盖。此前 `UiMessage::Approval` 直接赋值给 `app.approval`，同一回合的第二个审批会挤掉第一个——被挤掉的 oneshot sender 一 drop，Harness 那边 `rx.await` 失败按 **Deny** 处理，于是用户没看见的那次审批被静默拒绝，回合莫名其妙失败。现在改成队列：先到先弹，解决一个立刻顶上下一个，弹窗标题显示「还有 N」。
- 切换会话时待处理审批改为显式拒绝并给出提示（`切换会话已拒绝待处理的审批`）。此前 `load_session` 把 `approval` 置空，效果同样是 Deny，但没有任何提示。
- 审批到达时写入活动流一行 `等待你确认 · <描述首行>` 并响铃，进度列能直接看出回合为什么不动了。

说明：`ask_user` 提问不做自动弹出——它会在用户正打字时抢走输入行，且已有 `task.waiting_answer` 通知；这条是有意保留的差异。

### Tests
- `tui/test_suite.rs` 新增 4 项：第二个审批入队而非静默拒绝首个（并断言解决后立刻顶上）、切换会话显式拒绝且有提示、到达时写活动流、标题显示队列深度。

## [0.22.0-rc1] - 2026-08-13

### Added
- Shell 命令改为**两级审批**，对齐 macOS Xedit 的 `SafeCommand` + 安全判官结构。此前 `smart` 模式只有一条硬编码白名单（`cargo test` 加 `grep/head/tail` 管道），`ls`、`cat`、`git status`、`rg` 这类纯只读命令统统要人点一次，一条会话下来审批卡比结论还多。现在：
  - 新增 `willdeep-core::safety` 静态分类器，按 Shell 语义（分段、引号、`$()`、重定向）给出 `AlwaysSafe` / `AlwaysDangerous` / `NeedsJudgment`。只读检查与受限工作区操作（`ls`/`cat`/`rg`/`git status`/`git log`/`cargo test`/`cargo clippy`/`find`/`mkdir -p` 等）直接执行；`rm -rf`/`sudo`/`chmod -R`/`git push --force`/`git reset --hard`/`mv`/`kill`/`dd if=`/fork 炸弹/`xargs rm` 判为破坏性，**绕过 AI 直接交用户**。
  - 新增 `willdeep-core::judge` AI 判官，处理中间地带（`curl`、`npm install`、`git commit`、`sed -i`、重定向写文件、任意脚本）。一次非流式调用，只认 `<verdict>YES</verdict>`；NO、回复畸形、网络失败一律回落到用户审批卡——判官只能减少打扰，不能扩大权限。默认开启，`[agent] safety_judge = false` 可关，`judge_model` 可指定模型（默认取 profile 的廉价模型）。
  - 注入防御三层：命令中的 `KEY=…` / `--password …` / `Bearer …` / `sk-…` 在出网前本地替换成 `[REDACTED]`；命令、工具名、任务意图分别封进 XML 标签并用零宽空格打断内部闭合标签与 `<verdict>`；裁决只接受唯一一个格式完整的标签，命令回声无法伪造 YES。
  - 同一「工具 + 命令 + 任务意图」的 YES 缓存 30 分钟，NO 从不缓存。
- 新增审批审计日志 `$WILLDEEP_HOME/approvals.jsonl`（`0600`，命令已脱敏），逐条记录放行来源 `static` / `judge` / `always-allow` / `user` 及原因，回答「这条命令为什么没问我」。

### Changed
- 删除 `tools.rs` 中的 `is_test_inspection_pipeline` / `split_inspection_pipeline`：新分类器覆盖它允许的全部形状，且不再把 `cargo test | tee result.txt`、`cargo test > result.txt` 误判为同类。原有断言以新分类器等价重写。
- 用户每轮输入通过 `ToolRegistry::set_task_context` 作为**惰性上下文**传给判官，只用于判断某个受限操作是否与当前目标相关；它不授予权限，也不会让破坏性操作变安全。

### Tests
- `safety.rs` 10 项：只读放行、破坏性拒绝、引号内危险词仍是数据、`$()` 有界展开、单段污染整链、Heredoc/引号不闭合降级、MCP 只读名识别。
- `judge.rs` 5 项：裁决标签解析（含双标签伪造）、凭据脱敏、注入字段中和、任务意图缺省、缓存键归一化。
- `tools.rs` 3 项：`smart` 放只读 Shell 但仍拦 `curl`/`rm -rf`/重定向；只有中间地带命令会送到判官（`ls` 与 `rm -rf build` 都不送）；判官说 NO 时回落用户。
- `config.rs` 1 项：`safety_judge` 未配置即为开启，且可显式关闭与指定 `judge_model`。

### Docs
- `docs/APPROVALS.md` 重写「三档审批模式」并新增「两级审批」一节：三类结论的处理与例子、Shell 语义规则、判官的三层防御与回落语义、配置项、审计日志位置。
- `docs/ARCHITECTURE.md` 安全边界同步为两级审批描述。
- `config.example.toml` 补 `safety_judge` / `judge_model` 示例。

## [0.21.0-rc67] - 2026-08-12

### Fixed
- 侧栏「需要关注 · 最近完成」不再无限堆积后台任务。此前 `attention_items()` 把 `background_tasks` 全量塞进 Inbox，只有用户手动标记已读才会消失，一条 26 秒前就结束的 `后台命令 · 已完成` 能挂到会话结束。现在 `BackgroundTaskSnapshot` 带上 `settled_millis`（终态时间戳，运行中为 `None`），顺利完成的任务在 Inbox 停留 60 秒后自动回收；失败、超时、被杀的任务保持原样——那些还等着人处理。侧栏每秒刷新一次快照，回收无需额外触发。

### Tests
- 新增 `attention_inbox_recycles_settled_tasks_but_keeps_failures`，锁定「已完成过期即走、失败常驻」这条规则。

汇总合并三条并行开发分支。

### Fixed
- `SessionStore` 不再从非默认家目录去桥接桌面 App 的会话：`load()` / `list()` / `digests()` 此前无条件扫描 `~/Library/Application Support/WillDeep/agent-sessions`，与 Store 自己的家目录无关。现在只有默认家目录（`~/.willdeep/sessions`）下的 Store 才合并桌面会话，`WILLDEEP_HOME` 指向别处的 Store（测试、沙箱、独立数据根）保持自足。这是 `headless_runtime.rs` 中 `web_sse_disconnect_resumes_the_same_runtime_turn_without_resubmission` 间歇失败的真正原因：该目录在开发机上有 293 个文件 / 67 MB，测试等待的 `active_turn_id` 只存在约 5 秒，而首次轮询就把 10 秒预算耗光。Runtime 侧调度无竞态：`turn.queued` 到 `turn.started` 实测 5–17 ms。
- `/mobile` 配对二维码改为编码紧凑配对 URL（`/pair?r=<room>&t=<token>&d=<桌面名>`，自建中继追加 `u=<base>`）而非完整配对 JSON：载荷从 437 字节降到 114 字节，二维码从 81×81 模块降到 41×41，终端弹窗从 73 列 × 37 行降到 49 列 × 25 行（桌面名顶满 16 字节且全需百分号转义时的上界为 53 列 × 27 行）。`mobile-gateway.v1` 字段本身未改，`base_url`/`pairing_token`/`expires_at` 由手机端从 `u`/`t` 与常量补全，不再占二维码体积。配套改动：桌面名按 UTF-8 字节（而非字符数）截断到 16 字节——中文主机名每字节在 URL 里要转义成三个字符，按字符截断能把二维码顶大一整圈；手机端（Android 1.25.0-rc5、macOS Xedit 1.247.0-rc12）同步接受省略这三个字段的配对载荷。

### Tests
- 新增 `web_compress_command_is_served_by_the_harness_instead_of_the_provider`：经由 `POST /api/chat/stream` 提交 `/compress`，断言回复来自 Harness 压缩分支、Provider 请求数不增加、`/compress` 不落入会话历史。此前 Web 侧 `/compress` 依赖 `execute_runtime` 中硬编码的 `allow_compress_command: true`，链路上没有任何断言保护——把该开关改为 `false` 时全工作区测试仍全绿。
- 测试配置写入器支持可选 `[agent] language`，便于对多语言 Harness 文案做确定性断言。
- 新增 `bridges_desktop_sessions_only_for_the_default_home`，锁定桌面会话桥接只在默认家目录生效。
- 新增 `self_hosted_relay_keeps_its_base_url_in_the_pairing_url`，锁定自建中继地址必须随二维码下发。
- `headless_runtime.rs` 的活跃 Turn 等待改为可自证的形式：每次 `GET /api/sessions` 单独设 2 秒超时（一次卡死的请求不再吃掉整个等待预算），Turn 提前进入非在途状态时立即带着 Runtime 侧 Session/Turn 的 status 与 error 退出，超时信息同样附带该摘要，不再只报一个 `Elapsed(())`。

### Docs
- `MOBILE.md` 的"二维码尺寸"一节改写为"配对二维码"：列出 `r`/`t`/`d`/`u`/`v` 五个参数及缺省行为，说明哪些字段不进二维码及原因；房间格式同步为 `wd-<32 位十六进制>`。`ARCHITECTURE.md` 的 Mobile Relay 段同步更新。

## [0.21.0-rc65] - 2026-08-12

### Fixed
- `/mobile` 配对二维码不再铺满整个终端。终端里一个二维码模块占一个字符格，尺寸只由配对载荷字节数和纠错等级决定；此前载荷 437 字节 + 默认 M 级纠错 = 81×81 模块，弹窗要 93 列 × 49 行。现在载荷 337 字节、纠错取 L 级，二维码 65×65 模块，弹窗 73 列 × 37 行，面积约为原先的 60%。

### Changed
- 中继 token 由两个 UUID 拼成的 64 位十六进制改为单个 UUID 的 32 位十六进制（128 位熵，仍是随机 UUIDv4）；room 由 `willdeep-cli-<带连字符 UUID>` 改为 `wd-<32 位十六进制>`。两者在配对 JSON 里各出现一次或两次，是载荷的大头。
- 二维码纠错等级由默认的 M（15% 冗余）降为 L（7%）。屏幕显示不存在印刷污损与折角，L 级足够，且省下一到两个版本的模块数。
- 配对载荷中的 `desktop_name` 截断到 16 字符，避免长主机名把二维码顶到下一个版本。无 `HOSTNAME` 时仍为 `WillDeep CLI`，有则直接用主机名（不再加 `WillDeep CLI · ` 前缀）。
- 旧格式凭据（`willdeep-cli-<uuid>` room + 64 位 token）在下次加载 `mobile-relay.toml` 时自动重新生成为紧凑格式并覆盖原文件：**已配对的手机需要重新扫码**。`mobile-gateway.v1` 的字段本身未改动，Android 端无需跟进。

### Tests
- 新增 `pairing_qr_fits_the_terminal_popup`：渲染真实配对载荷，断言二维码列宽不超过 `MAX_QR_WIDTH`（73），任何加长配对字段的改动都会先撞到这条测试。
- 新增 `legacy_credentials_are_recompacted_on_load`：旧格式凭据加载后被重写为紧凑格式，且紧凑凭据不会被反复重置。

## [0.21.0-rc64] - 2026-08-12

### Fixed
- `GET /api/sessions` 不再把整个会话目录反复全量反序列化，Web 进程不再长期占满 CPU。此前 `SessionStore::list()` 会把每个会话文件连同全部消息正文解析成 `Session`，本机实测 290 个文件、67 MB；前端每 2 秒轮询一次，而单次请求要 40 秒以上，请求持续堆积，8 个 tokio worker 全被 serde_json 占死（`sample` 采样热点集中在 `SliceRead::parse_str` / `Value::deserialize`）。
- 会话列表的文件解析不再阻塞 async worker：`/api/sessions` 的目录扫描改走 `spawn_blocking`。

### Added
- `willdeep_core::SessionDigest` 与 `SessionStore::digests()`：只解析列表视图需要的元数据（id、标题、工作区、时间、置顶、是否有用户输入），不物化消息正文，并按文件 `(mtime, size)` 缓存解析结果，只有变动过的文件才重新解析；缓存清理只针对本次扫过的目录，不影响其它 `SessionStore` 实例。
- 消息正文用探针结构折叠成"是否非空"的布尔值，避免为正文分配 `String`。

### Changed
- `SessionStore::latest()`、`--list-sessions`、TUI 命令面板的会话列表、TUI 切换工作区时挑选最近会话，全部改用 `digests()`；需要完整会话的地方再按 id `load()`。`SessionStore::list()` 保留，但已不在任何轮询路径上。
- Web 前端 `/api/sessions` + `/api/runtime/activity` 轮询加入 in-flight 去重：上一轮没返回就跳过这一轮，避免慢响应时请求叠加放大。

### Performance
- 本机真实数据（290 个会话、67 MB）实测，release 构建：首轮 `digests()` 462 ms，命中缓存后 1.3–2.7 ms；对照 `list()` 2.17 s。debug 构建：首轮 2.71 s，命中缓存后 7–16 ms；对照 `list()` 3.85 s。稳态下这条路径的开销降到千分之一量级。

### Tests
- 新增 `digest_reports_user_input_and_reuses_the_cache_until_the_file_changes`：覆盖"空会话不算用户输入""只有附件也算用户输入"，以及文件未变动时缓存结果一致。
- 新增 `digest_drops_cache_entries_for_deleted_sessions`：会话删除后缓存条目同步清理。
- 原 `web.rs` 中的 `session_has_user_input` 及其两条测试随函数一起移除，语义改由上述 core 测试覆盖。

## [0.21.0-rc63] - 2026-08-12

### Fixed
- Web Runtime 侧栏不再把已结束的历史 Agent 当成活动 Agent 展示。此前 `GET /api/runtime/activity` 返回该工作区 `agents.json` 中的全部记录（只按工作区过滤，无状态或时间过滤），侧栏取前 4 条渲染，结果长期被"已完成、轮次 0"的历史根 Agent 占满，真正在跑的 Agent 反而看不到。
- Agent 时长不再产生误导。此前运行中和已结束共用同一个"XX 秒"字段，而已结束根 Agent 的 `elapsed_seconds = completed_at - created_at` 是整个会话跨度（实测显示到 47148 秒），看起来像有进程活了 13 小时。现在运行中标注"已运行"，已结束标注"耗时"，并按 `秒 / 分秒 / 时分` 分级格式化。

### Changed
- 侧栏 Agent 列表只展示：仍在运行的 Agent（`idle` / `working` / `queued` / `blocked` / `waiting_*` / `cancelling`）、Runtime 未记录结束时间的 Agent，以及结束不超过 5 分钟的 Agent；从未执行过轮次（`current_turn = 0`）的已结束根 Agent 直接过滤掉。Web 与 TUI 套用同一套规则。
- TUI 状态栏「Runtime 智能体」同步上述过滤：`sidebar_runtime_agents()` 成为唯一可见列表来源，`runtime_agent_move()` 与 `selected_runtime_agent()` 都基于它取值，避免键盘选中项与屏幕上看到的行错位而把指令发给一个已经结束几小时的 Agent。TUI 时长同样改为「已运行 / 耗时」并按 `s / m s / h m` 分级，替换原先的 `{:.1}s`。
- 计数行的 Agent 数量改为可见活动 Agent 数，被折叠的历史记录以 `(+N 已结束)` 附注，不再用一个只增不减的总数冒充活动量。
- `GET /api/runtime/activity` 的 Agent 摘要新增 `finished_seconds_ago` 字段（未结束为 `null`），供前端判断"刚结束"；该字段与既有摘要一样不含 Workspace、Prompt、报告或内部错误。
- 新增 `web/src/runtimeAgents.ts` 承载 Agent 可见性与时长格式化逻辑，供侧栏和详情面板共用，避免 `RuntimeSidebar` 与 `RuntimeDetailPanel` 之间产生运行时循环依赖。

### Tests
- 新增 `web_runtime_agent_reports_run_duration_and_time_since_completion`：校验已结束 Agent 的 `elapsed_seconds` 取运行区间、`finished_seconds_ago` 取距今时长。
- 既有脱敏回归测试补充 `finished_seconds_ago` 在运行中 Agent 上为 `null` 的断言。
- 新增 `sidebar_drops_long_finished_agents_and_keeps_selection_on_the_visible_ones`：13 小时前结束、轮次 0 的根 Agent 不渲染，计数行显示 `(+1 finished)`，`selected_runtime_agent()` 命中可见的子 Agent。
- 既有 TUI 渲染测试的 Agent 时间戳改用真实时钟基准（原先用 epoch 1/2，在新规则下会被整体过滤掉，测不到渲染路径）。
- `cargo test --bin willdeep` 181 passed；Web ESLint 与 TypeScript/Vite 构建通过。

### Known gaps
- `agents.json` 仍无保留策略，终态记录只增不删；两端侧栏已不受影响，但文件会持续增长，计划单独一版处理。
- 可见性规则目前在 TS（`web/src/runtimeAgents.ts`）和 Rust（`tui/sidebar.rs`）各实现一份，语义手工对齐，没有跨语言的一致性测试。

## [0.21.0-rc62] - 2026-08-12

### Fixed
- Web 层 `POST /api/turns/{id}/stop` 补上工作区归属校验：此前该端点拿到任意合法 Turn UUID 就直接转发给 daemon，未做 allowlist 或归属检查，可跨工作区中断他人正在执行的 Turn。现在先经 Runtime 解析出该 Turn 所属 Session，再校验 Session 的工作区在服务器 allowlist 内；Turn 不存在、Session 读取失败、工作区不在 allowlist 三种情况统一返回 404，避免通过错误码差异探测其他工作区的 Turn id。与 Agent/审批/提问操作已有的 `authorize_runtime_agent` / `authorized_runtime_snapshot` 校验级别对齐。

### Changed
- 新增 `daemon::remote_turn_session()`：按 Turn id 反查所属 Session，Runtime 返回 `NotFound` 时给出 `None`，其余 API 错误照常上抛。
- Web 会话列表默认只展示未归档会话；已归档会话收进列表底部"已归档 (N)"折叠组，默认收起且不渲染行 DOM，点击才展开全部归档会话，展开后仍支持悬停操作（取消归档、删除等）。切换工作区时折叠组自动收起。
- Web 历史会话列表限制最大高度（42vh）并使用独立细滚动条，移除原先只显示前 20 条的截断，滚动即可浏览全部会话。

### Fixed
- Runtime 启动恢复不再因历史任务引用已删除的 Session 而失败：悬空引用的任务降级绑定无会话根 Agent，Daemon 正常完成启动。此前用户删除会话后重启 Daemon 会直接无法启动。

### Tests
- 新增"任务引用已删除会话时启动恢复存活"的回归测试。
- Web ESLint 与 TypeScript/Vite 构建作为本版本验收项；实测置顶经归档/取消归档往返后 `pinned_at` 保持不变。

## [0.21.0-rc61] - 2026-08-12

### Docs
- `README.md` 重新定位为面向读者的项目介绍：价值主张、30 秒上手、能力一览、安全须知与文档索引，不再承载完整参考内容，篇幅从 546 行降到约 130 行。
- 新增 `docs/README.md` 作为文档总索引，按「上手 → 三种使用方式 → 能力专题 → 排查 → 协议与架构」组织全部文档。
- 按主题拆分出 13 篇文档：`INSTALL.md`、`CONFIGURATION.md`、`AUTHENTICATION.md`、`CLI_REFERENCE.md`、`TUI_GUIDE.md`、`WEB_GUIDE.md`、`SOMEIM_INTEGRATION.md`、`RUNTIME_DAEMON.md`、`SUBAGENTS.md`、`APPROVALS.md`、`SKILLS_AND_MCP.md`、`MOBILE.md`、`TROUBLESHOOTING.md`。
- `docs/TUI_GUIDE.md` 新增鼠标章节，说明 SSH 下鼠标事件由本地终端模拟器上报、SSH 仅作透明字节管道，因此远程可用；tmux 需 `set -g mouse on`，GNU screen 支持残缺；`Ctrl+S` 文本选择模式、`?1003h` 全移动上报在高延迟链路上的上行开销，以及终端不支持鼠标时的纯键盘等价路径。
- `docs/AUTHENTICATION.md` 首次完整记录四类凭据的边界：Provider API Key 四层解析链（含子 Agent 不继承 `--api-key` / `WILLDEEP_API_KEY`）、some.im 浏览器登录为「打开 URL + 轮询」而非 OAuth 授权码交换、Runtime 控制 Token 覆盖全部端点且 `/v1/internal` 缺标记时返回 404 而非 401、手机中继二维码明文携带 relay token。
- `docs/CLI_REFERENCE.md` 按真实 Clap 命令树重写，补上此前 README 未记录的 `--listen`（默认 `127.0.0.1:9847`）、`daemon capabilities` 与 `instruct-agent`。
- `docs/WEB_GUIDE.md` 记录完整 JSON API 清单、工作区 allowlist 的「启动白名单 ∩ Runtime 注册表」双层模型、事件游标与退避重连，并明确 1 MiB 请求体上限对图片附件的实际约束，以及 Vite 代理端口硬编码为 9847。
- `docs/SOMEIM_INTEGRATION.md` 说明 host 精确匹配（`some.im` / `api.some.im` / `api.niuwoai.com`）、视觉回退的模型判定规则与默认 `qwen3-vl-plus`、`web_search` 端点为替换整条路径的 `/api/v1/customer/web-search`，并澄清 `x-willdeep-session-id` / `x-willdeep-workspace-id` 是 Provider 实例级随机 UUID，与会话 UUID 和工作区路径无关。

## [0.21.0-rc60] - 2026-08-12

### Added
- Web 聊天中 AI 回复使用 `react-markdown` + `remark-gfm` 渲染基础 Markdown（标题、列表、粗斜体、行内代码、代码块、表格、引用、链接），链接在新标签页打开且不使用 `dangerouslySetInnerHTML`；用户消息保持纯文本展示。
- 会话新增 `pinned_at` 置顶元数据（可选 Unix 秒时间戳，与 Xedit 的 `pinnedAt: Date?` 同语义）：本地会话写入会话 JSON；Xedit 桥接会话读取其 ISO8601 `pinnedAt`，置顶/取消置顶时就地补丁 Xedit 会话文件而不产生本地影子副本。置顶不改动 `updated_at`，不打乱最近使用排序。
- Web 新增 `POST /api/sessions/{id}/pin` 与 `POST /api/sessions/{id}/unpin`；会话列表按"置顶（最近置顶在前）→ 最近更新"排序，置顶会话带 📌 标记。
- Web 会话列表行悬停显示重命名、置顶/取消置顶、归档/取消归档、删除操作图标；删除改用 Chakra Dialog 确认弹窗（展示会话标题与不可撤销提示），不再使用原生 `window.confirm`。

### Changed
- Web 侧栏"新建只读子 Agent"改为纵向布局：Profile 下拉与任务描述各占整行，任务输入升级为两行 Textarea（Enter 提交、Shift+Enter 换行），创建按钮单独一行，不再与输入框挤在一行导致无法看清任务内容。
- 会话底部操作条精简为分叉与导出；重命名、归档、删除移入列表行悬停图标。

### Tests
- 新增置顶往返、置顶不触碰 `updated_at`、ISO8601 解析/格式化往返的单元测试。
- Web ESLint、TypeScript/Vite 构建、Rust 全工作区测试、Clippy 与 rustfmt 作为本版本验收项。

## [0.21.0-rc59] - 2026-08-11

### Fixed
- TUI 活动窗口纳入正式焦点模型，支持鼠标点击聚焦，并使用与输入、聊天和状态栏一致的焦点边框与状态提示。
- `Ctrl+W` 现在按输入、聊天、活动、状态栏的顺序循环；活动区聚焦后可用 Enter/Space 展开或收起工具详情，Esc 返回输入区。

### Tests
- 扩展焦点循环、鼠标命中和多语言焦点标签测试，覆盖活动窗口交互。

## [0.21.0-rc58] - 2026-08-11

### Fixed
- TUI Diff Unified/Side-by-side 渲染前统一展开真实 Tab，并把 ESC、响铃等终端控制字符转换为可见转义，避免源码内容移动物理终端光标后污染聊天区。
- 从 Diff 文件内容返回列表或关闭 Diff 模态时强制完整清屏并重建 Ratatui 缓冲，清除可能存在的终端残影。
- Web 聊天消息、工具轨迹和单行思考状态采用更紧凑的垂直间距；Composer 聚焦时只保留外层单一边框，不再叠加 Textarea 与额外阴影轮廓。
- CLI crate 显式跟踪 `web/dist` 变化；前端重新构建后 Debug/Release 二进制会重新嵌入最新资源，不再出现 Cargo 报成功但本地可执行文件仍携带旧 Web UI 的情况。

### Tests
- 新增包含 Tab、ESC 与响铃字符的 Diff 安全渲染回归测试，并继续验证并排替换与 CJK 显示宽度。
- 运行全部 Diff 定向测试、Rust 全工作区测试、Clippy、格式检查与 `git diff --check`。

## [0.21.0-rc57] - 2026-08-11

### Added
- Web 技能候选面板新增独立搜索框，并继续支持 Composer 中 `$` 后随输入即时过滤。
- Web 侧栏 Footer 从服务端健康接口展示当前版本，避免前端重复硬编码版本号。

### Changed
- Web Runtime 信息默认收起且可展开/收起，后台状态轮询不受展示状态影响；历史会话保持默认展开。
- 历史会话按钮、搜索输入和空状态使用明确的暗色主题前景、背景与悬停颜色。

### Fixed
- 空白 `New session` 不再进入 Web 历史列表；只有存在真实用户文字/附件输入或仍在运行的会话才会显示。

### Tests
- 新增空白、仅欢迎消息、普通用户输入和仅附件输入的会话可见性回归测试。
- Web ESLint、TypeScript/Vite 构建以及 Rust 全工作区测试与 Clippy 作为本版本验收项。

## [0.21.0-rc56] - 2026-08-11

### Added
- 后台 Shell 新增同版本隐藏 Supervisor：命令通过匿名长度帧 stdin 发送，父 Harness 持有同一管道作为存活租约，命令不进入进程参数或 Runtime 资源索引。
- 后台 Shell 生命周期以 `background_shell:<job_id>` 持久 Tool 资源绑定 Session、Turn、Execution Task 与 Root Agent；事件仅记录稳定 ID、状态、退出码、耗时和输出字节数。

### Changed
- Unix 后台命令在独立进程组运行；取消、超时或父端断开时，只在仍持有且确认尚未退出的 Child 句柄期间终止该进程组，避免留下命令子进程及 PID 复用误杀。
- Tool Store 自行创建私有持久目录，不再依赖调用方预先建立目录。

### Fixed
- 修复 Tokio 阻塞 stdin 监视可能拖住 Supervisor 正常退出的问题；父端 EOF 改由独立标准线程观察并通过 oneshot 回传。
- Daemon 重启时运行中的后台 Shell 不再丢失归属或静默消失；现在与其他 Tool 一样收敛为 Interrupted，并且恢复事件只写一次。

### Security
- Supervisor 的命令和输出正文不会持久化到 Tool 索引、恢复事件或公开 DTO；隐藏入口缺少内部环境标记时拒绝执行。

### Tests
- 新增 Supervisor 正常完成、内部入口拒绝、父端断开与真实子 PID 消失的进程级测试。
- 扩展持久落盘、精确 Session/Turn/Task/Agent 归属、单元恢复和真实双 Daemon 重启测试；第二次启动不得重复写入后台 Shell 恢复事件。

## [0.21.0-rc55] - 2026-08-11

### Added
- Runtime 启动恢复为运行中的 Child Agent、Tool、未应用 Agent 命令和外部 Spawn 预留 Child 补写一次性脱敏事件，观察客户端可沿原事件游标获知精确收敛结果。

### Changed
- 未应用 Agent 命令在重启后明确变为 Rejected；尚未真正执行的外部 Spawn 对应预留 Child 明确变为 Failed，不再与已运行后中断的 Child 混为一类。
- Child Agent 的专属 Worktree、分支和待审内容在恢复时原地保留，继续交由精确 Diff Review、Merge 或 Quarantine 流程处理。

### Fixed
- 运行中的 Child Agent 与 Tool 重启后不再静默改状态；现在持久收敛为 Interrupted 并记录稳定 Task/Agent/Tool 归属，且重复恢复报告不会重复写入事件。

### Tests
- 新增跨资源恢复测试，同时覆盖 Child Agent、专属 Worktree、运行中 Tool、Stop 命令、外部 Spawn 命令、事件脱敏和一次性消费。
- 新增真实双 Daemon 进程恢复测试：首个进程从中断快照收敛资源，停止并再次启动后只新增 Daemon 生命周期事件，四类资源恢复事件不重复。

## [0.21.0-rc54] - 2026-08-11

### Added
- 新增 `/v1/internal` 私有传输层，承载进程内 Harness Task、Interaction、Agent 命令、私有 Session 创建及 Daemon 生命周期。
- 私有请求在 Runtime Token 之外必须携带内部传输标记；缺失或错误时返回 404，避免内部端点被误识别为公开 API。

### Changed
- 公开 Runtime Client 移除未使用的任意 Query GET 与 JSON POST 方法；内部调用统一由 CLI crate 内不可导出的专用 Client 发出。
- rc53 的 drain/shutdown 仅保留一次升级兼容桥；rc54 接管后使用新的私有生命周期路径。

### Fixed
- SSE 与 NDJSON 长连接订阅 Runtime shutdown 状态；排空时主动结束事件流，不再让 graceful shutdown 永久等待活跃观察连接。
- Web SSE 重连若 Turn 已在断线期间完成，会从持久 Session/Turn 立即返回最终消息，不再因 `active_turn_id` 已清除而响应 409。

### Tests
- 新增内部传输标记精确校验，扩展认证测试以确保 Runtime Token 与内部标记缺一不可，并验证空闲事件流关闭及 Turn 在断线期间完成后的持久恢复。
- 扩展真实双 Daemon 接管测试：活动 Turn 运行期间保持已排空历史的 NDJSON 长连接，旧 Runtime 必须主动结束观察流、完成任务并交棒，替换 Runtime 随后继续接单。
- Headless 真实进程测试改用 Axum Mock Provider，完整消费 HTTP 请求并优雅关闭，消除手写 TCP 响应提前断连造成的偶发假失败。

## [0.21.0-rc53] - 2026-08-11

### Changed
- CLI 的 Task、Agent、审批、问答和 Diff 管理统一改用类型化 Runtime Client 与显式 Request ID，不再直接拼接旧资源 URL。
- TUI 的 Turn 提交、事件 Task 归属与 Diff Center 移除 404 旧 Daemon 回退；当前客户端只通过统一控制 API 使用公开 Runtime 能力。
- 进程内 Harness 的任务、Interaction、Agent 命令队列和 Daemon 生命周期路由继续作为私有传输边界保留，不与公开协议混用。

### Tests
- 全工作区类型检查约束公开 Client 返回 DTO；后续完整测试继续覆盖 CLI/TUI 交互、Diff、安全审批和真实 Runtime 进程接管。

## [0.21.0-rc52] - 2026-08-11

### Fixed
- Runtime 优雅排空只等待仍处于活动状态的 Task；终态 Task 遗留的 cancellation 句柄不再导致进程永久停留在 `draining`。
- Runtime 的 TCP 与本地 Socket 监听器改用有状态广播关闭信号；即使监听器稍晚进入等待，也不会漏掉排空通知并留下半退出进程。
- TUI 侧栏滚动继续使用可见范围过滤和饱和坐标计算，避免越界下溢 panic 后退出 TUI、露出原终端内容。

### Tests
- 新增终态 Task 遗留 cancellation 句柄的排空回归测试，固定全部活动与终态 Task 状态分类，并验证并行及晚订阅监听器都能收到关闭信号。

## [0.21.0-rc51] - 2026-08-11

### Added
- 共享 Rust Runtime Client 补齐 Runtime 状态、Workspace 注册/确保/激活/移除，以及 Session 创建/重命名/Fork/归档/删除/导出的类型化方法。
- 协议新增可前向兼容的 Runtime 健康状态和对象删除/移除结果 DTO；`runtime.status` 能明确报告 `draining`。

### Changed
- CLI、TUI 的 Workspace/Session/Turn 管理统一改用类型化 Client 和显式幂等 Request ID，不再手写统一操作名、参数 JSON 或 Workspace 旧 HTTP 路由。
- 所有统一 API 修改操作由单一清单进入 Pending→Completed 持久幂等路径；测试确保清单无重复且全部属于公开协议操作。

### Tests
- 新增真实 Unix Socket 契约测试，验证 Workspace、Session 与 Runtime 状态操作的 Token、操作名、Request ID、严格参数和响应 DTO。

## [0.21.0-rc50] - 2026-08-11

### Added
- Web 新增按 Session 恢复活动 Turn 的 SSE 端点；服务端从 Session 推导 Turn、Task、Workspace 和事件起点，浏览器只能提供最后已应用游标与语言。
- 会话列表公开安全的 `active_turn_id`，前端记住当前工作区最后会话与每个 Turn 的事件游标；刷新会自动重载历史并重新附着，网络中断按指数退避续接。

### Changed
- Web SSE 事件携带单调 `cursor` 与 SSE `id`；重连按游标去重，终态后从持久 Session 重载正式消息，不重复提交 Prompt 或追加 Assistant 答案。

### Tests
- 新增隔离 Web Server、延迟 Mock Provider 和真实 Runtime 的进程级断线恢复测试：首条 SSE 主动断开后接回同一 Turn，Provider 只请求一次且会话只保存一条 Assistant 回复。

## [0.21.0-rc49] - 2026-08-11

### Added
- Web Runtime 侧栏新增当前工作区后台 Task 列表；进行中与最近五分钟完成的任务可打开结构化详情。
- Task 详情展示状态、Profile、耗时、退出码、失败域，以及按 Task 归属过滤的 Tool 时间线和 Workspace Change Artifact 摘要。

### Security
- Web Task 摘要不下发 Workspace、Prompt、命令、参数、输出、内部错误、报告、路径、模型、配置或 PID；序列化回归测试固定该边界。

## [0.21.0-rc48] - 2026-08-11

### Fixed
- 修复 TUI 侧栏滚动到不可见逻辑行时，命中坐标提前执行无符号减法并触发 panic、导致 TUI 退出的问题。
- 侧栏可见范围、滚动偏移和终端坐标改为饱和计算；极小视口和极端手动滚动会安全夹紧。

## [0.21.0-rc47] - 2026-08-11

### Added
- Web Runtime 侧栏新增 Agent 与待处理 Task 详情，可查看按归属过滤的工具时间线、状态、耗时及 Diff Artifact 摘要。

### Security
- Web 结构化日志只使用公开 Runtime DTO；回归测试确保工具参数、输出、报告、Workspace、路径和内部错误不会下发浏览器。

## [0.21.0-rc46] - 2026-08-11

### Changed
- TUI Diff Review 支持鼠标滚轮浏览当前 Diff、文件列表和 Commit Preview，内容标题同步提示滚轮与方向键操作。

### Fixed
- Diff Review 打开时会优先消费所有鼠标事件；滚轮、点击和移动不再穿透到聊天区、Composer 或侧栏。

## [0.21.0-rc45] - 2026-08-11

### Added
- TUI Runtime Agent 状态分组新增只读子 Agent 创建入口，并支持 `/agent spawn scout|reader|deep <task>`。
- Web Runtime 侧栏新增 Scout、Reader、Deep 只读子 Agent 创建器；请求只接受当前 Workspace 中的活动父会话、任务正文与只读 Profile。

### Changed
- TUI 状态行常驻显示 `Ctrl+S 选择`；进入后释放终端鼠标捕获，可在完整输出区原生拖选并通过 `Cmd+C` / `Ctrl+Shift+C` 复制，按 `Esc` 或再次按 `Ctrl+S` 返回交互模式。
- Web 将聊天流式输出状态与 Runtime 控件提交状态解耦；父 Harness 运行时仍可审批、回答、控制或创建子 Agent，会话活动状态每两秒与 Runtime 对齐。

### Fixed
- 文本选择模式不再把复制快捷键当作退出事件，避免复制时意外结束 TUI。
- 修复 Web 在父 Harness 运行期间禁用全部 Runtime 控件、导致审批和 Agent 控制入口实际不可用的问题。

## [0.21.0-rc44] - 2026-08-11

### Added

- TUI Agent 详情新增键盘与鼠标可点击的补充指令、停止、原模型重试、指定模型重试和 Worktree Diff 控件；已有 Composer 草稿或附件不会被覆盖。
- TUI `/agent` 支持 `instruct`、`stop`、`retry` 与 `retry --model`，Web Runtime Agent 侧栏同步提供指定模型重试且全部文案覆盖中英日。

### Fixed

- Diff Inbox 弹窗现在先于残留 Task、Agent、Diff 和底层聊天面板捕获 D/Enter、Y、N，打开互斥详情时会清理旧覆盖层，按键不再泄漏到底层输入区。
- 旧 Runtime 不支持统一 Diff 控制 API 时，快照、内容和审查自动回退既有受 Token 路由；操作失败仅显示状态提示，不再退出整个 TUI。
- Web Agent 重试请求严格拒绝客户端夹带额外作用域，并对可选模型执行 1–256 字节边界校验。

## [0.21.0-rc43] - 2026-08-11

### Added

- 统一 `agent.retry` 参数新增可选 `model`，Rust Client 提供 `retry_agent_with_model`；旧的仅 ID 调用保持兼容。
- 终态后台 Child Agent 可在重试边界基于原 Provider 配置重建新模型实例，生命周期与 Agent Store 随后记录实际模型。
- Diff Inbox 详情提供键盘与鼠标可点击的“查看 Diff / Y 通过 / N 拒绝”；整批决定记录到精确当前快照并将本次指纹标记已处理。

### Changed

- 运行中的 Agent 不支持中途热切模型；模型覆盖只在可重试终态生效，避免同一轮请求混用模型。
- TUI 发版版本号改为低对比度并固定在状态侧栏右下角，不再占用顶部主信息位置。

### Fixed

- 统一 Runtime API 返回 HTTP 404 时，Turn 提交使用相同 request ID 安全回退到旧 Session Turn 路由，兼容仍在运行的旧版 Daemon，不自动重启或打断任务。

## [0.21.0-rc42] - 2026-08-11

### Added

- Agent 详情弹窗支持方向键、Page Up/Down、Home/End 和鼠标滚轮浏览长工具时间线、Diff 摘要与结果报告。

### Fixed

- 详情内容超过终端高度时按实际换行行数限制滚动范围，不再截断底部内容或滚入空白区。

## [0.21.0-rc41] - 2026-08-11

### Added

- TUI Agent 详情按 Agent ID 展示最近工具时间线、执行耗时、Diff Artifact 摘要和已有结果报告。
- 侧边栏按 Enter 时通过受保护的 `agent.get` 单项接口加载详情；列表继续使用脱敏摘要。

### Security

- 不新增 Prompt 原文持久化或公共事件下发；工具详情仍只含名称和生命周期，文件路径与内容继续由 Diff 授权接口控制。

## [0.21.0-rc40] - 2026-08-11

### Added

- Root 与 Child Agent 持久记录实际模型；Subagent 生命周期事件和统一 Runtime Agent DTO 暴露向后兼容的可选模型字段。
- TUI 与 Web Agent 树显示模型、累计 Token、运行耗时和专属 Worktree 摘要，Web Child 按父级缩进。

### Fixed

- Runtime Task 持久保存模型，Daemon 重启恢复 Root Agent 时不再丢失本轮模型选择。

## [0.21.0-rc39] - 2026-08-11

### Added

- TUI 侧边栏顶部显示当前编译版本，便于确认实际运行的是否为最新发版。

### Fixed

- `/webapp` 明确加入通用斜杠命令的委派集合，避免被兜底逻辑误报为未知命令；新增命令候选与分发回归测试。

## [0.21.0-rc38] - 2026-08-11

### Added

- Core Session 持久记录手动压缩代次、压缩前后消息数与时间戳，旧 Session 缺少字段时按未压缩状态兼容读取。
- Runtime Turn 持久记录消息边界所属压缩代次，Daemon 重启后的安全重放同样校验代次一致。

### Fixed

- 修复 `/compress` 改写持久消息后，旧 Turn 下标仍可能被当作精确 Fork 边界的问题；旧代次现在明确拒绝，当前代次仍可精确 Fork。

## [0.21.0-rc37] - 2026-08-11

### Changed

- Root 与 Child Agent 的 Usage 事件改为饱和累计 input/output/total Token；Provider 未给 total 时使用 input + output 补全。
- 同一 Session Root 进入下一 Task 时保留累计 Token，Child Agent 重试也保留既有消耗；轮次、当前工具和终态仍按新执行重置。

### Fixed

- 修复 Agent 右栏与统一控制 API 只显示最后一次 Provider Usage、并在下一 Turn 清零的问题；累计值现在随 Agent Store 持久化并在 Daemon 重启后恢复。

## [0.21.0-rc36] - 2026-08-11

### Added

- Runtime 重启会自动重新排队可证明安全的活跃 Session Turn，并由既有启动调度器按原 Session 串行约束续跑。
- 已持久化的末尾用户消息可被重放 Harness 原样复用，不删除、不重复追加；连续重启仍保持同一消息边界。
- 恢复事件区分旧 Task 的 `task.interrupted` 与可续跑 Turn 的 `turn.requeued`，便于客户端解释状态变化。

### Changed

- 重启时原 Pending Approval/Ask User 仍保守取消；重放再次到达同一工具或问题时重新创建交互，旧审批结果不会自动沿用。

### Security

- 只有旧 Task 没有任何持久工具活动，且 Core 历史恰好停在 Turn 起点或仅多出与私有队列 Prompt/附件完全一致的一条用户消息时才自动重放。存在工具副作用证据或额外持久内容时保留全部历史并维持 `Interrupted`，不截断、不猜测。

## [0.21.0-rc35] - 2026-08-11

### Added

- Core Session 新增向后兼容的模型字段，与 Provider Profile、配置引用一起成为客户端恢复当前会话执行设置的来源。
- 重启后领取排队 Turn 的测试验证 Runtime Session 会恢复原 Provider Profile、模型和配置，而不是使用新客户端的启动默认值。

### Fixed

- TUI 切换 Session 后同步目标会话的 Provider、模型和配置；提交下一轮时使用目标 Session 模型，避免沿用 TUI 启动参数。
- Workspace 切换不再覆盖已有目标 Session 的配置或模型；新会话才继承当前启动默认值。

### Security

- Skills 与 MCP 权限不从历史 Session 快照恢复，而是在每个 Runtime Task 开始前从当前持久 Workspace 策略重新绑定，防止旧会话恢复已经撤销的能力。

## [0.21.0-rc34] - 2026-08-11

### Changed

- TUI `/goal <目标>` 与 `/goal off` 现在把 Goal 写入当前 Core Session；重启、同工作区切换 Session 或切换 Workspace 后会恢复各自的 Goal。
- 旧版 Session 缺少 Goal 字段时按未设置处理，无需破坏性迁移；Goal 仍只在发送 Prompt 时注入，不会伪装成聊天消息。

## [0.21.0-rc33] - 2026-08-11

### Added

- Runtime Session 元数据升级为 schema 2，并加入显式标题来源，区分待自动命名、自动标题、用户标题和旧版标题。
- 新建且未指定标题的 Session 在首个 Turn 入队前生成本地、有界的自动标题，并发布不含标题正文的 `session.renamed` 事件。

### Changed

- schema 1 会话文件在首次打开时先创建权限受限的原始备份，再通过既有原子写入升级；完成迁移后不会重复备份，未来 schema 继续明确拒绝降级读取。
- 用户 Rename、收养已有 Core Session 和 Fork 标题被标记为非自动来源，后续 Prompt 不会覆盖。

### Security

- 自动标题不调用 Provider、不外发 Prompt；遇到密码、Token、API Key、私钥、常见凭据前缀或高熵字段时回退为通用标题，最大 80 个字符。

## [0.21.0-rc32] - 2026-08-11

### Added

- 新增 `willdeep daemon upgrade [--timeout SECONDS] [--force]`，以 Drain-and-Handoff 完成 Runtime 版本交接：旧进程继续执行活跃任务，归零后释放单实例租约，由当前二进制接管。
- Runtime 健康状态新增 `draining`，事件日志记录 `daemon.draining`；进程级端到端测试覆盖旧任务完成、新工作拒绝、PID 更替和替换进程继续执行。

### Changed

- draining 期间拒绝新的 Turn、外部 Agent Spawn、Retry 和补充 Prompt；停止/审批/回答等收敛操作保持可用，尚未领取的 Turn 留在持久队列由替换 Runtime 调度。
- Headless 客户端在确认 Runtime Token 已更替后重建本地 Client，并沿原事件游标继续读取，避免 Unix Socket/Named Pipe 交接窗口造成假失败。
- 源 Runtime 早于 rc32、不支持 Drain 时明确保持任务原样并要求首次手动迁移，不会暗中退化为取消活跃任务的 Shutdown。

### Fixed

- 通过异步读写闸门串行化任务领取、提交与 Drain 起点，消除升级开始瞬间仍有新任务穿透并被旧进程关闭流程取消的竞态。

## [0.21.0-rc31] - 2026-08-11

### Added

- 统一控制 API 与 Rust Client 新增真实 `agent.spawn`，可从活跃 Runtime Session 派生后台 `scout`、`reader` 或 `deep` 子 Agent，并以稳定 Child Agent ID 配合 `agent.wait` 观察终态。
- 跨语言协议夹具新增严格的 Agent Spawn 请求；进程级端到端测试覆盖 Root 等待、公共 Spawn、Child Provider 执行和 Wait 完成链路。

### Security

- Spawn 的父 Agent、Task 与 Workspace 全由 Runtime 根据 Session 推导；请求 DTO 拒绝路径和额外权限字段。
- 外部 Spawn 只允许工具集合经二次校验的只读 Profile，明确拒绝 `editor`、写目标和未知 Profile；Prompt 在命令应用或恢复拒绝后立即从持久命令记录清除。

## [0.21.0-rc30] - 2026-08-11

### Added

- `willdeep run` 默认通过持久 Runtime 创建或续接 Session/Turn，断开客户端不再中止任务；`--local` 保留显式进程内兼容入口。
- Runtime Task 公共 DTO 新增可向后兼容的失败域，用于保持 Provider、策略与 Harness/Tool 的稳定 CLI 退出码。
- 新增真实二进制、隔离 Runtime Daemon 与回环 Mock Provider 组成的 Headless 进程级端到端测试。

### Fixed

- 修复活跃 Runtime 追加事件时，控制 API 偶发读取到未完成 NDJSON 末行并误报内部错误的竞态。
- Headless 客户端在判断 Turn 终态前按游标读完全部事件页，避免高并发时遗漏生命周期事件或完成元数据。

### Security

- 进程级 API Key、API Base 和临时 Harness 参数不写入 Runtime Task；需要这些覆盖时自动保留本地执行路径。
- Ctrl+C 仅停止本次 Headless 调用提交的精确 Turn，不猜测或停止其他 Session 的任务。

## [0.21.0-rc29] - 2026-08-11

### Added

- `willdeep doctor --bundle PATH` 可导出标准 ZIP 诊断包，包含诊断报告、配置结构统计和安全说明。

### Security

- 诊断包默认排除配置值、Profile 名称、Provider 地址、模型、凭据、Prompt、工具载荷、日志和本地路径；以私有权限原子创建并拒绝覆盖已有文件。

## [0.21.0-rc28] - 2026-08-11

### Added

- 新增 `willdeep doctor [--json]`，离线检查配置、Provider 完整性、工作区、Git、Web 资源和 Runtime 状态。

### Security

- Doctor 只报告凭据可用性，不输出 API Key、Runtime Token、环境变量名、Provider 地址或本地路径；Runtime 版本不匹配会明确告警。

## [0.21.0-rc27] - 2026-08-11

### Added

- 新增受工作区边界和输出上限保护的只读 `git_log` 与 `git_blame` Harness 工具。

### Security

- Git 历史参数不经 Shell 拼接；路径必须解析到工作区内并以 `--` 隔离，Log 与 Blame 分别限制为 100 条提交和 2000 行。

## [0.21.0-rc26] - 2026-08-11

### Added

- 新增顶层 `willdeep session list/get/turns/stop`，直接查询或控制持久 Runtime Session。

### Security

- Session Stop 只操作服务端返回的精确 `active_turn_id`；空闲 Session 拒绝停止，不回退到“最近任务”猜测。

## [0.21.0-rc25] - 2026-08-11

### Added

- 新增 Bash、Zsh、Fish 和 PowerShell 动态补全脚本生成命令。
- 新增由当前 Clap 命令树生成的 roff man page。

## [0.21.0-rc24] - 2026-08-11

### Added

- 新增 `willdeep run`，支持 Prompt/stdin、显式输入文件、文本/图片附件、Session 续接、text/JSON/NDJSON 与静默输出。
- 新增稳定退出码契约，区分调用输入、Provider、策略拒绝和 Harness/Tool 执行失败。

### Security

- CLI 图片附件只接受 PNG/JPEG/WebP/GIF，验证真实格式、尺寸、像素数和总载荷；机器事件不输出 Tool 参数、结果或子 Agent 报告。

## [0.21.0-rc23] - 2026-08-11

### Changed

- Web Fetch 改为流式读取响应；无 `Content-Length` 时也会在超过 3 MiB 后立即中止。

### Security

- 重定向新增环路即时检测；每个目标继续重做公网校验，跨域重新审批并拒绝 HTTPS 降级。

## [0.21.0-rc22] - 2026-08-11

### Added

- 新增 `willdeep config init/check/show`，支持安全生成、严格校验和脱敏展示 TOML 配置。

### Security

- `config init` 使用私有文件权限且拒绝覆盖；`config show` 永不输出内联 API Key 的原值。
- 明确公开 `agent.spawn` 不能把客户端传入路径直接视为已审批写目标；编辑型 Profile 待结构化目标授权链完成后再开放。

## [0.21.0-rc21] - 2026-08-11

### Added

- 稳定 Worktree Review、Merge、Audit、Quarantine 公共请求/返回 DTO，并将四个操作接入统一 API Dispatch。
- Rust Runtime Client 新增 Worktree 类型化方法；CLI/TUI bridge 从旧专用 HTTP 端点迁移，Merge/Quarantine 显式使用幂等 Request ID。
- `ApiResponse::into_result()` 和标准 `ApiError` 统一成功/错误信封解包，同时保留错误码与重试字段。

### Security

- Worktree Merge 继续绑定精确 Review ID，Quarantine 绑定 Agent、Child Snapshot 与显式确认；严格参数拒绝额外删除意图字段。

## [0.21.0-rc20] - 2026-08-11

### Added

- Rust Runtime Client 新增 Diff 快照、内容、审查、验证、归因、Commit Preview 和安全撤销的完整类型化方法。

### Changed

- TUI/CLI Diff bridge 的全部统一 API 调用迁移到共享 Client；审查、验证记录和撤销显式携带幂等 Request ID。

## [0.21.0-rc19] - 2026-08-11

### Added

- Web Runtime 侧栏支持允许一次、拒绝、始终允许审批，以及候选单选、多选提交和自定义回答。
- Web 可对后台 Agent 补充 Prompt、停止或重试；操作后立即刷新 Activity，并保留轮询最终一致性。

### Security

- 每个 Web Runtime 写端点重新验证 Workspace 注册表、启动白名单和 Gate/Agent 目标归属；严格请求体拒绝客户端夹带额外作用域。

## [0.21.0-rc18] - 2026-08-11

### Added

- Web Activity API 在 Workspace 双重白名单边界内增加 Agent、待审批/回答 Gate 和关注项计数。
- React 新增独立 Runtime 侧栏组件，展示 Agent 状态与轮次、待处理详情、工具和产物摘要；新增文案覆盖简中、英语和日语。

### Security

- Web Agent 摘要不下发 Workspace 路径、Agent 报告或内部错误，相关字段由回归测试守护。

## [0.21.0-rc17] - 2026-08-11

### Added

- 协议 crate 新增 Agent Prompt/Wait、Approval Resolve、Question Answer、Event List、Workspace Ensure 严格参数 DTO，以及脱敏的 Agent Command/Interaction Result 返回 DTO。
- Rust Runtime Client 新增 Workspace、Session、Agent、Turn、Task、Approval、Question 与 Event 高频类型化观察和控制方法；修改操作显式接收幂等 Request ID。
- 跨语言兼容夹具加入 Agent、审批、提问、事件控制请求与公共结果，Rust 测试逐项解码。

### Changed

- TUI Runtime bridge 的 Session 搜索、Turn、Event、Agent、Task、Inbox、Tool 与 Artifact 调用迁移到共享 Client 的类型化方法。

## [0.21.0-rc16] - 2026-08-11

### Fixed

- 修正 Rust Runtime Client 的 `tool.get` 与 `artifact.get` 类型签名：成功响应直接解码公共对象，未找到时由统一错误信封表达，不再错误要求服务端返回 `Option<T>`。
- 新增 Unix Socket 真实 `tool.get` 往返测试，覆盖稳定操作名、ID 参数和直接对象响应解码。

## [0.21.0-rc15] - 2026-08-11

### Added

- 新增 `public-api-v1.json` 跨语言兼容夹具，覆盖 Runtime 的 11 类稳定公共对象及统一响应信封，供 Swift、Android 和第三方客户端做解码契约测试。
- 协议测试逐类反序列化兼容夹具，并确认示例不包含 API Key、认证头或 Runtime Token。

### Changed

- Object、Capability 和 Transport 能力枚举遇到未来新增值时降级为 `unknown`，旧客户端不会因新服务器增加能力而拒绝整个能力响应。

## [0.21.0-rc14] - 2026-08-11

### Added

- Rust Runtime Client 新增 Tool 与 Artifact 的列表、单项类型化查询方法，调用方不再手写操作名和返回 DTO。
- 新增 Unix Socket 真实往返测试，验证类型化 Tool 查询携带 Runtime Token、稳定过滤参数并正确解码公共 DTO。

## [0.21.0-rc13] - 2026-08-11

### Added

- Web 新增 `/api/runtime/activity`，仅允许查询启动时白名单中的 Workspace，并返回统一 Runtime Tool/Artifact DTO。
- React 侧栏每两秒刷新工具总数、运行数、产物数和最近工具状态，新增完整中英日文案。

### Security

- Web Activity 的 Workspace 选择通过 Runtime 注册表与 Web 启动白名单双重校验；浏览器不能借查询参数枚举其他本机 Workspace。

## [0.21.0-rc12] - 2026-08-11

### Added

- 协议 crate 新增稳定 `RuntimeArtifact`、`ArtifactKind`、`ListArtifactsParams` DTO，以及 `artifact.list`、`artifact.get` 操作。
- Runtime 将工具窗口内由内容指纹确认的 Diff Attribution 映射为 Workspace Change Artifact，绑定 Session、Turn、Task、Agent、来源快照与变更项数量。

### Changed

- TUI Runtime 快照直接查询结构化 Tool/Artifact，右栏展示当前工作区的工具总数、运行数、产物数和最近工具状态，不再只依赖聊天事件聚合。

### Security

- Artifact 元数据不暴露 Workspace 路径、变化文件名或内容；调用方必须使用受 Workspace 授权和精确快照校验的 Diff API 获取具体内容。

## [0.21.0-rc11] - 2026-08-11

### Added

- 协议 crate 新增稳定 `RuntimeTool`、`ToolStatus`、`ListToolsParams` DTO，以及 `tool.list`、`tool.get` 操作。
- Runtime 新增有界持久 Tool Activity 索引，记录主/子 Agent 工具的 Session、Turn、Task、Agent 归属和毫秒级生命周期；重启时运行中记录收敛为 Interrupted。

### Security

- Tool Activity 不持久化也不返回工具参数、输出正文、Workspace 路径或内部错误；工具名有长度上限，列表数量受服务端限制。

## [0.21.0-rc10] - 2026-08-11

### Added

- Core Session 以向后兼容字段持久化私有配置引用，供 Runtime 在收养现有 Session 时自行恢复。
- 协议 crate 新增严格拒绝未知字段的通用 `IdParams`，供 Session 与 Turn 查询/控制操作复用。

### Changed

- `daemon sessions/session/search/rename/fork/archive/export/delete` 与 Turn 提交、列表、查询、停止全部改走共享 Runtime Client，自动使用 Unix Socket 或 Windows Named Pipe。
- TUI 与 Web 收养 Session 改用统一 `session.create`，客户端不再重复发送配置文件路径。

### Security

- `CreateSessionParams` 明确拒绝 `config` 等未知字段；收养操作只接收稳定 Session ID，并以 Runtime 已持有的 Core Session 配置为准，不信任客户端提供的路径。

## [0.21.0-rc9] - 2026-08-11

### Added

- Runtime Daemon 在 Unix 平台监听权限为 `0600` 的本地 Socket，在 Windows 平台监听拒绝远程客户端的随机 Named Pipe；共享 Runtime Client 支持两种本地传输。

### Changed

- CLI、TUI 与 Web 的 Runtime 控制客户端优先读取 daemon 状态中的本地端点，旧版状态仍自动回退到受 Token 保护的回环 TCP。
- 能力协商只报告当前进程实际启用的本地传输；关闭 Daemon 时安全清理自有 Unix Socket。

### Fixed

- `daemon stop` 正确接受关闭端点返回的 HTTP 202 空响应，不再在 Daemon 已退出后误报 JSON 解码失败。

### Security

- Unix Socket 启动时拒绝覆盖同名普通文件，仅在确认目标仍为 Socket 后清理；Windows Named Pipe 禁止远程客户端连接，所有传输继续要求随机 Runtime Token。

## [0.21.0-rc8] - 2026-08-11

### Added

- 协议 crate 新增 Session 搜索结果、文本/图片附件、Turn 提交与列表参数 DTO，以及 `session.search`、`turn.list` 稳定操作。
- 统一 API 实现 Session 组合搜索和 Turn 提交、列表、查询与停止。

### Changed

- TUI Session 搜索和 TUI/Web Turn 提交、停止改走共享 Runtime Client；Turn 提交同时使用外层 Request ID 与会话内 Turn Request ID 去重。

### Security

- 统一 Turn 提交在 Runtime 边界限制 1 MiB Prompt、12 个附件、文本字符数、图片 MIME/尺寸和 10 MiB 总载荷；参数严格拒绝 Workspace、权限或其他未知控制字段。

## [0.21.0-rc7] - 2026-08-11

### Added

- 统一 API 实现 `session.create/rename/fork/archive/delete/export`；协议 crate 新增创建、重命名、Fork、归档和精确确认删除参数 DTO。

### Changed

- Web Session 列表、重命名、指定 Turn Fork、归档/恢复、删除和导出改走共享 Runtime Client；Session 修改操作纳入持久 Request ID 幂等。

### Security

- Web 在调用统一 Session 管理前仍校验启动时 Workspace 白名单；公开 Session 响应不包含配置文件路径、排队 Prompt、附件或内部错误，删除同时要求目标 ID 与 confirmation 完全一致。

## [0.21.0-rc6] - 2026-08-11

### Added

- 协议 crate 新增 Diff Snapshot、File Content、Review、Verification、Attribution、Commit Preview 与 Revert 稳定 DTO，并公开完整的 `diff.*` 操作集合。

### Changed

- TUI Diff Review Center 的快照、内容、审查、验证、归因、提交预览和撤销全部改走共享 Runtime Client，不再手写旧 Diff HTTP 请求。
- `diff.review`、`diff.verification.record` 和 `diff.revert` 纳入持久 Request ID 幂等，陈旧 Snapshot 使用稳定 `stale_snapshot` 错误码。

### Security

- 统一 Diff API 复用 Runtime Workspace 授权、精确 Snapshot 校验、审查备注/验证摘要上限、敏感验证命令拒绝与 Recovery 保守撤销；客户端不能通过新增参数强制覆盖冲突。

## [0.21.0-rc5] - 2026-08-11

### Added

- 协议 crate 新增稳定 Runtime Workspace、Workspace Access 和注册参数 DTO；统一 API 补齐 `workspace.register/ensure/activate/remove`。

### Changed

- `workspace.list` 返回公开 DTO；配置注册、自动确保、激活和移除均进入统一响应信封与 Request ID 持久幂等。
- TUI `/workspace` 与 Web 启动/Workspace 列表使用的 bridge 改走共享 Runtime Client，不再直接调用旧 Workspace HTTP 端点。

### Security

- Workspace 根目录继续由 Runtime 规范化并验证为真实目录；访问模式、Provider、Skills 与 MCP 允许列表只由服务端注册表持久化，任务提交时客户端字段不能扩大权限。
- 公共 Workspace 事件只记录稳定 Workspace ID，不再把根路径写入新事件；公开 DTO 的路径字段保持可选，为后续远程权限裁剪保留语义。

## [0.21.0-rc4] - 2026-08-11

### Added

- 协议 crate 新增稳定 Runtime Task、Pending Approval 和 Pending Question DTO，以及 `task.list/get/cancel`、`question.list` 统一操作。
- 修改类统一请求新增私有持久幂等日志：执行前记录 Pending，完成后记录响应；Runtime 重启后相同 Request ID 返回原响应，崩溃窗口中的不确定请求拒绝自动重放。

### Changed

- TUI Attention Inbox 的 Task、审批和问题列表改走共享 Runtime Client；取消后台 Task 改用幂等 `task.cancel`。
- Web/TUI 的事件补读改用统一 `event.list`，不再读取旧原始事件端点；实时 TUI 继续使用共享 NDJSON Client。
- 审批和问题解决响应只返回 ID、Task ID、Resolved 状态与时间，不回显审批描述、回答正文或内部 Interaction 存储字段。

### Security

- 公开 Task DTO 不包含 PID 和内部错误；Pending DTO 不包含 Resolution、回答正文或解决时间。
- 幂等文件仅保存参数指纹和脱敏响应，不保存原始 Prompt、Agent 指令或用户回答；Pending 日志落盘失败时不会执行修改操作。

## [0.21.0-rc3] - 2026-08-11

### Added

- 协议 crate 新增稳定 `RuntimeSession`、`RuntimeTurn`、`RuntimeAgent`、`RuntimeEvent` 及对应状态枚举；公开 DTO 不包含配置文件、队列 Prompt、附件、消息边界或内部错误字段。
- Runtime Client 新增跨 Chunk NDJSON UTF-8/多信封解码测试，覆盖流式网络切包边界。

### Changed

- 统一 API 的 Session/Turn/Agent 查询与 Agent Wait 改为公开 DTO；Agent 列表不返回报告，只有显式 `agent.get`/`agent.wait` 返回有界报告详情。
- TUI 实时事件改用共享 Runtime Client 的 NDJSON 流；Agent 列表、补充 Prompt、停止、重试、审批和问题回答也改走统一 API，旧 SSE 仅保留服务端兼容端点。
- `agent.stop` 与 `agent.retry` 加入统一调度和 Request ID 幂等范围。

### Security

- 新写入的 Tool 事件不再持久化工具参数和输出，子 Agent 事件不再持久化报告及 Workspace 路径。
- 统一 `event.list` 和 NDJSON 流会在读取边界净化旧日志中的 Tool 参数/输出、Agent 报告、Workspace 路径及内部错误，保留工具名称、状态和 AI 正式回复。

## [0.21.0-rc2] - 2026-08-11

### Added

- 新增受 Token 保护的 `POST /v1/api` 统一 JSON 控制入口，以及 `willdeep api <operation> --params-file <JSON|->`；Session、Agent、Turn、Approval、Question 与 Event 的首批操作使用协议 crate 定义的请求/响应信封。
- 新增可续传 `GET /v1/events/stream.ndjson`，每行均为完整统一事件信封；CLI `willdeep api event.stream --ndjson` 与共享 Rust Client 可按 `after` 游标消费长连接。
- 新增 `willdeep-runtime-client` crate，集中处理回环端点校验、Runtime Token、能力协商、统一调用及有界 NDJSON 解码。
- TUI Prompt 新增 `/webapp` 与 `/webapp status`；前者在当前 Workspace 启动内嵌 Web App，候选列表同步提供命令说明。

### Changed

- CLI 的统一 API、能力查询与 NDJSON 事件消费改用共享 Rust Client，不再各自拼装 HTTP 请求。
- 修改类统一请求按 Request ID 做进程内有界幂等去重；同一 ID 携带不同参数会返回冲突。
- Web App 启动不再强制进入 Provider 首次设置；无 Provider 时仍可查看本地 Runtime 状态，真正提交模型任务时再返回配置错误。

### Security

- 幂等缓存只保留请求参数指纹，不额外保存 Prompt 或回答正文；内部错误对客户端统一脱敏，详细上下文只进入 Runtime 本地日志。
- `/webapp` 只允许回环监听地址；远程访问仍须由反向代理、VPN 或 SSH Tunnel 显式提供认证与暴露边界。

## [0.21.0-rc1] - 2026-08-11

### Added

- 新增独立 `willdeep-runtime-protocol` crate，定义协议版本、11 类控制对象、稳定 namespaced operation、能力、传输类型、协议限制和统一成功/错误信封。
- Runtime 新增受 Token 保护的 `GET /v1/capabilities`，CLI 新增 `daemon capabilities`；响应包含服务端/最低客户端协议版本、服务版本和可选请求 ID。

### Changed

- 阶段 7 从共享协议 crate 开始演进；现有 `/v1/*` 原始响应暂时保持兼容，后续客户端迁移不需要复制协议常量和错误语义。

### Security

- 能力文档与响应只描述公开协议元数据，不包含 Runtime Token、Provider Key、Prompt、工具参数、文件正文或用户路径；端点显式执行本地 Runtime Token 校验。

## [0.20.0-rc6] - 2026-08-11

### Added

- 新增受 Token 保护的 Worktree 审计 API/CLI `worktrees-audit`，区分 Active、Reviewable、Merged、Clean、Quarantined、Missing 和无 Agent 记录的 Unknown 状态。
- 新增 `quarantine-agent-worktree --snapshot <ID> --yes`：把符合条件的完整 Git Worktree 移入 Runtime Recovery，并持久记录新路径和隔离时间。

### Changed

- 保守清理不调用 `git worktree remove`、不删除文件或分支，改用 `git worktree move` 保留完整目录与 Git 关联；状态持久化失败时尝试移回原路径。

### Security

- Worktree Review、合并、审计与隔离 Handler 均显式校验 Runtime Token；隔离还要求终态 Agent、WillDeep 托管直系目录、精确 Child 快照、显式 `--yes`，且仅允许干净或已精确合并且无冲突/未跟踪内容的 Worktree。

## [0.20.0-rc5] - 2026-08-11

### Added

- 子 Agent 成功报告附带最多 16 KiB 的 Worktree 路径、分支和 `git status --short` 状态，可靠回流主 Harness。
- 新增受 Token 保护的 Agent Worktree Review/合并 API、CLI `agent-worktree-review` 与 `merge-agent-worktree --review <ID> --yes`，以及 TUI Agent 详情 `W` 审查、`M` 批准合并。

### Changed

- Review ID 精确绑定 Agent、Child Diff 快照、Root Diff 快照和二进制补丁；任一侧变化后旧 Review 自动失效，合并只应用工作区补丁，不自动 Commit、Push 或清理 Worktree。

### Security

- 合并前执行 `git apply --check`，同文件并发修改、Child 未解决冲突、未跟踪文件、不可表示变更和超过 2 MiB 的补丁全部阻断；CLI 必须同时提供精确 Review ID 和 `--yes`，TUI 必须先打开 Review 再显式按 `M`。

## [0.20.0-rc4] - 2026-08-11

### Added

- 内置 Editor 子 Agent 默认创建 `willdeep/agent-<id>` 专属 Git Worktree；TOML Profile 可用 `worktree = "shared" | "dedicated"` 覆盖策略。
- 子 Agent 生命周期与 Runtime Agent Store 新增实际 Workspace、根 Workspace、Worktree 分支和隔离标记。

### Changed

- Editor 已审批目标按根 Workspace 相对路径映射到专属 Worktree；Child 写工具的 Diff Attribution 在自己的 Worktree 内采集，不再误归属父工作区。

### Security

- 专属 Worktree 仅能从包含当前 Workspace 的 Git 仓库创建，使用不可预测 Agent UUID 路径；任务结束时保留分支与目录供审查，不自动删除或合并用户改动。

## [0.20.0-rc3] - 2026-08-11

### Added

- Web Workspace API/选择器返回 Runtime 注册表中的稳定 ID、名称、active 状态和 `read-only/smart/workspace-write` 模式；Composer Skills 使用 Workspace 允许列表。

### Changed

- Web 可见 Workspace 是 Runtime 注册项与 `--workspace/--web-workspace` 启动白名单的交集；新 Web Session 优先使用 Workspace 默认 Provider。
- Coding Workspace 自动注册默认 `workspace-write`，即目录内文件写入免审；Shell、MCP、网络与越界访问继续使用现有审批状态机，`read-only` 仅显式启用。

### Security

- 新增原子 Workspace `ensure` API：只在目录尚未注册时创建默认项，绝不覆盖已有访问策略、Provider、Skills 或 MCP 设置；浏览器请求不能扩大服务启动时的路径上界。

## [0.20.0-rc2] - 2026-08-11

### Added

- TUI 新增 `/workspace list` 与 `/workspace switch <id>`，并加入 `/` 命令候选；可从 Runtime 注册表查看和切换工作区。

### Changed

- TUI 切换 Workspace 时保存当前 Session 游标与 Inbox 状态，恢复或创建目标 Session，重启 Runtime 事件跟随并刷新状态、Skills 与移动会话；旧 Workspace 后台任务继续运行。

### Security

- 跨 Workspace 切换后禁用仍绑定启动目录的进程内 `/local` Harness，避免旧工具边界被用于新路径；Runtime 模式继续以服务端注册表重建边界。

## [0.20.0-rc1] - 2026-08-11

### Added

- Runtime 新增持久 Workspace 注册表与受 Token 保护的注册、更新、列表、激活、移除 API；CLI 增加 `register-workspace`、`workspaces`、`activate-workspace` 和 `remove-workspace`。
- 每个 Workspace 独立保存规范化根目录、访问策略、默认 Provider Profile、Skill 与 MCP 允许列表；任务和 Session 首次使用目录时自动注册。

### Changed

- 激活 Workspace 只改变默认项，已入队 Task 和既有 Session 继续绑定原根目录；移除注册不会删除工作区文件、Session 或历史。

### Security

- Workspace 策略字段不接受客户端反序列化，并由 Runtime 在任务入队时强制覆盖；只读策略会在审批前拒绝 Shell、文件写入、Worktree 创建、MCP 调用和 Editor 子 Agent。
- Workspace 注册表使用 Runtime 私有原子写入机制，不存储 Provider Key、Prompt 或文件正文。

## [0.19.0-rc8] - 2026-08-10

### Fixed

- TUI Inbox 不再长期显示已完成或已取消的 Runtime 任务，完成超过 5 分钟后自动移出“最近完成”。
- Runtime Gate 增加 Task ID 关联；点击等待审批/回答的任务或在详情中按 Enter，会直接打开实际审批/回答控件，而非停留在不可操作的任务详情。

## [0.19.0-rc7] - 2026-08-10

### Added

- Runtime 在主 Agent 与子 Agent 的潜在写工具调用前后采集工作区内容指纹，持久绑定 Session、Turn、Task、Agent、Tool 和真实变化路径。
- 新增受 Token 保护的 Diff Attribution API、CLI `daemon diff-attributions`，TUI Diff Review 显示每个文件最近的责任 Agent 与工具，并沿连续快照链保留多次工具变更。

### Fixed

- 归属计算只比较工具窗口前后发生变化的路径，不再把调用开始前已经存在且未变化的脏文件误算给当前 Agent。

### Security

- Diff Attribution 仅持久化内容指纹、结构化 ID、工具名和相对路径，不持久化文件正文、工具参数或凭据。

## [0.19.0-rc6] - 2026-08-10

### Added

- Runtime 新增精确快照 Commit Preview API；CLI `daemon diff-commit-preview` 与 TUI Diff Review 的 `P` 面板展示提交消息、分支、暂存/未暂存文件、Remote、推送目标和可选 Tag。
- Commit Preview 在敏感文件或疑似凭据、冲突、空暂存区、Detached HEAD、缺失 Remote 和无效 Tag 时给出结构化阻断原因，并且不执行任何 Git 写操作。

### Fixed

- 增加 Runtime 聊天纯净度回归测试，锁定对话区只展示用户输入和 AI 最终回复；轮次、Task/Agent ID 和工具活动仅保留在状态层。

### Security

- Remote URL 在展示前移除用户信息、查询参数和片段；敏感检查只返回文件路径与规则代码，不回传或记录匹配到的凭据内容。

## [0.19.0-rc5] - 2026-08-10

### Added

- Core `run_command` 新增结构化验证报告器，自动识别常见前后台测试命令并上报命令、退出码、Passed/Failed/TimedOut/LaunchFailed 与失败摘要。
- Runtime 新增精确 Diff Snapshot Verification 持久化 API；CLI `daemon diff-verifications` 和 TUI Diff Review 展示最近验证结果。

### Security

- 验证摘要限制为 8 KiB、40 行且保持 UTF-8 边界；普通 Shell 不记录，命令出现 API Key、Token、Secret、Password 或 Authorization 标记时拒绝持久化。

## [0.19.0-rc4] - 2026-08-10

### Fixed

- 修复 TUI/Web 隐式启动 Runtime Daemon 时，启动状态直接写入终端 stdout 并穿透 Ratatui、显示在 Prompt 区域的问题；只有显式 `daemon start` 保留控制台提示。
- TUI Runtime 提交确认改为底部短暂状态，不再向聊天区写入 Turn/Agent ID；AI 完成消息移除 Runtime ID 前缀，只展示模型真实返回内容。

## [0.19.0-rc3] - 2026-08-10

### Added

- Runtime 新增持久 Diff Review 记录，支持 Accepted、Rejected、Changes Requested、Reviewed；CLI 新增 `daemon diff-review`，TUI 使用 A/D/C/M 保存并显示文件审查状态。
- 新增 `POST /v1/diffs/{id}/revert`、CLI `daemon diff-revert` 与 TUI `R` 二次确认，可按 Combined/Staged/Unstaged 安全撤销单文件。

### Security

- 审查和撤销必须匹配当前 Workspace 的精确内容快照；文件在打开后发生任何变化都会以冲突拒绝操作。
- 未跟踪文件及 HEAD 中不存在的新增内容不会直接删除，而会原子移动到 `runtime/recovery` 可恢复目录；冲突文件拒绝自动撤销。

## [0.19.0-rc2] - 2026-08-10

### Added

- TUI Diff Review 新增 Unified/Side-by-side 双视图；并排模式将相邻删除与新增行配对，按 Unicode 显示宽度截断和补齐双栏。
- 新增 Combined/Staged/Unstaged 范围循环切换，以及当前文件增量搜索、匹配高亮和前后跳转。

### Changed

- 将 Diff Review 状态/渲染与聊天 Markdown 渲染拆分为独立 TUI 模块，主文件降至 3000 行以内。

## [0.19.0-rc1] - 2026-08-10

### Added

- 新增 Workspace Diff 快照模型，记录 HEAD、文件新增/修改/删除/重命名/冲突/未跟踪状态、暂存与未暂存范围、二进制标记、增删统计及内容指纹。
- Runtime 新增受 Token 保护的 `GET /v1/diffs` 和 `GET /v1/diffs/{id}/content`；CLI 提供 `daemon diff-snapshot`、`daemon diff-file`，TUI `/diff` 提供文件导航、滚动和 Unified Diff 着色。

### Security

- Diff API 只允许访问已注册 Session/Task 的规范化 Workspace；文件内容请求必须匹配当前快照 ID，变更后返回冲突，且拒绝绝对路径、父目录穿越与符号链接逃逸。
- 单文件 Diff 输出限制为 512 KiB，并在 UTF-8 边界安全截断；Runtime API 不暴露 Provider 凭据或未请求的文件正文。

## [0.18.0-rc3] - 2026-08-10

### Added

- 子 Agent 完成事件携带最多 64 KiB 的成功报告或失败原因，Runtime Agent Store 持久化结果；CLI 单 Agent 详情和 TUI Enter 详情层可查看状态、策略、实时用量与报告。
- 新增 `willdeep daemon instruct-agent <AGENT_ID> <INSTRUCTION>`、受 Token 保护的 `POST /v1/agents/{id}/instructions`，以及 TUI `/agent instruct <AGENT_ID> <INSTRUCTION>`。
- Core 新增子 Agent 指令收件箱，在下一次模型请求前注入父级补充指令；补充指令与模型结束发生竞态时继续下一轮，不丢指令。

### Changed

- CLI Agent 列表显示当前/最大轮次、已用/预算 Token 与执行时限；单 Agent 查询额外打印策略和最终报告。

### Security

- 追加指令限制为 1–16384 字节且只允许发送给当前 Harness 内运行中的后台子 Agent；命令应用、拒绝或 Runtime 重启后立即清除正文，事件审计不记录正文。
- Agent 报告采用 UTF-8 边界安全截断，并只通过本地受 Token Runtime API 暴露。

## [0.18.0-rc2] - 2026-08-10

### Added

- 子 Agent Profile 新增 `token_budget`、`timeout_seconds` 和 `max_consecutive_failures` TOML 策略；示例配置给出保守默认值和环境变量凭据用法。
- 核心 Agent Loop 累计 Provider 用量，在达到 Token 总预算时以结构化错误终止；子 Agent 执行支持硬超时。
- Runtime Agent 持久化启动时的最大轮次、Token 预算和执行时限，TUI 右栏同时展示实时用量与策略上限。

### Changed

- 同一子 Agent Profile 连续失败达到阈值后开启进程内熔断，拒绝继续启动；任一成功执行会自动复位该 Profile 的失败计数。

### Security

- 预算、时限与熔断均在 Core/Runtime 执行，不依赖客户端自觉；策略事件不包含 Prompt、工具参数、Provider 凭据或文件内容。

## [0.18.0-rc1] - 2026-08-10

### Added

- 新增 `docs/HERDR_RESEARCH_AND_INTEGRATION.md`，基于 Herdr 官方资料记录状态权威、Snapshot+事件、状态聚合、Socket 控制面、许可证边界和分阶段集成方案。
- 新增 `willdeep integrations herdr status [--json]`，检查 Herdr CLI、Pane 环境、Socket 配置和生命周期上报就绪状态，不输出 Socket 路径。
- Runtime 在 Herdr Pane 环境内将全部 Task 聚合为 `working/blocked/idle`，通过公开 `herdr pane report-agent` CLI 非阻塞上报并去重相同状态。

### Security

- Herdr 仅为可选进程级适配器，不成为 Runtime 状态来源；未安装、版本不兼容或上报失败均不影响 Harness。
- 上报命令使用结构化进程参数，不经过 Shell，不传输 Prompt、API Key、工具参数、文件内容或 Socket 路径。
- 研究文档明确 Herdr 当前 AGPL/商业双许可证边界；WillDeep 不复制或链接 Herdr 源码。

## [0.17.0-rc8] - 2026-08-10

### Added

- Runtime Session 持久化可选模型覆盖；创建 Session、独立 Runtime 任务及 Session Turn 均把模型传入原生 Harness。
- CLI `fork-session` 新增 `--provider-profile`、`--model`，TUI `/session fork` 支持 `--profile`、`--model` 和 `--through`，Web Fork API 支持相同结构化字段。
- Session Search 支持组合 Workspace、状态、Provider Profile、模型及更新时间上下界；CLI 与 TUI 均可组合文本和结构化过滤条件。

### Changed

- CLI Session 列表与搜索结果显示实际 Provider Profile 和模型；Fork 的 Provider 覆盖同步写入 Core Session Profile，Runtime 仍是模型覆盖的事实来源。

### Security

- Provider Profile 和模型覆盖统一去除首尾空白、拒绝空值并限制长度；搜索没有文本时必须至少提供一个结构化过滤条件。

## [0.17.0-rc7] - 2026-08-10

### Added

- Runtime Turn 持久记录 Core 消息起止边界；CLI `fork-session --through-turn`、Runtime/Web Fork 请求和 TUI `/session fork-turn` 支持精确保留到指定已完成 Turn。
- TUI 新增 `/session switch <SESSION_ID>`，可在同一 Workspace 内不退出进程切换并恢复聊天会话。

### Changed

- TUI Runtime 事件跟随器将可见任务关联到 Session，聊天区只渲染当前 Session 的模型、工具和子 Agent 输出。

### Security

- 指定 Turn Fork 不根据消息角色或文本猜测边界；旧 Turn 缺少持久边界、Turn 未完成或不属于源 Session 时一律拒绝。
- TUI 暂不允许跨 Workspace 原地切换，避免复用旧 Workspace 的 Agent、Skills 和安全边界。

## [0.17.0-rc6] - 2026-08-10

### Added

- Runtime Session 新增 Rename、完整消息快照 Fork、Archive/Unarchive、Delete、JSON Export 和标题/消息 Search 的受 Token API 与 CLI。
- TUI 新增 `/session rename|fork|archive|unarchive|search|export|delete`，命令面板可发现；Web 会话侧栏新增本地标题筛选以及重命名、分叉、归档、导出和删除操作。
- Session 生命周期操作发布 `session.renamed/forked/archived/unarchived/deleted` 持久事件。

### Changed

- 将 TUI Session 命令和命令候选拆分为独立模块，使主 TUI 文件重新低于 3000 行上限。

### Security

- Rename、Fork、Archive 和 Delete 拒绝活跃或仍有排队 Turn 的 Session，避免与 Harness 消息写入竞争；Delete 要求精确 ID 二次确认，TUI 禁止删除当前打开会话。
- Fork 不复制 Turn、Task、Interaction、事件游标或 Inbox 已读状态；Export 不包含队列私密 Prompt、附件副本、Runtime Token、Provider 凭据和内部游标。
- Web Session 操作只允许作用于配置的 Workspace；未认证 Web 不新增消息摘要全文搜索端点，全文搜索保留在受 Runtime Token 保护的 CLI/TUI。

## [0.17.0-rc5] - 2026-08-10

### Fixed

- 修复 Runtime 在等待审批或 `ask_user` 时重启，只取消 Interaction 却让 Task 永久停留在 Waiting 状态的问题；所有遗留 Running、Cancelling 和 Waiting Task 现在统一收敛为 Interrupted。
- 修复 `/v1/shutdown` 先等待 HTTP Handler 退出、后取消 Harness，导致 Pending Interaction 与优雅停止互相等待的问题；现在先异步取消并收敛 Harness，再关闭 Server。
- 修复 Daemon 异常退出后首次 `start` 等待时间短于陈旧锁期限、必然无法接管的问题；单实例锁现在由活 Daemon 定期续租，启动方会持续等待并安全接管过期租约。
- 清除恢复任务中的遗留 PID，并为恢复的 Task、Turn 和 Pending Interaction 补写持久事件，使 TUI/Web 从旧游标重连后能观察中断与取消状态。

## [0.17.0-rc4] - 2026-08-10

### Added

- 新增 CLI/Runtime 共用 Harness Factory 和非交互执行入口，统一 Provider、视觉降级、审批、Skills、MCP、Tools、子 Agent Profile、会话写入与后台结果回流。
- 新增 Runtime 原生事件 Sink，Agent 事件直接写入持久 EventLog 并同步 AgentStore。

### Changed

- Runtime TaskManager 改为在 Daemon 内调度 Harness Future，不再为每个 Turn 启动 `willdeep --web-input-json` 子进程或转写 stdout/stderr。
- 审批、`ask_user`、取消和后台 Agent 控制继续使用原持久 Runtime 状态机；其他 CLI 客户端解决交互后，同一个 Harness Future 从等待点恢复。

### Fixed

- Runtime 取消现在直接撤销完整 Harness Future，并通过唯一收尾路径提交终态，避免子进程退出与 Turn 状态竞争。

### Security

- Runtime Token 不再通过每 Turn 私有 stdin 发送给子进程，只保留在 Daemon 和 Harness 内存中，且不会传入 Shell 或 MCP 环境。

### Documentation

- 定义并完成 Daemon 内原生 Harness 的共享 Factory、Invocation、Event Sink、审批、取消、生命周期、迁移步骤和可验证验收条件。

## [0.17.0-rc3] - 2026-08-10

### Added

- Web 新增持久 Session 历史读取和真实 Runtime Turn 停止 API；SSE 提交事件返回稳定 Session、Turn 和 Root Agent ID。
- 路线图加入覆盖 Runtime、Web/TUI、审批、多模态、后台任务、Agent Team、Workspace、远程、Computer Use、CLI、Skills、质量和 Swift 替换的完整逐项交付清单。

### Changed

- Web 聊天改为创建或收养 Core Session 并提交统一 Runtime Turn，转发持久 Runtime 事件；浏览器断开只分离观察者，后台任务继续执行。
- Web 历史会话从 Core Session 恢复用户与助手消息，完成后刷新会话列表；API 响应统一暴露 `X-App-Version`。

### Fixed

- Web 停止按钮即使在 Turn ID 返回前被点击，也会等待提交完成后取消真实 Turn，避免只中断浏览器而留下后台任务继续运行。
- 修复 Runtime 在 Turn 已被调度器领取但尚未绑定 Task 时取消，仍可能启动 Harness 并把 Cancelled 覆盖成 Completed 的竞态；Task 绑定现在会原子复核 Turn 与 Session 所有权。

## [0.17.0-rc2] - 2026-08-10

### Added

- TUI 新增 `/local <任务>`，可显式让单轮任务使用原进程内 Harness，作为 Runtime 迁移期兼容入口。

### Changed

- TUI 普通 Prompt 默认进入当前长期 Runtime Session 的持久 Turn；`/runtime` 保留为明确别名，并与普通输入复用同一提交状态机。
- 命令候选、命令面板和帮助文案明确展示默认 Runtime 与 `/local` 行为。

### Fixed

- Runtime 托管 Session 收到中间事件时先重载 Core 历史，仅合并游标与已读状态，防止 TUI 内存旧快照覆盖后台 Harness 刚保存的消息。

## [0.17.0-rc1] - 2026-08-10

### Added

- Runtime 可幂等收养现有 Core Session，保持 Session ID、历史消息、Workspace 和稳定 Root Agent 身份不变。

### Changed

- TUI `/runtime` 改为向当前长期 Runtime Session 提交 Turn，不再为每轮创建彼此孤立的一次性 Runtime Task。
- Runtime 托管会话由 Harness 独占写入消息历史；TUI 立即显示用户输入，并在 Turn 终态从 Core Session 同步完整历史，避免重复消息或覆盖后台上下文。
- TUI 在调度 Turn 前持久化 Runtime 托管标记和事件游标，避免极快 Provider 完成后被客户端旧会话快照覆盖。

## [0.16.0-rc10] - 2026-08-10

### Added

- 定义稳定 Runtime Session / Root Agent / Turn / Execution Task 身份、状态、幂等、恢复与安全协议。
- Runtime 新增持久 Session/Turn 元数据、稳定 Root Agent ID，以及受 Token 保护的 Session/Turn 创建、列表、详情和停止 API。
- CLI 新增 `daemon create-session/sessions/session/submit-turn/turns/turn/stop-turn`，支持从脚本管理长期 Runtime 会话。
- Turn 使用客户端 `request_id` 幂等入队，同一 Session 按持久序号严格串行；成功执行后清除队列中的 Prompt 和附件副本。
- Runtime 为 Turn 发布 queued/started/completed/failed/cancelled/interrupted 事件，并在审批或提问时同步等待状态。

### Changed

- Runtime 启动时将遗留活动 Session/Turn 明确恢复为 Interrupted，自动恢复未开始的排队 Turn，同时保持 Core Session 消息文件为唯一历史来源。
- 排队 Turn 可在 Task 绑定前安全取消并释放 Session，避免领取/启动竞争窗口阻塞后续队列。

## [0.16.0-rc9] - 2026-08-10

### Added

- Runtime 新增受 Token 保护的 `GET /v1/events/stream` SSE 事件流；连接时从指定游标分页补齐持久事件，随后实时推送新事件。
- SSE 客户端落后广播窗口时自动从 NDJSON 日志继续追赶，并使用单调序号消除历史读取与实时广播交界处的重复事件。
- TUI Runtime Bridge 使用可取消的长连接 follower 实时消费事件；连接旧版 Daemon 时自动回退到原轮询接口。

### Changed

- EventLog 与 SSE 实现拆入独立模块，Agent 控制 HTTP/CLI 处理器迁入 `agent_control`，Daemon 主文件恢复到 2000 行以内。

### Security

- SSE 端点沿用 Runtime 随机本地 Token 鉴权；TUI 仍按当前 Workspace 查询任务归属，不向会话展示其他 Workspace 的事件内容。

## [0.16.0-rc8] - 2026-08-10

### Added

- 后台任务保存稳定 Child Agent UUID，并支持按 Agent UUID 精确取消或重试；重试产生新的后台任务 ID，但保持同一 Agent 身份。
- Runtime 新增受 Token 保护的持久 Agent 命令队列和 stop/retry API；原 Harness 轮询执行命令并回写 applied/rejected 结果。
- CLI 新增 `daemon stop-agent` 与 `daemon retry-agent`；TUI Runtime 区支持上下选择 Agent，并用 `K/R` 请求停止或重试。

### Changed

- 后台子 Agent 的 Running、Completed、Failed、Blocked 与 Cancelled 状态统一由后台任务生命周期驱动，重试时清理旧轮次、工具、Token 和错误状态。
- TUI 侧栏交互方法继续拆入独立模块，主文件保持在 3000 行以内。

### Security

- Agent 控制 API、Harness 拉取和结果确认均要求 Runtime 本地随机 Token；命令仅能作用于所属 Runtime Task 的后台 Child Agent。

## [0.16.0-rc7] - 2026-08-10

### Added

- Core 新增带稳定 UUID 的子 Agent 启动、轮次、工具、用量和完成事件；前台及正常完成的后台 `spawn_agent` 均上报 Profile、Label 与运行模式。
- Runtime 根据结构化事件创建 Child Agent，并持久关联 Root `parent_id`；子 Agent 独立记录轮次、当前工具、Token、终态与 Blocked 状态。
- TUI Runtime 区域按父子层级展示 Root/Child Agent，标记后台模式；聊天活动栏同步子 Agent 的轮次和工具进度。
- CLI Agent 输出补充 Profile、Label 和前台/后台模式。

### Changed

- `spawn_agent` 返回值同时包含稳定 Agent ID 与后台任务 ID，便于父 Harness、Runtime 和用户精确关联。
- 子 Agent 的 Blocked 不再映射成 WaitingApproval，避免不存在待处理审批时误导用户。

## [0.16.0-rc6] - 2026-08-10

### Added

- Runtime 新增持久 `RuntimeAgent` 实体；每个托管任务拥有稳定 Root Agent ID，并预留 `parent_id` 支持后续原生子 Agent 树。
- 受 Runtime Token 保护的 `GET /v1/agents`、`GET /v1/agents/{id}` 以及 `daemon agents/agent` 查询结构化 Agent 生命周期。
- Agent 持久记录 Workspace、Profile、状态、当前轮次、当前工具、Token 用量、完成时间和错误；Daemon 重启时将遗留活动 Agent 标记为 Interrupted。
- TUI 右栏 Runtime 区域展示当前 Workspace 的 Agent、状态、Profile、轮次、工具和 Token 摘要。

### Changed

- Harness 的轮次、工具和用量事件直接驱动 Agent 状态，不通过终端文本或画面推断。
- 旧 Runtime Task 在加载时自动补建 Root Agent 并回填 `agent_id`，保持现有任务数据向后兼容。

## [0.16.0-rc5] - 2026-08-10

### Added

- TUI 按持久事件游标补读 Runtime 的模型轮次、工具请求、工具结果、Token 用量和完成事件，并实时映射到聊天区与活动栏。
- Runtime 健康响应暴露最新事件序号；任务保存首个事件序号，使首次从 TUI 提交时既不回放旧日志，也不会遗漏快速完成的任务。
- Session 保存 Runtime 事件游标、`/runtime` 用户消息和正式回复；退出并恢复同一会话后聊天记录保持完整且不重复。

### Changed

- Runtime Inbox、Interaction 与事件补读按当前 Workspace 过滤；不可见事件仍推进全局游标，避免跨工作区泄漏或反复扫描。
- `/runtime` 允许仅附件任务，文本和附件不能同时为空。
- 扩展 CLI、TUI、Computer Use、Herdr 互操作和 Swift Harness 替换的完整产品路线图与执行顺序。

## [0.16.0-rc4] - 2026-08-10

### Added

- TUI 每秒同步 Runtime 任务与 Pending Interaction，并合入右栏 Attention Inbox；远端运行、等待、失败和完成状态与本地条目统一排序。
- 在 Runtime 审批或提问条目上按 Enter，复用现有 Allow/Disallow/Always Allow 和 ask_user 弹窗；解决结果发回原后台 Harness。
- Runtime 任务条目支持 `K` 请求停止；TUI 状态栏显示成功或失败反馈。
- Composer 新增 `/runtime <task>`，把文本、粘贴文本附件和图片附件提交给可分离 Runtime；TUI 退出不终止任务。

### Changed

- `/` 命令候选和帮助加入 `/runtime`，Runtime 提交沿用当前 Workspace、Provider Profile、配置路径和显式 Skills 展开结果。

## [0.16.0-rc3] - 2026-08-10

### Added

- Runtime 托管 Harness 遇到工具审批或 `ask_user` 时进入持久 Pending Interaction，不再因无终端 stdin 而直接拒绝。
- 新增 `daemon pending/resolve/answer`：支持 Allow once、Deny、Always allow，以及候选外自由输入；解决后原 Harness 在同一任务中继续。
- Runtime Task 新增 WaitingApproval 与 WaitingAnswer 状态，创建、解决、取消和最终执行顺序写入可续传事件流。

### Security

- Runtime 控制 Token 仅通过启动时私有 stdin 消息传入 Harness 内存，不再放入子进程环境，Shell 和 MCP 子进程不会继承该凭据。
- Interaction 类型与解决类型严格匹配；不可 Always Allow 的审批不能通过伪造解决请求升级权限。

### Changed

- 将 Daemon 单元测试拆入独立模块，生产实现保持在 2000 行以内。

## [0.16.0-rc2] - 2026-08-10

### Added

- Runtime 新增持久任务模型与 `daemon submit/tasks/task/cancel`，非交互 Harness 由 Daemon 持有，提交客户端退出后继续执行。
- Prompt 通过私有 stdin 传给 Harness 子进程，不出现在进程参数；模型事件、最终结果、session_id、错误和退出状态回流持久事件日志。
- 任务支持 Queued、Running、Cancelling、Completed、Failed、Cancelled、Interrupted 状态，Daemon 异常重启时将未完成记录安全标记为 Interrupted。
- Daemon 新增跨平台单实例租约锁、并发启动协调和失效锁恢复；停止 Runtime 时主动取消其持有的 Harness 进程。

### Changed

- 所有 Runtime 本地 API 响应统一携带 `X-WillDeep-Version`，任务元数据使用私有原子 JSON 文件保存。

## [0.16.0-rc1] - 2026-08-10

### Added

- 新增 `willdeep daemon start/status/stop/logs`，提供跨平台持久 Runtime 的首个可运行控制面。
- Daemon 使用随机回环 TCP 端点和仅本机可读的随机控制 Token，健康响应携带当前服务版本。
- Unix 后台进程使用独立进程组，Windows 使用 detached process flags，启动命令退出后 Runtime 仍持续运行。
- Daemon 状态采用原子写入，Unix 状态文件和日志权限收紧为 `0600`，支持优雅关闭和失效状态恢复。
- 新增持久化 NDJSON Runtime 事件日志与单调序号；`willdeep attach --after <cursor>` 可补齐离线事件，`Ctrl+C` 或 `willdeep detach` 不会停止 Daemon。

## [0.15.0-rc4] - 2026-08-10

### Added

- Git 未合并冲突和待审 Diff 进入 Attention Inbox；条目 ID 包含真实暂存/未暂存差异及受限未跟踪内容签名。
- Inbox 的 Worktree/Diff 条目支持精确详情、标记已读，新内容不会错误继承旧 Diff 的已读状态。
- 子 Agent 因审批拒绝时结构化上报 `blocked`，可在权限条件改变后重试。
- Core 新增 Agent→Session→Workspace 状态树，TUI 运行状态使用相同优先级逐层上卷。

## [0.15.0-rc3] - 2026-08-10

### Added

- Inbox 已读集合随会话 JSON 持久化，旧会话缺少该字段时保持向后兼容。
- 后台 Shell 和子 Agent 保存安全的可重放启动器，失败或取消后可按 `R` 创建新任务并真实重试。
- 后台任务成功、失败或取消时发送终端 BEL 提示，并继续把结果交还主 Harness。

## [0.15.0-rc2] - 2026-08-10

### Added

- Attention Inbox 支持键盘选择、自动跟随滚动、鼠标打开详情、停止运行任务以及标记终态条目已读。
- 右栏快捷键帮助补充 Inbox 的 Enter、K、M 操作。

### Changed

- 将 TUI 侧栏渲染和测试拆分为独立模块，使生产主文件保持在 3000 行以内。

## [0.15.0-rc1] - 2026-08-10

### Added

- Core 新增统一 Runtime 状态、Attention 来源与分组模型，覆盖审批、提问、后台 Shell、子 Agent、Worktree 和 Diff 审查。
- 后台任务状态可映射为统一的工作中、失败、完成和取消状态，并支持按人工介入优先级排序与父级状态聚合。
- TUI 右栏新增 Attention Inbox，按“需要你处理”“正在工作”“最近完成”聚合审批、提问、失败任务、后台 Shell 和子 Agent。

## [0.14.0-rc4] - 2026-08-10

### Fixed
- `ask_user` 长问题自动换行时，按视觉行计算弹窗高度与选项鼠标命中位置。

### Added

- 聊天区加入 TUI 焦点循环；`Ctrl+W` 在 Prompt、聊天和状态栏之间切换，点击或滚轮进入聊天焦点，方向键滚动，Esc 返回 Prompt。
- `/` 命令和 `$` 技能候选支持鼠标点击插入；审批按钮支持鼠标决策，单选 ask_user 支持点击提交，多选支持点击勾选并从底部操作区发送或跳过。
- 聊天搜索框支持鼠标定位编辑光标，后台任务详情支持鼠标滚轮。

### Changed

- Prompt、聊天和状态栏统一使用高亮边框、焦点标题和底部状态提示。

## [0.14.0-rc3] - 2026-08-10

### Added

- TUI 增加 `Ctrl+P` 全局命令面板，统一模糊搜索命令、Skills、当前与最近会话、后台子 Agent/任务和工作区文件。
- 命令面板支持方向键、Tab/Shift+Tab、Enter、Esc 和鼠标选择；命令、技能与文件插入 Prompt，后台任务直接打开详情。

### Security

- 工作区文件候选最多读取 300 个相对路径，跳过重型构建目录与符号链接，不读取文件正文，也不越过当前 Workspace。

## [0.14.0-rc2] - 2026-08-10

### Added

- TUI 增加 `Ctrl+F` 聊天搜索，支持大小写不敏感匹配、高亮、`Enter`/`Shift+Enter` 循环跳转及 Esc 关闭。
- 右侧状态栏标题支持点击折叠或展开，滚轮改为滚动内容；后台任务可点击或从任务分组按 Enter 打开详情，查看元数据与最近输出，运行中的任务可按 `K` 请求停止。

### Changed

- 右栏鼠标命中基于实际可见行计算，手动滚动时不再被当前选中分组强制拉回。

## [0.14.0-rc1] - 2026-08-10

### Added

- 新增 `docs/CLI_TUI_RUNTIME_ROADMAP.md`，记录从 TUI 交互、Attention Inbox、Runtime Daemon 到 Agent Mission Control、Review、统一 API、移动端、Workflow 和 Herdr 互操作的完整实施计划与验收标准。
- TUI 增加 `F1` 全局快捷键帮助；空 Prompt 时也可用 `?` 打开，避免拦截普通文本中的问号。
- Prompt 与状态栏使用明确的焦点边框和标题，底部状态行同步显示当前焦点及帮助入口。

### Changed

- 当前开发阶段进入 v0.14.0，按路线图推进 TUI 交互基础收尾。

## [0.13.0-rc5] - 2026-08-10

### Added

- TUI 右侧状态栏支持鼠标或 `Ctrl+W` 聚焦、方向键选择、Enter/Space 折叠展开、Esc 返回 Prompt，以及 `Ctrl+B` 整体显示或隐藏；窄终端使用覆盖层呈现。

## [0.13.0-rc4] - 2026-08-10

### Added

- TUI Prompt 输入 `/` 或命令前缀时展示带说明的命令候选，支持上下方向键选择、Enter/Tab 插入及 Esc 关闭，选择候选不会立即执行命令。

## [0.13.0-rc3] - 2026-08-10

### Added

- TUI Prompt 输入 `$` 或 `$关键词` 时展示技能名称与描述候选，支持上下方向键选择、Enter/Tab 插入及 Esc 关闭；技能正文仍只在发送后读取。

## [0.13.0-rc2] - 2026-08-10

### Fixed

- TUI 改用 Ratatui 实际排版行数计算聊天区滚动范围，避免按单词换行或 Markdown 样式渲染后最后一行被裁掉。

## [0.13.0-rc1] - 2026-08-10

### Added

- TUI 在聊天区末尾临时展示单行、限长的模型阶段文本，正式回复完成后自动移除并替换为答案。
- Web 增加吸附在 Composer 上方的单行思考/工作状态、逐轮工具轨迹，以及发送/停止图标按钮；停止会断开 SSE 并终止对应 Harness 子进程。
- Web 支持粘贴长文本和图片、发送前预览与删除；附件仅在点击发送后交给当前 Provider，非多模态模型继续使用配置的视觉模型解析。
- Web Composer 支持 `/` 命令候选、`/help`、`/goal`、`/compress`、`/skills`、`/clear`，以及 `$` 技能名称/描述候选。
- Web 技能候选排除可能包含历史 Prompt 的 `auto-*` 录制项，并隐藏带明显凭据标记的描述。
- Web 聊天区使用独立历史滚动容器，用户向上浏览时暂停自动追底；TUI 增加鼠标滚轮浏览历史。
- TUI 渐进渲染 Markdown 标题、粗体、行内代码、引用、列表、代码块和链接。
- TUI 增加 `Ctrl+S` 文本选择模式，释放鼠标给终端原生拖选与复制，再次切换恢复滚轮和点击。

### Changed

- 未配置审批模式时默认使用 `smart` 智能审核。
- 智能审核精确免审 `cargo test` 及其后仅包含 `grep`、`head`、`tail` 的输出过滤管道；重定向、命令连接、命令替换及其他 Cargo 子命令仍需审批。

## [0.12.0-rc1] - 2026-08-10

### Added

- TUI 与 Web 支持简体中文、英语和日语；可通过 `agent.language`、`--language`、`WILLDEEP_LANGUAGE` 或 Web 语言选择器切换。
- Web 语言偏好保存在浏览器本地，并随 SSE 请求传递，使实时 Harness 状态也使用所选语言。

### Changed

- TUI 审批弹窗将 `Y`/`A` 显示为黄色高亮键，将拒绝键 `N` 显示为红色高亮键，动作说明同步本地化。

## [0.11.0-rc1] - 2026-08-10

### Added

- 新增 `--web` 内嵌 Web Server，提供 React + Chakra UI + Vite 纯 CSR 聊天页、健康检查、会话索引和 SSE Chat API。
- Web 模式支持严格 allowlist 内的多工作区切换；应用层保持单用户无鉴权，认证与 HTTPS 由 Nginx/VPN 等上游负责。
- 用户消息发送后立即显示；Harness 轮次、工具与压缩阶段通过 SSE 实时更新，TUI 同步显示最近三条可验证工作进度。
- 新增 Xedit 工具能力对照文档，明确 Rust 内核、Skill/MCP 上层能力及 macOS Computer Use 安全实现路线。

### Changed

- JSON 完成事件增加 `session_id`，Web 客户端可继续同一历史会话；SSE 不发送工具参数、输出或模型私有思维链。

## [0.10.0-rc2] - 2026-08-09

### Fixed

- 空白新会话进入 TUI 时立即显示包含当前工作区名称的本地欢迎语；恢复历史会话不重复显示，也不把欢迎语写入模型上下文。

## [0.10.0-rc1] - 2026-08-09

### Added

- 首次使用交互式 onboarding，支持手动 Provider 配置和 some.im 浏览器登录轮询，并以 `0600` 权限保存 TOML 配置。
- 加载 `~/.willdeep/CLAUDE.md`、项目根 `PRODUCT_OVERVIEW.md`、`AGENTS.md` 与 `CLAUDE.md` 规则。
- 新增 `git_diff`、`list_worktrees`、`create_worktree` 工具；TUI 右栏显示项目、分支、diff 文件数和 worktree 数。
- 在 macOS 上读取 Swift WillDeep 的 Project 列表与历史会话，可用 `--project` 和 `--resume` 继续工作。

### Changed

- 从 Swift 导入的会话续聊会安全保存为 Rust 副本，不覆盖 Swift 原始 JSON；待共享 schema 稳定后再开放双向原地写入。

## [0.9.0-rc1] - 2026-08-09

### Added

- 增加 Core Harness `ask_user` 工具，支持候选单选、多选、跳过以及自由输入其他答案；TUI 与普通终端均可交互。
- 审批决策升级为 Allow once、Disallow、Always allow；TUI 使用 Y/N/A，普通终端提供同等选项。
- Always Allow 规则持久化到 `$WILLDEEP_HOME/always-allow.json`，增加 `--list-approvals` 与 `--clear-approvals`。

### Security

- Shell Always Allow 仅匹配规范化后的完整命令，含管道、重定向、命令连接或换行的命令不提供持久授权；MCP 按精确 server/tool 记忆。
- 文件写入、网络跳转、后台任务取消和 editor 单文件授权不允许持久放行。
- `ask_user` 问题、选项和答案均限制长度，用户自由输入在回传模型前进行标记转义。

## [0.8.0-rc2] - 2026-08-09

### Fixed

- `web_fetch` 不再一律拒绝 HTTP 重定向：同 hostname 最多自动跟随 8 跳，跨 hostname 时重新审批；每一跳重新校验公网地址，并拒绝 HTTPS 降级到 HTTP。

## [0.8.0-rc1] - 2026-08-09

### Added

- `run_command` 支持 `run_in_background`，返回 `job_xxxxxx`；增加 `get_job_output` 与 `kill_job`。
- 后台 Shell 和后台子 Agent 成功、失败、超时或取消后，结果自动注入主 Harness 并触发后续处理。
- 增加 `spawn_agent`，支持前台或后台运行及 `scout`、`reader`、`deep`、`editor` 四种内置工种。
- TOML 增加 `[subagents.<trade>]`，可为各工种绑定不同 Provider Profile、模型、上下文和轮数。
- TUI 宽屏右栏实时显示后台任务/子 Agent 的 ID、状态、耗时和标签。

### Security

- 子 Agent 禁止嵌套派生；只读工种不含写工具和 Shell。
- `editor` 必须单独审批 canonicalize 后的现有目标文件，子 Agent 只能编辑该文件。

## [0.7.0-rc1] - 2026-08-09

### Added

- 增加 `/compress` 本地命令，可立即总结较旧会话、保留最近六条消息并原子保存压缩后的当前会话。

## [0.6.0-rc1] - 2026-08-09

### Added

- some.im 纯文本主模型收到图片时，使用同一 API Base 和 API Key 调用 `qwen3-vl-plus` 生成描述，再把描述交给主模型；支持 `vision_model` 覆盖。
- 增加 `web_search` 和带公网目标校验、拒绝跳转、大小限制的 `web_fetch`，网络操作始终审批。
- TUI 状态栏显示上下文占比、最近输入/输出 Token 和耗时；宽屏增加后台状态侧栏。
- 上下文达到配置窗口约 80% 时生成临时摘要请求视图，保留完整会话存档。

### Changed

- `search_files` 与 `grep_files` 优先调用 `rg`，不可用或执行异常时回退到内置跨平台扫描器。
- Prompt 区最低保持三行，思考与工具阶段默认压缩为单行活动摘要，`Ctrl+O` 可展开工具明细。

## [0.5.0-rc2] - 2026-08-09

### Fixed

- 为无 checkout 的发布 Job 显式设置 `GH_REPO`，修复四平台产物完成后 GitHub Release 创建失败。

## [0.5.0-rc1] - 2026-08-09

### Added

- 增加 `/goal`、`/skills`、`/clear`、`/help` 本地命令和 `$skill-name` 显式技能引用。
- 增加 `/mobile` 二维码界面，通过 `j.niuwoai.com` WebSocket Relay 接入现有 Android Mobile Gateway 协议。
- 手机消息进入当前 CLI 会话，运行中的请求自动排队；助手回复以 `message.append` / `message.done` 回传手机。
- TUI 按用户、助手、系统和错误类型显示不同颜色。
- 增加 Linux AMD64/ARM64 交叉测试、WSL ABI 烟测和四产物 tag Release workflow。

### Changed

- 审批模式对齐为 `strict`、`smart`、`workspace-write`；后两者仅免审当前工作区内的创建与编辑，Shell、MCP 和网络仍需审批。

## [0.4.0-rc1] - 2026-08-09

### Added

- TUI Prompt 升级为可换行编辑器，支持左右/上下、Home/End、鼠标点击定位和内部滚动。
- 支持 `Shift+Enter`、`Alt+Enter` 或 `Ctrl+J` 插入换行，`Enter` 发送。
- 支持 Bracketed Paste；多行或长文本显示为可删除的粘贴附件。
- 支持从系统剪贴板读取图片、编码 PNG、展示附件摘要，并通过 `Ctrl+D` 删除附件。
- 图片附件原生编码到 Chat Completions、Responses 和 Anthropic Messages 三种协议，并随会话持久化。

## [0.3.0-rc2] - 2026-08-09

### Fixed

- TUI 聊天记录支持方向键、翻页键、Home/End 滚动，并在手动查看历史时暂停自动跟随。
- Tool Use 默认聚合为紧凑活动摘要，支持 `Ctrl+O` 展开最近明细，失败计数保持可见。

## [0.3.0-rc1] - 2026-08-09

### Added

- 增加 Ratatui 多轮交互界面，并在界面内处理工具审批和 Agent 事件。
- 增加版本化 JSON 会话存储、原子写入、`--list-sessions` 与 `--resume`。
- 增加多目录 `SKILL.md` 发现、`list_skills`、`read_skill` 和安全资源边界。
- 增加 TOML `mcp_servers` 配置、stdio 生命周期、MCP 初始化、工具发现和命名空间调用。
- MCP 调用接入统一审批，会话在模型请求前先持久化用户输入。

### Changed

- Agent 支持用历史消息继续运行，并始终刷新当前系统提示词。
- 工具定义改为可拥有字符串，以支持运行时 MCP 工具注册。

## [0.2.0-rc1] - 2026-08-09

### Added

- 增加 `~/.willdeep/config.toml` 配置加载与 `--config`/`--profile`。
- 支持在 TOML 中定义多个 Provider、API Dialect、API Base、模型和输出上限。
- 支持 `api_key_env` 安全引用环境变量，也允许受权限保护的明文 `api_key`。
- 增加 Agent 最大轮数与审批模式配置。
- 增加可直接复制的 `config.example.toml`。

## [0.1.0-rc1] - 2026-08-09

### Added

- 建立 Rust workspace、Core crate 与 CLI crate。
- 实现 OpenAI Chat Completions、OpenAI Responses、Anthropic Messages 三种协议适配器。
- 实现 some.im Provider 识别、Bearer 鉴权和会话/工作区上下文请求头。
- 实现模型—工具多轮 Agent Harness。
- 实现与 Swift App 同名的首批工作区工具：搜索、正则搜索、文件读取、目录列表、Git 状态、命令执行、文件创建与精确编辑。
- 实现工作区路径隔离、交互审批、全自动模式和 NDJSON 事件输出。
- 增加三协议完整工具往返契约测试和 macOS/Linux/Windows CI。
