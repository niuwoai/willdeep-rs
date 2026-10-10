# WillDeep 短剧工坊

短剧工坊是一个完整的本地 WillDeep 插件：从短剧策划、角色固化、可复用资产（造型、场景、道具、声音）、分集剧本和分镜台词，一路走到镜头参考包、首尾帧候选、对白配音、异步视频生成与交接导出。

插件的 MCP 服务是唯一的产品 API：WillDeep 主 Agent、Claude Code、Codex 或任何 MCP 客户端都用同一套工具驱动整条流水线，harness 不需要为短剧新增任何工具，只带 `skills/video-studio/SKILL.md` 和一条 MCP 配置。架构见 [ARCHITECTURE.md](ARCHITECTURE.md)，在 WillDeep 之外运行见 [docs/standalone.md](docs/standalone.md)，资产与参考包的设计见 [docs/design/asset-continuity-system.md](docs/design/asset-continuity-system.md)。

## 能力

- 资产库：造型、场景与场景变体、道具、声音独立建档、出图、选定、归档与审核；声音资产须确认授权才能生成。
- 镜头参考包：每一镜绑定场景、出场角色（造型、声音、主次、站位、换装说明）与道具；服务端按用途与 Provider 能力编译成有顺序的参考，超限不静默截断，提交视频时写入参考快照。
- 配音与导出：`voice.generate` 给台词配音并记时长与台词指纹；`drama.export_manifest` 输出按集按镜的交接清单。

- 短剧项目：首页按剧名管理；AI 策划只在用户确认 `short_drama_plan` 卡片后落库。
- 引用技能：策划对话框输入 `$` 列出并引用 WillDeep 已启用的技能（含插件自带的）。页面只递技能 identifier，SKILL.md 正文与磁盘路径都不出宿主；一次最多引用 3 个。
- 角色与剧本：角色定妆独立于分镜帧；分集可切换，剧本、分镜剧情和逐句台词都可编辑。
- 图片候选：宿主通过 some.im 调用用户选择的 `gpt-image-2` 或 `nano-banana-2`，支持一次生成多张角色定妆、首帧和尾帧候选；密钥不出宿主。
- 文生视频：不提供参考图时提交 `t2va`。
- 首帧图生视频：提供本地 PNG、JPEG 或 WebP 时提交 `fl2va` 与 `input_reference`。
- AI 导演：读取 WillDeep 已启用的聊天 Provider 和模型；默认沿用当前 Provider 的 Flash 档，也可在页面中固定 Provider/模型。
- 异步任务：保存本地 ID 与远端任务 ID，区分草稿、提交、排队、生成、完成和失败。生成队列页在有活动任务且窗口可见时每 8 秒问一次上游，任务全部跑完就停；开关在页头，关掉后只在手动刷新时问。
- 成品管理：完成后默认流式下载 MP4，也支持关闭自动下载后手动下载或重试下载；可在访达中定位，移除任务记录不会删除视频文件。
- 就地播放：队列页直接播放已下载的成片，默认显示首帧，支持拖进度、调音量和放大播放。成片本身仍留在用户的输出目录，只在插件媒体目录里做一个硬链接给页面加载（见「数据与安全边界」）。
- 聊天接力：选中聊天文本执行“发送到视频工坊”，先保存为可编辑草稿，不会直接产生费用。

## 安装与配置

1. 运行 `ruby scripts/package.rb`，在 WillDeep 插件中心选择生成的 `build/willdeep-video-studio-0.26.0.zip`。安装包只包含运行文件，不含前端开发依赖和设计稿。
2. 审核插件声明的进程、网络、凭据、AI 聊天、图片生成、Provider 读取和技能读取权限后安装。
3. 在插件设置中填写 `Video API Key`。该字段为 `secret`，由宿主安全存储，不写入任务文件或页面。
4. 默认 API Base 为 `https://hub.tsingfly.com`，默认视频模型为 `MiniMax-H3`，默认输出目录为 `~/Movies/WillDeep Video Studio`。

聊天模型和 some.im 图片模型的密钥都由 WillDeep 宿主管理。插件页面只选择允许的 Provider ID 与模型名，不接触凭据。

## Provider 抽象

页面和任务存储只依赖统一的异步生命周期：

1. `create(request)` 返回远端任务 ID、状态和可选输出地址；
2. `retrieve(remoteID)` 返回标准状态 `queued / in_progress / completed / failed`；
3. `download(url, remoteID)` 把成品流式保存到本地。

默认 `OpenAIVideosAsyncAdapter` 对接 Tsingfly Hub 的 MiniMax-H3：以 multipart 请求 `POST /v1/videos`，通过 `GET /v1/videos/{id}` 查询。任务由 `extra_params.task` 决定：`t2va` 文生、`fl2va` 首帧图生（`input_reference`）、`ref2va` 全参考（1~3 段参考视频走 `input_references`，或首帧加对白音轨走 `input_reference` 加 `audio_reference`）。FPS 固定 24，时长 4~15 秒，`flow_shift=12` 与 `audio_flow_shift=3` 成对。提示词由服务端按 H3 的三段式 / 六段式渲染。后续接入其他视频服务时，只需实现同一适配器边界，不必改 React 页面或任务仓库。

创建请求若发生网络超时、返回非法 JSON 或缺少任务 ID，任务会标记为“提交结果不明确”，插件不会自动重新 POST。MCP 重启后也会恢复中断的提交为不明确状态。用户核对服务端后才能经页面确认显式重试。查询阶段的 429 会保留任务及 `Retry-After` 并退避，既不误判生成失败，也不重叠堆积轮询请求。

## 本地开发

前端使用 Yarn：

```bash
cd PluginExamples/video-studio/ui
yarn install
yarn test
yarn build
```

服务端测试使用本地假网关，不访问真实服务，并可生成 Markdown 与 JSON 报告（CI 逐个执行 `scripts/*_test.rb`）：

```bash
ruby scripts/server_test.rb build/ci/server_report.md build/ci/server_report.json
```

```bash
ruby scripts/standalone_pipeline_test.rb build/ci/standalone_report.md build/ci/standalone_report.json
```

`standalone_pipeline_test.rb` 不带任何 WillDeep 扩展、只靠环境变量直连的假网关，从策划一路跑到导出清单，是「harness 不加工具」这条原则的验收。

插件提交中包含 `ui/dist`，安装器无需现场安装依赖或运行构建。

## 数据与安全边界

- 数据都落在宿主按 pluginID 分的同一个目录 `~/Library/Application Support/WillDeep/plugin-data/willdeep-video-studio/`（独立运行用 `VIDEO_STUDIO_DATA_DIR` 指定）：任务 `jobs.json`（最多 500 条）、短剧 `dramas.json`（最多 100 部，含资产与参考包）、历史 `history/`、生成的图片与音频 `generated-images/`。旧版本的 `plugin-data/video-studio/jobs.json` 会在首次启动时复制过来，原文件保留不删。在 willdeep-rs 的 Web 宿主里，用到的媒体还会在 `~/.willdeep/plugin-media/willdeep-video-studio/` 里出现一份硬链接（即宿主下发媒体用的目录）。
- 候选图与音频只记文件名，绝对路径与页面 URL 在读出时按媒体目录派生；旧存档里的绝对路径照常认。
- 出图、配音的直连密钥只通过环境变量进入 MCP 进程（`VIDEO_STUDIO_IMAGE_API_KEY`、`VIDEO_STUDIO_TTS_API_KEY`），不写入任何存档，工具结果也不返回。WillDeep 内出图优先借宿主凭据；宿主宣告 `willdeep/audio/synthesize` 时配音也优先由宿主代管，不另配 key。
- 存档读不出来时插件拒绝写入并原样保留文件，不会按空档覆盖。遇到 `-32002` 错误请先去看那个 JSON 文件。
- 模型递上来的图片路径必须落在 `generated-images/` 内，符号链接一律拒绝。
- API Key 只通过插件 secret 设置注入 MCP 进程，状态接口和持久化文件均不返回它。
- API Base 只接受 HTTPS；仅测试时允许 localhost HTTP。
- 参考图限制为 PNG/JPEG/WebP 且不超过 32 MiB；单个视频下载上限 1 GiB。
- 输出下载地址必须使用 HTTPS；本地集成测试只放行 localhost HTTP。
- 插件不会创建后台定时任务。轮询只发生在页面打开、停在生成队列页、窗口可见且确实有活动任务时；任务跑完轮询自己停，用户也可以直接关掉。
- 页面播放成片走宿主放行的本地媒体入口：macOS 宿主是 `willdeep-plugin://bundle/__media__/`（指向插件数据目录下的 `generated-images/`），willdeep-rs 的 Web 宿主是同源的 `/plugin-media/willdeep-video-studio/`（指向 `~/.willdeep/plugin-media/willdeep-video-studio/`，插件按需硬链接过去，跨卷退化为拷贝）。插件在 `generated-images/` 里为成片建一个同名硬链接并抽一张首帧 PNG，成片原件仍在用户的输出目录，不上传、不外发。
- 服务端按 MCP `initialize` 的 `clientInfo.name` 认宿主（willdeep-rs 发 `willdeep`），据此决定媒体根、页面 URL 前缀和递给宿主的绝对路径；认不出来时按 macOS 宿主的老行为。`VIDEO_STUDIO_HOST_MODE=web|desktop`、`VIDEO_STUDIO_HOST_MEDIA_ROOT` 可手动覆盖。
