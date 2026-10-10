# Changelog

## [0.46.0-rc1] - 2026-10-09

### Added
- 短剧持续改进账本：自动收集指定剧的失败与需要人工处理任务，持久化带摘要链的事件和私有证据，区分依赖、创作与待复现工程问题。
- 固定复现包、原版失败/候选通过对照、跨重启最多两个候选、原验证器保护；只有完整回归、真实运行 readback 与实际产物核对后才能记 resolved。
- 安装启动与安装结果未知分别记录；结果未知时拒绝盲目重装。补充九项隔离行为测试和持续运行规范。

## [0.45.0-rc1] - 2026-10-09

### Added
- Rust 宿主短剧制作与插件修复技能 `production-repair`，支持持久化制作检查点、固定回归验证、摘要绑定安装和新进程 readback；保护原验证器与权限，不修改宿主注册表。
- 只读 MCP 工具 `system.status` 暴露真实插件版本、安装根与进程归属，防止把文件已安装误判为旧进程已更新。
- 更新前检查后台任务与已提交的上游视频；存在在飞工作时拒绝切换。安装与运行验证分阶段记录，Web 重连前保留 pending 状态。

## [0.44.0-rc2] - 2026-10-07

### Fixed
- Linux 成片字幕渲染兼容 ImageMagick convert，CI 与发布流水线安装渲染器和 Noto CJK 字体，避免只有 ffmpeg 时字幕断言失败。

## [0.44.0-rc1] - 2026-10-07

### Added
- 主框架及单集专家席新增「圆桌创作」：空稿生成初稿，已有稿先审；按必改项修订并复审，默认最多两次修订，未解决则保留最新稿与意见交给用户。MCP 工具 `drama.write_with_panel` 支持后台进度、取消、请求去重和明确的修订上限。
- 写稿使用助手模型，审稿使用审核模型；每次采用保留历史，并以修订号、内容与账本版本防止覆盖运行期间的用户修改。已有待采用草稿时拒绝启动。支持 Rust 0.90.0-rc1 的宿主圆桌及旧宿主的插件专家席回退。

## [0.43.0-rc1] - 2026-10-07

### 用户影响
- **成片可以按配音节奏剪。** 成片页设置新增「节奏来源」：画面节奏（每镜整条用，现状）或配音节奏（台词说完再留 0.4 秒就切到下一镜；没台词的镜头最多留 2 秒；两个数都可调）。分镜页多一个「留白镜头」勾选，勾上（或运镜 / 剧情里写了「留白」「反应镜头」「空镜」）的镜头保留全长。逐镜可以单独选节奏。算出的裁剪只随计划走、不落库，成片页逐镜显示「配音节奏：只用到 N 秒」，改回画面节奏就恢复；已有的手动 / 质检裁剪在它的窗口内再收。
- **配音节奏下少付视频的钱。** 请求时长按台词反推：有口型音轨的镜头按音轨长度加留白向上取整（下限 4 秒），没台词的按上限，留白镜头照分镜时长。台词重配后口型对的是旧配音的镜头，在配音节奏下从提醒升为合成拦截，先重新生成那一镜。
- **口型分两档。** 分镜技能写明：情绪重的台词给说话人近景并走「首帧加对白音轨」（口型跟配音走），交代信息的台词用反应镜头、过肩、背影、手部或画外音。镜头参考包对「有台词、说话人近景或特写、却没有对白音轨」的镜头提醒 `lip_sync_risk`。
- **专家席可以在 WillDeep 的圆桌页里开。** 装了 WillDeep 1.412.0-rc1 及以上时，「开专家席」整场交给宿主的专家圆桌：六位专家轮流发言（真流式）、轮末小结、终稿，终稿之后主持人按同一份契约出结论写回审核面板；圆桌页里能看到每位专家怎么说，发言记录带立场。设置页 `panelRoundtable` 可关（关了就走插件自己并行问专家的路，便宜、快、看不见过程），`panelRounds` 定讨论轮数（1～3）。旧版 WillDeep 不受影响。
- 无数据迁移。旧剧的分镜没有 `holdFull`，按运镜 / 剧情文字判断；成片设置缺省画面节奏。

### Added
- 成片设置 `pacing` / `shotPacing` / `pacingTailSeconds` / `pacingSilentMaxSeconds`（`episode.save_compose_settings`，`ComposeStore` 规范化与夹取）；`episode.compose_plan` 每镜带 `pacing` 与 `paceTrim`（`EpisodePlan.pace_trim`，入点沿用存着的裁剪），告警 `pace_trimmed` / `pace_hold`，拦截项 `dialogue_stale`（配音节奏下）；合成按 `EpisodePlan.effective_trim` 截取。
- 分镜字段 `holdFull`（`drama.save_shot`、`drama.save_shot_drafts`、`drama.save_draft scope=shot`、历史快照与页面表单）。
- `VideoService#paced_duration`：配音节奏下的请求时长；`ReferencePackage.lip_sync_warning`。
- `review.run aspect=panel` 的宿主圆桌路径（`ReviewPanel::Runner#run_via_host`）：`willdeep/roundtable/run` 请求（标题、材料、专家席 `{id, name, expertise, persona}`、`chairPersona`、`verdictInstruction`、轮数、模型路由），结论经同一套解析与落库，另记 `via`、`roundtable {reportID, sessionID, rounds}`，发言记录带 `stance`。`review.get_material aspect=panel` 返回 `hostRoundtable`。
- `video.settings` 新参数 `panelRoundtable`（默认开）、`panelRounds`（1～3，默认 1）。
- `scripts/pacing_test.rb`（CI 新增步骤）；`scripts/episode_compose_test.rb` 加配音节奏端到端用例；`scripts/review_panel_test.rb` 加宿主圆桌用例。

### MCP 契约
- `episode.save_compose_settings` 的 `settings` 新增 `pacing`、`shotPacing`、`pacingTailSeconds`、`pacingSilentMaxSeconds`；`episode.compose_plan` 每镜新增 `pacing`、`paceTrim`；`drama.save_shot` / `drama.save_shot_drafts` 新增 `holdFull`；`drama.record_review` 新增可选 `via`、`roundtable`；`video.settings` 新增 `panelRoundtable`、`panelRounds`。只加字段，不改旧含义。

## [0.42.0-rc1] - 2026-10-07

### 用户影响
- **主框架和每一集剧本可以开「专家席」审读。** 六位专家——总策划（弧光与爽点）、节奏剪辑师（前三秒与每集钩子）、平台审核员（合规红线）、连续性编辑（设定账本与角色口径）、分镜导演（可拍性与口型策略）、制片（镜数与成本）——各自审一遍，主持人去重、裁决冲突、归纳成一份结论。结论进现有的审核面板：每条意见标「必改 / 建议」和提出人，各位专家的立场一览列在下面，只剩建议的算做完，有必改项可以「已知悉」，内容改了提示复审。策划页头部与分集剧本页各多一枚「专家席」徽标，点「开专家席」交给插件后台跑（七次模型调用，几分钟），页面不自动开。
- **助手也能开**：`review.run scope=drama|episode aspect=panel async=true`；`drama.get_progress` 在策划与每一集写好正文后排「专家席」待审，`review.run_batch` 默认连专家席一起审，`aspects: ["panel"]` 只补专家席。不想要时在 `video.settings` 关掉 `panelReview`。
- **专家席可改**：设置 → 创作技能 → 「专家席审稿」，每位专家一节（`### 名字 · 职责`，职责后写 `[guard]` 表示带上合规红线全文），增删改都行；少于两位专家或没有「主持人」节时拒绝开席、不花钱。
- 决策与取舍见 `docs/decisions/0010-expert-panel-review.md`；配音节奏驱动剪辑的方案另记在 `docs/decisions/0009-dialogue-paced-cut.md`，本版未实现。
- 无数据迁移。

### Added
- `server/lib/review_panel.rb`：专家席技能解析（`## 专家席` 下的 `### 名字 · 职责 [guard]`、`## 主持人`）、专家并行发言（各占一个 `qaConcurrency` 名额）、主持人归纳（结论读不出重试一次）、过半专家失败即停（`panel_failed`）、结论与发言记录落库。
- 技能 `skills/creative/panel.md`（阶段 `panel`）：共用规则、六位专家、主持人。
- 审核方面 `panel`（`drama` / `episode`）：`review.run`、`review.get_material`（另给 `panel {experts[{name, role, system}], chair}` 与一人分饰整席的 `system`）、`drama.record_review`（`issues[].raisedBy`、可选 `panel[]` 发言记录）、`drama.acknowledge_review`。
- `review.run_batch` 新参数 `aspects`；`video.settings` 新参数 `panelReview`（默认开）。
- `schemas/creative-v1.json` 审核结论：问题新增可选 `raisedBy`，类别新增 `hook`、`pacing`、`arc`、`character`、`dialogue`、`shootability`、`cost`。
- `schemas/review-labels.json` / 页面字典：专家席材料标签与提示词（`reviewTargetDramaPanel`、`reviewFieldFormatCount`、`reviewFieldCanonRev`、`panel*`）。
- 页面：`ReviewPanel` 的 `kind="panel"`（徽标「专家席：」、按钮「开专家席」、提出人、各位专家立场）；`waitForBackgroundJob`（`ui/src/backgroundJobsApi.ts`）。
- `scripts/review_panel_test.rb`（CI 新增步骤）；审核一致性夹具加入两个 panel 目标。

### MCP 契约
- `review.run` / `review.get_material` 的 `aspect` 枚举、`drama.record_review` / `drama.acknowledge_review` 的 `aspect` 枚举新增 `panel`；`drama.record_review` 新增可选 `panel`；`review.run_batch` 新增可选 `aspects`；`video.settings` 新增 `panelReview`。审核结论的 `issues[]` 可带 `raisedBy`，存档结论可带 `panel[]`。`drama.get_progress` 的 `review` 多一个 `panel` 键（设置开着时）。只加字段与枚举值，不改旧含义。

## [0.41.0-rc8] - 2026-10-07

### 用户影响
- **进入已有短剧后点「策划」，看到的是这部剧自己的策划。** 以前这里显示的是新建策划的空白对话页，在那里确认会另建一部新剧。现在可以直接修改剧名、类型、形式、受众、基调、一句话梗概、核心冲突和整季弧光，保存后上一版留在历史里；画幅、时长和画面风格仍在上方「剧集元数据」里改。要新建一部剧，点右上角「策划新短剧」。
- **表单里输入的文字改成常规字重、14 号字。** 以前所有输入框和多行框沿用了标签的 11 号粗体，剧本、分镜、角色设定这类长文读起来费劲。多行框的最小高度也恢复了，分镜里的开场动作、收尾动作、镜头语言不再只露两行。
- 页面标题旁的按钮不再被挤成两行。
- 无数据迁移。

### Fixed
- 开发页在只读真实数据模式下照现行宿主声明能看图、看视频。0.41.0-rc7 走查时导播卡上的「请升级到 1.377.0」只出现在开发页，装在 WillDeep 里的插件不受影响。

## [0.41.0-rc7] - 2026-10-07

### 用户影响
- **插件各页面的布局错乱逐页修过一遍**（在与 WillDeep 同内核的 WebKit 里用真实剧集走查）：
  - 角色固化：模型、张数、比例那一排控件不再挤成一团，窄窗口下整齐换行；「生成定妆候选」改成主按钮（深色底白字）。
  - 资产库：声音卡片不再互相叠压；造型 / 场景 / 道具 / 声音 / 角色五个页签排成一行，「角色」不再单独掉到第二行。
  - 生成队列：「自动刷新」勾选框正常显示，勾上有对勾。所有勾选框、单选框统一样式。
  - 导播中心 / 生成队列：任务标题写「第 9 集 · 第 2 镜 · 打个编号」，不再把整段提示词当标题；提示词最多显示三行，悬停看全文；质检结论的字号与卡片其余说明一致。
  - 设置：「刷新模型列表」有了按钮样式，卡片之间的大空隙收紧。
  - 「本集时长规划」只在分集剧本、分镜、首尾帧、视频生成页显示，并与页面内容左右对齐；页面顶部的空隙收小。
  - 左侧栏在流程菜单较长、窗口较矮时可以单独滚动，底色不再只铺一屏。
- 无数据迁移。

### Added
- 只读真实数据开发模式：`LIVE_PLUGIN_DATA=1 yarn dev`（launch 配置 `ui-dev-live`），开发页经本机插件网关读真实剧集，只放行只读工具。见 `ARCHITECTURE.md`。

## [0.41.0-rc6] - 2026-10-06

### 用户影响
- **WillDeep 主会话的确认卡显示中文名。** 以前助手调用短剧工坊的工具时，确认卡写的是「mcp__video-studio__episode_generate_frames」，参数是 dramaID、requestID、retryOnBlock 这类英文键名。现在每个工具都带中文名（「整集出首尾帧」「查看短剧进度」「配音」……），每个参数也有中文名（短剧、分集、请求编号（防重复计费）、全被拦截时重出轮数……）。需要 WillDeep 1.410.0-rc3 及以上才会显示；旧版宿主照旧显示原名，不受影响。
- 无数据迁移。

### Added
- `server/lib/tool_i18n.rb`：90 个工具标题、154 个参数名的简体中文对照，随 `tools/list` 放在每个工具的 `_meta["willdeep/i18n"]["zh-Hans"]`（`title`、`arguments`），不进 `inputSchema`。`scripts/agent_tools_test.rb` 守住每个工具、每个参数都有中文名。

## [0.41.0-rc5] - 2026-10-06

### 用户影响
- **在 WillDeep 主会话里查进度不再卡一分钟。** 《回村养鸭》做到第 9 集时，`drama.get_progress` 要 37～47 秒、返回 205 KB，主会话的助手问一次进度就卡在「正在调用」一分多钟，结果还大得塞不进上下文。现在约 1.4 秒。
  - 慢的原因：读一项设置要把 11 MB 的任务台账整份解析一遍，而查进度时每个镜头、每张候选图都要读设置。现在设置按文件指纹缓存，台账写入后自动失效。生成队列、合成、质检等读设置的地方也一起变快。
  - 大的原因：24 集、309 镜的逐镜明细一次全给。现在超过 6 集的剧默认每集只给一行摘要（镜头数、已选首帧、已完成视频、被拦的成片、连续性问题）；要看某一集，带 `episodeOrder`（或 `episodeID`），只返回这一集的逐镜明细和它的下一步，约 25 KB。需要全部明细时传 `detail: "full"`。6 集以内的剧不变。
- **技能里写错的工具名改正。** 0.41.0-rc3 的技能说明把主会话里的工具名写成 `mcp__video_studio__…`，实际是 `mcp__video-studio__…`（服务名保留连字符）。助手照着错名调用时，WillDeep 查不到这是只读工具，查进度也弹确认卡。现在写对了，并提醒照工具列表里的原名调用。
- 无数据迁移。

### Changed
- `VideoStore#settings`：按（修改时间、大小、inode）缓存，返回副本；`scripts/video_store_settings_test.rb`。
- `drama.get_progress` 新参数 `episodeOrder` / `episodeID` / `detail`；回包新增 `focusEpisodeID`、`episodesCompact`，摘要集带 `shotsOmitted`。回归用例见 `scripts/agent_tools_test.rb`。

## [0.41.0-rc4] - 2026-10-06

### 用户影响
- 出首尾帧时，参考图说明里的「发型」不再误收皮肤描述。0.41.0-rc1 起长相摘录没提到头发时会补一句发型，但许大强的「肤色黝黑发亮」因为含「黑发」两个字被当成了发型，写成「发型：肤色黝黑发亮，黑色短寸头」。现在只写「发型：黑色短寸头，鬓角剃得很短」。
- 无数据迁移；已出的图不受影响。

### Fixed
- `ReferenceLegend::HAIR_WORDS`：「黑发 / 白发…」后面紧跟亮、红、黄、紫、青、光、烫、胖、福时不算说头发。回归用例见 `scripts/image_generate_test.rb`。

## [0.41.0-rc3] - 2026-10-06

### 用户影响
- **可以在 WillDeep 主会话里直接做短剧。** 主会话里的助手调用短剧工坊的工具（出图、审核、配音、出视频、合成）时，不会再因为一次调用超过 10 秒就被宿主当成无响应、把插件进程结束掉：等待上限放到 180 秒。批量和长任务照旧在后台跑，用进度工具查。
- **生成的图、视频、配音会直接贴在对话里给你检查。** 技能里写明：出完首帧、配音、成片或整集后，助手把要你看的文件按一行一个贴出来，macOS 版对话里会显示成图片、视频播放器和音频播放器（终端和其他客户端显示为链接），并用一句话说明这次重点看什么（人物、服装、跳切、语速）。
- 主会话里能在技能列表里看到「短剧工坊 · 写作指南」，可以直接查分镜、首尾帧、审核等阶段的写法；插件页的技能设置不受影响，不会多出一项。
- 无数据迁移。

### Changed
- `mcp.json`：`startup_timeout_sec` 10 → 180。`jobs.wait` 经 stdio 的上限仍是 8 秒（`BackgroundTools::STDIO_WAIT_LIMIT`），说明文字改为「宿主对同一插件的 stdio 请求排队，长等会挡住插件页面」。
- `skills/video-studio/SKILL.md` 新增「Driving it from the WillDeep chat」：`mcp__video_studio__<tool>` 命名、`list_mcp_tools` 预热、单参数 `arguments_json` 的传法、长任务走后台，以及贴图 / 视频 / 音频的写法（尖括号绝对路径，路径常含空格）。

### Added
- `skills/creative/SKILL.md`：写作指南索引，供宿主技能目录发现（macOS 宿主只认带 `SKILL.md` 的目录，且 `read_skill` 只能读该目录内的文件）。`CreativeSkillStore` 跳过它（`INDEX_FILE`），回归测试见 `scripts/drama_test.rb`。

## [0.41.0-rc2] - 2026-10-06

### 用户影响
- **分镜页可以直接填「画面字幕」。** 分镜编辑页在「画外配乐」下面新增「画面字幕」输入框（例如「第 3 天」，最多 60 字），随「保存分镜」一起保存，合成时叠在这一镜开头的左上角。0.41.0-rc1 只有助手能通过工具写这个字段。版本历史与分镜共创也认这个字段。
- 其余页面外观与操作不变：这一版把分镜表单和生成队列的任务卡从主页面代码里拆了出来，生成队列、导播中心、回收站的任务卡行为照旧。

### Changed
- `ui/src/storyboard/ShotFields.tsx`（新）：分镜字段表单；`ui/src/jobs/QueueCard.tsx`（新）：任务卡与媒体栏、`activeJob`。`App.tsx` 2995 → 2937 行。
- 分镜草稿保存（`drama.saveDraft` scope=shot）、未保存检测、版本历史字段名、分镜共创工具参数与 `ShotDraftEntry` 带上 `caption`。

### Added
- `ui/src/storyboard/ShotFields.test.tsx`：字幕输入与上限、单句台词编辑、页面接入与 `App.tsx` 行数上限。

## [0.41.0-rc1] - 2026-10-04

### 用户影响
- **天数、地点这类字幕改在合成时叠加。** 分镜新增「画面字幕」（`caption`，最多 60 字，例如「第 3 天」），合成时叠在这一镜开头的左上角：白字、半透明黑色圆角底，淡入后停约 2.5 秒再淡出（镜头更短就停到镜头结束）。以前《回村养鸭》第 3 集把「画面角落字幕：第 3 天」写进了首帧描述，出图模型把字画进画面、视频跟着画，成片画面质检判「烧录叠字」拦下，两镜只能带着问题放行。现在首尾帧描述里还写着字幕 / 叠字的镜头，成片计划会提醒「把这行字移到画面字幕」。
- 字幕用 macOS 自带的系统能力渲染（苹方字体），不用另装东西；装了 ImageMagick 的也能用。万一渲染不出来，合成照常完成，只是这一镜没有字，并在成片页提示。
- **首尾帧更少「两张都不合格」。** 出首尾帧的提示词多写三样：
  - 人物发型。原来长相只摘开头约 36 个字，发型常在后面被截掉：三奶奶的「花白小圆髻插木簪」画成了黑发，许禾的半扎小揪画成了扎后脑。现在摘录里没提到头发时，补一句「发型：…」。
  - 场景长什么样。原来只写「场景「许家鸭棚·外」的空间与陈设」，第 3 集 5 镜的两张候选都把红砖鸭棚画成了铁皮顶木棚或竹棚，被质检以场景不符拦下。现在写成「场景「许家鸭棚·外」——当代南方丘陵小村的一座红砖鸭棚外景，长条形单层红砖棚，灰黑色石棉瓦双坡顶…」，结尾规矩改为「场景的建筑、材质与布局以场景参考图为准」。
  - 镜头景别。首尾帧描述多半只写人和事，景别只写在运镜里（「中景，固定，过许禾肩」），模型默认画成从头到脚的全景；第 5～8 集兜底选用的被拦首帧里，must 级问题一大半是景别不符。现在按运镜在描述前加一句「镜头取景：中景，人物膝盖以上入画，过肩构图，前景是许禾的肩膀与后脑。」——首帧取运镜里第一个景别，尾帧取最后一个（「中景开场，推到特写」尾帧是特写）。
- 已有候选图的提示词变了，质检结论会标「待复检」（0.40.0-rc1 的规则），不影响已选定的图。无数据迁移。

### Added
- `server/lib/caption_overlay.rb`：字幕 PNG 渲染（JXA + AppKit，退回 ImageMagick `label:`，开头的 `@` 转义）、overlay 滤镜（位置、淡入淡出、`enable` 时窗）。
- 镜头字段 `caption`：`drama.save_shot`、`drama.save_shot_drafts` 入参与草稿白名单（上限 60）。
- `episode.compose_plan` 每镜带 `caption`；告警 `caption_in_prompt`（首尾帧提示词里写了字幕 / 叠字 / 标题卡）。合成任务告警 `caption_not_rendered`。页面中英文文案（`ui/src/caption/captionI18n.ts`）。

### Fixed
- `scripts/agent_tools_test.rb`「shots walk frame prompt → start frame → selection → video」在 Ruby 3 下报 `ArgumentError`（0.40.0-rc1 起 `select_image` 带关键字参数，用例里不带花括号的字符串键哈希被当成关键字）。只改用例；生产代码的调用都带花括号或传变量，`/usr/bin/ruby` 2.6 不受影响。

### Changed
- `server/lib/frame_framing.rb`（新）：运镜里的景别（大特写 / 特写 / 中近景 / 近景 / 中景 / 中全景 / 全景 / 远景，长的先认）与机位（俯拍、仰拍、低机位、过某人肩、侧面）翻成一句正面的「镜头取景：…」，`image.generate` 首尾帧放在参考图说明与描述之间。
- `ReferenceLegend`：身份项补「发型：…」（只在摘录没提头发时；排除衣物词与否定分句，约 24 字）；场景项带场景描述摘录（约 45 字，去掉色调 / 光线标签句）；结尾规矩措辞。

## [0.40.0-rc1] - 2026-10-03

### 用户影响
- **候选图不用再一张张找错。** 首尾帧、定妆图和资产参考图出图后，每张候选都由设置页选定的审核模型对照出图时用的参考图、提示词和镜头要求看一遍：人物的性别、年龄与长相（动物的品种与标志物）、是否穿着本镜的造型、画面里的字是否与要求一字不差（简繁体、错字、多出的数字、包装上的外文）、有没有像真实品牌的标志或真实招牌、构图是否符合镜头要求、画幅。候选卡左下角显示「质检通过 / 有提醒 / 质检拦截 · 分数」，悬停看逐条问题；最好的一张右上角标「推荐」，被拦截的不会被推荐。
- **整集出首帧时自动选好。** 助手跑整集首尾帧时，每镜出完图就质检，推荐图通过（或只剩建议）就直接选上；一镜的候选全被拦截时，按问题写一句正面的修正说明（例如「画面中的「许禾」是二十七岁女性……长相与图1一致」「画面中可读的文字是简体「许家鸭棚」」）再出一轮，仍不行就标「需要你看」并写明原因。你在批量进行中手动选的图不会被换掉。
- **改了提示词或参考图，旧结论标「待复检」**，不再参与推荐。
- 设置页新增「候选图自动质检」：可关掉自动质检、自动选定，调整全被拦截时重出几轮（默认 1 轮，重出按张计费）。每张候选多一次审核模型调用，与成片质检共用同时质检的名额。
- 无数据迁移。已有的候选没有质检结论，可让助手用 `image.qa` 补检；行为与以前相同，直到质检跑过。
- **配音语速不对会提醒。** 《回村养鸭》里给 38 岁生意人用的预置音色「Arthur」是慢吞吞的讲故事腔，每秒只有 1.3～1.9 个字，听起来「像聊斋」（正常对白每秒约 3.5～5 个字）。现在每条试听与台词配音都记下语速；一个声音的音频平均低于每秒 2.5 字或高于 6.5 字时，配音结果、资产库里的声音卡（「语速偏慢 1.9字/秒」）和成片计划里用到它的镜头都会提醒，建议换音色或调整预设语速。以前配好的音频按台词与时长补算，不用重配就能看到。

### Fixed
- **重配台词后重新生成的视频没被选上**（《回村养鸭》第 1 集 `duck-e1-revideo-v1`，0.38.0-rc3 起的选片规则）：照对白音轨生成的成片现在记下当时用的每句配音（`dialogueAudio`）；台词重配后，旧片的口型对不上新配音，批量 / 补救产出的新片直接替换它（不再和它比质检、也不让给人工放行），逐项带 `replacedStaleDialogue` / `dialogueStale`。新片与旧片质检同级时照换（回归测试覆盖 warn → pass、warn → warn、旧 pass 重配后新 warn）。成片计划与进度对正在用的口型过期成片告警（`dialogue_stale`、步骤 `regenerate_dialogue_stale_video`）。批量写过「selected」、结束时镜头却回到旧片的项改报 `selection_reverted` 并写明现在是谁选的，不再误报「已选上」。
- 逐镜并发的画面质检偶发「Digest::Base cannot be directly inherited in Ruby」（Ruby 2.6 自动加载 `Digest::SHA256` 的线程竞争）：`server/lib/video_qa.rb` 启动时就加载 `digest/sha2`。

### Added
- `server/lib/image_qa.rb`：候选图质检（材料与指纹、逐张问审核模型、结论规范化与落库、画幅不符直接拦截、失败当「不知道」）、读出时的推荐与过期标记、`episode.generate_frames` 每镜的质检 / 重抽 / 自动选定、`episode.accept_recommended_frames`。
- `server/lib/image_remediation.rb`：按问题类别拼正面补救句（身份带年龄性别摘录与图号、造型、提示词里写明的字、运镜与动作、画幅），逐分句扫否定词，不合格退回固定句。
- 技能 `skills/creative/image-qa.md`（阶段 `imageQA`）：检查项、must / advice、pass / warn / block、分数与 `candidate_image_qa` 输出格式。
- MCP 工具 `image.qa {dramaID, target, characterID | episodeID + shotID | assetID, candidateIDs?, castIDs?, force?, async?}`：返回 `checked[]`、`failed[]`、`recommendedCandidateID`、`recommendation`；后台任务类型 `image.qa`。
- MCP 工具 `episode.accept_recommended_frames {dramaID, episodeID, target?, shotOrders?, onlyPassing?, replaceSelected?}`。
- `video.settings`：`imageAutoQA`（默认 true）、`imageAutoSelect`（默认 true）、`imageRetryOnBlock`（默认 1，0～2）。
- 页面：候选卡质检角标、悬停问题列表、「推荐」标签（分镜首尾帧、角色定妆、资产参考图），设置页「候选图自动质检」面板（`ui/src/imageqa/`），中英文文案。
- `server/lib/speech_rate.rb`：配音语速（中日韩按字、否则按拉丁词；短于 0.8 秒或少于 3 个字不算）、每个声音资产的平均语速（试听 + 台词音频，存了 `charsPerSecond` 就用、旧音频按台词与时长现算）、告警 `speech_rate_slow`（< 2.5 字/秒）/ `speech_rate_fast`（> 6.5 字/秒，拉丁词 1.2 / 4.0），文案建议换 `providerVoiceID` 或调预设 `speed`。页面声音资产卡与声音详情的「语速偏慢 1.9字/秒」角标（`ui/src/assets/SpeechRateBadge.tsx`），成片页的镜头告警文案。

### Changed（MCP 契约，向后兼容）
- 图片候选新增 `qa {status, score, summary, issues[{category, level, detail}], basis, model, rubric, checkedAt, castIDs?}`；读出时附只读的 `qa.stale`。
- 返回整部剧的 `drama.*`（`drama.get` / `drama.list` 与写入回包）、`drama.get_asset`：镜头新增只读 `recommendedStartID` / `recommendedEndID`、`startRecommendation` / `endRecommendation {candidateID, status, score, eligible}`，角色与资产新增 `recommendedCandidateID`。`drama.list_shots` 每镜带同样的推荐字段。
- `drama.select_image` 记 `selectedStartAt` / `selectedStartBy`（尾帧 `selectedEndAt` / `selectedEndBy`，来源 user / batch / recommendation）。
- `image.generate`：新参数 `autoQA`、`extraDirectives`（正面句，带否定词拒收 `negative_wording`）；结果带目标当前的 `recommendedCandidateID`，开着质检时同步调用带 `qa {state: queued, jobID}`、`async: true` 带 `qa {checked, recommendedCandidateID}`；`generated[]` 带 `qa`（有结论时）；dryRun 带 `extraDirectives`。
- `episode.generate_frames`：新参数 `autoQA` / `autoSelect` / `retryOnBlock`；每镜一条流水线（出图仍逐镜排队，质检并发占 `qaConcurrency`）；逐项带 `stage`（queued / generating / qa / retake / selecting / done / needs_human / failed）、`imageQA {candidates, recommendedCandidateID, retries, remediation, failed}`、`selectedCandidateID`、`selectionReason`（selected / already_selected / newer_selection / worse_qa / auto_select_off / needs_review / all_block / qa_failed）、`selectionKept`、`reasons`、`nextStep`；任务结果带 `imageQA` 汇总。质检没开时只出图（多了 `stage`）。
- `drama.get_progress`：镜头带 `startRecommendation`、`startQA {checked, block, unchecked}`；有可直接用的推荐时步骤为 `accept_recommended_frame`（`batch.tool: episode.accept_recommended_frames`），全被拦截 / 推荐有必改项时 `select_start_frame` 的理由写明。
- 经验库：候选图重抽记 `kind: image`、`action: image_retry`、`remediationID: image/<类别>`；`qa.lessons` 的 `aggregate` 按类别 + 补救句 + kind 分组并带 `kind`。
- `ReviewRunner#ask_model`（内部）：候选图质检与 `review.run` 共用审核模型路由。
- 语速：音频候选新增 `charsPerSecond` / `speechUnit`；`voice.generate` 的 `generated[]` 带 `charsPerSecond`，结果新增 `warnings[]`（`speech_rate_slow` / `speech_rate_fast`，带 assetID、voiceName、charsPerSecond、samples、message）；读出的声音资产带只读 `speechRate {charsPerSecond, unit, samples, status}`，`drama.list_assets` 的声音摘要另带 `warnings`；`episode.compose_plan` 的镜头 `warnings` 可含这两个码，并带 `speechRate[]` 明细（只提醒、不挡合成）。

### Docs
- 新增 `docs/decisions/0008-candidate-image-qa.md`；更新 `ARCHITECTURE.md`、`PRODUCT_OVERVIEW.md`、`skills/video-studio/SKILL.md`（新增「Candidate image QA」）、`docs/design/video-qa.md`（第 11 节）、复盘 `docs/lessons/2026-10-02-回村养鸭-生产复盘.md`（4.2 标为已实现）。

### Tests
- 新增 `scripts/image_qa_test.rb` 20 项：结论规范化与解析；推荐排序（block 不推荐、状态 → 分数 → 先后）与「只剩建议」；补救句逐类无否定词、带人物年龄性别与图号、带要求的招牌字；端到端（真实服务进程 + 假宿主扮演出图与审核模型，每次审核 1 秒）——性别错的候选被拦截、通过的那张被推荐并自动选定；全被拦截的镜带正面补救句重抽一轮仍拦截后 needs_human 并写明原因、记经验；批量质检进行中手动选的图不被覆盖；同时在跑的质检不超过 `qaConcurrency`；结论带指纹，改分镜提示词或换身份图后标 stale、推荐消失；`image.qa` 复检与复用；`autoSelect: false` 时进度建议接受推荐、`episode.accept_recommended_frames` 一次选上；同步出图把质检排成后台任务；否定的 `extraDirectives` 被拒；同一 requestID 重跑不再花钱。
- 新增 `scripts/selection_regression_test.rb` 5 项（真实服务进程，ref2va 图 + 音轨）：首轮成片记下所用配音；旧 warn → 新 pass 选新片；旧 warn → 新 warn（同级）选新片；旧 pass、重配台词后新 warn 仍选新片并带 `dialogueStale`；选回口型过期的旧片时成片计划告警 `dialogue_stale`、进度带 `dialogueStale`。
- `scripts/review_run_test.rb`：图片审核那一项先关掉 `imageAutoQA`——同步出图现在会排一次候选图质检（也是审核模型调用），该项只断言 review.run 自己的那次请求。
- `scripts/episode_compose_test.rb`：手工造的 ref2va 任务补上 `dialogueAudio`（照现在选定的配音生成），否则按新规则会被判为口型过期。
- `scripts/parallel_pipelines_test.rb` 去掉两处随机器负载时过时不过的墙钟断言：「先出片先质检」改为让假视频网关把第三条成片推迟约 3 秒完成（`FakeGateways#video_polls_by_submission`），先后顺序不再被负载抹平；「总耗时小于串行之和」删去上限，并行改由「质检在途峰值 = 3」与「B 在 A 第一条重拍前完成」两条与负载无关的断言证明。负载 10–20 下连跑 5 次全绿。
- 新增 `scripts/speech_rate_test.rb` 8 项：计数（字 / 词）、语速与不计的情形、告警阈值与建议文案、慢试听记 `charsPerSecond` 并告警、对白只报慢的那个声音、声音摘要带 `speechRate` 与告警、成片计划的镜头告警与明细、旧音频现算。
- 新增 `ui/src/assets/SpeechRateBadge.test.tsx` 3 项；`ui/src/EpisodeFinal.test.tsx` 的动态键加上两个语速告警。
- 新增 `ui/src/imageqa/CandidateQABadge.test.tsx` 6 项：角标状态与分数、悬停问题、推荐标签、待复检、未知类别、页面三处挂载与文案键齐全、设置面板只存改动的那一项。

## [0.39.0-rc1] - 2026-10-03

### 用户影响
- **成片可以只用一段。** 成片页每一镜多了「裁剪」：填入点、出点（秒，留空即从头 / 到尾），保存后徽标显示「已裁剪 0–2.0s」，合成时只用这一段——画面和声音一起裁；照台词音轨生成的镜头，口型音轨跟着裁，口型照样对得上。成片计划里的时长按裁剪后的算。换选这一镜的另一条成片，裁剪自动作废。
- **中途跳场景的成片，先裁再说，不急着重拍。** 大量成片是「前半段对、中途突然切到别的画面」，以前质检判跳场景后只能重拍（每次排队 15 分钟到几小时、按条计费）。现在自动补救先看质检给的时间点：切点之前那段至少 1.5 秒、台词在那之前说完，就裁到切点前约 0.15 秒并选定这条，不重拍、不花钱；切在一开头就裁掉片头。台词说不完、还有人物错位等别的问题时才照旧重拍，任务结果里写明为什么没裁。
- **裁掉的那段里的跳场景不再拦合成。** 质检结论本身不改，合成、进度和自动选片按「裁剪之后」的结果判断。
- **人工放行过的成片也能裁。** 对整集补救指定只处理跳场景时，放行过的成片只试裁剪，裁不了就保持原样，不会为它重拍。
- 设置页「成片自动补救与经验库」可以关掉自动裁剪；助手可用 `video.settings` 调最短长度（`trimMinSeconds`）与让出的余量（`trimMargin`）。
- 每次自动裁剪记进经验库（「裁到切点前」），`qa.lessons` 能看到它解决了多少次跳场景。
- 无数据迁移。已有的镜头没有裁剪，行为不变。

### Added
- `server/lib/clip_trim.rb`：质检时间点解析（`约2.0s处` / `about 2.3s` / `1.0s起` / `约2.3秒处` / `1.2–3.4s` / `00:02.5`，取最早）、自动裁剪的取舍（`plan_auto`）、生效结论（`effective_review`：裁掉区间里的 scene_jump 不算，按剩下的问题重算状态）、手动裁剪校验与 `drama.set_clip_trim`。
- MCP 工具 `drama.set_clip_trim {dramaID, episodeID, shotID, jobID?, inSeconds?, outSeconds?, clear?, reason?}`：只能裁这一镜现在用的那条成片（`trim_not_selected`），点要落在片长之内（`trim_out_of_range`），至少留 0.5 秒（`trim_too_short`），`invalid_trim`；返回 `trim`、`clipDurationMs`、`keptDurationMs`、`framesQA {verdictStatus, effectiveStatus, excluded}`。插件清单命令 `drama.setClipTrim`，中英文标题。
- `video.settings`：`autoTrim`（默认 true）、`trimMinSeconds`（默认 1.5，0.5～10）、`trimMargin`（默认 0.15，0～1）。
- 页面：成片页每镜的裁剪编辑器（`ui/src/trim/`：徽标、入点 / 出点、保存、取消裁剪，自动裁剪说明切点时间）；设置页自动裁剪开关；中英文文案。

### Changed（MCP 契约，向后兼容）
- 镜头新增 `trim {jobID, inSeconds?, outSeconds?, source: user | auto, reason?, issueTime?, excludes?, at}`；`drama.select_video` 选了别的成片时删掉它。
- `video.remediate` / `episode.remediate_videos` / `episode.generate_videos` 的自动补救：重拍前先试裁剪，裁了时逐项 `outcome: trimmed`、`attempts: 0`、`trim`；没裁时 `trimSkipped {jobID, reason}`（`dialogue_past_cut` / `dialogue_unknown` / `dialogue_in_head` / `too_short` / `no_cut_time` / `cut_outside_clip` / `clip_length_unknown` / `other_issues_remain`）。重拍复审后仍有 scene_jump 也先试裁那条重拍，`takes[]` 里带 `trim`。dryRun 的项带 `wouldTrim`。
- 人工放行（`qaOverride`）的成片：categories 含 `scene_jump` 且开着 `autoTrim` 时计划项为 `trimOnly`（只试裁剪，裁不了 `skipped: qa_override`）；以前一律跳过。
- `episode.compose_plan`：每镜 `trim`（带 `clipDurationMs` / `keptDurationMs`）、`effectiveDurationMs`；`estimatedDurationMs` 改为各镜 `effectiveDurationMs` 之和（没有裁剪时与以前相同）；`qaReview` 是生效结论（另带 `verdictStatus`、`trimExcluded`），`qa_blocked` 按它判；新告警 `trim_shorter_than_dialogue`、`trim_cuts_dialogue_head`（裁剪的镜头不再报 `audio_too_long`）。
- `episode.compose`：按裁剪截取（输入端 `-ss` / `-t`，口型音轨同窗）；台词比裁后画面长时照旧定格补足。
- `drama.get_progress` 的 `framesQA`：`status` / `remediable` 按生效结论，另带 `trim`。
- 经验库：裁剪记 `remediationID` / `lessonID: trim_before_cut`、`action: trim`；去重键改为「成片 + 类别 + action」。
- 自动选片比较（`VideoSelection.qa_status` / `statuses_for`）按生效结论。

### Docs
- 新增 `docs/decisions/0007-clip-trim.md`；更新 `ARCHITECTURE.md`、`PRODUCT_OVERVIEW.md`、`docs/design/episode-compose.md`（3.5、5）、`docs/design/video-qa.md`（5.5、10.6）、`docs/decisions/0004-qa-lessons.md`、`skills/video-studio/SKILL.md`。

### Tests
- 新增 `scripts/clip_trim_test.rb` 17 项：时间点解析（中英文、区间、秒 / s / sec、读不出的）；自动裁剪取舍（裁尾、裁头、裁头 + 后一个切点、剩太短、台词过切点、口型音轨在片头、台词算不出、没有时间、切点不在片里）；生效结论；手动校验；端到端（真实服务进程 + 假宿主 + 假视频网关 + 4 秒成片）——补救裁剪而不重拍、裁剪随选定落库、记经验；切在片头裁头；口型音轨比切点长时退回重拍并写明原因；人工放行的成片只裁不拍、放行保留、dryRun 给出 `wouldTrim`；`drama.set_clip_trim` 的校验、生效结论与进度；合成计划的裁剪时长与总时长、裁剪的 scene_jump 不再 `qa_blocked`；换选作废、清除；设置往返。
- `scripts/episode_compose_test.rb` 新增 2 项：裁剪后合成的时长；裁掉片头时口型音轨同窗截取（按频段音量断言）。
- 新增 `ui/src/trim/ClipTrimEditor.test.tsx` 5 项；`ui/src/EpisodeFinal.test.tsx` 新增 1 项（徽标、保存与取消裁剪调用 `drama.setClipTrim`），i18n 动态键加上裁剪告警与错误码。

## [0.38.0-rc4] - 2026-10-03

### 用户影响
- **没绑造型的角色，首帧里不再换衣服。** 《回村养鸭》第 2 集首帧：参考图说明对每个人只写了开头两句长相（截到约 36 个字），镜头没绑造型时提示词里根本没有穿着，出图模型就自己编——许禾穿成深色牛仔外套（她的标志是浅蓝牛仔衬衫、旧帆布围裙、墨绿高筒胶靴），许大强丢了酒红 POLO，陆青山丢了藏青立领夹克、眼镜和棕色兽医包。现在没绑造型时，说明里这一项写成「长相与日常装参考」，后面跟一行「日常装：…」，取自角色设定里的标志服饰与不可变项；按首帧生成视频（fl2va）时，提示词里的人物也带这一行。
- 绑了造型的镜头不变：只写造型，角色的日常穿着不写（孝衣不会被牛仔衬衫顶掉）。
- 无数据迁移；已出的首帧不受影响，重新出图即用新说明。

### Fixed
- `image.generate`（首尾帧）参考图说明：没绑造型（或造型类别是发型 / 妆容 / 年龄）的身份图项改为「「名字」的长相与日常装参考——<长相摘录>；日常装：<日常装>」，按文字描述的人物同样带日常装，末尾规则加「写了日常装的人物按所写日常装穿着」。长相摘录改从去掉穿着的身份文字里取，前两句不再被衣服占掉。
- H3 fl2va 提示词：没绑造型的主体写「<名字> shown in <Picture 1>, wearing <日常装>」（以前只有名字）。t2va / ref2va 的主体定义本来就带整段身份文字，不变。

### Changed（MCP 契约，向后兼容）
- 参考包编译的 `subjects[]` 新增 `wardrobe`（穿着不由造型决定时的日常装一行，否则为空串）；`drama.preview_reference_package` / `video.generate dryRun` 的 `subjects` 里可见。
- `server/lib/identity_text.rb` 新增 `IdentityText.wardrobe_line`：标志服饰 / 服装 / 穿着等开头的句子去掉标签整句收下，其他句子只收提到衣物配饰的分句，不可变项里的衣物配饰只收前面没提到的；色彩主调与带否定的分句不收。`ReferenceLegend::NEGATION` 改为与它共用。

### Tests
- `scripts/qa_remediation_test.rb` 新增 3 项：日常装一行的取法（标志服饰、不可变项去重、否定与色彩主调不收、英文）；H3 fl2va 没绑造型带日常装、绑了造型只有造型；ref2va 主体定义同一规则。
- `scripts/image_generate_test.rb` 新增 2 项：没绑造型的参考图说明带全套日常装且不带色彩主调与否定句；绑了造型的那项不带日常装、同镜没绑造型的群演带上自己的。原有林默的说明改为新格式，造型图的说明不变。

## [0.38.0-rc3] - 2026-10-03

### 用户影响
- **整集重新生成视频后，成片用的是新片。** 《回村养鸭》第 1 集：为了换掉造型错的旧片，强制重新生成了 11 镜；新片质检通过（或只有建议、没什么可重拍的）的第 5、8～13 镜仍选着上一轮的旧片，只有经过补救重拍的镜头换成了新片，合成出来还是旧造型。现在整集生成视频为一镜产出新片后，就绪即设为这一镜的选定（开着自动质检时在质检与补救之后，从这次产出的几条里取最好的；没开时下载好就选），替换原来的选定。
- **你在批量跑的过程中自己选的那条不会被覆盖。** 批量开始之后在页面上（或让助手）给某一镜另选了一条，批量结束时以你的选择为准。
- **你人工放行的那条，不会被质检更差的新片顶掉。** 同一集第 6、7 镜：你对旧片点了「人工放行」，再次强制生成后新片质检被拦截（需要你看），批量却把拦截的新片选上，放行被冲掉。现在新片只有在画面质检不比当前选定差时才换（通过 > 有提醒 > 拦截 > 没审）；你人工放行过的那条，只让给质检通过的新片。没换的镜头在任务里写明原因（`selectionReason`）。
- **新片质检没给出结论时不自动换。** 没审过的片可能有问题，这一镜保持原来的选定，并提示你看一眼。质检关着时，没审过的新片也不会替换已经审过的旧片。
- **「能不能合成」前后一致。** 成片计划说没有拦截项、合成却立刻被拒（第 6、7 镜画面质检拦截）：计划里的字段一直叫 `blocking`，按 `blockers` 读的助手拿到的是空的。现在计划同时给 `blockers` 和 `composable`，合成拒绝时带回同一份 `blockers`，两边用同一个判断。
- **旧片会被提醒。** 成片页在「这一镜选定的视频比后来批量生成的那条旧」时显示提醒，进度里也会列出这一步；选上新片，或者再选一次旧片表示就要它，提醒就消失。
- **补救任务说清楚选的是哪条。** 整集 / 单镜自动补救和整集生成视频的每一项（包括跳过的）都写明这一镜现在选定的是哪条、这次有没有换。
- 无数据迁移。旧数据里的选定没有记选定时间，重新生成过的镜头若还选着旧片，会出现上面的提醒。

### Fixed
- `episode.generate_videos`（含 `force: true`）：新成片不再只有经过重拍的镜头才被选定。补救循环在原片直接通过（`passed`）或没有可重做问题（`nothing_to_remediate`）时只在结果里写 `selectedJobID`、没有调用选片，镜头仍选着旧片，`episode.compose` 于是用旧片。现在所有选片走 `server/lib/video_selection.rb` 的 `VideoSelection.adopt`；质检关着时下载好就选。新片画面质检失败（`qa_failed`）时仍按 0004 不选没审过的片。
- 自动选片不再换掉人工放行（`qaOverride`，by 不是 auto）的选定，除非新片画面质检 pass；新片结论比当前选定差（pass > warn > block > 没有结论）时也不换。以前补救在都没过时仍把 block 的最好一条选上。
- `episode.compose_plan` 与 `episode.compose` 共用 `EpisodeComposer#gate`；计划新增 `blockers`（与 `blocking` 同一份）与 `composable`，`compose_blocked` 带回同一个 `blockers`。

### Changed（MCP 契约，向后兼容）
- 镜头新增 `selectedVideoAt`（UTC 毫秒）与 `selectedVideoBy`（user / batch / remediation），每次选定写入；`drama.select_video` 记 `user`，返回新增 `changed`。
- `episode.compose_plan` 新增 `blockers`（与 `blocking` 同一份）、`composable`；`episode.compose` 的 `compose_blocked` 新增 `blockers`（`blocking` 保留）。
- 自动选片在同一次文件锁里过闸门，命中时不写：`newer_selection`（批量开始，即后台任务 `createdAt`，之后有人另选了本次产出以外的那条）、`user_qa_override`、`worse_qa`。逐项与补救结果新增 `selectionReason`（selected / already_selected / newer_selection / user_qa_override / worse_qa / qa_failed / not_attempted / changed_by_user），不换时另带 `selectionKept`、`bestJobID`、`selectionCompared {currentJobID, currentStatus, candidateStatus}`；补救都没过且没换时 `nextStep` 写明保留的成片。
- `episode.generate_videos` / `episode.remediate_videos` / `video.remediate` 的每一项（含 skipped / canceled / failed）新增 `previousSelectedJobID`、`selectedJobID`（批量结束时的实际选定，没有为 `null`）、`selectionChanged`，变了时带 `selectedBy`；补救结果的 `selectedJobID` 改为读回的实际选定。
- `video.remediate` / `episode.remediate_videos`：原片通过或没有可重做问题时也把它设为选定（已经选着就不写）。
- `episode.compose_plan`：选定的成片比批量生成的新片旧、且选定发生在新片出现之前时，该镜 `warnings` 加 `stale_selection`，另给 `staleSelection {selectedJobID, newerJobID, selectedCreatedAt, newerCreatedAt, newerFramesStatus?, selectedAt?}`；不挡合成。
- `drama.get_progress`：镜头上 `staleSelection`，`summary.staleSelections`，`nextSteps` 新增 `review_stale_selection`。
- 页面成片页新增提示 `finalWarning_stale_selection`（中英文）。

### Docs
- 新增 `docs/decisions/0006-batch-take-selection.md`；更新 `ARCHITECTURE.md`、`docs/design/video-qa.md` 10.3、`skills/video-studio/SKILL.md`、`PRODUCT_OVERVIEW.md`。

### Tests
- 新增 `scripts/batch_selection_test.rb` 20 项：规则（批量开始后的人工选择、旧选定判定：旧数据算、补救重拍不算、有意再选不算）；端到端（真实服务进程 + 假宿主 + 假视频网关）——`force` 重生成已有选定的镜头：质检 pass、warn 无可重做问题、质检关着三种情况新片都成为选定且逐项 `selectionChanged: true`；质检时 / 生成中人工 `drama.select_video` 另一条不被覆盖（`selectionKept`）；新片质检无结论时不选、`episode.compose_plan` 与 `drama.get_progress` 给出旧选定告警、再选一次旧片后消失；`episode.remediate_videos` 每一项（含跳过的）都带 `selectedJobID`、`selectionChanged` 与 `selectionReason`；人工放行的选定不被 block 的新片换掉、被 pass 的新片换掉；新片 block 而旧片 pass 时不换（`selectionCompared`）；质检关着时没审过的新片不替换审过的旧片；`episode.compose_plan` 的 `blockers` 与 `episode.compose` 的 `compose_blocked` 逐项相等（`qa_blocked`），拦截解除后 `composable: true`。
- `ui/src/EpisodeFinal.test.tsx`：`finalWarning_stale_selection` 两种语言都有。

## [0.38.0-rc2] - 2026-10-03

### 用户影响
- **整集生成与自动补救不再一镜一镜地排队。** 《回村养鸭》第 1 集 11 镜：视频是一起提交的，但质检与补救严格按镜头顺序做，一镜要重拍时整批等这条新视频（10～15 分钟）再复审，然后才去审下一镜，70 分钟只审完 7 镜。现在每镜各走各的——生成、质检、重拍、复审、选片按镜并发；哪一镜的成片先出来就先审，一镜在重拍时别的镜照常往下走。一集的总耗时接近「最慢的那一镜」，而不是所有镜头加起来。`episode.generate_videos`、`episode.remediate_videos`、`video.remediate` 都是这样。
- **同时跑多少有上限，不会把后端或模型额度一下打满。** 整个插件同时最多 2 条画面质检、6 条在生成的视频（批量提交的，含补救重拍；视频后端单账号约 7～8 条并发）。可以让助手用 `video.settings` 调：`qaConcurrency`（1～4）、`videoConcurrency`（1～20）。花费不变：每次重拍仍按一条新视频计费。
- **后台任务面板逐镜显示进度。** 生成队列页的「后台任务」里，整集生成视频和自动补救任务下面多一行逐镜进度：排队中、生成中、质检中、重拍第 N 次、复审第 N 条、选片中、完成、需要你看、失败，上面一行按步骤汇总。
- **补救做到一半插件重启，再发起一次能接着做。** 以前重跑会从「最新那条重拍」另起一轮补救、次数从头算，可能多花钱；现在接着上次的重拍链做，已经提交过的重拍直接取回、已经审过的不再审。整集生成视频重跑时，上次没做完质检 / 补救的镜头只补做质检与补救，不重新生成。
- 无数据迁移。

### Changed（MCP 契约，向后兼容）
- `episode.generate_videos` / `episode.remediate_videos` / `video.remediate`：每镜一条流水线并发跑（`server/lib/shot_pipelines.rb`）。JobRunner 的工作线程只协调并按 `VIDEO_STUDIO_JOB_POLL_SECONDS` 统一轮询本批所有在生成的视频，流水线线程等轮询结果，不各自睡觉轮询。`episode.generate_videos` 在 `autoQA` + `autoRemediate` 时对每镜成片下载好后立即质检与补救。
- 逐项新增 `stage`（queued / generating / qa / retake / selecting / done / needs_human / failed）、`attempt`（qa 时 > 0 即复审第几条重拍）、`finishedAt`；补救结果的 `takes[]` 里重拍带 `readyAt`；补救因别的镜碰到后端故障而停下时带 `stoppedBy`。`jobs.status` / `jobs.wait` 的任务新增 `stages[]`（key、order、label、state、stage、attempt），`includeItems: false` 时也有。
- `video.settings` 新增 `qaConcurrency`（默认 2，1～4）与 `videoConcurrency`（默认 6，1～20）：整个插件进程里批量同时在跑的画面质检与同时在生成的视频。
- 停止与取消：某镜碰到后端故障类错误时，其余镜不再拿新名额（没开始的记 `skipped`，reason `stopped_<错误码>`），已经在生成的照常等完；取消时排队拿名额的放弃，等视频的立即返回（远端照常跑）。
- 重跑：补救从补救链的链头（重拍的 `remediation.sourceJobID`）接着做（这一镜还没选定成片时；`video.remediate` 点名 `jobID` 时仍从那条做起）；`episode.remediate_videos` 的计划项带 `resume` / `resumeFrom`。`episode.generate_videos` 对「已有完成的批量成片、还没选定、上次的自动质检 / 补救没做完」的镜头只做质检与补救（计划项 `remediateOnly`）。
- 经验库：同一条重拍的同一类问题只记一次（`qa-lessons.json` 按 `jobID` + `category` 去重）。

### Fixed
- 几条画面质检同时第一次复制审核技能时报 `Errno::ENOENT`（`creative-skills/review.md.<pid>.tmp` 被另一个线程 rename 走）：创作技能的临时文件名加上线程号。

### Tests
- 新增 `scripts/parallel_pipelines_test.rb` 15 项：名额（不超过容量、先来先得、取消时放弃排队）；设置往返与截断；端到端（真实服务进程 + 每次质检 1.5 秒、在别的线程里回的假宿主 + 假视频网关）——A 镜重拍 2 次、B 镜一次过、C 镜重拍 1 次：B 在 A 的第一条重拍出来之前就做完、总耗时小于 6 次质检串行之和、三条质检真正同时在跑、页面看到的 `stages[]`；整集生成 3 镜：`videoConcurrency` 2 时同时在生成的视频不超过 2 条、第一条质检在最后一条视频完成之前就开始、E 镜补救一次；补救做到一半结束进程：任务标 `interrupted`，重跑后两镜都补救完、两轮合计只提交 3 条重拍（requestID 不重复）、`qaConcurrency` 1 时质检不重叠、经验每条重拍每类只记一次。
- `scripts/support/fake_host.rb`：`on_slow`（在另一个线程里延迟回话，统计同时在途的请求数）、`kill`；`scripts/support/fake_gateways.rb`：视频任务同时在途数峰值、完成时刻、可配置几次查询后完成。
- 新增 `ui/src/jobs/ShotStages.test.tsx` 5 项；`ui/src/BackgroundJobsPanel.test.tsx` 新增 1 项（逐镜进度显示，已中断的任务不显示）。

## [0.38.0-rc1] - 2026-10-02

### 用户影响
- **一镜之内少了「硬切跳场景」。** 《回村养鸭》第 1 集第 7 镜，前半段许禾蹲在鸭子旁，中途突然切成鸭子特写、许禾远远站着。现在每条单镜视频提示词默认带一句正面的「一镜到底、同一机位、同一地点」；分镜运镜里写了「切到 / 转场 / 跳切 / 多机位」的镜头不加。
- **质检发现问题会自动补救。** 画面质检判出跳场景、人物错位、画面叠字时，插件按一张补救表给每类问题加一句正面描述重拍，再审一遍，在原片和重拍里选最好的一条设为本镜成片。默认每镜最多重拍 2 次；**每次重拍都按一条新视频计费**。开着「成片自动质检」与「自动补救」（默认都开）时，整集生成视频跑完自动做这一轮；也可以让助手对某一镜或某一集单独做（`video.remediate` / `episode.remediate_videos`）。都没通过时不会自动放行，留给你决定（设置里可以改成自动放行最好的一条，不建议）。
- **复核经验会攒下来。** 每次补救是否奏效都记进插件的经验库；反复奏效的补救句会自动加进以后每一镜的提示词。设置页新增「成片自动补救与经验库」：开关自动补救、每镜重拍次数、哪几类问题自动重拍、是否自动放行；查看每条经验的尝试 / 解决次数并单独开关；也能手动加一条经验（写进提示词的只能用正面描述，例如出图规则可以记成备注）。
- **绑了造型的角色不再穿回日常衣服。** 第 1 集第 3 镜许禾绑了「戴孝」造型，成片多半还是牛仔衬衫加围裙：提示词把造型和角色设定里的日常穿着一起写了。现在绑了造型时，角色描述只留长相（脸、发型、身材、眼镜），穿着全听造型；出首尾帧、造型图时同一口径。没绑造型的镜头不变。
- **WillDeep 里的成片画面质检能用了。** 0.33～0.37 在 WillDeep 内每次画面质检都报「Attached media is outside this plugin's generated-media folder」：抽帧拼图放在了子目录里，宿主不收。现在放在生成目录第一层。旧的 `generated-images/qa/` 文件夹不再使用，可以删。
- 无数据迁移。新文件 `qa-lessons.json` 与 `dramas.json` 同目录，备份数据目录时一并带上。

### Added
- 补救表 `schemas/qa-remediations-v1.json`（Ruby 入口 `server/lib/qa_remediation.rb`）：质检类别 → 正面补救句、是否自动重做、最多几次；内置预防句（一镜到底）与升级阈值（尝试 ≥ 3 次且解决率 ≥ 60%）。默认自动重做 `scene_jump` / `identity` / `text_overlay` / `injury` / `blood`，`continuity` / `framing` 只提示。
- MCP 工具 `video.remediate` {jobID | dramaID+episodeID+shotID, maxRetries?, categories?, dryRun?, requestID?} 与 `episode.remediate_videos` {dramaID, episodeID, shotOrders?, categories?, maxRetries?, dryRun?, requestID?}：后台任务（`jobs.status` / `jobs.wait`），循环在 `server/lib/video_remediation.rb`。缺结论或结论过期先审；每类问题选一句补救句经 `video.generate` 镜头路径重拍（requestID `remediate:<源任务>:<第几次>`，任务记 `remediation` 来源）；轮询、下载、播放镜像、复审、记经验；选最好的一条（`drama.select_video`），都没过时留 `nextStep`，`remediateAutoOverride` 开着才放行（`qaOverride.by = auto`）。画面质检失败当「不知道」：重试一次，仍失败就停下（`outcome: qa_failed`）、不比较那条重拍、把镜头钉在已审过的最好那条上。需要宿主模型（`host_review_unsupported`）与视频后端（`video_unconfigured`）。
- 经验库 `qa-lessons.json`（`server/lib/qa_lesson_store.rb`、`server/lib/qa_lessons.rb`，`VIDEO_STUDIO_QA_LESSON_STORE` 可覆盖）：每次尝试记类别、补救句、视频后端与模型、镜头特征（mode / combo / 有无对白音轨 / 出场人数）、resolved | unresolved、时间。MCP 工具 `qa.lessons`（`readOnlyHint`）：全部经验与统计、按类别 + 补救句汇总（另按 mode 分开）、当前生效的预防句；`qa.save_lesson`：手动经验（remediation / prevention / note，video / image），提示词类拒收否定措辞（`negative_wording`），内置经验只能开关（`builtin_lesson_readonly`）。
- `video.settings` 新增 `autoRemediate`（默认 true）、`remediateMaxRetries`（默认 2，0–5）、`remediateCategories`（默认 scene_jump / identity / text_overlay）、`remediateAutoOverride`（默认 false）。
- `server/lib/identity_text.rb`：角色身份描述的长相 / 穿着拆分。
- 页面：设置页「成片自动补救与经验库」面板（`ui/src/qa/`，中英文）；生成队列页「后台任务」认得两种补救任务。插件清单登记命令 `qa.lessons` / `qa.saveLesson`。

### Changed（MCP 契约，向后兼容）
- 成片提示词：参考包编译（`video.generate` 镜头路径、`drama.preview_reference_package` purpose=video、`episode.generate_videos`）在运镜行后加预防句；`cameraIntent` 写了切镜 / 转场 / 多机位时跳过一镜到底类。`video.generate` 新增 `extraDirectives`（镜头路径的额外正面句子），dryRun 与参考包预览返回 `directives` / `directivesSkipped`；视频任务记 `directives`。
- 绑了造型（有描述、类别不是 hair / makeup / age）的出场角色：成片提示词与出图（参考图说明、造型图提示词）的身份描述去掉标志服饰、色彩主调与衣物配饰类不可变项；六段式 retention 写「identity (face, hair, build) is retained; wardrobe follows <Appearance: 造型名>」。没绑造型时逐字不变。
- 画面质检拼图改写在生成目录第一层（`qa-sheet-<任务键>-<指纹>-NN.jpg`），换了成片删同一任务的旧拼图。画面质检说明补一句：片段中途硬切到分镜没写的机位、景别或主体也记 `scene_jump`（清单版本 `frames-v1` 不变，已有结论不失效）。
- `episode.generate_videos`：`autoQA` 与 `autoRemediate` 都开、宿主能问模型时，跑完对每条新成片自动质检 + 补救，逐项结果带 `remediation`，任务结果带 `autoRemediation` 计数。
- `drama.get_progress`：每镜新增 `framesQA {jobID, status, categories, remediable, qaOverride}`；有可补救问题时排 `remediate_video`（批量提示 `episode.remediate_videos`，进 `batchSteps`）。
- `drama.select_video` 新增可选 `qaOverrideBy`（user / agent / auto，默认 user）。
- `jobs.status` 的 `kind` 枚举加 `video.remediate` / `episode.remediate_videos`。

### Fixed
- WillDeep 内画面质检全部失败（`review_model_failed`：Attached media is outside this plugin's generated-media folder）：拼图在 `generated-images/qa/` 子目录，宿主只收生成目录的直接子文件。假宿主 `scripts/support/fake_host.rb` 现在按宿主同一规则拒收子目录 / 符号链接里的附件与参照图。
- 绑了造型的角色在成片里穿回身份设定的日常衣服（造型与身份穿着同时写进 subject_definitions，retention 还要求保留「wardrobe」）；造型图出图同样把身份里的日常穿着拼在前面。

### Tests
- 新增 `scripts/qa_remediation_test.rb` 26 项：补救表无否定词、默认类别；切镜意图中英文判定；H3 两种格式里预防句位置；身份拆分（第 1 集第 3 镜原文、不可变项过滤表、无造型与发型造型不变）；经验库统计、升级阈值、否定措辞拒收、手动补救句轮换、内置经验开关；端到端（真实服务进程 + 假宿主 + 假视频网关，ffmpeg 现做成片）：提示词默认带一镜到底、切镜镜头不带、造型镜头只写孝衣、出图同口径、无造型不变、retention 措辞、设置往返（截断与过滤）、`video.remediate` dryRun 不花钱、scene_jump → 重拍一次通过并选定 + 记 resolved 经验、maxRetries 用完不放行并留下一步、质检失败重试一次后停下不选片、进度建议 `episode.remediate_videos`、`episode.generate_videos` 自动补救 identity、批量计划的跳过原因。
- `scripts/review_run_test.rb` 新增 2 项：画面质检附件是媒体根的直接子文件（假宿主拒收子目录路径）；换了成片后旧拼图被删。
- 新增 `ui/src/qa/QaLessonsPanel.test.tsx` 7 项（列表与统计、开关经验、只存改了的设置、否定措辞提示、读取失败、两种语言的键、App.tsx 不超过 3000 行）。

## [0.37.0-rc1] - 2026-10-02

### 用户影响
- **每部剧有了一本「全剧设定账本」。** 《回村养鸭》并行写的 24 集里，鸭子数 486 / 479 / 474 混用、定金 50 / 90 两说，策划写「白鸭成群」、剧本写「全是麻鸭」，审核要改掉的「赌约」「粪坑」「钳子」散在策划、角色、剧本、分镜四处，全靠人工对账。现在剧集元数据下方多了「全剧设定账本」：记关键数字随剧情的变化（例如「存栏：开篇 486 只；第 44 天起 479 只，病死七只」）、道具与用途、视觉口径、禁用词与替换词，以及没有角色档案但可以说台词的功能性人物。保存后上一版留在历史里。
- **写作时 Agent 拿到这本账。** 写策划、角色、分集、剧本、分镜、首尾帧时，Agent 收到的材料里都带着账本，阶段技能要求它照账本写数字、换掉禁用词；账本里没有的数字或道具要先提出来、等你同意再加，不再自己编。
- **「检查一致性」一键对账。** 面板里点「检查一致性」，按账本扫一遍策划、角色、资产、全部剧本和分镜（不调用模型、几乎瞬间完成），逐条列出：禁用词出现在第几集哪一行、原文是什么、该换成什么；台词里说话但不在角色表里的人（例如「王婶」）；和账本对不上的数字，例如第 13 集「第 48 天」还写着「四百八十六。齐了。」，而账本第 44 天起已是 479——这类标「可能」，请对着原文确认；以及违反视觉口径的说法（「你这一棚全是麻鸭」）。Agent 看进度时也会看到「修一致性」这一步。
- 旧剧的账本是空的，不影响任何现有内容；账本空着时检查只报未建档的说话人。无数据迁移。

### Added
- MCP 工具 `drama.save_canon`（`dramaID`、`canon`、`expectedRev`）：部分更新全剧设定账本 `canon`，给了哪块就整块替换哪块（`facts` / `props` / `visualRules` / `bannedTerms` / `allowedExtras`）；`expectedRev` 对的是 `canon.rev`，对不上返回 `revision_conflict`；条目不全返回 `invalid_canon`（消息指出是哪一项）。被覆盖的那一版进历史，`drama.list_history` / `drama.read_version` / `drama.restore_version` 支持 `scope=canon`。规范化在 `server/lib/drama_canon.rb`。
- MCP 工具 `drama.check_consistency`（`readOnlyHint`，不调模型，`server/lib/consistency_check.rb`）：扫已采用的策划、角色、资产、分集标题 / 梗概 / 剧本、分镜各文字字段与台词，按类返回 `issues.bannedTerms` / `unknownSpeakers` / `facts` / `visualRules` 与 `summary` 计数，每条带定位（`scope`、`episodeOrder`、`shotOrder`、`field`、`line` 或 `lineIndex`、对象 ID）与原文片段。数字检查是启发式：汉字 / 阿拉伯数字 0–99999（`server/lib/chinese_numeral.rb`，认得「一百二」「两万五」「1,200」「5万」），数后紧跟 fact 的 `unit` 或前后窗口里有 `label` / `keywords` 才算，排除「第 N」与跟着别的量词的数，只看账本数值一半到两倍之间的量级；剧情天数取剧本里的「第 N 天」，没写的沿用上一集；结果 `confidence: possible`。
- 插件清单登记命令 `drama.saveCanon` / `drama.checkConsistency`（中英文标题）。
- 页面：剧集元数据下方的「全剧设定账本」面板（`ui/src/canon/CanonPanel.tsx`、`ConsistencyReport.tsx`、`canonForm.ts`、`canonApi.ts`，文案 `canonI18n.ts` 中英文齐全），可编辑五块列表、保存、撤销修改、跑一致性检查并按类显示报告。

### Changed（MCP 契约，向后兼容）
- `drama.get` 等返回的剧对象新增只读形状固定的 `canon`（旧剧补空账本，`rev: 0`）。
- `drama.get_stage_context`：planning、characters、episodes、script、storyboard、shot、frames 的 `context` 在账本非空时多一段「全剧设定账本」摘要（frames 只带道具、视觉口径、禁用词）；结果新增 `canonRev`。各阶段 `write.note` 补写账本要求。
- `drama.get_progress`：新增 `consistency`（`bannedTerms`、`unknownSpeakers`、`facts`、`visualRules`、`total`、`canonRev`、`canonEmpty`）；有问题时 `nextSteps` 在审核类步骤之后、生产步骤之前排一步 `fix_consistency`。
- 技能：`skills/video-studio/SKILL.md` 新增「Series canon」一节；`skills/creative/` 的策划、角色、分集串联、单集剧本、分镜、首尾帧技能补写账本要求（照账本写数字、换禁用词、缺数字先提出、剧情时间写「第 N 天」）。

### Tests
- 新增 `scripts/canon_consistency_test.rb` 17 项：汉字数字解析表（41 个写法，含口语尾数、全角、千分位、越界与不规整写法）与位置扫描；账本保存 / 读回 / 部分更新 / 冲突 / 非法条目 / 历史与回滚；阶段材料带账本、首尾帧只带三块、空账本不出现；禁用词定位与片段、替换词里的禁用词不报；未建档说话人（剧本行与分镜台词）；经典冲突「账本第 44 天起 479，第 13 集第 48 天写『四百八十六。齐了。』」在剧本和分镜台词里都被标出，同集写对的 479、第 48 天的 48、「三十只」、第 1 集的 486 不报；视觉口径禁用说法；进度的计数与 `fix_consistency` 排序；`tools/list` 注解与 MCP 端到端调用。
- 新增 `ui/src/canon/CanonPanel.test.tsx` 8 项。

## [0.36.0-rc1] - 2026-10-02

### 用户影响
- **出图、审核期间页面和 Agent 的读操作不再卡住。** 《回村养鸭》里，出图期间 `drama.get` 等读操作要排几分钟。现在 Agent 可以把出图、审核、配音交给后台：调用立即返回一个任务号，活在插件进程里另起的工作线程上跑，期间页面、`drama.get`、`drama.list` 照常秒回。不带新参数的老用法行为不变。
- **一集一次调用。** Agent 不必再一镜一镜地循环：`episode.generate_frames` 出全集首帧（已有候选的镜自动跳过）、`episode.dub` 配全集台词（已配好的跳过，台词改过的重配并换上新音频）、`episode.generate_videos` 提交全集视频并在后台轮询、下载（已有成片的镜不重复提交）、`review.run_batch` 按范围批量审核（结论仍有效的对象跳过，不重复花钱）。复盘 4.1 的目标是 4 个调用把一集从「有分镜」推到「有首帧、配音、视频」；首帧仍要选定（Agent 看图后选，或人来选），所以实际是 3 个批量调用加一轮选图。
- **生成队列页多了「后台任务」面板。** 显示聊天里的助手提交的后台任务：做的是什么（整集出首尾帧、整集配音、批量审核……）、哪部剧哪一集、进度条与「完成 / 失败 / 跳过」计数；进行中的可以取消（正在出的那一张图、正在提交的那条视频会做完并保留）。
- **插件重启后不会悄悄重花钱。** 插件进程重启时还没做完的任务标为「已中断」，已经做好的都保留；不自动续跑（被打断的那一次可能已经计费），让助手再发起一次即可，做过的项自动跳过。
- `drama.get_progress` 的待办里，能用批量工具一次做完的会直接给出批量调用；Agent 看到的是「第 1 集有 12 镜没有首帧 → 一次 `episode.generate_frames`」，而不是 12 条单镜待办。
- 无数据迁移。新文件 `background-jobs.json` 与 `dramas.json` 同目录，备份数据目录时一并带上。

### Added
- 后台任务执行器 `server/lib/job_runner.rb`：进程内工作线程（默认 3 个，`VIDEO_STUDIO_JOB_WORKERS`，上限 8），排队上限 100（超出 `jobs_queue_full`），协作式取消；台账 `server/lib/background_job_store.rb`（`background-jobs.json`，`VIDEO_STUDIO_BACKGROUND_JOB_STORE` 可覆盖，文件锁 + 临时文件 + rename，最多 500 条、只淘汰已结束的）。启动时把主人进程已不在的 queued / running 标 `interrupted`（`error.code = server_restarted`），主人还活着的（宿主装新版时旧进程未退）不碰。
- MCP 工具（入口 `server/lib/background_tools.rb`，按集批量 `server/lib/episode_batch.rb`）：
  - `episode.generate_frames` {dramaID, episodeID, target start|end, shotOrders?, countPerModel, models?, castIDs?, onlyMissing (默认 true), force, requestID}
  - `episode.dub` {dramaID, episodeID, shotOrders?, presetID?, force, requestID}
  - `episode.generate_videos` {dramaID, episodeID, shotOrders?, mode?, onlyMissing (默认 true), force, requestID}；轮询间隔 `VIDEO_STUDIO_JOB_POLL_SECONDS`（默认 15），总时长上限 3 小时（`VIDEO_STUDIO_JOB_POLL_LIMIT_SECONDS`），超时项记 `poll_timeout`、远端照常跑。
  - `review.run_batch` {dramaID, scope（数组或 `drama|characters|assets|episodes|shots` 字符串，默认全部）, episodeID?, skipFresh (默认 true), force, requestID}
  - `jobs.status`（只读）{jobIDs | jobID, dramaID, episodeID, kind, state（含 active / finished）, limit ≤ 100, includeItems}
  - `jobs.wait`（只读）{jobIDs ≤ 20, timeoutSeconds ≤ 60, includeItems}：经 stdio 最多等 8 秒（宿主 10 秒等不到应答就结束插件进程），超出返回 `capped: true`。
  - `jobs.cancel` {jobID}
  批量总是后台执行，返回 `{ok, async, jobID, job, planned {items, pending, skipped}, unknownOrders?}`；逐项（`items[]`：pending / running / waiting / completed / failed / skipped / canceled，带 reason 与 error）调用与单个工具相同的服务方法，带派生 requestID。同 requestID 返回同一任务（失败 / 中断 / 取消的除外）；无 requestID 时同一部剧同一集同一种批量在跑就返回它（`alreadyRunning`）。额度用完、后端不可用时剩下的项标 `stopped_<code>`。
- 插件清单登记命令 `jobs.status` / `jobs.cancel`（中英文标题）。页面组件 `ui/src/BackgroundJobsPanel.tsx`（`ui/src/backgroundJobsApi.ts`、`ui/src/background-jobs.css`），文案进 `ui/src/i18n.ts`。
- 决策记录 `docs/decisions/0003-background-jobs.md`。

### Changed（MCP 契约，向后兼容）
- `image.generate` / `review.run` / `voice.generate` 新增 `async: true`：参数、对象与后端可用性当场检查（出图、配音先跑一次 dryRun），不合格照旧直接报错；通过后返回 `{ok, async, jobID, job}`。`review.run` 另接受 `requestID`（async 时去重）。不带 `async` 时行为与返回值不变。
- `drama.get_progress`：能批量做的步骤（`generate_start_frame`、`run_review`、`generate_video`、`refresh_video`、`regenerate_dialogue_audio`）多出 `batch {tool, arguments}`；新增 `batchSteps`（按工具与集汇总：`tool`、`arguments`、`covers`、`actions`、`reason`，同类任务在跑时给 `runningJobID`；有已授权声音但台词没配音的集给 `episode.dub` 与 `dialogueLinesWithoutAudio`）与 `backgroundJobs`（本剧排队中 / 进行中的任务）。原有字段与步骤顺序不变。
- `HostBridge`：后台任务里的反向请求限时 15 分钟（`HostBridge.with_timeout`，线程局部；超时错误码 -32004），前台调用不变；发起方已放弃的请求、宿主迟到的响应直接丢弃并记日志，不再进主循环被当成请求回错。
- `ImageGeneration#available?`、`VoiceGeneration#available?`、`ReviewRunner#preflight`：后台提交前的检查。
- 页面：生成队列页在视频任务列表上方显示「后台任务」面板（只读 `jobs.status`，有任务在跑每 3 秒读一次、空闲 15 秒；可取消）。`App.tsx` 只加了引用与一行挂载。
- `skills/video-studio/SKILL.md`：工作循环新增「长活走后台」一节，首帧、配音、视频、审核各步写明批量工具与计费口径。
- `ARCHITECTURE.md`、`docs/AGENT_AUTOMATION_DESIGN.md`（新增 3.7 节与一条风险）同步更新。

### Tests
- 新增 `scripts/background_jobs_test.rb` 29 项：执行器（同 requestID 去重、排队上限、取消排队中的任务不再执行、处理块抛错只让该任务失败、启动恢复只标主人已死的任务）；端到端（`/usr/bin/ruby` 起服务、假网关出图拖慢 2 秒）：async 出图立即返回、任务在跑时 `drama.get` / `drama.list` 每次 < 0.5 秒、任务完成并落库；async 参数错误当场返回且不建任务；`jobs.wait` 超时返回 `timedOut`、经 stdio 封顶 8 秒；取消排队任务不再出图；`episode.generate_frames` 跳过已有候选与缺提示词的镜、同 requestID 返回同一任务、重跑不再出图、张数超限花钱前拒绝；`drama.get_progress` 的 `batch` / `batchSteps` / `backgroundJobs`；`episode.dub` 配完全集并选定音频、重跑全部跳过；`episode.generate_videos` 提交、轮询、下载并跳过没选首帧的镜，重跑不重复提交；进程在出图途中退出后任务显示 `interrupted`、同 requestID 重新开始并完成；宿主模式下工作线程经 HostBridge 出图（反向请求在 `jobs.wait` 期间被应答）、async `review.run`、`review.run_batch` 跳过仍有效的结论、改过的对象只重审那一个、非法范围被拒。
- `scripts/support/fake_gateways.rb`：新增 `image_delay`，模拟慢出图后端。`.github/workflows/release.yml` 单列这组测试（`ci.yml` 按 glob 已包含）。
- 新增 `ui/src/BackgroundJobsPanel.test.tsx` 5 项；`ui/src/commands.test.ts` 把后台任务命令纳入「页面用到的命令都已登记」检查。

## [0.35.0-rc2] - 2026-10-02

### 用户影响
- **首尾帧里的人物不再被画成别人。** 《回村养鸭》第 1 集 14 镜首帧里，有 5 镜把 27 岁的女主许禾画成了男人：参考包带了许禾的身份图、戴孝造型、男村民的身份图和鸭棚场景图，提示词却只有分镜描述，模型不知道「许禾」是哪张图里的人。现在出首尾帧、造型图、场景变体图时，提示词开头写明每张参考图是什么——「图1 是「许禾」的长相参考——27岁女性，鹅蛋脸……；图2 是「许禾」本镜的服装造型「许禾·戴孝」；图3 是「村民甲」的长相参考——55岁男性……；图4 是场景「许家鸭棚·外」的空间与陈设」，并要求人物长相、性别、年龄以对应参考图为准。
- **没绑出场角色的镜头也带上角色的造型。** 只绑了场景、没绑角色的镜头，以前按文字带上角色时只带脸，许大强因此丢了标志性的衣服；现在沿用该角色在本集最近一镜里绑的造型，并提示「按镜头文字带上了某某，造型沿用第 N 镜的某某」。
- **画幅不对的图会被标出来。** 模型偶尔无视要求的尺寸，竖屏剧回一张横图。现在首尾帧、场景图出完会核对方向：不对的照常保存（已经计费，不丢），候选左上角显示红色「画幅不符」，鼠标悬停看实际尺寸，建议重抽、不要选它进成片。
- 已有候选图不受影响；新生成的候选记录的提示词会包含这段参考图说明。无数据迁移。

### Fixed
- `image.generate` target=start/end、appearance、sceneVariant：带参考图时提示词最前面拼参考图说明（`server/lib/reference_legend.rb`，唯一实现）。说明、发给后端的 `reference_paths` 与 `referenceSummary` 由同一个有序列表生成，图 N 就是第 N 张参考图。角色描述取 visualPrompt 前两句、去掉带否定词的分句（「不戴首饰」这类不进说明）、约 36 个汉字截断；只写正面的话。参考包里有、但没有身份图的出场人物按文字描述写一句。
- 参考包编译（`ReferencePackage.compile`）：`cast` 为空时推断出的角色沿用本集其它镜头给该角色绑的造型（前面最近一镜优先，再找后面），并给 `cast_inferred` 警告。空 `cast` 仍按推断处理而不是「本镜无人」：`drama.set_reference_package` 总把 `cast` 写成数组，只绑场景的包与明确无人的包分不出来；要纯空镜用 `castIDs: []`。依据写进设计稿 5.2 第 6、7 条。
- 出图后读回图片宽高（`server/lib/image_dimensions.rb`，只读 PNG / JPEG / WebP 文件头），首尾帧、场景、场景变体图的方向与请求尺寸（`drama.frame.image`）相反时，候选记 `aspectMismatch: true`、`actualSize`、`requestedSize`。读不出宽高时不标记。

### Changed（MCP 契约，向后兼容）
- `image.generate` dryRun 新增 `referenceLegend`（拼在 `prompt` 最前面的那段，没有参考图时为空串）；`referenceSummary[]` 与真出图结果的 `references[]` 每项新增 `index`（从 1 起，对应「图 N」）与 `legend`（这张图的说明）。
- `image.generate` 真出图结果新增 `aspectMismatchCount`；画幅不符的候选在 `generated[]` 里带 `aspectMismatch` / `actualSize` / `requestedSize`，`warnings[]` 多一条 `code: aspect_mismatch`（带 `candidateID`）。
- 视频侧提示词不变：H3 的 `subject_definitions` 已按 `<Subject N>` 定义人物，视频请求也只带首帧或参考视频，身份图、造型图不随视频发出。
- 页面：首尾帧候选与资产库候选上显示「画幅不符」角标（`ui/src/AspectBadge.tsx`，中英文文案）。`skills/video-studio/SKILL.md` 与工具说明补写参考图说明与画幅标记。

### Tests
- `scripts/image_generate_test.rb` 新增 7 项：参考图说明按参考图顺序逐张命名（身份、造型、身份、场景）并与路径、摘要一一对应；说明里没有否定词、第三句不进摘录；真发出去的提示词和参考图顺序与 dryRun 一致；空 `cast` 推断角色并沿用前后最近一镜的造型；`castIDs: []` 不带任何角色；竖屏剧回横图时候选照记、打标、计数并写进存档；PNG / JPEG / WebP 文件头宽高读取与方向判定。两条既有断言改为带参考图说明的完整提示词。
- 新增 `ui/src/AspectBadge.test.tsx` 3 项。

## [0.35.0-rc1] - 2026-10-02

### 用户影响
- **定妆图、资产参考图只审选定的那一张。** 以前所有候选一起送审，选定的定妆图干干净净，落选的早期候选上有像品牌的皮带扣、真实店招，结论照样是「有风险」，怎么改选定图都消不掉。现在选定了图就只送这一张，材料里写明「只审选定的这 1 张图」；还没选定时照旧连同全部候选一起审（最多 12 张）。换选一张后，审核徽标会提示「内容已改，待复审」。首尾帧仍审全部候选（选定的在前）。
- **审核能收尾了。** 每条问题标「必改」或「建议」：必改是不改大概率过不了平台审核，建议是风格、口味或可改可不改的优化。只剩建议的结论显示为「N 条建议」、按已处理的颜色显示，不再弹红色提示。
- **「已知悉」。** 有必改项、但你看过后决定先不改时，点开审核面板点「已知悉」，可以写一句备注（例如「店名已获授权」）。徽标变成「N 处风险（已知悉）」，面板里写明谁在什么时候确认、备注是什么，随时可以撤销。内容改了或重新审核后确认自动失效。剧情、角色、分集、镜头、资产、成片内容审核和画面质检都可以确认；「不通过」不能确认，必须改。
- 聊天里的 Agent 看到的进度同一口径：只剩建议或已知悉的结论不再排进待办，带必改项又没人确认的会排一条「处理审核风险」。
- 升级后，旧的「有风险」结论里的问题没有分级，一律按「建议」算，多半直接显示为处理完；想要分级结果，重新审核一次即可。
- 无数据迁移。

### Added
- MCP 工具 `drama.acknowledge_review`（`dramaID`、`scope`、对象 ID、`aspect`、`acknowledgedBy` user | agent、可选 `note` ≤ 500 字、`basis`、`revoke`）与 `video.acknowledge_review`（`id`、`aspect` content | frames，其余同上）：在存好的 warn 结论上记 `acknowledgement = { acknowledgedBy, note, at, basis }`，不动 `rev`、不进历史、不改任务状态。错误码：`review_not_found`（还没审过）、`review_not_acknowledgeable`（不是 warn）、`review_changed`（传入的 `basis` 不是当前那份结论）、`invalid_acknowledged_by`。插件清单登记命令 `drama.acknowledgeReview` / `video.acknowledgeReview`（中英文标题）。
- 决策记录 `docs/decisions/0002-review-convergence.md`。

### Changed（MCP 契约，向后兼容）
- 审核结论的每条问题新增 `level`（`must` | `advice`）。输出契约（`reviewContract`）与审核技能 `skills/creative/review.md` 要求模型给出；解析与落库时 `block` 一律 `must`，`warn` 缺省或写错按 `advice`。`schemas/creative-v1.json` 新增该字段（字符串、刻意不设枚举，写错不会让整份结论作废）。`drama.record_review` / `video.record_review` 照单接受，没有 `level` 的按上面规则补。
- `review.get_material` / `review.run`：`character/images` 与 `asset/images` 有选定图时只附选定的那一张，标签为「角色「X」选定的定妆图」「资产「X」选定的参考图」，材料多出「选定图的出图提示词」「审核范围」两段，返回 `selectedCandidateID`；选定 ID 计入 `basis`。没选定时与以前逐字一致（指纹不变）。`review.run` 另返回 `mustFix`（必改条数）。
- `drama.get_progress`：状态字符串不变；有 warn 的节点多出 `reviewDetail[aspect] = { must, advice, acknowledged, acknowledgedBy? }`（成片在 `video.reviewDetail`）；`nextSteps` 新增 `resolve_review_warn`（带必改项且没人确认的 warn，排在 `resolve_review_block` 之后、`run_review` 之前），只剩建议或已确认的 warn 不排步骤。
- 页面：审核面板每条问题显示「必改 / 建议」；warn 结论有「已知悉」按钮（可写备注、可撤销）；徽标新增「N 条建议」「N 处风险（已知悉）」两种已处理状态；只剩建议的审核结果用普通提示，不弹红条。角色列表、资产卡的徽标同一口径。视频生成页的候选卡质检面板改用质检文案。
- 页面代码：成片卡上的两块审核面板拆到 `ui/src/JobReviewPanels.tsx`，「已知悉」两条命令在 `ui/src/reviewCommands.ts`，审核请求消息的拼法移到 `review.ts#reviewMessages`，`App.tsx` 保持在 3000 行以内。
- `skills/video-studio/SKILL.md`「Content review」一节补写只审选定图、必改与建议、何时确认已知悉、不要对没改的内容反复重审。

### Tests
- `scripts/review_run_test.rb` 新增 7 项：选定定妆图后只送这一张、`get_material` 与 `review.run` 的材料 / 指纹一致；问题分级与 `resolve_review_warn`；已知悉不动 rev、清掉待办，`basis` 不对与 `acknowledgedBy` 非法被拒；换选变待复审、换回恢复；复审替换确认、只剩建议不排待办；只有存在的 warn 能确认；成片结论确认与撤销。一致性夹具新增「选定了一张已不存在的候选」的资产，改了选定图审核的两条期望值，其余期望值未变（旧结论的指纹不变）。
- `ui/src/review.test.ts` 新增 / 改写 4 项（只审选定图与回退、资产选定图、问题分级、建议 / 已知悉 / 失效）；新增 `ui/src/ReviewPanel.test.tsx` 4 项；`ui/src/App.test.tsx` 新增成片「已知悉」1 项，两条既有用例的结论改为显式 `level: must`。

## [0.34.0-rc2] - 2026-10-02

### Fixed
- **成片画面质检（`review.run` / `review.get_material` 的 `aspect=frames`、页面「质检」）在用户机器上一次都跑不通。** 两处叠在一起：
  - 服务端跑在 `mcp.json` 钉死的 `/usr/bin/ruby` 2.6 上，0.33.0-rc1 的抽帧和镜头上下文用了 2.7 才有的 `filter_map`，一调用就 `NoMethodError`（Agent 看到「Video Studio internal error」）。CI 用 Ruby 3.2，且当时没有任何测试真跑一次质检，所以一直是绿的。改成 `map … compact`。
  - Homebrew 的 ffmpeg 没有 `drawtext` 滤镜（不带 libfreetype），带时间戳的拼图命令整条以「No such filter: 'drawtext'」失败。现在先问 ffmpeg 有没有这个滤镜（`FFmpegTool.filter?`），没有就出不带时间戳的拼图，并在材料的本地自动检测里写明版式（`sheetLayout`：6 列 × 4 行、每格 0.5 秒、是否烧了时间戳），复核说明要求模型据此换算时间点。
- **每次打开页面都把「成片自动质检」改回开启。** 页面开机时用 `video.status` 读这个开关，但插件清单没登记这条命令，宿主当场拒绝；页面按默认值「开启」把设置写回插件。现在清单登记了 `video.status`（中英文标题齐全）；读不到开关时，同步设置不再带上 `autoQA`，用户的选择不会被默认值覆盖。
- 成片卡上的「画面质检」面板和「内容审核」面板显示同样的「审核：未审核」与「审核」按钮，分不清点的是哪个。质检面板现在用设计稿里的质检徽标（未质检 / 质检通过 / 质检提醒 / 质检拦截），按钮写「质检」「重新质检」；英文同步改为 Frames not checked 等。

### Tests
- 下面三项此前一直失败、被当成「基线失败」放过，现已逐条查明并修好，全量测试恢复全绿：
  - `ui/src/commands.test.ts`「declares every command id the page calls」：页面调用的 `video.status` 没在清单里，是真问题（见上），按清单补登记修复。
  - `ui/src/App.test.tsx`「宿主不收视频时成片不送审，并说明原因」：0.33.0-rc1 给成片卡加了质检面板后，两块面板文字一模一样，测试按名字找按钮找到两个。改的是界面（按设计稿区分两块面板），测试同时断言两块都置灰、各自说明原因。
  - `scripts/episode_compose_test.rb`「choosing the video's voice keeps the clip's own sound…」：测试要求告警为空，但按设计稿 `docs/design/video-qa.md` 4.1，开着自动质检、还没质检的镜头会带 `qa_pending`（提示、不拦合成）。代码符合设计，改测试为「配音方面没有告警，只有 `qa_pending`」。
- `scripts/review_run_test.rb` 新增 1 项：用 ffmpeg 现场生成的片段真跑一次画面质检（`/usr/bin/ruby` 起服务），核对拼图、版式、镜头上下文和结论落在 `reviews.frames`；没装 ffmpeg 时记为跳过。
- 新增 `scripts/ruby_compat_test.rb`：按名字扫 `server/` 下 Ruby 2.6 没有的方法（`filter_map`、`tally`、`Hash#except` 等），CI 的 Ruby 3.2 上也能挡住。
- `ui/src/App.test.tsx` 新增 1 项：读到的自动质检开关原样同步，读不到时不带 `autoQA`。

### 用户影响
- 成片画面质检在装了 Homebrew ffmpeg 的 Mac 上能用了；拼图格子上没有时间戳时，审核结论里的时间点由模型按版式换算，精度约 0.5 秒。
- 关掉的「成片自动质检」不再被打开页面这个动作改回开启。之前被改回的，需要在设置页重新关一次。
- 无数据迁移。

## [0.34.0-rc1] - 2026-10-02

### 用户影响
- 以前侧栏全局导航里的「资产库」看起来是所有剧共用的，实际只显示当前选中那部剧的资产，而且一律停在「造型」页签。剧里只有道具、没有造型时，打开就是空的，看起来像资产库坏了；5 个已定妆的角色也不在资产库里出现。现在：
  - 进剧后「制作流程」里多了一项「资产库」，紧跟「角色固化」，就是原来那一页（本剧资产）。后面各阶段的编号顺延一位（分集剧本 04、视频生成 07……）。
  - 侧栏全局的「资产库」改成跨剧浏览：所有剧的资产按剧分组，剧名旁写各类数量，可按「全部 / 造型 / 场景 / 道具 / 声音 / 角色」筛选和搜索。这一页只浏览，点资产卡进入那部剧的资产库并选中它，点角色卡进入那部剧的角色固化并选中这个角色。
  - 资产库打开时停在第一个有资产的页签（造型 → 场景 → 道具 → 声音，已归档的不算）；一个都没有时停在造型，并说明资产是什么、角色定妆照在「角色固化」里，附「去角色固化」按钮。
  - 本剧资产库新增只读「角色」页签：每个角色选定的定妆照和名字，没定妆的显示「未定妆」占位；点角色跳到角色固化并选中它。

### Added
- `ui/src/assets/GlobalAssetLibrary.tsx`：全局资产库。数据取 `drama.list` 已返回的整部剧（含 `assets`、`characters`），不新增 MCP 工具，服务端无改动。
- 页面阶段 `dramaAssets`（本剧资产库），侧栏高亮：本剧资产库算创作流程，亮的是全局「短剧」；全局「资产库」只在跨剧页亮。

### Changed
- `AssetLibrary` 新增可选属性 `initialAssetID`（打开时选中并切到它的页签，已归档的会同时打开「显示已归档」）与 `onOpenCharacter`。
- 剧内对象的显示值与派生状态（`episodeView`、`characterView`、`draftDifferences`、`characterProgress` 等）从 `App.tsx` 拆到 `ui/src/drama-views.ts`，`App.tsx` 回到 3000 行以内；`App` 仍导出原来那几个函数。
- 测试：`ui/src/assets/AssetLibrary.test.tsx` 新增 4 项（默认页签、全空时的说明与跳转、按 `initialAssetID` 选中、「角色」页签渲染与回调）；新增 `ui/src/assets/GlobalAssetLibrary.test.tsx` 6 项（分组与计数、页签过滤、搜索、点击回调、全空说明、文案 key）；`ui/src/App.test.tsx` 改写资产库入口用例为「全局跨剧 + 本剧入口紧跟角色固化 + 侧栏高亮」，新增「角色」页签跳角色固化用例，阶段编号随之顺延。

## [0.33.0-rc3] - 2026-10-02

### Fixed
- **策划、角色、分集、镜头的内容审核（`review.run`）一律报「Video Studio internal error」。** 0.33.0-rc1 给成片质检加了 `systemExtra`（抽帧拼图的复核说明），拼审核提示词时直接对它调 `empty?`；只有成片质检的材料带这个键，其余审核拿到的是 nil，请求还没发给审核模型就崩了。页面上点「审核」和 Agent 经 MCP 调 `review.run` 都受影响，`review.get_material` + `drama.record_review` 不受影响。现在缺这个键时按空串处理。
  - 测试：`scripts/review_run_test.rb` 原本就覆盖这条路径（13 项），此前因这个崩溃整份脚本中途退出，被误当成「基线就失败」；修复后 13 项全部通过。
- `0.33.0-rc2` 的更新说明里写定妆取景用「平静基准表情」，实际发布的取景行只写「按描述的基准表情」，不追加平静中性，以免和许大强这类设定为假笑的角色冲突；本条更正说明，代码不变。

### 用户影响
- 升级后页面和 Agent 都能正常跑内容预审。无数据迁移。

## [0.33.0-rc2] - 2026-10-02

### Fixed
- **出图额度用完时，调用方分不清「过会儿重试」和「重试也没用」。** 以前上游任何失败都报 `image_generation_failed`、逐张 `host_error` 加原话，403「配额不足」（`insufficient_quota`）和 Cloudflare 504 长得一样，Agent 和页面只能盲目重试，一次 4 张照样逐张打满。现在宿主代管与直连两条出图路径都按错误码（`upstream_quota_exhausted` / `insufficient_quota` / `quota_exceeded` / `billing_not_active`）或原话措辞（配额不足、额度不足、余额不足、insufficient quota / balance）认出额度耗尽：
  - 逐张 `failed[]` 记 `code: quota_exhausted`、`retryable: false`、`upstreamQuota`（`true` 为 some.im 网关 0.246.0-rc3 起回的平台上游额度耗尽 503，`false` 为本账号自己的额度 / 余额）和 `httpStatus`；同一次调用剩下的张不再请求，结果带 `stopped: { code, skipped }`。
  - 一张都没出时整次报 `image_quota_exhausted`，信息里写明该谁补额度、现在重试也会失败；`error.retryable: false`、`error.upstreamQuota` 同上。
  - 其他失败逐张带 `retryable`：超时、断网、408 / 425 / 429、5xx（含 504）为 `true`，其余 4xx、配置错、参照图不对为 `false`；整次 `image_generation_failed` 的 `error.retryable` 取逐张任一可重试。错误码 `host_error` / `image_api_error` 不变。
  - 页面（角色定妆、首尾帧、资产库）对 `image_quota_exhausted` / `quota_exhausted` 显示中英文说明；角色定妆和首尾帧多模型出图时，碰到额度用完不再给下一个模型发请求。逐张失败的提示以前统一显示「操作失败」，现在按错误码翻译、没有翻译时显示服务端原话。
  - `image.generate` 工具说明与 `skills/video-studio/SKILL.md` 补写上述错误码与处理方式。视频、配音后端与出图没有共用的错误归类代码，本次不动。
- **定妆、造型、道具参考图背景和景别随机，当参照图时把杂物带进首尾帧。** 以前角色定妆只发 `visualPrompt`、造型 / 道具只发资产描述。现在提示词末尾固定拼一行棚拍取景（`ImageGeneration::IDENTITY_FRAMING` / `PROP_FRAMING`）：定妆与造型为全身正面站姿、按描述的基准表情、从头到脚完整入画；道具为描述的主体单独居中、完整入画；都是浅灰无缝背景、柔和均匀光。只写要什么、不写否定句（出图模型会把提到的东西画出来）；不拼剧的 `visualStyle`（那是场景的光线色调）。首尾帧、场景、场景变体不加。dryRun 返回的就是拼好的整段提示词，候选记录的也是这一段。
- **聊天里的 Agent 经 MCP 网关建的剧、写的角色和候选图，页面关掉重开之前一直看不到。** 页面只在打开时读一次短剧列表，外部写入不经过页面。现在窗口重新获得焦点、页面回到前台时重读一次，页面可见期间每 15 秒再静默读一次；读到的内容与页面手上的一字不差就不更新，不触发任何按剧重算的界面。页面正在出图、生成、保存、AI 流式写作，或当前集 / 镜 / 角色的编辑框里有还没保存的改动时，这一轮整个跳过，用户的字不会被外部版本冲掉；选中的剧、集、镜不变。静默刷新失败不提示，下一轮再读。
  - 测试：`ui/src/App.test.tsx` 新增 4 项——焦点 / 回到前台后外部新剧出现；15 秒定时重读；内容没变时资产摘要不会跟着重拉；当前集编辑框没动过时跟着外部更新，正在改时不被冲掉也不发请求。
- 测试：`scripts/image_generate_test.rb` 新增 8 项——宿主 403 `insufficient_quota` 只发一张即停并报 `image_quota_exhausted`；网关 503 `upstream_quota_exhausted` 标为平台上游；Cloudflare 504 可重试、不提前停；`classify_failure` 的错误码 / 措辞 / 状态码表（含被截短的 JSON）；直连接口对同样三种响应的归类；定妆 dryRun 带取景、首尾帧不带；造型 / 道具带取景、场景不带且不拼 `visualStyle`；取景行不含否定词。`ui/src/App.test.tsx` 新增：额度用完时显示中文说明，且不再给第二个模型发请求。

## [0.33.0-rc1] - 2026-09-28

### Added
- **插件内成片画面质检**：完成的视频可由设置页选定的多模态审核模型（例如 Qwen3.8-Max）按带时间戳的抽帧拼图复核；支持手动补审和新视频自动复核，结论按视频任务保存。
- **质检参与合成门禁**：被拦截的已选镜头会阻止合成；候选卡显示质检状态与摘要，人工放行需二次确认，成片页可跳回对应镜头。
- **本地自动线索**：复核材料附带时长异常和疑似硬切信息，供审核模型与人工参考。
### Changed
- `review.get_material` / `review.run` 的 `scope=video` 新增 `aspect=frames`；`video.record_review` 新增可选 `aspect`（`content` 或 `frames`），`video.settings` 新增 `autoQA`，`drama.select_video` 新增可选 `qaOverride`。

### Known limitations
- 当前未实现：配音包络对位检测、按问题时间点生成原分辨率截图、对本剧存量镜头的一键批量补审，以及设计稿第 5 节的生成提示词根因修复。

## [0.32.0-rc7] - 2026-09-27

### Fixed
- **点「放大播放」后，原来的小播放器还在接着放，两路声音叠在一起。** 生成队列、成片页、资产库的放大入口共用一个放大层，打开时只管放大层自己自动播放，不管页面上别的播放器；点小播放器本身还会触发它的原生播放。现在放大层一打开，就先把页面上其他正在播的视频 / 音频都停下；放大层开着期间，别的播放器一开始播也立刻停掉（点击触发的原生播放晚于放大层打开）；放大层自己不受影响，关掉放大层后不再拦。
- 测试：`ui/src/App.test.tsx`「完成的任务补上播放镜像后就地播放」补断言：放大后小播放器被暂停；放大层开着时小播放器再播放会被停下、放大层自己不受影响；关掉放大层后不再拦（去掉修复后这条会失败）。

## [0.32.0-rc6] - 2026-09-27

### Fixed
- **照台词音轨生成的镜头（ref2va）合成后台词发糊、听不清。** 这类镜头以前一律用视频自带的声音：视频模型把台词重新念了一遍（32 kHz AAC），和配音原文件的包络相关只有 0.8–0.9，用户反馈「配音有一些听不清」。现在本镜选「单独配音」（默认）时，合成直接铺生成这条视频时喂进去的那条对白音轨（任务记录 `referenceAudioPath`，单句是那句配音、多句是拼好的整条），视频原声静音，不会念两遍。
  - 音轨从 0 毫秒起铺，不加普通配音镜头第一句前的 0.3 秒留白：视频的口型就是照这条音轨从 0 毫秒对的，加了留白声音会比嘴晚 0.3 秒。
  - 音轨文件已不在时退回视频原声，并给 `synthetic_voice` 提示（文案随之改为「音轨文件已经找不到了」）；本镜选「视频配音」照旧用视频自带的声音。
  - 这类镜头不再逐句补配 TTS（台词都在音轨里）；合成计划多一个 `dialogueTrack`（只带 `durationMs`，文件路径不给页面）。
- **成片整体偏小声。** 合成没有统一响度，TTS 原文件多在 -20～-24 LUFS，整集约 -23 LUFS，手机外放得把音量开很大才听得清台词。现在混完背景音乐后多一步两遍 `loudnorm`，把整集拉到 -16 LUFS、真峰值 -1.5 dBTP（线性增益，不压动态，台词与闪避后音乐的比例不变）；页面进度仍显示在「混音」这一步。整集无声测不出响度时跳过并给 `loudness_skipped` 提示。实测《老宅有锁》第 1 集：-23.0 → -16.1 LUFS，峰值 -1.5 dBFS。
- 测试：`scripts/episode_compose_test.rb` 新增 3 项：音轨从 0 毫秒起（0.05–0.25 秒就有声、0.6 秒后已结束）、片段自带的声音被静音、不调 TTS；音轨文件缺失时退回视频原声并提示；选「视频配音」时保留片段原声。主合成用例加断言：成片积分响度在 -16 ± 1.5 LUFS。

## [0.32.0-rc5] - 2026-09-27

### Fixed
- **视频候选卡里的「选用这条视频 / 已选定」被拉成竖长块，不像按钮。** 0.28.0-rc4 那次修复没生效：按钮规则写成单独的 `.video-candidate-select`（优先级 0,1,0），压不过图片候选的 `.candidate-grid button { aspect-ratio: 9/12 }`（0,1,1），按钮照样继承 9:12 竖向比例，宽 100% 时高度跟着列宽涨，还被套上图片候选的边框。现在规则写成 `.candidate-grid .video-candidate-select`（0,2,0），恢复为约 32px 高的单行按钮。
- 测试：新增 `ui/src/video-candidate-styles.test.ts`，检查样式表里按钮规则带 `.candidate-grid` 前缀、`aspect-ratio` / `height` 为 `auto`，且不再有单独的 `.video-candidate-select` 规则（jsdom 不算布局，只能查规则本身；换回旧写法会失败）。

## [0.32.0-rc4] - 2026-09-27

### Fixed
- **ref2va（首帧 + 对白音轨）全部被后端拒绝：「backend HTTP 400: Invalid JSON in form field.」。** 视频后端（vLLM-Omni）把 `audio_reference` 当 JSON 解析，要的是 `{"audio_url":"data:audio/wav;base64,…"}`；插件照 Tsingfly Hub 当时的文档发了裸 data URL。Hub 与 h3 网关都原样转发，任务先排上队、派给后端时才失败。2026-09-27 实测：一部剧 229 条 ref2va 全部以 `backend_request_rejected` 失败；Hub 09-10 那次唯一成功的图 + 音频实测用的正是 JSON 形式，09-11 起文档、教程页与测试脚本被改成了裸 data URL。现在按 JSON 对象发送（只多约 16 字节，仍在 1 MiB 文本字段上限内）。测试假网关同步改为按真实后端解析这个字段，`standalone_pipeline_test.rb` 断言 JSON 形状（换回裸 data URL 会失败）。
- **视频任务存档按新旧一刀切截到 500 条，把已完成的成片记录删掉了。** `jobs.json` 每次写入后只留最新 500 条，不看状态。同一天一部 24 集的剧经过几轮服务商故障与排队重试，500 条里 325 条是失败记录，早期已完成的成片记录连同另一部剧的全部任务被挤掉：镜头的「已选成片」指向不存在的任务，整集合成找不到片段（MP4 文件还在磁盘上）。现在上限提到 2000，超出时只淘汰最旧的失败 / 取消记录；已完成、在途与草稿一律保留（极端情况下可以超过上限）。读档时同样按这条规则整理。新增 `scripts/video_store_retention_test.rb` 并加入发版流水线。

## [0.32.0-rc3] - 2026-09-27

### Fixed
- **配音服务商选百炼（dashscope）时一句都合成不出来，报「DashScope audio URL must use HTTPS.」。** 百炼回包里 `output.audio.url` 是阿里云 OSS 的签名直链，实际给的是 `http://`；适配器下载前只认 HTTPS，于是每一句都被拒收。2026-09-27 实测：配好百炼 Key 后 `voice.generate` 试听第一句即失败。现在阿里云主机（`*.aliyuncs.com`）的 `http://` 直链改走 HTTPS 下载（OSS 同一主机支持 HTTPS，签名与协议无关，查询串原样保留）；其他远程主机的 `http://` 仍然拒收，本机测试照旧放行 HTTP。
- 测试：`scripts/tts_dashscope_test.rb` 补「OSS 的 http 直链改走 HTTPS、签名查询串不变，其他远程 http 与伪装成 aliyuncs 的主机仍拒收」。

## [0.32.0-rc2] - 2026-09-27

### Fixed
- **WillDeep macOS 与 willdeep-rs 的 Web 宿主同时开着时，macOS 那边经网关提交视频报「Video API key is not configured.」。** 两个宿主拉起插件时都不传 `VIDEO_STUDIO_DATA_DIR`，两个插件进程共用 `~/Library/Application Support/WillDeep/plugin-data/willdeep-video-studio`，各自把随机端口和 token 写进同一份 `mcp-http.json`，后写的赢。macOS 宿主的插件 MCP 网关照这份文件转发，请求就落进了 willdeep-rs 拉起的插件进程：视频 Key 是 macOS 宿主经 `mcp.json` 注入的插件设置，那个进程里没有；出图、审核这类反向请求也会发给另一个宿主。2026-09-27 实测：文件里的端口经 `lsof` 对上 willdeep-rs 的插件进程，macOS 的插件在另一个端口；停掉 willdeep-rs 那边的插件后文件被它删掉，macOS 网关才退回 stdio、回到自己的插件。
  - 现在连接文件写在**拉起本进程的宿主的网关先读的那一处**，按 stdio `initialize` 的 `clientInfo.name` 认宿主（`lib/mcp_http_endpoint.rb`）：macOS 宿主（`WillDeep Desktop (some.im)`，早期不带 clientInfo 的同样算）照旧写数据目录的 `mcp-http.json`；willdeep-rs（`willdeep`）写 `<WILLDEEP_HOME 或 ~/.willdeep>/plugin-data/willdeep-video-studio/mcp-http.json`，这正是 willdeep-rs 网关先读的目录；其他 MCP 客户端写 `mcp-http.<客户端名>.json`，不占任何网关读的位置。两个网关都不用改。
  - 只有连接文件分开，`dramas.json` 等存档仍在同一个数据目录，两个宿主看到同一批短剧。
  - 端口仍在启动时打开，连接文件改在 stdio `initialize` 时写（网关总是先经宿主拉起插件再读文件）；HTTP 客户端的 `initialize` 不挪它。写法改为同目录临时文件（建时即 `0600`）再 `rename`，网关不会读到半份，也没有按 umask 对别人可读的一瞬。
  - 退出时仍只删 token 还是自己的那份。
- 不按 `VIDEO_STUDIO_HOST_MODE` 判断宿主：它是媒体根的强制开关，macOS 宿主下强制 Web 媒体也不该挪走 macOS 网关要读的文件。

### MCP 契约
- MCP 工具、字段、错误码均无变化。
- 宿主网关契约（`docs/decisions/0001-plugin-mcp-gateway.md`）新增「修订 1」，**连接文件位置变了**：willdeep-rs 拉起的插件进程不再写 macOS 数据目录的 `mcp-http.json`，改写 `<WILLDEEP_HOME 或 ~/.willdeep>/plugin-data/willdeep-video-studio/mcp-http.json`；第三方 MCP 客户端拉起时改写 `mcp-http.<客户端名>.json`。macOS 宿主拉起时位置不变。现有网关（WillDeep macOS ≥ 1.405.0-rc1、willdeep-rs ≥ 0.83.0-rc1）按原逻辑就能读到，不属破坏性变更。
- 连接文件在 `url`、`token` 之外新增 `host`（`willdeep-macos` / `willdeep-rs` / `other`）、`pid`（插件进程）、`parentPID`（拉起它的进程，即宿主）。两个网关只读 `url` / `token`，多出的字段被忽略。
- 修订 1 附宿主侧可选加固（尚未实现）：`parentPID` 不等于网关所在进程就当文件不存在；willdeep-rs 退到 macOS 路径时拒收 `host` 为 `willdeep-macos` 的文件。插件版本混装（某个宿主装的是 ≤ 0.32.0-rc1）时，旧进程仍会写 macOS 那份，只有这条加固能挡住。

### 协调
- 宿主侧无须改动即可生效。Xedit（`docs/decisions/0001-plugin-mcp-gateway.md`）与 willdeep-rs（`docs/decisions/2026-09-26-plugin-mcp-gateway.md`）各有一份契约副本，需同步修订 1；上面的可选加固由两个宿主各自排期。
- WillDeep macOS 内置的插件升到本版本后，macOS 那份文件只剩 macOS 拉起的进程会写；willdeep-rs 装的插件也需升到本版本，它才会改写自己的目录。

### 测试
- `scripts/streamable_http_test.rb` 新增 21 项，共 31 项：
  - 表驱动断言各宿主的连接文件位置（含 `../`、非 ASCII 客户端名）。
  - 不带 clientInfo 的宿主按 macOS 算，文件里的 `host` / `pid` / `parentPID` 对得上。
  - HTTP 客户端自称 `willdeep` 做 `initialize`，连接文件不挪。
  - 两个插件进程共用一个数据目录，按「macOS 先 / willdeep-rs 后」「willdeep-rs 先 / macOS 后」「macOS 先 / 第三方客户端后」三种顺序：后起的不改先起的那份；两份文件各自 `0600`、`host` 与 `pid` 指向各自进程、端点不同；经各自那份调 `video.status`，只有 macOS 那份的 `apiConfigured` 为真（Key 只注入给 macOS 进程，复现当日报错的判据）；后起的退出只删自己那份；先起的退出删掉自己那份。
  - 同一数据目录里新旧进程并存的原有用例（「退出只删自己的」）保留。
- `support/fake_host.rb` 可指定 `clientInfo`，并暴露插件进程 pid。
- 反验：同一份测试放到 0.32.0-rc1 的服务端上跑，31 项里 14 项失败。其中「macOS 先 / willdeep-rs 后」经 macOS 那份得到 `apiConfigured=false`，即当日症状；willdeep-rs 进程退出时把共用的那份删掉，也与实测一致。
- 结果（本机 Ruby 3.2，服务端进程跑在 `/usr/bin/ruby` 2.6）：15 个 Ruby 测试脚本全部通过。`streamable_http_test.rb` 31 项；其余 `server_test.rb` 49、`standalone_pipeline_test.rb` 33、`drama_test.rb` 29、`asset_tools_test.rb` 28、`tts_dashscope_test.rb` 18、`media_host_test.rb` 17、`agent_tools_test.rb` 14、`image_generate_test.rb` 14、`episode_compose_test.rb` 13、`music_backend_test.rb` 13、`review_run_test.rb` 13、`download_recovery_test.rb` 5、`manifest_locales_test.rb` 4、`version_test.rb` 4 项不变。页面源码未改，UI 12 个文件 273 项通过；`yarn --cwd ui build` 后 dist 与 0.32.0-rc1 相比只差内嵌版本号。

## [0.32.0-rc1] - 2026-09-27

### Fixed
- **写给编剧的基调被烧成了成片字幕。** H3 成片提示词的风格行此前原样拼进剧的 `tone`（`tone: …, genre: …`），而 `tone` 在策划时常写成「人物说话不端着……暧昧靠眼神和身体靠近完成，不写裸露……每集结尾落在一个动作或一句反话上」这类写作要求。2026-09-27《老宅有锁》的 MiniMax-H3 成片把这些句子当字幕叠进了画面（E6 第 9 镜），另一条成片在墙上写了「潮湿的」，E5 第 6 镜还把台词烧成了字幕。现在：
  - 剧新增 `visualStyle`（画面风格）：一句只写画面的话（光线、色调、质感、镜头、年代与美术），是成片风格行的唯一来源，写成 `Live-action, cinematic. Visual style: ….`；`tone` 和 `genre` 不再进成片提示词。
  - 旧剧没写 `visualStyle` 时，`tone` 只有形如「冷峻、克制」这种一串短形容词（无句读、每段不超过 6 个汉字或 12 个字母、不含「台词 / 说话 / 每集 / 不写」等写作字眼）才借用；否则风格行只剩 `Live-action, cinematic.`。出视频的参考包预览与 `video.generate dryRun` 给出 `visual_style_missing` 告警，提示在剧集元数据里补写。
  - 每条渲染出的 H3 提示词（三段式的 `[Shot 1]` 末尾、六段式的 `detailed_description` 末行）追加约束：不出字幕、说明文字和叠加文字，提示词里的字是指令、不是要画出来的字，只有剧情本身出现的文字（手机屏幕、信、招牌）可以出现；本镜有台词时再加一句「台词只说出来，不显示在屏幕上」。自带段名、原样透传的自定义提示词不受影响。
- 《老宅有锁》此前已临时把基调换成一句纯画面描述（`drama.save_draft scope=drama`，原文在历史记录里）。升级后建议：先把那句画面描述写进画面风格，再把原基调写回 `tone`。也可以用 `drama.restore_version scope=drama` 采用旧版，但那会把剧名、梗概、弧光等剧级文字一起换回当时的内容；画面风格不受影响（升级前的版本里没有这个字段，回滚只写版本里有的字段）。

### Added
- 策划卡单独列出画面风格；剧集元数据里新增「画面风格」输入框，保存即采用（`drama.save_draft scope=drama commit=true`，上一版进历史），没写时元数据标题上标出「未写画面风格」。编辑器拆在 `ui/src/VisualStyleEditor.tsx`，App.tsx 不再变长。
- 创作技能：`skills/creative/planning.md` 新增「基调和画面风格分开写」，输出字段加 `visualStyle`；`frames.md` 要求首尾帧的光线、色调、质感跟画面风格走；`skills/video-studio/SKILL.md` 的 Plan 一步说明两者的区别与补写方式。页面内置的策划提示词（中英）同步。
- `drama.get_stage_context`：planning 的 `write.fields` 加 `visualStyle` 并附说明，planning 与 frames 的材料里带「画面风格」。
- 剧级内容审核材料加「画面风格」一栏（服务端 `lib/review_material.rb` 与页面 `ui/src/review.ts` 同步，标签在 `schemas/review-labels.json`）。空着时不出现，已审过的旧剧指纹不变。

### MCP 契约
- 仅新增，无破坏性变更：`drama.confirm_plan` 的 `plan` 与 `drama.save_draft scope=drama` 的草稿白名单新增 `visualStyle`（字符串，≤300）；剧对象读出新增同名字段（旧剧没有这个键）；`schemas/creative-v1.json` 的 plan 定义加 `visualStyle`，并给 `tone` / `visualStyle` 加 `description` 说明用途；`drama.preview_reference_package` / `video.generate dryRun` 的 `ir` 新增 `visualStyle`、`styleSource`，`warnings` 新增 `visual_style_missing`。
- `tone` 的字段、长度和存取不变。变化在渲染结果：带镜头的 `video.generate` 默认提示词不再包含 `tone` 与 `genre`，并多出上面的文字约束。
- 没有新增插件设置或命令，`.willdeep-plugin` 清单与 locale 不变。

### 协调
- 本条最初按修复编为 0.30.0-rc5；集成时按「新增字段 + 界面属于新功能」定为 0.32.0-rc1，排在 plain-prompt 插入位置修复（`claude/reverent-solomon-8f4bdd`，0.31.0-rc3）与背景音乐路径补丁（0.31.0-rc4）之后合入。与 plain-prompt 修复都改 `lib/h3_prompt.rb`，改动不重叠，约定每个镜头行仍以 `[Shot 1]` 开头。

### 测试
- `scripts/standalone_pipeline_test.rb` 新增 8 项：confirm_plan 分开存 `tone` / `visualStyle`；风格行来自 `visualStyle` 且不含基调原句、`tone:`、`genre:`；有台词与无台词两种文字约束；六段式风格行在 `[Shot 1]` 之前、约束在末行；planning 阶段材料带 `visualStyle`；旧剧的写作型基调被丢弃并告警；短形容词基调被借用且告警点名；`drama.save_draft scope=drama` 写入画面风格、留历史、告警消失。
- `scripts/drama_test.rb` 新增 2 项（表驱动）：`tone_as_look` 的借用判定（中英文形容词、句子、写作字眼、引号、空值）与风格行的取值优先级。
- 审核对拍夹具 `scripts/fixtures/review-parity-*.json` 加入 `visualStyle`，剧级材料的两个语言的指纹随之更新；`review_run_test.rb` 与 `review-labels.test.ts` 两端各自复算一致。
- 页面：`App.test.tsx` 新增 2 项（剧集元数据里编辑并保存画面风格、策划卡列出画面风格且确认时与基调分开提交）。
- 结果（rebase 到 0.31.0-rc2 之后，UTF-8 locale）：15 个 Ruby 测试脚本全部通过——`drama_test.rb` 29、`standalone_pipeline_test.rb` 31，其余 `server_test.rb` 49、`asset_tools_test.rb` 28、`tts_dashscope_test.rb` 18、`media_host_test.rb` 17、`agent_tools_test.rb` 14、`image_generate_test.rb` 14、`music_backend_test.rb` 13、`review_run_test.rb` 13、`episode_compose_test.rb` 12、`streamable_http_test.rb` 10、`download_recovery_test.rb` 5、`manifest_locales_test.rb` 4、`version_test.rb` 4 项；服务端文件在 `/usr/bin/ruby` 2.6 下语法检查通过；UI 12 个文件 273 项通过（第一次整套运行时本机负载均值 200 以上，`App.test.tsx` 有 5 项超时，单独重跑与再次整套运行均全部通过）。`yarn --cwd ui build` 后 dist 随源码更新。没有对真实 MiniMax-H3 重跑《老宅有锁》，新约束对字幕的实际抑制效果待下一次成片验证。

## [0.31.0-rc4] - 2026-09-27

### Fixed
- **在 WillDeep Web（willdeep-rs）里点「导入背景音乐」没反应。** `episode.import_music` 只会用 `osascript` 弹 macOS 选文件框，Web 宿主下插件跑在服务器上，框弹在没人看的屏幕上。现在工具接受可选参数 `path`：带了就直接用这个文件，没带才弹框；扩展名（mp3/wav/m4a/aac/flac/aiff）与 200 MB 上限对两条路一样。willdeep-rs 0.84.0-rc1 起由宿主接管这条命令：浏览器选文件、上传，再把服务端路径作为 `path` 转交，调用结束后删掉上传件。macOS 宿主不带 `path`，行为不变。
- 测试：`scripts/episode_compose_test.rb` 新增一条：带 `path` 时不走对话框（`VIDEO_STUDIO_PICK_MUSIC` 指着另一份文件也不用它），类型不对、文件不存在照样被拒。

## [0.31.0-rc3] - 2026-09-27

### Fixed
- **带镜头调用 `video.generate` 时传的一句自然语言，被插到了错误的位置。** 服务端此前把它接在提示词里**第一个** `[Shot 1]` 后面，可这个位置在两种版式里都不是镜头描述：fl2va 开头的对齐句是 `<Picture 1> (from [Shot 1]) is fully referenced.`，句子被塞进了括号（2026-09-27 dryRun 实测为 `(from [Shot 1] …) is fully referenced.`）；ref2va 六段式里 `retention_analysis` 的 `(appears in [Shot 1])` 排在 `detailed_description` 前面，句子进了保留分析。页面每次提交视频都走这条路（带 `shotID`，`prompt` 取分镜的概要，没有概要时取运镜），所以此前页面发出的 fl2va / ref2va 任务，镜头概要都没进镜头描述。
  - 现在由 `H3Prompt.insert_into_shot` 接在镜头描述开头的 `[Shot 1]` 后面：三段式是 `integrated_multimodal_description: [Shot 1]`，六段式是 `detailed_description` 段行首的那个。t2va 以前碰巧放对了位置，这次位置不变。
  - 句子里的换行折成一行。页面一次提交多条时，第二条起的提示词带 `\n\n(variation N)`，这个空行原先会把对齐句、`retention_analysis`（t2va 则是 `integrated_multimodal_description`）从中间断开，多出一段不属于任何段名的文字。
  - 已含段名的整段提示词照旧原样发出；不传 `prompt` 时照旧发编译出来的整段。
- 设计稿（`docs/design/asset-continuity-system.md` 第 5 条）写明插入点，不再只写「插进 `[Shot 1]` 之后」。
- 测试：本地（UTF-8 locale）15 个 Ruby 测试脚本全部通过。`standalone_pipeline_test.rb` 25 项（新增 2 项：fl2va 断言 `integrated_multimodal_description: [Shot 1] 她缓缓抬头。` 且对齐句原样；ref2va 断言句子落在 `detailed_description` 行首的 `[Shot 1]` 后、`retention_analysis` 原样。原来的断言只查 `[Shot 1] 她缓缓抬头。`，对齐句里那处也能匹配，所以一直是绿的）；`asset_tools_test.rb` 28 项（在已有的两项里补断言：t2va / fl2va / ref2va 三种版式的插入点、换行折叠、空句不改、找不到镜头描述时报错）。两条新用例在旧实现上都失败，fl2va 那条复现了 `(from [Shot 1] 她缓缓抬头。)`。其余 `server_test.rb` 49、`drama_test.rb` 27、`tts_dashscope_test.rb` 18、`media_host_test.rb` 17、`agent_tools_test.rb` 14、`image_generate_test.rb` 14、`music_backend_test.rb` 13、`review_run_test.rb` 13、`episode_compose_test.rb` 12、`streamable_http_test.rb` 10、`download_recovery_test.rb` 5、`manifest_locales_test.rb` 4、`version_test.rb` 4 项不变；UI 12 个文件 271 项通过（第一次跑时机器负载 200 以上，`App.test.tsx` 有 3 项超过 15 秒超时，重跑全部通过）。`yarn --cwd ui build` 后 dist 与 0.31.0-rc2 相比只差内嵌版本号。

## [0.31.0-rc2] - 2026-09-27

### Fixed
- **装进 WillDeep 后整个插件加载不出来。** 0.31.0-rc1 在插件设置里新增了配音服务商（`ttsProvider`）和背景音乐服务（`musicProvider`），其中三个选项 `dashscope`、`none`、`acestep` 在 `.willdeep-plugin/locales` 里没有文案。WillDeep 的插件加载器把枚举设置的每个选项都当作本地化键，`en.json` 缺一个就拒绝加载整个插件，短剧工坊在 WillDeep 里会整个消失。插件自己的 CI 不走这个加载器，所以一直是绿的。现在中英两份 locale 都补上了：阿里云百炼（Qwen TTS）、不使用、ACE-Step（本机服务）。
- 测试：新增 `scripts/manifest_locales_test.rb`，规则与 WillDeep 加载器一致：导航入口、命令的 `titleKey`，设置的 `titleKey` / `descriptionKey`，以及枚举设置的每个选项，在 en、zh-Hans 两份 locale 里都必须有非空文案；另外检查枚举默认值在选项之内。CI 会自动跑这条测试，发版流水线也在打包前跑它。反验：换回 rc1 的 locale 后，这条测试准确报出两份文件各缺的 3 个键。

## [0.31.0-rc1] - 2026-09-27

### Added
- **分集成片：视频生成之后，按集配音、合成、拼接成一个文件。** 短剧内新增「成片」阶段，每集一键「合成本集」，在后台进程里跑（宿主对每个请求只等 10 秒，一集要几十秒到几分钟），页面显示步骤与进度、可取消，完成后按集播放、在访达中显示。设计稿 `docs/design/episode-compose.md`。
  - 每镜取**选定的视频**，没选时取该镜最新一条已完成的视频；缺视频的镜头会挡住合成并指出是哪一镜。
  - 配音来源可选：**单独配音**（默认）——台词用 TTS 补配（已有且文本没改过的直接用），按顺序排入（首句前与句间各留 0.3 秒），原声压到约 30% 当环境声，台词比画面长时定格最后一帧补足；**用视频生成的配音**——原声原样保留、不调 TTS。每集设默认，单镜可覆盖；用台词音轨生成的片段自动按「视频配音」处理，不会念两遍。
  - 各镜统一成这部剧画幅表规定的成片宽高（`drama.frame.video`，如 9:16 为 576x1024、升级前的旧 9:16 剧为 768x1152；读不到时取第一镜的宽高），尺寸不符的等比缩放加黑边、30 fps、H.264 + AAC 48 kHz 立体声，无音轨的片段补静音轨，再按镜头顺序无损拼接。
  - 成片写到 `<输出目录>/<剧名>/第NN集-<集标题>.mp4`，同时镜像进插件媒体目录供页面播放。
  - 新工具：`episode.compose_plan`、`episode.compose_status`（只读）、`episode.save_compose_settings`、`episode.compose`、`episode.compose_cancel`、`episode.generate_music`、`episode.import_music`、`episode.reveal_output`。
- **百炼原生 Qwen TTS。** 新增 DashScope 适配器（`qwen3-tts-flash`，音色填百炼音色名如 Cherry、Ethan、Dylan）：超过 600 字按句切开再拼，返回的 24 小时音频链接当场下载，语速用 ffmpeg `atempo` 处理。原有 OpenAI 兼容 `/v1/audio/speech` 保留作备选。插件设置新增配音服务商、地址、API Key、模型。
- **纯背景音乐。** 接本机 ACE-Step 1.5（MIT，可商用，纯音乐）：按提示词生成与本集时长相当的曲子；也可每集导入自己的音乐文件（mp3 / wav / m4a / aac / flac / aiff）。混音时循环或截断到整集长度，有台词 / 原声时自动压低，首尾淡入淡出。插件设置新增背景音乐服务与地址；部署说明见 `docs/standalone.md`。
- 测试：`scripts/episode_compose_test.rb`（ffmpeg 现场造片段，真跑后台合成：阻塞、选定视频、设置裁剪、导入音乐、补配音、定格补足、视频配音不调 TTS、取消）、`scripts/tts_dashscope_test.rb`、`scripts/music_backend_test.rb`；CI 与发版流水线安装 ffmpeg 并执行。

## [0.30.0-rc3] - 2026-09-27

### Fixed
- **首尾帧和成片不是同一个比例，9:16 的剧其实是 2:3。** 出图尺寸此前写死 1024x1536（2:3），9:16 剧的成片是 768x1152（也是 2:3），16:9 / 1:1 / 4:5 剧的首尾帧仍是竖 2:3，和成片对不上，上游只能裁切或拉伸首帧。现在首尾帧和场景图（合成首尾帧的参照）按剧的画幅出图，成片默认宽高取同一档：
  - 9:16：出图 1080x1920，成片 576x1024
  - 16:9：出图 1920x1080，成片 1024x576
  - 1:1：出图 1024x1024，成片 768x768
  - 4:5：出图 1024x1280，成片 768x960
  - 角色定妆、造型、道具不进成片，仍是 1024x1536。
- **直连出图（独立运行）时 nano-banana-2 永远出方图。** 它不认 `size`，只认 `aspect_ratio`。直连 OpenAI 兼容接口时，非 gpt-image / dall-e 家族的模型额外带 `aspect_ratio`（按 size 约分后贴到最近的一档，与 WillDeep 宿主的换算一致）；gpt-image / dall-e 家族仍只发 `size`。宿主代管那条不变，比例由宿主换算。

### Changed
- **旧剧保持原样。** 新建的剧写入 `aspectSpec: 2`，按真实比例出图出片；升级前建的 9:16 剧没有这个标记，继续按 2:3 出图（1024x1536）、出片（768x1152），已经画好的首尾帧和成片不受影响。只有在剧集元数据里把画幅换成另一档（页面或 `drama.save_metadata`）才会写入标记；页面保存时画幅和时长一起提交，旧剧只改时长不会被升级。旧剧确实要改成真 9:16，先换到别的画幅保存、再换回 9:16。
- **旧剧的 16:9 / 1:1 / 4:5 直接改用新尺寸。** 它们的首尾帧此前本来就是竖 2:3，这次顺带修正。其中 16:9 的成片默认尺寸从 832x480 变为 1024x576；独立调用 `video.generate`（不带 dramaID）仍用设置里的默认值，显式传 width / height 仍优先。
- **gpt-image-2 出不了 9:16、16:9、4:5。** 它只支持 1024x1024 / 1536x1024 / 1024x1536。这三档剧的首尾帧、场景图选了它时，`image.generate` 返回 `image_model_aspect_unsupported` 并说明该画幅能用哪些模型，不会静默换模型；1:1 和旧剧的 2:3 照常可用，角色定妆、造型、道具不受限。
- 页面：画幅下拉旁显示实际出图与成片尺寸，旧剧注明「按 2:3，与升级前一致」，下拉选了和存档不同的画幅时给出保存后的尺寸（旧剧下拉不动就不提示）；首尾帧与场景图的出图模型选择器把当前画幅出不了的模型置灰并说明原因，之前只选了它时自动落到可用模型上。用上一镜尾帧当首帧时，上一镜成片比例和本剧成片不一致会拒绝并说明。
- 剧读出时附只读字段 `frame`（`aspect`、`image`、`video`、`legacy`、`imageModels`），页面和 Agent 都按它，不各算一遍。画幅表 `schemas/aspect-ratios-v1.json` 由服务端（`server/lib/aspect_ratios.rb`）与页面（`ui/src/aspect-ratios.ts`）共用。`image.generate` 的 dryRun 与结果里的 `size` 是实际发出的尺寸。
- 测试：本地（UTF-8 locale）11 个 Ruby 测试脚本全部通过：`drama_test.rb` 27 项（新增 8 项：画幅表比例自洽、新剧 1080x1920 / 576x1024、旧剧 9:16 仍为 1024x1536 / 768x1152、旧 16:9 用新表、`frame` 不落盘、旧剧只改时长不升级且不写 `aspectSpec`、换画幅才写 `aspectSpec` 且换回 9:16 即真 9:16、显式宽高优先）、`image_generate_test.rb` 14 项（新增 4 项：dryRun 报实际尺寸、9:16 首尾帧拒绝 gpt-image-2 且不发请求、16:9 场景图 1920x1080 而道具仍 1024x1536、旧剧首尾帧 1024x1536 可用 gpt-image-2）、`standalone_pipeline_test.rb` 23 项（新增 4 项：直连对 nano-banana-2 带 `aspect_ratio`、对 gpt-image-2 不带、比例贴档换算、成片默认 576x1024），其余 `server_test.rb` 49、`asset_tools_test.rb` 28、`media_host_test.rb` 17、`agent_tools_test.rb` 14、`review_run_test.rb` 13、`streamable_http_test.rb` 10、`download_recovery_test.rb` 5、`version_test.rb` 4 项不变；UI 10 个文件 257 项通过（新增 `aspect-ratios.test.ts` 3 项，App 4 项、AssetLibrary 1 项覆盖模型置灰与尺寸展示）。`yarn --cwd ui build` 后 dist 随源码更新；基线提交重建得到原来的产物哈希，差异全部来自本次改动。

## [0.30.0-rc2] - 2026-09-27

### Fixed
- **本机 HTTP 入口补回并发和超时保护。** 这组保护原本在 `fix/streamable-http-stability`（cb8bd36，当时自编 0.29.0-rc2），WillDeep macOS 一直带的就是那一版插件，但 0.29.0-rc3 / 0.30.0-rc1 是从它分叉之前的提交长出来的，没有包含它。不补的话，WillDeep 升级到 0.30.0 系列反而会丢掉已经发出去的保护。现在 `StreamableHTTPServer` 最多同时服务 32 个连接，超出的连接排队等槽位；读请求限时 15 秒，挡住只连不发的连接；等待工具结果最多 900 秒，超时返回 504 `Video Studio request timed out.`，不再无限占住线程。
- cb8bd36 里另外两项（HTTP `initialize` 不覆盖宿主能力、`HostBridge` 缓存行不被主循环饿死）在 0.29.0-rc3 已换了一种实现（`transport: :stdio` 判定、读线程 + HTTP 工作线程），这次没有移植，只保留上面这组边界。
- 测试：本地（UTF-8 locale）11 个 Ruby 测试脚本全部通过，其中 `streamable_http_test.rb` 10 项、`server_test.rb` 49 项；UI 9 个文件 249 项通过；`yarn --cwd ui build` 后 dist 只差内嵌版本号，已随提交更新。新加的三道边界（32 并发、15 秒读、900 秒等待）没有专门的回归用例，行为与 WillDeep 现带的 cb8bd36 一致。

## [0.30.0-rc1] - 2026-09-26

### Added
- **外部 MCP 客户端不必再先点开短剧工坊。** 新增宿主侧「插件 MCP 网关」契约（`docs/decisions/0001-plugin-mcp-gateway.md`）：WillDeep macOS 1.405.0-rc1、willdeep-rs 0.83.0-rc1 起，宿主在 `127.0.0.1` 常驻一个网关，地址与 token 固定写进 `mcp-gateway.json`；第一条请求到达时宿主才经自己的 MCP 客户端拉起插件（反向请求出图、审核照常可用），再转发到插件的本机 HTTP 入口。插件进程被结束后，下一条请求会重新拉起。`docs/standalone.md` 改为推荐经网关连接。
- 首页「最近创作」卡片的封面和剧名、短剧列表里的剧名都能点，直接进入继续创作（此前只有「继续创作」按钮和列表封面能点）。

### Changed
- 插件侧栏去掉顶部的「短剧工坊」字样：宿主标题栏已经显示插件图标和名字，两处重复。回到短剧列表用侧栏的「短剧」。

## [0.29.0-rc3] - 2026-09-26

### Fixed
- **经本机 HTTP 入口接进来的客户端一 `initialize`，WillDeep 的代管出图和审核就全部失效。** HTTP 与宿主共用同一个进程，此前 HTTP 的 `initialize` 也走了宿主那条记录逻辑：第三方客户端（Claude Code、Codex……）不带 `io.willdeep/host-requests` 扩展，宿主宣告的反向请求被清空，之后页面和所有客户端的 `image.generate` / `review.run` 都返回 `host_image_unsupported`，直到宿主重启插件；客户端若自称 `willdeep`，媒体根还会被切到 Web 宿主。现在宿主能力与媒体根只认 stdio 那头的宿主，HTTP 的 `initialize` 只返回 serverInfo 和会话号。
- **经 HTTP 入口出图或审核时，打开插件页（比如生成队列的自动刷新）就会让 WillDeep 结束插件进程。** HTTP 请求此前在唯一的主循环里执行，一次出图要一两分钟，期间宿主发来的任何请求都得排队；宿主对每个请求只等 `startup_timeout_sec`（10 秒），等不到就 SIGTERM 插件、下一次请求再拉起，在途的 HTTP 调用随之断开。现在 HTTP 请求在独立的工作线程里逐个执行，主线程始终及时应答宿主；`HostBridge` 改成唯一的 stdin 读线程，反向请求的响应按 id 交回发起方（两个线程可以同时向宿主发请求），stdout 加锁写。
- 宿主重启插件时新旧进程会短暂并存，旧进程退出时把新进程刚写好的 `mcp-http.json` 一并删了：新进程开着 HTTP 端口，却没有任何客户端知道怎么连。现在退出时只删 token 仍是自己的那份。
- **Agent 只传 `shotID` 调 `video.generate` 时，成片一律是 4 秒。** 带镜头的提交没读镜头自己的时长，落到了全局默认值；页面一直自己传时长所以没暴露，经 MCP 驱动的 Agent 则把 8 秒的戏压成一半。现在调用方没给 `duration` 时按镜头时长提交（夹到 4–15 秒），显式传入的仍然优先。
- 测试：新增 `scripts/streamable_http_test.rb`（连接文件权限、Bearer 鉴权、HTTP 会话、HTTP `initialize` 后宿主出图仍可用、媒体根不被带偏、HTTP 调用等宿主期间宿主请求照常应答），并加入发版流水线；`support/fake_host.rb` 能挂起反向请求、按超时等回复；`standalone_pipeline_test.rb` 补「按镜头时长提交」。

## [0.29.0-rc2] - 2026-09-23

### Fixed
- **在 willdeep-rs 的 Web 宿主里（浏览器打开 `127.0.0.1:9847`）图片与成片一律不显示。** 页面里的媒体地址固定是 `willdeep-plugin://bundle/__media__/<文件名>`，这个自定义 scheme 只有 macOS 宿主认；浏览器页面既不认识它，页面 CSP 也只允许同源媒体，于是所有候选图、首尾帧、成片封面都是空白，成片也播不了。
  - 服务端：按 MCP `initialize` 的 `clientInfo.name` 认宿主（willdeep-rs 发 `willdeep`）。Web 宿主下媒体根切到 `<WILLDEEP_HOME 或 ~/.willdeep>/plugin-media/willdeep-video-studio/`，`mediaURL` 前缀改用同源的 `/plugin-media/willdeep-video-studio/`；读出候选与成片时按需把文件从 macOS 的 `generated-images/` 硬链接过去（跨卷退化成拷贝），`filePath` / `mediaPath` 也按当前媒体根重算——审核的 `imagePaths` / `videoPaths` 因此也能过 Web 宿主的路径钳制。存档仍然只落 `fileName`。
  - 页面：需要自己拼媒体地址的地方（镜头参考包等）统一经 `ui/src/media.ts` 解析，浏览器下换成本地 Web 宿主的同源地址，macOS 宿主下原样使用；服务端随剧一起下发的候选地址直接渲染。
  - 没认出来的客户端（第三方 MCP 客户端、独立运行）保持原行为；`VIDEO_STUDIO_HOST_MODE` 与 `VIDEO_STUDIO_HOST_MEDIA_ROOT` 可手动指定宿主与 Web 媒体目录。

## [0.29.0-rc1] - 2026-09-20

### Added
- 为 WillDeep 托管的同一个 Ruby MCP 进程增加可选的本机 Streamable HTTP 入口：请求排回原 MCP 主循环执行，继续复用宿主代管的视频、图片、TTS 和 API Key 能力，不启动第二个 Ruby 服务。
- HTTP 入口仅绑定 `127.0.0.1`，使用随机端口和 Bearer token，并在插件数据目录写入权限为 `0600` 的 `mcp-http.json` 连接信息。

### Changed
- `mcp.json` 仅对 WillDeep 托管启动显式开启 HTTP 入口；独立 stdio 测试和无此环境变量的运行方式保持兼容。

## [0.28.0-rc5] - 2026-09-19

### Fixed
- **「已完成」却没有成片文件的任务能救回来了。** 实际发生过：同一次刷新里两条任务完成，第一条的下载没落地（进程在下载途中被结束），台账留下「已完成、无文件、无错误」；此前轮询只看活动任务，页面上那条永远是一块「完成并下载后…」的空白，也没有任何按钮能处理。
  - 服务端：`video.refresh_active` 顺手补下载这类任务（自动下载开着时，一次最多 3 条）；`video.list` 报 `pendingDownloads`；下载阶段的非 Provider 异常也记成 `downloadError`，不再无声消失。
  - 直链过期：Hub 的成片直链 7 天有效，下载遇到 403 / 401 / 400 时重新查一次任务拿新链接再下，链接写回台账。
  - 页面：进生成队列时自动补下载这类任务（每条只试一次，自动下载关着时不动）；卡片上写明「生成已完成，但成片还没下载到本地」并给「下载成片」按钮；下载完成后照常做播放镜像。
- 生成队列的「回收站」筛选此前显示的是原始键名 `queueFilter_recycle`，补上中英文文案。
- 测试：新增 `scripts/download_recovery_test.rb`（恢复下载、直链过期换链、自动下载关闭、非 Provider 异常留痕），页面补两条用例。

## [0.28.0-rc4] - 2026-09-18

### Fixed
- 修复视频候选的“选用这条视频”按钮误继承竖向候选卡比例，恢复为紧凑的正常按钮高度。
- 视频候选预览区改为更紧凑的横向比例，减少页面纵向占用。

## [0.28.0-rc3] - 2026-09-18

### Fixed
- 修复视频候选“选用”调用了不存在的驼峰命令，改为实际注册的 `drama.select_video`。
- 视频候选的选用按钮移到播放器下方，不再遮挡画面或原生控制条。
- 首帧区新增“直接使用上一镜尾帧”，可把上一镜已选视频的最后一帧直接登记并选为当前首帧，不触发生图。

## [0.28.0-rc2] - 2026-09-17

### Fixed
- 视频候选的选中按钮避开原生播放器控制条，并用高亮边框和高亮按钮明确当前选中项。

## [0.28.0-rc1] - 2026-09-17

### Fixed
- 视频候选卡改为直接显示可播放的成片，不再只显示首帧封面；选择按钮与播放器控制分离。

## [0.28.0] - 2026-09-17

### Added
- 视频生成页支持查看并选定本镜的多个视频候选，也支持使用上一镜已选视频的最后一帧作为本镜首帧继续生成。
- 生成队列与导播中心支持将视频移入回收站；每部短剧新增视频回收站，可恢复误删任务。
- 完成视频播放镜像时同时抽取尾帧，供后续镜头连续性使用。

## [0.27.0-rc1] - 2026-09-17

### Fixed
- 生成队列与导播中心的视频标题现在带上该段时长，点击视频本体即可进入放大播放层。

## [0.27.0] - 2026-09-17

### Added
- 剧集元数据维护：支持画幅比例与每集目标时长，并让带短剧上下文的视频任务按画幅选择默认尺寸。
- 项目导播中心：集中列出当前短剧的全部视频任务，完成后可直接选段播放。
- 分镜时长规划：分镜页实时显示本集分镜总时长与目标时长差额，目标时长会带入 AI 分镜规划上下文。
- 插件侧栏底部显示当前版本号。

## [0.26.0-rc1] - 2026-09-17

### Fixed
- 带 `dramaID` 的视频任务按短剧画幅生成：竖屏短剧默认使用 `768x1152`，横屏短剧保留 `832x480`；显式传入宽高时仍以显式值为准，避免竖版生图后生成横版视频。

## [0.26.0] - 2026-09-17

### Added
- **资产连续性系统（设计稿 `docs/design/asset-continuity-system.md`）。** 造型、场景、场景变体、道具、声音成为可复用资产，存在剧的 `assets` 里，与角色同一把锁、同一套 rev / 草稿 / 历史；图片与音频走候选 + 选定；被镜头引用的资产只能归档不能删。
  - 镜头参考包：每一镜绑定场景（含变体）、出场角色（各带造型、声音、主次、站位、换装说明）与道具，只存引用；提交时由服务端展开成有顺序的参考槽位，按用途（出图 9 张 / 视频看 Provider 能力）裁到上限并报出被裁掉的项，写进视频任务的 `referenceSnapshot`。参考包编译只有服务端一份（`server/lib/reference_package.rb`），页面调只读预览工具。
  - 出图：`image.generate` 新增 `appearance` / `scene` / `sceneVariant` / `prop` 目标；首尾帧改按参考包带参照（身份图、造型图、场景图、道具图）；`dryRun` 先看提示词与参照再花钱；`requestID` 去重。
  - 配音：新工具 `voice.generate`（试听或整镜对白），音频挂在台词上并记时长与台词指纹；声音资产的授权未确认一律拒绝生成。
  - 进度：`drama.get_progress` 报告资产状态、按 `shotID` 关联视频（旧任务仍按首帧路径反查）、每镜的连续性检查（引用丢失或已归档、没选图、造型突变没说明、声音未授权、音频过期或超过镜头时长、参考超限），并把能修的排成步骤；`maxSteps` 可放宽 12 步的上限。
  - 材料：`drama.get_stage_context` 新增 `appearance` / `scene` / `prop` / `voice` / `package` 阶段，共用新技能 `skills/creative/assets.md`。
  - 交接：`drama.export_manifest` 按集按镜给出选定帧、成片、对白音频、参考快照、审核状态与合成声音标记，绝对路径可直接喂给剪辑工具。
- **Ref2VA 按 Tsingfly Hub 的真实形状接入（教程 2026-09-17 核实）。** Hub 的 `ref2va` 只收两种互斥组合：1~3 段参考视频（重复的 `input_references`），或恰好一张图加一段音频（`input_reference` 加 `audio_reference` data URL，原始音频约 750 KB 以内）。参考包在视频侧据此编译：绑定了 `videoRefs` 走组合一，有首帧且本镜台词配了音走组合二（多句对白按同格式 WAV 拼成一条音轨，中间留 300 毫秒静音，纯 Ruby 实现 `server/lib/wav_tools.rb`），否则回落到 fl2va / t2va；`mode=ref2va` 而材料不够时报 `mode_unsupported` 并说明缺什么。
  - 提示词改为 H3 的结构化写法（`server/lib/h3_prompt.rb`）：t2va / fl2va 三段式（fl2va 带 `<Picture 1>` 对齐声明），ref2va 六段式，台词写成 `(S1) <d>[Chinese]…</d>`，说话人 ID 按角色顺序固定，`retention_analysis` 用固定标记。`video.generate` 带镜头时默认发它；传一句自然语言会插进 `[Shot 1]`，传已含段名的整段则原样发出。dryRun 与预览工具都能看到全文。
  - 分镜新增 `soundscape`（环境与动作音）与 `music`（画外配乐）两个文字字段，对应提示词后两段；空则用默认句与 `N/A`。页面分镜表单可编辑。
  - 参考包新增 `videoRefs`（最多 3 条已完成任务）与 `audioRetention`（`fully_copy` 照搬对白音轨、`reference` 只参考音色）。
  - 请求成对带 `flow_shift=12` 与 `extra_params.audio_flow_shift=3`（此前只带后者，教程说明只改单边会破坏音画同步）。时长合同区间改为 4~15 秒（此前允许 2 秒，实际不受支持）。
- **工具全在插件 MCP 里，任何 harness 都能跑。** 出图与 TTS 后端均支持宿主代管，宿主能力不存在时回落到环境变量直连（OpenAI 兼容 images / speech 接口）；审核新增只读的 `review.get_material`，调用方自己判后用 `record_review` 写回，`review.run` 退为 WillDeep 内的便捷封装；`media.read` 以 MCP image / audio 内容块返回候选，看图不依赖 harness；`video.capabilities` 如实声明 t2va / fl2va 与参照上限；`VIDEO_STUDIO_DATA_DIR` 一个变量指定数据目录。Claude Code 与 Codex 的配置见 `docs/standalone.md`。
- 页面：侧栏「资产库」启用（造型 / 场景 / 道具 / 声音的建档、出图、选定、归档、审核、授权与试听）；首尾帧页与视频页新增参考包面板，实时显示本镜将发送的参考、被裁掉的项与解析出的模式。
- 测试：`scripts/asset_tools_test.rb`（资产、参考包、进度、材料、导出、配音、看图、审核材料）、`scripts/standalone_pipeline_test.rb`（不带任何宿主扩展、只靠本地假网关从策划跑到导出清单）、`scripts/version_test.rb`（清单、前端包、serverInfo、CHANGELOG、产品概览版本一致）；审核对照夹具加入资产目标；新增 `ARCHITECTURE.md`。

### Changed
- 候选图与音频只持久化 `fileName`；`filePath` / `mediaURL` 在服务端读出时派生（`server/lib/media_ref.rb`），旧候选的绝对路径照常认。数据目录搬家或换宿主不再让存档里的路径失效。
- `video.generate` 接受 `dramaID` / `episodeID` / `shotID` / `mode` / `dryRun` / `requestID`：带镜头时按参考包解析模式、默认用选定首帧与编译出的 PromptIR。页面提交视频时随带镜头 ID。
- 分镜台词改文字时，同一条台词上的音频候选保留；台词变了由进度报「音频过期」。
- `drama.save_draft` / `commit_draft` / `discard_draft` / `list_history` / `restore_version` / `record_review` / `review.run` 的 scope 新增 `asset`（`assetID`）。
- `serverInfo.version` 改从 `SERVER_VERSION` 常量读，与插件版本一致（此前停在 0.24.0）。
- `skills/video-studio/SKILL.md` 改写为与宿主无关：流水线加入资产、参考包、配音、导出与通用审核路径。

### MCP 契约
- 新增工具：`drama.list_assets`、`drama.get_asset`、`drama.save_asset`、`drama.archive_asset`、`drama.record_asset_media`、`drama.select_asset_media`、`drama.set_reference_package`、`drama.preview_reference_package`、`drama.select_dialogue_audio`、`drama.export_manifest`、`voice.generate`、`video.capabilities`、`review.get_material`、`media.read`。只读工具带 `readOnlyHint`。
- 兼容性变更（不破坏旧调用）：`image.generate` 的 `references` 摘要每项多了 `semanticType` / `fileName`；`drama.record_image` / `record_character_image` 的 `mediaURL` 不再必填；工具结果可能带 image / audio 内容块（`media.read`），text 块仍在最后；`video.capabilities` 新增 `durationRange` 与 `ref2va` 子对象；`drama.preview_reference_package` / `video.generate dryRun` 新增 `combo`、`audioTrack`、`videoRefs`。
- 收紧：`video.generate` / `video.settings` 的 `duration` 从枚举 2 / 4 / 6 改为 4~15 的整数，2 秒会被拒绝（Provider 本就不支持）。

## [0.25.0] - 2026-09-17

### Changed
- **全局界面视觉升级。** 统一冷灰画布、深色导航、紫靛强调色、卡片层级、12/16px 圆角与柔和阴影；重排首页、策划、角色、剧本、分镜、队列和设置页的内外边距与控件高度。
- 修复窄窗口下标题、按钮、标签和长文本互相挤压的问题，补齐 `min-width: 0`、可换行、单列退化和移动端导航布局，减少横向溢出与异常换行。

## [0.24.0] - 2026-09-17

### Changed
- **「思考过程」收起时也能看出模型还在动。** 策划、角色、剧本、分镜四处的思考折叠框，此前收起时只有一个「思考过程」标题，推理模型正文开始前往往要想几十秒，用户分不清是在想还是卡住了。现在标题行后面实时显示最新一行思考；放不下时截掉开头、保留结尾，因为最新的字在行尾。点开看完整思考，并停在最新处，此时标题行的预览收起。
  - 思考文本从只留最后 600 字改为保留完整内容（上限 2 万字），点开才看得全。
  - 预览行设了 `width: 0`：不换行的整句会把最小内容宽度一路传给聊天面板，实测把页面撑出横向滚动条。

## [0.23.0] - 2026-09-17

### Added
- **`review.run`：主 Agent 在聊天里也能跑内容合规预审。** 此前审核只在插件页面里自动触发，Agent 替用户出完图、出完片没人审——插件的 MCP 进程没有模型凭据。
  - 走宿主反向请求 `willdeep/ai/complete`（WillDeep 1.380.0-rc1 起），用设置页选定的审核模型（没选就跟随策划模型），与页面同一份审核技能、合规红线和输出契约。宿主没宣告这个方法时返回 `host_review_unsupported`，不发请求。
  - 审什么与页面一致：策划、角色设定、定妆图（附图）、分集剧本、分镜、首尾帧（附图）、成片（附视频，宿主抽帧）。结论经同一个 `record_review` 写回对象，页面上看到的就是这一份。
  - 解析同页面：先认 ```short_drama_review``` 围栏再找配平对象，校验不过报 `review_invalid` 并带回原文片段、不落库；列了 block 级问题一律按「不通过」算。
- **审核材料与指纹在服务端逐字复刻页面**（`server/lib/review_material.rb`）。结论里存的指纹是页面判断「待复审」的依据，两边差一个字，Agent 审过的对象在页面上就显示过期。指纹按 UTF-16 码元算、trim 用 JS 的空白定义；标签文字抽成 `schemas/review-labels.json` 两边共用。`scripts/fixtures/review-parity-*.json` 是一份刻意带全角空格、emoji、variation 后缀、乱序镜头的输入与期望值，TypeScript 与 Ruby 各自对它断言。
- **`drama.get_progress` 报告审核状态**：每个对象每一面是「没审 / 待复审 / pass / warn / block」，与页面同一口径。不通过的排在最前；已产出却没审或已过期的排在生产步骤之前（空壳不排）；成片的审核状态挂在对应镜头的视频上。
- 测试：`scripts/review_run_test.rb`（13 项：一致性、请求内容与模型路由、附图附视频、结论落库、解析失败不落库、宿主报错、英文页面）；`ui/src/review-labels.test.ts`（5 项）。假宿主抽到 `scripts/support/fake_host.rb`，出图与审核测试共用。

### Changed
- 页面把审核模型、策划模型和界面语言同步进插件设置（`video.settings` 新增 `reviewProviderID`、`reviewModel`、`uiLocale`）。页面的这些选择存在宿主给页面的存储里，MCP 进程读不到；不同步的话，`review.run` 用的模型和拼材料的语言会与页面不同。存储读回之前不同步，免得挂载时的空值冲掉服务端设置。

## [0.22.0] - 2026-09-17

### Added
- **`drama.get_progress`：Agent 一次调用就知道这部剧做到哪、下一步做什么。** 此前只能反复 `drama.get` 读回整部剧自己推断，一部十集的剧几十 KB，读几次上下文就满了，还容易漏掉某一镜没选首帧。
  - 返回每个角色（有没有设计、几张定妆图、是否锁定形象）、每集（梗概、正文、草稿）、每镜（首帧提示词、候选、选定、视频任务状态）的完成情况与审核结论，外加一份汇总计数。
  - `nextSteps` 按生产顺序排好：审核「不通过」的先处理 → 角色设计 → 定妆图 → 选定形象 → 分集正文 → 拆分镜 → 首帧提示词 → 首帧图 → 选首帧 → 提交视频 / 刷新 / 处理失败。每步给出原因、该调的工具和目标 ID，一次最多 12 步并报告还剩几步。
  - 只认**已采用**的内容：草稿随时会被丢，按草稿算完成会让 Agent 在不作数的文字上花钱出图。
  - 视频任务不记是哪一镜的，按「提交时的参考图就是该镜选定首帧」对上号，同一张首帧取最新一次提交。
- **`drama.get_stage_context`：把页面喂给模型的创作材料交给 Agent。** 返回 `system`（阶段技能 + 内容合规红线 + 可选题材技能）、`context`（与页面同一套装配：剧名题材基调、梗概、弧光、角色表、当前对象、相邻集或本集分镜）、`target`（正式内容与未采用草稿分开给，带 rev / draftRev）和 `write`（该用哪个工具、能写哪些字段）。支持 planning、characters、episodes、script、storyboard、shot、frames 七个阶段。Agent 不必自己拼材料，也就不会漏掉合规红线。
- `scripts/agent_tools_test.rb`：覆盖进度推进的每一步、草稿不算完成、审核拦截优先、各阶段材料与只读注解。

### Changed
- **只读工具在 `tools/list` 里带上 MCP 标准注解 `annotations.readOnlyHint: true`**（`drama.list / get / get_progress / get_stage_context / list_history / read_version / list_shots`、`skill.list / get / compose`、`chat.list`、`video.status / list`）。WillDeep 1.379.0-rc1 起据此在「标准信任」下免确认；会改存档的工具一律不标。

## [0.21.0] - 2026-09-17

### Added
- **主 Agent 能在聊天里替你出图：新 MCP 工具 `image.generate`。** 起因是让 WillDeep 主 Agent 跑完整条短剧流水线，而出图此前只有插件页面能做——Agent 调的是插件的 MCP 服务进程，那里没有页面桥，也不该另配一份出图 key。
  - 走宿主反向请求 `willdeep/images/generate`（WillDeep 1.378.0-rc1 起）：工具调用中途请宿主用自己的 some.im 凭据出图，权限、参考图路径钳制与页面出图同一份实现。宿主在 initialize 里没宣告这项能力时直接返回 `host_image_unsupported`，不发请求干等到超时。
  - `target: character` 用已采用的 `visualPrompt` 出定妆候选；`target: start / end` 用已采用的首尾帧提示词出帧候选，并带上本镜提到的、已锁定形象图的角色作参照（也可用 `castIDs` 指定）。规矩与页面一致：分镜有未采用草稿时拒绝，多张之间靠 `(variation N)` 区分，模型交错排、相邻请求间隔 1 秒。
  - 生成的图直接记为候选并返回候选 ID，Agent 接着用 `drama.select_character_image` / `drama.select_image` 挑定。
  - **一次最多 4 张**（模型数 × 每模型 1/2/4 张，默认 `nano-banana-2` 每模型 2 张）：工具调用是同步的，宿主等这次调用期间不接这条连接上的别的请求，不封顶的话一次调用能卡十几分钟。超限在花钱之前就拒绝。
  - 出图失败原样回给 Agent；图已生成却记不进剧本时把文件路径一并交回，钱不白花。
- `scripts/image_generate_test.rb`：起真实服务进程、脚本扮演宿主，覆盖旧宿主报错、超限、定妆与首尾帧参照、宿主报错、草稿拒绝，以及等宿主回复期间插进来的请求不会丢。

### Changed
- MCP 服务主循环从 `ARGF.each_line` 改为经 `HostBridge` 读 stdin：等宿主回复期间读到的其它行先缓存，主循环接着处理。`video-studio` 服务版本随之标为 0.21.0。
- `skills/video-studio/SKILL.md` 补上 `image.generate` 的用法、张数与计费提示、各错误码的处理。

## [0.20.0] - 2026-09-16

### Changed
- **AI 出稿格式正确就直接采用，不用再点「采用草稿」。** 角色共创、「AI 出第一稿 / 批量出稿」、单集写作、改选中段落、「AI 串起全部 N 集」、分镜共创和首尾帧描述共创都改成这样：结果通过格式校验后，服务端在**同一次写入**里把它并进正式内容，页面随即弹一条「AI 这一版已自动采用」。
  - 刻意不是前端「先 saveDraft 再 commitDraft」两跳：中间隔一次往返，期间一次手改就会把 rev 顶掉，结果又卡回草稿里等人确认。`drama.save_draft`、`drama.save_episode_drafts`、`drama.save_shot_drafts` 新增 `commit` 参数。
  - 批量写分集时，只写了梗概、正文仍为空的集**不自动采用**，留在草稿里并提示集号——那种采用等于把这一集唯一一份内容清掉，理由同「采用 N 集草稿」。
  - 手动编辑失焦保存仍然先进草稿，「采用草稿 / 丢弃草稿」按钮保留给手改用。
  - 生成期间你改过草稿时，结果照旧先留着等复核，点「采用这份结果」同样直接采用。

### Added
- **历史记录。** 角色、分集剧本、分镜三处多了「历史记录」按钮：列出这个对象之前的版本（时间、每个字段的摘要和字数），可以展开全文，也可以「采用这一版」。
  - 每次覆盖正式内容之前（AI 自动采用、手动采用草稿、保存角色/本集/分镜、采用历史版本）都会先留一版，所以回滚本身也能再滚回来。全空的版本不记。
  - 存档一部剧一个文件（插件数据目录 `history/<剧 ID>.json`），不混进 `dramas.json`：分集正文一条两万字，几十版堆进主存档会拖慢每一次全量覆写。每个对象保留 20 版，一部剧最多 600 条。
  - 新增 `drama.list_history`（只回摘要）、`drama.read_version`（全文）、`drama.restore_version`。采用历史版本会清掉未采用的草稿，页面在有草稿时先确认。
  - 历史写入失败不回滚主写入，只在 stderr 记一行：正式内容已经落盘，这时报失败会让人以为 AI 那一版没存上而重复生成。
- **角色列表带进度徽标。** 每张角色卡上标出两件事：设定到哪一步（已设计 / 设定不全 / 待设计），形象图到哪一步（已定妆 / N 张待选 / 待出图），有未采用的草稿时再加一枚「有草稿」。一眼能看出还有哪几个角色要补。
  - 按正式内容判，不看草稿：草稿里写满了但没采用，不算已设计。
  - 没有视觉提示词一律算「待设计」，哪怕小传里有一句定位——策划带出来的角色骨架就是这样，离能出图还差整份外形设定；与「给空角色批量出稿」的待办范围一致。
  - 已选中的候选图被删掉时不算已定妆。
- **内容合规：设计时就带上红线，完成后自动审核。**
  - 新增技能「内容合规红线」（`skills/creative/compliance.md`）：政治与国家、民族宗教、违法犯罪、色情低俗、暴力血腥、未成年人、封建迷信、价值导向、侵权与真实人物、广告与医疗十类底线，以及各阶段怎么落实。它**不由用户勾选**，服务端拼进策划、角色、分集、分镜、首尾帧每一个阶段的系统提示词（放在阶段技能之后、题材技能之前）。设置页可编辑、可恢复内置版本。
  - 新增审核技能「内容审核」（`skills/creative/review.md`）：按红线逐条审，结论为通过 / 有风险 / 不通过，每条问题带类别、位置、原文摘录、原因和修改建议。审核只叠红线、不叠言情穿越这类题材技能，免得审核员被「拉扯要到位」带偏。
  - 自动审核的节点：策划确认落库后、角色 AI 定稿后、单集写作 / 改段落 / 批量写集之后（批量写完逐集审，最后汇总一条）、分镜生成后（按整集审）、角色定妆图和首尾帧出图之后。
  - 结论存在对象上（新增 `drama.record_review`，不动 rev、不进历史），带内容指纹：内容之后被改过，徽标变成「内容已改，待复审」。顶栏（整部策划）、角色编辑区与定妆图、分集剧本、分镜、首尾帧各有徽标与问题清单，可手动重审；角色列表上审出问题的角色也会挂一枚。
  - 模型说「通过」却列了「不通过」级问题时按问题算，前后端同一条规则；结论格式不对就不记录，不猜一个通过。
  - 提交视频生成前，本集剧本、本集分镜、本镜出图任一审核「不通过」时要二次确认。刻意不硬锁：这是 AI 预审，会误判。
  - 设置页新增「内容审核模型」，可以单独选审核用的服务商和模型（例如 qwen3.8-max 这类多模态模型），不选则跟随策划模型。
  - **画面本身送审（需要 WillDeep 1.377.0-rc1 / willdeep-rs 0.74.0-rc1）**：宿主桥 2.6.0 起 `ai.complete` 的消息可以带插件生成目录里的图片和视频路径。宿主声明 `ai.images` 时，定妆图和首尾帧审核会把候选图一并送审（选定的在前，每次最多 12 张）；声明 `ai.videos` 时，生成队列里的成片在做好播放镜像后自动送审（宿主抽 8 帧），结论记在任务上，队列卡片上有徽标与意见。
  - 旧宿主上自动降级：出图审核按提示词进行，成片审核不可用，界面上分别写明原因；每条结论也注明「看过 N 张图片、N 个视频」还是「只审了文字」。
  - 选的审核模型不能识图时，宿主当场拒绝，页面提示去设置里换多模态模型。
  - 服务端：新增 `video.record_review`；成片任务带上镜像文件的绝对路径 `mediaPath`（旧任务按当前媒体目录补齐）；两类审核结论共用 `lib/review_record.rb` 做校验与裁剪。
  - 审核结论是 AI 预审，不代表平台或主管部门的审核结论，界面上同样写明。
- **出图模型可多选，张数按「每个模型」算。** 角色定妆和首尾帧的模型从下拉改成可多选的开关，张数下拉改为「每个模型 N 张」，下方实时提示「共出 N 张：M 个模型 × 每个 K 张」，生成按钮上的张数也是总数。至少保留一个模型；选择记在宿主存储里。多个模型的请求交错排队，一家上游出问题时另一家的图也能先出来。
- `creative-v1` schema 新增 `review` 定义，并在前后端两份校验器里补上 `enum` 关键字。

### Fixed
- 「给空角色批量出稿」写草稿时用的是页面当前选中的角色 ID，而不是循环里正在出稿的那个角色，结果要么写错人、要么撞上版本冲突被挂起。现在按目标角色写。
- 角色编辑框此前只在切换角色时同步，AI 写入或回滚后输入框停在旧内容上。现在按修订号同步，且只覆盖用户没动过的字段，失焦保存的回包不会吃掉另一个框里正在敲的字。

## [0.19.0] - 2026-09-14

### Added
- **生成队列页现在能直接播成片。** 此前完成的任务只给一行落盘路径和一颗「在访达中显示」——要看一眼刚生成的四秒钟，得跳出插件、开访达、再等播放器起来。现在完成的任务就地播：默认显示首帧，原生控件带拖进度和音量，「放大播放」还能把它铺到整屏（Esc 或点背景关掉）。访达入口原样保留。
  - 页面加载不到成片是有原因的，不是没做：插件页跑在 `willdeep-plugin://` 下，CSP 是 `default-src 'self' data: blob:; connect-src 'none'`，`file://` 和 `http://127.0.0.1` 都被宿主明确挡掉（"Loopback and private hosts are never reachable from a plugin page"）。宿主唯一放行的本地媒体入口是 `willdeep-plugin://bundle/__media__/`，只指向插件数据目录下的 `generated-images/`，而成片落在用户自己的输出目录。
  - 所以新增 `video.prepare_playback`：在那个目录里给成片建一个**硬链接**——同一份数据两个目录项，不多占磁盘；只有跨卷时才退化成拷贝。页面渲染完成的任务时按需调用，一条只做一次。
  - 宿主万一不放行 mp4，`<video>` 的 error 事件会把这条任务退回「封面 + 在访达中显示」，并写明原因，而不是留一个点不动的黑框。
- **默认显示成片首帧。** 服务端在做镜像时顺手抽一张首帧 PNG：有 ffmpeg 用 ffmpeg（拿到的是真正的第一帧），没有就用系统自带的 `qlmanage`。两条都不通时也不报错——播放器读到元数据后自己会显示第一帧，封面只是更稳的那一层。
- **进行中的任务自动刷新状态。** 0.17.0 刻意不做轮询，代价是一个早就跑完的任务会在页面上停在「生成中」，只能靠用户自己去点刷新。现在改成有约束的自动刷新：**只在**队列页打开、窗口可见、确实有活动任务、且用户没关开关时，每 8 秒问一次上游；任务全部跑完轮询自己停。开关在页头，关掉的选择会被记住，页面会写明「状态停在上次刷新那一刻」。
  - 自动那一路不弹通知：挂着页面十分钟会攒出一屏。失败只写在页头的状态行里。
  - 窗口不可见时只把下一次排上、不发请求——"用户没看着的时候不替他打上游"这条没有变。

### Fixed
- **首页的封面位不再一律「暂无封面」。** `coverURL` 从建剧那一刻起就被写成 `nil`，而插件里**没有任何一条路径会写它**——于是每一部真实短剧在首页都是灰底四个字，只有开发用的演示数据带图。现在先从这部剧已经有的图里挑一张：已选定的角色定妆照 → 任一角色候选 → 选定的分镜首帧 → 任一首帧候选；一张图都还没生成时才写「暂无封面」，那时候它是实话。
  - 旧存档里 `candidates` / `shots` 这些数组可能整个缺席，挑封面时一律按空处理，不会因为一部老剧把首页打崩。

### Changed
- 生成队列页重做：一条任务一张卡，左边是成片（能播就地播），右边是状态、进度、提示词和落盘路径；筛选标签带计数，页头有一条状态行说明自动刷新在不在跑、上次问上游是什么时候，进行中时那个圆点会呼吸。失败和完成的卡片左侧有颜色标识，窄窗口下自动改成竖排。

## [0.18.0] - 2026-09-13

### Fixed
- **提交视频任务失败时，界面报的是成功。** 用户点「提交视频生成」，看到绿色的「已提交 N 个视频任务」，按钮立刻回弹，然后什么也没发生——队列里只有失败记录，成片目录空着。实测在一台机器上这样静默吞掉了 9 次提交，跨两天，错误全是同一条 `Video API key is not configured.`，请求根本没发出去。
  - 根因是判据错了：服务端把失败写成 `{ok: false, error}` 这样一条**正常回执**返回，不抛异常。代码用 `Promise.allSettled` 只数 `status === 'fulfilled'`，在它眼里被拒绝的提交也是成功的。现在逐条看回执里的 `ok`，失败的计入失败并把服务端的原话显示出来。
  - 同一类判据错误在图片那条路上还有两处：`drama.recordImage` 和 `drama.recordCharacterImage` 的回执同样没被检查。图已经生成、已经计费，写不进剧本时用户只会看到「已生成 N 张」，然后候选区空着。
  - 回归测试钉住：服务端回 `ok:false` 时必须显示服务端的错误原文，且**不能**出现「已提交」。

## [0.17.0] - 2026-09-13

### Added
- 「生成队列」页。这个入口在侧边栏摆了很久却一直写死禁用，背后根本没有页面——`Stage` 类型里就没有 `queue` 这个值，点不动，也无从知道提交的视频任务跑到哪一步。数据源本来就现成：`video.list` 能按全部 / 进行中 / 已完成 / 失败 / 草稿过滤，`video.refresh_active` 能拉一次上游状态，缺的只是列表页。
  - 每条任务展示状态、进度、提示词、错误原文和成片落盘路径；完成的可「在访达中显示」，失败的可重试。
  - **刻意不做自动轮询。** 进页面只读一次本地台账，只有按下刷新才去问上游——轮询等于在用户没看着的时候替他持续发请求。代价是状态天然滞后，所以页面上直接写了「进行中的状态是上次刷新时的快照」，免得早就跑完的任务被当成卡死。
- 首尾帧的「生成 N 张候选」按钮被禁用时，直接在按钮下面写出原因（草稿未采用 / 另一项生成在跑 / 未选分镜）。此前按钮只是灰掉，而解释藏在点击才触发的提示里，点不动的按钮永远触发不了它——用户看到的是「生成过一次就再也点不动了」，以为不能重新生成。实际上重新生成一直允许，新候选追加在后面，不覆盖已选定的那张。

### Notes
- 「角色资产」入口仍然禁用，它确实还没有页面。

## [0.16.0] - 2026-09-11

### Added
- 「分集剧本」页头多一颗「采用 N 集草稿」：一次把全季待采用的分集草稿写进正式内容，服务端一把锁里走完（新增 `drama.commit_episode_drafts`）。逐集点「采用草稿」在 24 集的工程里是 24 次切集加 24 次点击，而每一次都是一轮「读全量、改、全量覆写」。按钮只在真有草稿时出现，标签自带集数。
  - **正文会变空的集不采用。** 草稿里没有 `script`、这一集的正式正文也还是空的时候，采用下去的效果是正式内容照样没有正文，而那份草稿被一并清掉，用户手上什么都不剩。这几集连草稿一起留在原地，集号在回执里报出来（「第 1、7 集没有采用：正文还是空的，草稿给你留着了」）。只报一个成功计数就又变成「看起来全好了」。
  - 判据是「采用后正文空不空」，不是「这次草稿里有没有 script」：正式正文已经在的集，只改梗概的草稿照样能采用。
  - 没有草稿的集既不动也不报——24 集里 23 集没草稿是常态，逐条报出来只会把真正要看的那几行埋掉。

## [0.15.3] - 2026-09-11

### Fixed
- AI 写完一集，不换集也能在「完整剧本」里看见了。分集编辑缓冲此前只按 `episode.id` 同步：草稿写进去以后 `draftRev` 变了、`id` 没变，左边三个输入框一个字不动，看起来像「只有当前这一集写不进去」，切到别的集再切回来才出来。缓冲的依赖补上 `rev` 与 `draftRev`（分镜那条缓冲一开始就是这么写的）。
  - 比看不见更糟的是会丢：空着的正文框被碰一下再失焦，`onBlur` 判定「用户改了正文」成立，把空字符串当成编辑存进草稿，刚写好的正文当场覆盖；再点「保存本集」，草稿也一起没了。「AI 串起全部 N 集」写到当前选中那一集时同一个毛病。
  - 换集收起差异面板拆成单独一条 effect：否则生成刚打开的差异面板会被这次同步立刻关掉。

## [0.15.2] - 2026-09-10

### Fixed
- 表单字段名与输入框之间的空隙不再由右侧共创栏的长度决定：`label` 与编辑卡补 `align-content: start`，`.script-grid` 补 `align-items: start`。

## [0.15.1] - 2026-09-10

### Fixed
- 「AI 串起全部 N 集」不再丢正文：批量解析器只挑 order、title、summary，模型给出的 script 被静默扔掉，一轮跑完「完整剧本」仍是空的。

### Changed
- 批量请求标出还没有正文的集，系统提示词要求为这些集给出 script；已有正文的集不重发。

## [0.15.0] - 2026-09-09

### Added
- 从剧本生成第一批分镜草稿，提供草稿预览、差异、采用与丢弃。
- 首尾帧独立聊天与描述草稿，生成前由用户采用；跨轮对话保存在当前短剧中。
- 前后端共享版本化创作 Schema，批量修订检查防止覆盖较新的草稿。

### Fixed
- 角色长简介和聊天空白区挤压布局，模型选择器在窄窗口中折行。

## [0.14.1] - 2026-09-09

### Fixed
- 首稿请求明确要求 script 字段,「AI 写这一集」不再只回梗概、正文留空。
- 模型只回梗概时明确提示正文没写;梗概仍存进草稿。

## [0.14.0] - 2026-09-09

### Changed
- 分集选择改成「十集一段的下拉 + 标题平铺」，一行三集，点标题换集；挑段只换列表。

## [0.13.1] - 2026-09-09

### Fixed
- 放大图层靠上放、高度按视口收缩,不再被视口下沿切掉。
- 模型选择器贴到各面板的发送按钮上方,串起全季那颗按钮旁边也补了一组。

## [0.13.0] - 2026-09-09

### Added
- 角色定妆照头像可点开放大查看，背景、X、Esc 三种方式关闭。

### Changed
- 角色行的头像与选角拆成两个按钮，放大头像不再切换选中的角色。


## [0.12.0] - 2026-09-09

### Added
- 角色、剧本、分镜三个共创面板都能各自选 provider 和 model，不必回设置页改全局默认。
- 生成过程中显示模型的思考过程（可折叠），长任务不再只有一个转圈。
- 设置页新增创作技能维护：查看九份技能的正文、改写、以及显式恢复内置版本。技能是全局一份，改完下次生成即生效。

## [0.11.0] - 2026-09-09

### Added
- 分镜聊天微调：只改一镜或整集微调两种粒度，按镜号对齐写入。
- `drama.list_shots` 给模型的裁剪清单（不带候选图数组）与 `drama.save_shot_drafts` 批量草稿写入。

### Fixed
- 围栏匹配加边界，避免单镜围栏名认下整集围栏（前缀冲突）。
- 空分镜的提示改为「还没有分镜」；台词空数组按合法意图处理。

## [0.10.0] - 2026-09-09

### Added
- 「AI 串起全部 N 集」：整季分批丰富，默认十集一批，串行并显示进度，每批带相邻批次梗概做衔接。
- `drama.save_episode_drafts`：一次落一批草稿的原子写入，按集号对齐，找不到的集号报出来而不是静默丢。
- 一批格式坏了只跳过那一批，其余继续；单次批量上限三十集。

## [0.9.0] - 2026-09-09

### Added
- 各阶段的系统提示词搬到可维护的创作技能 markdown：策划、角色设定、多集剧情串联、单集剧本、局部改写、分镜六份阶段技能，加去 AI 味、言情、穿越与重生三份可勾选的通用技能。
- 技能存储分两层：插件包里那份是只读模板，用户编辑的副本落在插件数据目录，首次读取时复制过去。模板更新不会覆盖改过的副本，界面提供恢复内置版本。技能全局一份，改了对所有短剧生效。
- 新增 `skill.list` / `skill.get` / `skill.save` / `skill.reset` / `skill.compose` 五个工具。拼装顺序（阶段技能在前、通用技能按勾选顺序追加）与「阶段技能不能当通用技能挂上去」都由存储层保证。
- 策划页可勾选通用技能，勾选记在插件存储里，换短剧不用重勾。技能读不出来时回落到内置提示词，而不是发一个没有系统提示的请求——后者会让模型完全不知道输出格式，整轮白跑。

## [0.8.0] - 2026-09-09

### Added
- 提示消息改成队列（最多十条、带相对时间、十秒后收起成入口）。
- 生图两路有限并发 + 相邻请求至少隔一秒；生成中按钮换文案并显示进度。
- 按钮四态与候选图选中态；角色页动作分组重排、左列纵向堆叠。

### Fixed
- 修一处拼错的文案键名，并加守卫扫描所有 `t(...)` 引用。

## [0.7.0] - 2026-09-09

### Added
- 分集剧本共创：整集生成与只改选中那一段，两个围栏分开。选区记位置不记文本。
- 上游资料不含本集正文全文，避免撞上宿主的输入上限。
- 分集差异对长正文只报字数变化。

### Changed
- 策划、角色、剧本的流式改为显示正在生成的字段值，不再刷 JSON 原文也不再只给占位。

## [0.6.0] - 2026-09-09

### Added
- 分集剧本阶段检测剧本里的新面孔并提示建成角色，只提示不自动建，建出来只有名字。
- 检测规则：名字 2 到 6 字、冒号后须有台词、功能性称谓不算、至少出现两次。刻意不调模型，手写剧本一样有效。

## [0.5.0] - 2026-09-09

### Added
- 策划输出带出角色骨架（名字加一句话定位），确认时一并落库；视觉提示词留空等角色阶段单独出稿。
- 角色阶段的「AI 出第一稿」与「给空角色批量出稿」，批量串行并显示进度。
- 角色表为空时的提示。

### Fixed
- 落库时过滤重名、无名与非对象的角色条目；`characters` 缺失或类型不对时按空数组处理。

## [0.4.1] - 2026-09-09

### Fixed
- 策划失败时问题退回输入框、不再把失败那轮的消息留在历史里，模型原文进聊天记录。
- 策划失败原因区分「没给数据 / 格式错 / 输出被截断 / 缺剧名」四种，各给对应处置建议。
- JSON 提取改用括号配平扫描，接受 ```json、裸围栏与 ```short_drama_plan JSON 变体，前后夹带解释也不再毁掉解析。
- 策划提示词收紧围栏标记与输出长度要求；流式行补停止按钮。

## [0.4.0] - 2026-09-09

### Added
- 角色阶段支持与 AI 持续共创：按角色选中、阶段聊天持久化、草稿预览与逐字段差异、手改与 AI 交替编辑、明确采用、停止生成。模型回复不带围栏时按闲聊处理，不动草稿。
- 存档为短剧、角色、分集、分镜加入 `rev` / `draftRev` / `draft`，并新增 `drama.save_draft`、`drama.commit_draft`、`drama.discard_draft`。草稿写入按 `expectedDraftRev` 比对，定稿按 `expectedRev` 比对，重复确认返回冲突而非重复写入。
- 共创聊天按剧存独立文件，新增 `chat.append`、`chat.list`、`chat.clear`。
- 角色草稿只接受 `name`、`description`、`visualPrompt` 三个字段，模型返回的 id、候选图、身份版本一律丢弃。

### Changed
- 要求宿主 1.343.0-rc1 及以上：停止生成依赖桥 2.3.0 的 `ai.cancel`。
- 不再向宿主传 `maxOutputTokens`，该参数已在宿主侧删除。

## [0.3.2] - 2026-09-07

### Changed

- 不再向聊天选区菜单贡献「发送到短剧工坊」。这一项来自清单里的 `menus["chat.selection"]`，对着聊天正文划词时用不上它，白占一格。命令 `video.createDraft` 保留，MCP 工具 `video.create_draft` 与插件页内的入口都不受影响。
- MCP 服务端 `serverInfo.version` 从 `0.2.0` 对齐到插件版本 `0.3.2`。此前两者各走各的，服务端报的版本从 0.2.0 起就没跟过。

## [0.3.1] - 2026-09-04

### Fixed

- Every command failed against a real host. The page treated the host's MCP result envelope as the business payload, so `payload.ok` was always undefined and the library never loaded.

### Added

- The planner shows a live line of generated text above the composer while it waits, and the model picker now lists every model configured on the provider.

## [0.3.0] - 2026-09-04

### Added

- The planning conversation now supports `$` to reference a WillDeep skill. Typing `$` opens a picker, choosing an entry removes the token and adds a chip above the composer, and the request carries only the skill identifier. The host reads the skill body and injects it; the page never sees it.

## [0.2.1] - 2026-09-04

### Fixed

- Every input field was unusable: the workspace defined its sections as functions inside the component and rendered them as JSX elements, so each keystroke remounted the whole tree and dropped focus after one character.
- The English dictionary spread the Chinese one and only overrode part of it, leaving 89 keys, including the planner system prompt sent to the model, showing Chinese to English users.
- The image model is now `nano-banana-2`. The underscore spelling does not match the gateway's routing prefix and would have been rejected as an unconfigured model.
- `crypto.randomUUID` is only defined in a secure context, and the plugin page is served from a custom scheme. A guarded fallback replaces every call, including the one on first render.
- Candidate draws run concurrently and report per-item outcomes, so a failure partway through no longer hides the images that were already generated and billed.
- Generating frames no longer unlocks its own button mid-run, which used to let a second click start another paid round.
- Each draw in a batch varies its prompt, since the gateway forces `n` to 1 for the gpt-image family and drops `seed` on some upstream paths.
- Drafting a new shot no longer shows the first shot's candidates, and picking one no longer writes back to that shot.
- The theme now follows the host when only the color scheme changes, notifications are announced and keyboard dismissible, the page no longer forces a 700px minimum width, and status pills have dark-theme colors.
- Saving the first shot moves a drama from planning into production, so the library's status filters are no longer permanently empty. Filter chips show counts and disable themselves when empty.
- Development-only cover images are no longer copied into the production bundle.

## [0.2.0] - 2026-09-04

### Fixed

- Storyboard saves no longer crash on the pinned `/usr/bin/ruby` 2.6 interpreter; dialogue normalization dropped the Ruby 2.7 `filter_map` call.
- Partial saves keep untouched fields. Renaming a character no longer wipes its description, identity prompt, and identity version.
- An unreadable archive now refuses the write instead of overwriting it with an empty one, so a truncated file no longer costs the user every drama and job record.
- Generated-image paths are confined to the host's `generated-images` directory and symlinks are rejected.
- Stale `characterID` / `shotID` values are reported instead of silently creating duplicates, and `drama.confirm_plan` accepts a `requestID` for idempotent retries.
- Job and drama archives now share the host's per-plugin directory, with a non-destructive copy of any legacy job store on startup.

### Added

- Added a named short-drama library, AI planning confirmation cards, character identity locking, episode switching, editable scripts/dialogue, and per-shot start/end frame design.
- Added permission-scoped some.im image generation with selectable `gpt-image-2` and `nano-banana-2` models plus configurable candidate counts.
- Added multiple video draws per shot and explicit capability messaging when the default Tsingfly adapter cannot accept an end frame.

### Changed

- Redesigned the React workspace around the full short-drama production pipeline with the approved light macOS sidebar, searchable/status-filtered drama list, recent-work continuation, and responsive narrow-window layout.
- Reworked planning from a one-shot form into contextual AI chat with a persistent transcript and a separately confirmable structured card; all visible copy remains in i18n dictionaries.
- Added three optimized 600×800 WebP cover assets for development previews without increasing the host plugin-size limit.

## [0.1.0] - 2026-09-04

### Added

- Added text-to-video and first-frame image-to-video workflows through a provider-neutral asynchronous video adapter.
- Added Tsingfly Hub `POST /v1/videos` and `GET /v1/videos/{id}` support with multipart uploads, task persistence, polling, explicit retry, and automatic MP4 download.
- Added an AI creative-director workflow that reuses WillDeep's configured chat providers and models without exposing their credentials to the plugin.
- Added bilingual React UI, chat-selection capture, local reference-image picker, output reveal action, and a bundled Agent skill.
- Added manual downloads, in-page paid retry/removal confirmation, non-overlapping polling, Retry-After backoff, interrupted-submission recovery, and runtime-only ZIP packaging.
