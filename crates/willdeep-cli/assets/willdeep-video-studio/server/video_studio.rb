#!/usr/bin/env ruby
# frozen_string_literal: true

require "fileutils"
require "json"

Encoding.default_external = Encoding::UTF_8
Encoding.default_internal = Encoding::UTF_8
$stdout.sync = true
$stdout.set_encoding(Encoding::UTF_8)
$stderr.set_encoding(Encoding::UTF_8)

require_relative "adapters/openai_videos_async"
require_relative "lib/media_mirror"
require_relative "lib/media_host"
require_relative "lib/video_store"
require_relative "lib/video_service"
require_relative "lib/drama_store"
require_relative "lib/drama_service"
require_relative "lib/history_store"
require_relative "lib/chat_store"
require_relative "lib/creative_skill_store"
require_relative "lib/host_bridge"
require_relative "lib/image_generation"
require_relative "lib/drama_progress"
require_relative "lib/stage_context"
require_relative "lib/review_runner"
require_relative "lib/script_creation"
require_relative "lib/image_backend"
require_relative "lib/voice_generation"
require_relative "lib/export_manifest"
require_relative "lib/compose_store"
require_relative "lib/episode_composer"
require_relative "lib/music_backend"
require_relative "lib/media_reader"
require_relative "lib/reference_package"
require_relative "lib/streamable_http_server"
require_relative "lib/tool_i18n"
require_relative "lib/mcp_http_endpoint"
require_relative "lib/background_tools"
require_relative "lib/qa_lessons"
require_relative "lib/video_remediation"
require_relative "lib/clip_trim"
require_relative "lib/image_qa"

# 与插件清单、ui/package.json 一致；scripts/version_test.rb 断言三处相同。
SERVER_VERSION = "0.46.0-rc1"

# 宿主按 pluginID 给每个插件分一个目录，本插件的 pluginID 是
# willdeep-video-studio，生成的图片也由宿主写进这个目录。任务存档早先落在
# plugin-data/video-studio/，同一个插件占两个目录，卸载清理和备份会漏掉一半。
#
# 独立运行（Claude Code、Codex 等没有 WillDeep 的客户端）用 VIDEO_STUDIO_DATA_DIR
# 指定数据目录；各子路径仍可用原有的单项环境变量覆盖（设计稿 9.1）。
def plugin_data_directory
  override = ENV["VIDEO_STUDIO_DATA_DIR"].to_s.strip
  return File.expand_path(override) unless override.empty?

  File.expand_path("~/Library/Application Support/WillDeep/plugin-data/willdeep-video-studio")
end

def legacy_video_store_path
  File.expand_path("~/Library/Application Support/WillDeep/plugin-data/video-studio/jobs.json")
end

def video_store_path
  ENV["VIDEO_STUDIO_STORE"] || File.join(plugin_data_directory, "jobs.json")
end

# 只在新路径还没有存档时把旧存档搬过来，且不删旧文件——万一搬错了，用户的
# 任务记录还在原处。
def migrate_legacy_video_store
  return if ENV["VIDEO_STUDIO_STORE"]
  return if File.exist?(video_store_path)
  return unless File.exist?(legacy_video_store_path)

  FileUtils.mkdir_p(File.dirname(video_store_path))
  FileUtils.cp(legacy_video_store_path, video_store_path)
  warn "video-studio: copied legacy job store into #{File.dirname(video_store_path)}"
rescue SystemCallError => error
  warn "video-studio: legacy job store migration skipped (#{error.class}: #{error.message})"
end

def generated_media_directory
  File.join(plugin_data_directory, "generated-images")
end

def video_output_directory
  value = ENV["VIDEO_STUDIO_OUTPUT_DIRECTORY"].to_s.strip
  value = "~/Movies/WillDeep Video Studio" if value.empty?
  File.expand_path(value)
end

def drama_store_path
  ENV["VIDEO_STUDIO_DRAMA_STORE"] || File.join(plugin_data_directory, "dramas.json")
end

# 共创聊天一部剧一个文件。放在 dramas.json 旁边的子目录里，卸载清理仍然
# 只需要删这一个插件目录。
def chat_store_directory
  ENV["VIDEO_STUDIO_CHAT_STORE"] || File.join(plugin_data_directory, "chats")
end

# 定稿版本历史，同样一部剧一个文件，理由见 lib/history_store.rb 顶部。
def history_store_directory
  ENV["VIDEO_STUDIO_HISTORY_STORE"] || File.join(plugin_data_directory, "history")
end

# 创作技能：插件包里那份是只读模板，用户编辑的副本落在数据目录。技能是
# 全局一份，改了对所有短剧生效。
def creative_skill_template_root
  File.join(__dir__, "..", "skills", "creative")
end

def creative_skill_user_root
  ENV["VIDEO_STUDIO_SKILL_STORE"] || File.join(plugin_data_directory, "creative-skills")
end

migrate_legacy_video_store
STORE = VideoStore.new(video_store_path)
# 选定 provider 就用它自己的地址，选「自定义」才回落到手填的 API Base。
# 三家走的是同一套 OpenAI 兼容异步视频接口，所以只换地址、不换适配器。
PROVIDER_BASE_URLS = {
  "tsingfly" => "https://hub.tsingfly.com",
  "openai" => "https://api.openai.com"
}.freeze
DEFAULT_API_BASE = PROVIDER_BASE_URLS.fetch("tsingfly")

# 没有 VIDEO_STUDIO_PROVIDER 的老装机（设置里还没有这一项）继续按
# VIDEO_STUDIO_API_BASE 走，行为与加这个设置之前一致。
def resolved_api_base
  provider = ENV["VIDEO_STUDIO_PROVIDER"].to_s.strip
  known = PROVIDER_BASE_URLS[provider]
  return known if known

  base = ENV["VIDEO_STUDIO_API_BASE"].to_s.strip
  base.empty? ? DEFAULT_API_BASE : base
end

ADAPTER_FACTORY = lambda do
  OpenAIVideosAsyncAdapter.new(
    base_url: resolved_api_base,
    api_key: ENV["VIDEO_STUDIO_API_KEY"],
    output_directory: video_output_directory,
    open_timeout: (ENV["VIDEO_STUDIO_OPEN_TIMEOUT"] || 20).to_i,
    read_timeout: (ENV["VIDEO_STUDIO_READ_TIMEOUT"] || 90).to_i
  )
end
# 经验库（0.38.0-rc1，docs/decisions/0004-qa-lessons.md）：自动补救的每次尝试与手动经验。
# 与 dramas.json 同目录，理由同 background-jobs.json。
def qa_lesson_store_path
  ENV["VIDEO_STUDIO_QA_LESSON_STORE"] || File.join(File.dirname(drama_store_path), "qa-lessons.json")
end
QA_LESSONS = QALessons.new(store: QALessonStore.new(qa_lesson_store_path))
MEDIA_HOST = MediaHost.new(desktop_root: ENV["VIDEO_STUDIO_MEDIA_ROOT"] || generated_media_directory)
MediaRef.configure(MEDIA_HOST)
DRAMA_SERVICE = DramaService.new(
  store: DramaStore.new(drama_store_path),
  media_root: MEDIA_HOST,
  history: HistoryStore.new(history_store_directory)
)
SERVICE = VideoService.new(
  store: STORE,
  adapter_factory: ADAPTER_FACTORY,
  # 成片落在用户的输出目录，而插件页面只能加载媒体根下的文件。镜像器负责把
  # 两者接上，见 lib/media_mirror.rb 顶部；媒体根与 URL 前缀由 MEDIA_HOST 按
  # 宿主决定（Web 宿主下同步给 plugin-media/，见 lib/media_host.rb）。
  # 测试靠 VIDEO_STUDIO_MEDIA_ROOT 把 macOS 媒体根挪到临时目录，不碰用户的
  # 真实数据。
  media_mirror: MediaMirror.new(host: MEDIA_HOST),
  # 带 shotID 的提交要按镜头的参考包展开（设计稿 5.2）。
  dramas: DRAMA_SERVICE,
  # 成片提示词的预防句来自经验库（内置一镜到底 + 验证过的补救句 + 手动预防句）。
  directives: -> { QA_LESSONS.prevention_directives },
  # 配音节奏（0.43.0-rc1）：这一集的成片设置，请求时长按台词反推。COMPOSE_STORE 在下面才定义，调用时再取。
  pacing: ->(drama_id, episode_id) { COMPOSE_STORE.settings(drama_id, episode_id) }
)
HOST = HostBridge.new(input: $stdin, output: $stdout)
# 端口先开好，连接文件等 stdio 宿主 initialize 后再写：写到哪里取决于是哪个宿主
# 拉起了本进程（lib/mcp_http_endpoint.rb）。网关总是先经宿主的 MCP 客户端把插件
# 拉起来再读文件，那时 initialize 已经应答过。
HTTP_SERVER = if ENV["VIDEO_STUDIO_MCP_HTTP_ENABLED"] == "1"
                StreamableHTTPServer.new(
                  port: ENV.fetch("VIDEO_STUDIO_MCP_HTTP_PORT", "0"),
                  token: ENV["VIDEO_STUDIO_MCP_HTTP_TOKEN"]
                ).start
              end

# 除 url / token 外，附上宿主与进程号：网关据 parentPID 能认出文件是不是自己拉起
# 的进程写的（契约修订见 docs/decisions/0001-plugin-mcp-gateway.md）。
def publish_http_endpoint(params)
  return unless HTTP_SERVER

  location = MCPHTTPEndpoint.locate(params, data_directory: plugin_data_directory,
                                            willdeep_home: MEDIA_HOST.willdeep_home)
  HTTP_SERVER.publish(location.path, host: location.host, pid: Process.pid, parentPID: Process.ppid)
rescue SystemCallError => error
  warn "video-studio: HTTP connection file not written (#{error.class}: #{error.message})"
end

IMAGE_GENERATION = ImageGeneration.new(drama_service: DRAMA_SERVICE, host: HOST)
VOICE_GENERATION = VoiceGeneration.new(drama_service: DRAMA_SERVICE, host: HOST)
CHAT_STORE = ChatStore.new(chat_store_directory)
# 分集成片（设计稿 docs/design/episode-compose.md）：MCP 工具只建任务、读进度，
# 配音与转码在 compose_worker.rb 起的后台进程里跑。后台进程拿到的只有路径，密钥
# 经环境变量继承，宿主模式经 VIDEO_STUDIO_HOST_MODE 显式传入。
COMPOSE_STORE = ComposeStore.new(ENV["VIDEO_STUDIO_COMPOSE_STORE"] || File.join(plugin_data_directory, "compose.json"))
EPISODE_COMPOSER = EpisodeComposer.new(
  store: COMPOSE_STORE,
  dramas: DRAMA_SERVICE,
  video_jobs: -> { SERVICE.jobs_with_media },
  media_host: MEDIA_HOST,
  worker_spec: {
    "dramaStore" => drama_store_path,
    "historyDirectory" => history_store_directory,
    "videoStore" => video_store_path,
    "desktopMediaRoot" => MEDIA_HOST.desktop_root,
    "outputDirectory" => video_output_directory
  },
  tts_backend: -> { TTSBackend.select(host: HOST, media_root: DRAMA_SERVICE.media_root) },
  music_backend: -> { MusicBackend.select(env: ENV, media_root: DRAMA_SERVICE.media_root) }
)
CREATIVE_SKILLS = CreativeSkillStore.new(
  template_root: creative_skill_template_root,
  user_root: creative_skill_user_root
)
def build_drama_progress
  DramaProgress.new(drama_service: DRAMA_SERVICE, jobs: -> { SERVICE.jobs_with_media }, settings: -> { STORE.settings },
                    capabilities: -> { SERVICE.capabilities["video"] },
                    background_jobs: ->(drama_id) { BACKGROUND.active_for(drama_id) },
                    # 候选图推荐与过期标记（0.40.0-rc1）：读出时由 ImageQA 算。
                    annotate: ->(drama) { IMAGE_QA.annotate(drama) })
end
DRAMA_PROGRESS = build_drama_progress
STAGE_CONTEXT = StageContext.new(drama_service: DRAMA_SERVICE, skills: CREATIVE_SKILLS)
# 批量里逐镜并发的进程级名额（0.38.0-rc2）：画面质检与在生成的视频，所有批量共用一份；
# 专家席审稿（0.42.0-rc1）的每位专家发言也占一个质检名额。
PIPELINE_LIMITS = ShotPipelines.default_limits(-> { STORE.settings })
REVIEW_RUNNER = ReviewRunner.new(drama_service: DRAMA_SERVICE, video_service: SERVICE, video_store: STORE, skills: CREATIVE_SKILLS, host: HOST,
                                 video_qa: VideoQA.new(media_host: MEDIA_HOST), limits: PIPELINE_LIMITS)

# 后台任务（0.36.0-rc1，docs/decisions/0003-background-jobs.md）：出图、审核、配音与按集批量
# 在本进程的工作线程里跑，工具只建任务、立即返回。台账跟着 dramas.json 放在同一个目录：
# 任务记的是剧里的对象，两份档该一起备份、一起搬；只改了 VIDEO_STUDIO_DRAMA_STORE 的
# 测试与独立部署也不会把台账写进默认的用户数据目录。
def background_job_store_path
  ENV["VIDEO_STUDIO_BACKGROUND_JOB_STORE"] || File.join(File.dirname(drama_store_path), "background-jobs.json")
end
JOB_RUNNER = JobRunner.new(store: BackgroundJobStore.new(background_job_store_path),
                           workers: (ENV["VIDEO_STUDIO_JOB_WORKERS"] || JobRunner::DEFAULT_WORKERS).to_i)
# 画面质检的自动补救（0.38.0-rc1）：重拍、复审、记经验、选最好的一条。轮询节奏与视频批量一致。
VIDEO_REMEDIATION = VideoRemediation.new(
  limits: PIPELINE_LIMITS,
  dramas: DRAMA_SERVICE, videos: SERVICE, reviews: REVIEW_RUNNER, lessons: QA_LESSONS, settings: -> { STORE.settings }, host: HOST,
  poll_seconds: (ENV["VIDEO_STUDIO_JOB_POLL_SECONDS"] || EpisodeBatch::DEFAULT_POLL_SECONDS).to_f,
  poll_limit_seconds: (ENV["VIDEO_STUDIO_JOB_POLL_LIMIT_SECONDS"] || EpisodeBatch::DEFAULT_POLL_LIMIT_SECONDS).to_f
)
# 候选图质检与推荐（0.40.0-rc1，docs/decisions/0008-candidate-image-qa.md）：与成片质检共用审核模型路由与质检名额。
IMAGE_QA = ImageQA.new(dramas: DRAMA_SERVICE, images: IMAGE_GENERATION, reviews: REVIEW_RUNNER, skills: CREATIVE_SKILLS, host: HOST,
                       settings: -> { STORE.settings }, limits: PIPELINE_LIMITS, lessons: QA_LESSONS)
EPISODE_BATCH = EpisodeBatch.new(
  remediator: VIDEO_REMEDIATION, limits: PIPELINE_LIMITS, image_qa: IMAGE_QA,
  dramas: DRAMA_SERVICE, images: IMAGE_GENERATION, voices: VOICE_GENERATION, reviews: REVIEW_RUNNER, videos: SERVICE,
  # 批量审核在工作线程里算进度，用自己的实例：DramaProgress 每次调用会改实例上的语言。
  progress: -> { build_drama_progress },
  poll_seconds: (ENV["VIDEO_STUDIO_JOB_POLL_SECONDS"] || EpisodeBatch::DEFAULT_POLL_SECONDS).to_f,
  poll_limit_seconds: (ENV["VIDEO_STUDIO_JOB_POLL_LIMIT_SECONDS"] || EpisodeBatch::DEFAULT_POLL_LIMIT_SECONDS).to_f
)
SCRIPT_CREATION = ScriptCreation.new(dramas: DRAMA_SERVICE, stages: STAGE_CONTEXT, reviews: REVIEW_RUNNER)
BACKGROUND = BackgroundTools.new(runner: JOB_RUNNER, batch: EPISODE_BATCH, images: IMAGE_GENERATION, voices: VOICE_GENERATION,
                                 reviews: REVIEW_RUNNER, videos: SERVICE, remediator: VIDEO_REMEDIATION, image_qa: IMAGE_QA, script_creation: SCRIPT_CREATION)

# 读出的剧挂上候选图推荐与过期标记（0.40.0-rc1）：drama.get / drama.list / drama.list_shots。只读字段，不落库。
def with_image_qa(result)
  return result unless result.is_a?(Hash) && result["ok"]

  IMAGE_QA.annotate(result["drama"]) if result["drama"].is_a?(Hash) && result["drama"].key?("episodes")
  Array(result["dramas"]).each { |drama| IMAGE_QA.annotate(drama) if drama.is_a?(Hash) }
  result
end

# drama.get_asset：资产单独读出时同样带推荐与过期标记（按整部剧算，取这个资产的那份）。
def asset_with_image_qa(arguments)
  result = DRAMA_SERVICE.get_asset(arguments)
  return result unless result["ok"] && result["asset"].is_a?(Hash)

  loaded = with_image_qa(DRAMA_SERVICE.get("id" => arguments["dramaID"]))
  annotated = loaded["ok"] ? Array(loaded["drama"]["assets"]).find { |entry| entry["id"] == result["asset"]["id"] } : nil
  return result unless annotated

  result.merge("asset" => result["asset"].merge("candidates" => annotated["candidates"], "recommendedCandidateID" => annotated["recommendedCandidateID"]))
end

# drama.list_shots：每镜带首 / 尾帧推荐（不带候选数组）。
def list_shots_with_recommendations(arguments)
  listed = DRAMA_SERVICE.list_shots(arguments)
  return listed unless listed["ok"]

  loaded = with_image_qa(DRAMA_SERVICE.get("id" => arguments["dramaID"]))
  episode = loaded["ok"] ? Array(loaded["drama"]["episodes"]).find { |entry| entry["id"] == listed["episodeID"] } : nil
  shots = episode ? Array(episode["shots"]) : []
  listed["shots"].each do |entry|
    shot = shots.find { |item| item["id"] == entry["id"] }
    next unless shot

    %w[recommendedStartID recommendedEndID startRecommendation endRecommendation].each { |key| entry[key] = shot[key] }
  end
  listed
end

# 参考包预览：页面面板与 Agent 的预算检查都调它，编译只有服务端这一份。
def preview_reference_package(arguments)
  loaded = DRAMA_SERVICE.get("id" => arguments["dramaID"])
  return loaded unless loaded["ok"]
  drama = loaded["drama"]
  episode = drama["episodes"].find { |entry| entry["id"] == arguments["episodeID"].to_s }
  return failure_payload("episode_not_found", "Episode was not found.") unless episode
  shot = episode["shots"].find { |entry| entry["id"] == arguments["shotID"].to_s }
  return failure_payload("shot_not_found", "Storyboard shot was not found.") unless shot

  purpose = arguments["purpose"].to_s.empty? ? "image" : arguments["purpose"].to_s
  ReferencePackage.compile(drama, episode, shot, purpose: purpose, capabilities: SERVICE.capabilities["video"],
                                                 mode: arguments["mode"], media_root: DRAMA_SERVICE.media_root, jobs: SERVICE.jobs_with_media,
                                                 directives: QA_LESSONS.prevention_directives)
end

def export_manifest(arguments)
  loaded = DRAMA_SERVICE.get("id" => arguments["dramaID"])
  return loaded unless loaded["ok"]
  ExportManifest.build(loaded["drama"], SERVICE.jobs_with_media, DRAMA_SERVICE.media_root)
end

def creative_skill_list
  CREATIVE_SKILLS.list.merge("ok" => true)
end

def creative_skill_get(arguments)
  entry = CREATIVE_SKILLS.get(arguments["id"])
  entry ? { "ok" => true, "skill" => entry } : failure_payload("skill_not_found", "Creative skill was not found.")
end

def creative_skill_save(arguments)
  entry = CREATIVE_SKILLS.save(arguments["id"], arguments["body"])
  { "ok" => true, "skill" => entry }
rescue ArgumentError => error
  failure_payload("invalid_skill", error.message)
end

def creative_skill_reset(arguments)
  entry = CREATIVE_SKILLS.reset(arguments["id"])
  { "ok" => true, "skill" => entry }
rescue ArgumentError => error
  failure_payload("invalid_skill", error.message)
end

def creative_skill_compose(arguments)
  CREATIVE_SKILLS.compose(stage: arguments["stage"], common: arguments["common"])
end

def failure_payload(code, message)
  { "ok" => false, "error" => { "code" => code, "message" => message } }
end

def chat_append(arguments)
  entry = CHAT_STORE.append(arguments["dramaID"], arguments["stage"], arguments["message"])
  { "ok" => true, "message" => entry }
rescue ArgumentError
  { "ok" => false, "error" => { "code" => "invalid_stage", "message" => "Unknown co-creation stage." } }
end

def chat_list(arguments)
  { "ok" => true, "messages" => CHAT_STORE.list(arguments["dramaID"], arguments["stage"]) }
end

def chat_clear(arguments)
  CHAT_STORE.clear(arguments["dramaID"], arguments["stage"])
  { "ok" => true }
rescue ArgumentError
  { "ok" => false, "error" => { "code" => "invalid_stage", "message" => "Unknown co-creation stage." } }
end

# 只读工具：在 tools/list 里带上 MCP 标准注解 readOnlyHint。WillDeep 1.379.0-rc1
# 起据此在「标准信任」下自动放行——主 Agent 跑一部剧要读几百次，每次都弹确认
# 就没法用。列在这里的必须真的不写任何东西；video.refresh 会改任务状态、
# video.status 之外的 video.* 都会动存档，所以不在其中。
READ_ONLY_TOOLS = %w[
  system.status
  drama.list drama.get drama.get_progress drama.get_stage_context drama.list_history drama.read_version drama.list_shots drama.check_consistency
  drama.list_assets drama.get_asset drama.preview_reference_package drama.export_manifest review.get_material media.read
  skill.list skill.get skill.compose chat.list video.status video.list video.capabilities
  episode.compose_plan episode.compose_status jobs.status jobs.wait qa.lessons
].freeze

# 后台任务与按集批量（0.36.0-rc1）。
ASYNC_DESCRIPTION = " async: true queues the work as a background job and returns {ok, jobID} at once (argument errors, missing objects and a missing backend are still reported immediately); follow it with jobs.wait / jobs.status."
BATCH_COMMON = "Always runs as a background job: returns {ok, jobID, planned {items, pending, skipped}} at once and works through the items (episode.generate_videos and the remediation tools run every shot's pipeline concurrently, within video.settings qaConcurrency / videoConcurrency); jobs.status / jobs.wait show per-item results and progress. " \
               "Idempotent: a requestID returns the same job (unless it failed or was interrupted, which starts a new one); without requestID a second call while the same batch is still running returns that job (alreadyRunning). " \
               "Every item calls the single tool with a derived requestID, so re-running after an interruption skips work already recorded and never bills twice. jobs.cancel stops it before the next step of each shot."
JOB_TOOLS = [
  {
    name: "system.status",
    description: "Read the actual running plugin version, package root, PID and parent PID. No credentials or provider settings. Use after an update to verify that the host reconnected to the installed version.",
    inputSchema: { type: "object", properties: {} }
  },
  {
    name: "episode.generate_frames",
    description: "Generate start (or end) frame candidates for every shot of one episode, or the shots listed in shotOrders, with each shot's compiled reference package — the same path as image.generate target=start/end. countPerModel and models as in image.generate (models x countPerModel at most 4 per shot). onlyMissing (default true) skips shots that already have candidates for that target; force: true redraws them. Shots with an unadopted draft or without the frame prompt are skipped and listed. Stops the remaining shots when the image quota is exhausted. " \
                 "Candidate image QA (autoQA, default video.settings imageAutoQA; needs the host model bridge): every new candidate is checked against the references and the shot (identity, wardrobe, text, brand, composition, aspect) like image.qa, while the next shot is already drawing. " \
                 "When every candidate of a shot is blocked, retryOnBlock (default imageRetryOnBlock, at most #{VideoStore::IMAGE_RETRY_ON_BLOCK_LIMIT}) more rounds are drawn with positive remediation sentences built from the issues (billed like the first round); shots still all blocked end with stage needs_human and reasons. " \
                 "autoSelect (default imageAutoSelect) selects the shot's best new candidate when it passed or only has advice notes — unless someone selected another frame after the batch started (newer_selection) or the current frame's fresh QA is better (worse_qa). Items carry imageQA {candidates[{candidateID, status, score, categories}], recommendedCandidateID, retries, remediation}, selectedCandidateID and selectionReason. #{BATCH_COMMON}",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" }, target: { type: "string", enum: EpisodeBatch::FRAME_TARGETS },
                                                 shotOrders: { type: "array", items: { type: "integer" } }, countPerModel: { type: "integer", enum: ImageGeneration::COUNTS },
                                                 models: { type: "array", items: { type: "string", enum: DramaService::IMAGE_MODELS } }, castIDs: { type: "array", items: { type: "string" } },
                                                 onlyMissing: { type: "boolean" }, force: { type: "boolean" }, requestID: { type: "string" },
                                                 autoQA: { type: "boolean" }, autoSelect: { type: "boolean" },
                                                 retryOnBlock: { type: "integer", minimum: 0, maximum: VideoStore::IMAGE_RETRY_ON_BLOCK_LIMIT } },
                   required: %w[dramaID episodeID] }
  },
  {
    name: "episode.dub",
    description: "Dub the dialogue of one episode (or the shots in shotOrders) with TTS, like voice.generate per shot: each line uses the voice bound in the shot's reference package or the speaker's consented voice. Lines that already have selected audio for their current text are skipped; lines whose text changed since dubbing (stale) are redone and the new audio is selected; force: true redubs everything. Lines without a usable voice are reported in skippedLines. #{BATCH_COMMON}",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" }, shotOrders: { type: "array", items: { type: "integer" } },
                                                 presetID: { type: "string" }, force: { type: "boolean" }, requestID: { type: "string" } },
                   required: %w[dramaID episodeID] }
  },
  {
    name: "episode.generate_videos",
    description: "Submit one video job per shot of an episode (or shotOrders) through the video.generate shot path (selected start frame, dialogue audio track, compiled H3 prompt, shot duration), then poll and download each clip in the background until it completes or fails, and prepare it for playback. With video.settings autoQA and autoRemediate on (and the host model bridge), each shot's clip is frame-reviewed and remediated (as video.remediate) as soon as that clip completes, while other shots are still generating; a shot whose earlier automatic QA or remediation was interrupted continues instead of being regenerated. onlyMissing (default true) skips shots whose latest video is completed and keeps polling ones still generating instead of submitting again; force: true submits for every shot. Shots without a selected start frame are skipped unless mode is t2va or ref2va. " \
                 "Each new clip becomes the shot's selected take once it is ready (after QA and remediation when they run, the best take of this run; a clip whose frame review returned no verdict is not selected), replacing an older selection, unless someone selected another clip after the batch started (newer_selection), the current clip carries a user qaOverride and the new one did not pass (user_qa_override), " \
                 "or the new clip's frame QA is strictly worse than the current clip's (pass > warn > block > no verdict; worse_qa; equal QA takes the new clip); every item reports selectedJobID, selectionChanged and selectionReason. " \
                 "A current clip generated from a dialogue track whose lines were dubbed again since (dialogueStale) is replaced by the new take regardless of QA or qaOverride (replacedStaleDialogue). A selection that was back on the old clip when the batch ended is reported as selection_reverted. " \
                 "Every submission is billed: report the count to the user first. #{BATCH_COMMON}",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" }, shotOrders: { type: "array", items: { type: "integer" } },
                                                 mode: { type: "string", enum: ReferencePackage::MODES }, onlyMissing: { type: "boolean" }, force: { type: "boolean" }, requestID: { type: "string" } },
                   required: %w[dramaID episodeID] }
  },
  {
    name: "review.run_batch",
    description: "Run review.run over many objects of one drama in the background. scope: a list (or a string joined by | or ,) of drama, characters, assets, episodes, shots; default all. episodeID narrows episodes and shots to one episode. Only aspects that have material are listed (no images yet means no image review). With video.settings panelReview on (default) the drama and each scripted episode also get the expert panel (aspect panel, several model calls each); aspects (optional list of content, images, storyboard, panel) limits the run to those aspects, e.g. [\"panel\"] to add the panel verdicts only. skipFresh (default true) skips objects whose stored verdict still matches the current material (pass, warn or block with the same basis); force: true reviews everything. Needs the WillDeep host's model bridge like review.run. Per-item results carry status pass/warn/block and mustFix; follow up blocks and warns with must issues as for review.run. #{BATCH_COMMON}",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, scope: { type: %w[array string], items: { type: "string", enum: EpisodeBatch::REVIEW_SCOPES } },
                                                 aspects: { type: %w[array string], items: { type: "string", enum: EpisodeBatch::REVIEW_ASPECTS } },
                                                 episodeID: { type: "string" }, skipFresh: { type: "boolean" }, force: { type: "boolean" }, requestID: { type: "string" } },
                   required: %w[dramaID] }
  },
  {
    name: "jobs.status",
    description: "Background jobs (async image.generate / review.run / voice.generate and the episode batch tools), newest first: kind, state (queued, running, completed, failed, canceled, interrupted), progress {done, total, percent}, per-item results (items[] with state pending/running/waiting/completed/failed/skipped/canceled, reason, error), result and error. Video and remediation batches run shots concurrently: their items carry stage (queued, generating, qa, retake with attempt, selecting, done, needs_human, failed) and finishedAt, and the job carries stages[] (key, order, label, state, stage, attempt) even with includeItems: false. Filter by jobIDs, dramaID, episodeID, kind, state (or active / finished); limit defaults to 20; includeItems: false drops items[]. Jobs left running when the plugin process stopped show as interrupted: call the same tool again to continue.",
    inputSchema: { type: "object", properties: { jobIDs: { type: "array", items: { type: "string" } }, jobID: { type: "string" }, dramaID: { type: "string" }, episodeID: { type: "string" },
                                                 kind: { type: "string", enum: BackgroundTools::KINDS }, state: { type: "string", enum: BackgroundTools::STATE_FILTERS },
                                                 limit: { type: "integer", minimum: 1, maximum: BackgroundTools::MAX_STATUS_LIMIT }, includeItems: { type: "boolean" } } }
  },
  {
    name: "jobs.wait",
    description: "Wait until the given background jobs are all finished or timeoutSeconds pass (default 30, at most 60; over the WillDeep stdio connection at most #{BackgroundTools::STDIO_WAIT_LIMIT}, because the host queues stdio requests per plugin and a long wait would stall the plugin page). Returns done, timedOut, waitedSeconds and the jobs as in jobs.status. Reads never wait for jobs: drama.get and the rest answer while jobs run.",
    inputSchema: { type: "object", properties: { jobIDs: { type: "array", items: { type: "string" }, maxItems: BackgroundTools::MAX_WAIT_IDS }, jobID: { type: "string" },
                                                 timeoutSeconds: { type: "number", minimum: 0, maximum: BackgroundTools::WAIT_LIMIT }, includeItems: { type: "boolean" } } }
  },
  {
    name: "jobs.cancel",
    description: "Cancel a background job. A queued job is canceled at once; a running one stops before its next item (the image, review or video submission in progress finishes and is kept; submitted video jobs keep running remotely and stay in the generation queue). Finished jobs are returned unchanged with alreadyFinished.",
    inputSchema: { type: "object", properties: { jobID: { type: "string" } }, required: ["jobID"] }
  }
].freeze

# 画面质检自动补救与经验库（0.38.0-rc1，docs/decisions/0004-qa-lessons.md）。
REMEDIATE_COMMON = "Needs the WillDeep host's model bridge for frame review (like review.run) and a configured video provider. " \
                   "Every retake is billed: report the planned count to the user first (dryRun shows it). A failed frame review is treated as unknown, never as a pass. " \
                   "Every item reports selectedJobID (the shot's selected clip afterwards, null if none), selectionChanged and selectionReason; the best take is not selected when someone selected another clip after the job started, " \
                   "when the current clip carries a user qaOverride and the take did not pass, or when the take's frame QA is worse than the current clip's (selectionKept)."
QA_TOOLS = [
  {
    name: "video.remediate",
    description: "Fix one shot's clip after frame QA (scene_jump, identity, text_overlay, injury, blood by default per video.settings remediateCategories). Pass jobID, or dramaID + episodeID + shotID (the shot's selected clip, else its newest completed one). " \
                 "Runs frame QA first when the verdict is missing or stale; then, while remediable issues remain and up to maxRetries (default video.settings remediateMaxRetries, at most #{QARemediation::MAX_RETRIES_LIMIT}), submits a retake through the video.generate shot path with one positive remediation sentence per issue category (from the remediation table and the lessons store), waits for it, prepares playback and reviews it again. " \
                 "Before any retake, a scene_jump whose time the verdict gives (location such as 2.0s) is trimmed away when video.settings autoTrim is on: out point = cut − trimMargin if that leaves at least trimMinSeconds and the shot's dialogue ends before it (otherwise the head is cut when the jump is at the very start); the clip is selected with that trim (outcome trimmed, trimSkipped says why a trim was not possible) and nothing is billed. " \
                 "A clip released with qaOverride is only trimmed, never regenerated. " \
                 "Each attempt is recorded in the lessons store (qa.lessons; trims as remediationID #{ClipTrim::REMEDIATION_ID}). Finally selects the best take (pass first, then fewer block issues); if none passes it leaves a nextStep for a human unless video.settings remediateAutoOverride is on. " \
                 "categories narrows the issue types. dryRun returns the plan, the sentences that would be added and the compiled prompt without reviewing or submitting. Runs as a background job (jobID; follow with jobs.wait / jobs.status). #{REMEDIATE_COMMON}",
    inputSchema: { type: "object", properties: { jobID: { type: "string" }, dramaID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" },
                                                 maxRetries: { type: "integer", minimum: 0, maximum: QARemediation::MAX_RETRIES_LIMIT },
                                                 categories: { type: "array", items: { type: "string", enum: QARemediation::REGENERATE_CATEGORIES } },
                                                 dryRun: { type: "boolean" }, requestID: { type: "string" } } }
  },
  {
    name: "episode.remediate_videos",
    description: "video.remediate for every shot of one episode (or shotOrders) that has a completed clip: shots whose fresh verdict passes or has no remediable issue are skipped, shots without a verdict are reviewed first, and a clip the user released with qaOverride is left alone. Shots run concurrently; a shot whose earlier remediation was interrupted continues its retake chain (same derived requestIDs, nothing billed twice). categories and maxRetries as in video.remediate. #{BATCH_COMMON} #{REMEDIATE_COMMON}",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" }, shotOrders: { type: "array", items: { type: "integer" } },
                                                 maxRetries: { type: "integer", minimum: 0, maximum: QARemediation::MAX_RETRIES_LIMIT },
                                                 categories: { type: "array", items: { type: "string", enum: QARemediation::REGENERATE_CATEGORIES } },
                                                 dryRun: { type: "boolean" }, requestID: { type: "string" } },
                   required: %w[dramaID episodeID] }
  },
  {
    name: "qa.lessons",
    description: "The QA lessons store: every lesson (built-in remediation sentences per frame-QA category, the built-in prevention sentence, and manual lessons) with its enabled flag and stats {attempts, resolved, resolvedRate}; aggregate per category and remediation (also split by generation mode); the prevention sentences currently added to every single-shot video prompt (built-in ones, manual prevention lessons, and remediation sentences that resolved their issue in at least #{QARemediation::PREVENTION_MIN_ATTEMPTS} attempts with a rate of #{(QARemediation::PREVENTION_MIN_RATE * 100).round}% or more); and the remediation table. Filter by category or kind (video / image).",
    inputSchema: { type: "object", properties: { category: { type: "string" }, kind: { type: "string", enum: QALessons::KINDS } } }
  },
  {
    name: "qa.save_lesson",
    description: "Add or edit a manual lesson, or enable/disable any lesson. Built-in lessons (IDs starting with remediation/ or prevention/) only take enabled. A manual lesson has text, kind (video | image), category (a frame-QA category or general) and applyAs: remediation (a sentence automatic retakes may add for that category), prevention (added to every single-shot video prompt) or note (a rule for people and agents, never sent to a model — e.g. image side: \"multi-reference prompts must say which reference picture is whom\"). " \
                 "Prompt lessons (remediation / prevention) must describe the wanted picture positively: negations and words such as cut, blood, wound or text are refused (negative_wording) because models draw what a prompt mentions. Image-kind lessons are stored and listed for agents; image prompts do not consume them yet.",
    inputSchema: { type: "object", properties: { lessonID: { type: "string" }, text: { type: "string" }, kind: { type: "string", enum: QALessons::KINDS },
                                                 category: { type: "string", enum: [QALessons::GENERAL] + QARemediation::CATEGORIES },
                                                 applyAs: { type: "string", enum: QALessons::USES }, enabled: { type: "boolean" }, note: { type: "string" },
                                                 createdBy: { type: "string", enum: %w[user agent] } } }
  }
].freeze

# 候选图质检与推荐（0.40.0-rc1，docs/decisions/0008-candidate-image-qa.md）。
IMAGE_QA_TOOLS = [
  {
    name: "image.qa",
    description: "Check image candidates with the review model (the host model bridge, same routing as review.run): each candidate is shown with the reference images that were sent (in reference-legend order, the candidate last), the image prompt and the shot requirements. " \
                 "target and ids as in image.generate (characterID; episodeID + shotID for start/end; assetID for appearance/scene/sceneVariant/prop). candidateIDs (1 to #{ImageQA::MAX_CANDIDATES}) narrows it; by default every candidate of the target. Candidates with a fresh verdict are reused unless force: true. " \
                 "Each verdict is stored on the candidate as qa {status pass|warn|block, score 0-100, summary, issues[{category identity|wardrobe|text|brand|composition|aspect|other, level must|advice, detail}], basis, model, checkedAt}; candidates flagged aspectMismatch are blocked without a model call. " \
                 "A verdict goes stale (qa.stale in drama.get) when the frame prompt, the reference package or the shot requirements change. Returns checked[], failed[] (a failed check is unknown, never a pass) and recommendedCandidateID: the best fresh, non-blocked candidate by status then score (first on ties). #{ASYNC_DESCRIPTION}",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, target: { type: "string", enum: ImageQA::TARGETS }, characterID: { type: "string" },
                                                 episodeID: { type: "string" }, shotID: { type: "string" }, assetID: { type: "string" },
                                                 candidateIDs: { type: "array", items: { type: "string" }, maxItems: ImageQA::MAX_CANDIDATES },
                                                 castIDs: { type: "array", items: { type: "string" } }, force: { type: "boolean" }, async: { type: "boolean" }, requestID: { type: "string" } },
                   required: %w[dramaID target] }
  },
  {
    name: "episode.accept_recommended_frames",
    description: "Select the image-QA recommended start (or end) frame for every shot of an episode (or shotOrders) in one call (records selectedStartBy: recommendation). Shots that already have a selected frame are left alone unless replaceSelected: true. " \
                 "onlyPassing (default true) only accepts recommendations that passed or only carry advice notes; false also accepts a recommendation with must-fix notes (warn). Blocked candidates are never recommended. Returns items[] with state selected / skipped (reason no_recommendation, already_selected, has_selection, not_passing) / failed.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" }, target: { type: "string", enum: ImageQA::FRAME_TARGETS },
                                                 shotOrders: { type: "array", items: { type: "integer" } }, onlyPassing: { type: "boolean" }, replaceSelected: { type: "boolean" } },
                   required: %w[dramaID episodeID] }
  }
].freeze

ASSET_TOOLS = [
  {
    name: "drama.list_assets",
    description: "List a drama's reusable assets (appearance, scene, sceneVariant, prop, voice) as summaries: prompt, selected image, candidate count, review state and which shots reference them. Archived assets are hidden unless includeArchived is true.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, kind: { type: "string", enum: DramaService::ASSET_KINDS }, characterID: { type: "string" }, sceneID: { type: "string" }, includeArchived: { type: "boolean" } }, required: ["dramaID"] }
  },
  {
    name: "drama.get_asset",
    description: "Read one asset in full, candidates included, plus the shots that reference it.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, assetID: { type: "string" } }, required: %w[dramaID assetID] }
  },
  {
    name: "drama.save_asset",
    description: "Create or update a reusable asset. kind is required when creating and immutable afterwards. appearance and voice need characterID; sceneVariant needs sceneID; scene may set parentSceneID. Text fields (name, prompt, notes) are written as committed content with the previous version kept in history. Voice extras: language, dialect, referenceTranscript, providerVoiceID, presets [{name, instruction, speed}], consent {status pending|granted, grantedBy, note}; voice.generate refuses voices whose consent is not granted. Pass a stable requestID when creating so a retry does not create a twin.",
    inputSchema: {
      type: "object",
      properties: {
        dramaID: { type: "string" }, assetID: { type: "string" }, kind: { type: "string", enum: DramaService::ASSET_KINDS },
        name: { type: "string" }, prompt: { type: "string" }, notes: { type: "string" },
        characterID: { type: "string" }, category: { type: "string", enum: DramaService::APPEARANCE_CATEGORIES },
        parentSceneID: { type: ["string", "null"] }, sceneID: { type: "string" }, lighting: { type: "string" },
        language: { type: "string" }, dialect: { type: "string" }, referenceTranscript: { type: "string" }, providerVoiceID: { type: "string" },
        presets: { type: "array", items: { type: "object" } }, consent: { type: "object" },
        expectedRev: { type: "integer" }, requestID: { type: "string" }
      },
      required: ["dramaID"]
    }
  },
  {
    name: "drama.archive_asset",
    description: "Archive (or restore with archived: false) an asset. Assets are never deleted: shots that reference an archived asset keep resolving it, and drama.get_progress reports the reference so it can be rebound.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, assetID: { type: "string" }, archived: { type: "boolean" } }, required: %w[dramaID assetID] }
  },
  {
    name: "drama.record_asset_media",
    description: "Record an already generated or imported file inside the plugin media directory as an asset candidate (images for visual assets, audio for voices).",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, assetID: { type: "string" }, filePath: { type: "string" }, prompt: { type: "string" }, model: { type: "string" }, source: { type: "string", enum: %w[generated imported] }, durationMs: { type: "integer" } }, required: %w[dramaID assetID filePath] }
  },
  {
    name: "drama.select_asset_media",
    description: "Select the candidate that represents an asset (the image every shot referencing it will use, or the voice's reference audio).",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, assetID: { type: "string" }, candidateID: { type: "string" } }, required: %w[dramaID assetID candidateID] }
  },
  {
    name: "drama.set_reference_package",
    description: "Bind a shot to its scene (and variant), cast (each with optional appearance and voice, role lead|supporting|extra, screenPosition, appearanceChange note), props, and up to 3 reference videos (videoRefs [{jobID}] pointing at completed clips, for ref2va's video combination). Only references are stored; files are resolved when generating. generationMode auto|t2va|fl2va|ref2va; audioRetention fully_copy|reference decides whether the dialogue track is copied into the clip; excluded lists slot keys to leave out. Structural write: rev is not bumped.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, package: { type: "object" } }, required: %w[dramaID episodeID shotID package] }
  },
  {
    name: "drama.preview_reference_package",
    description: "Compile a shot's reference package without spending anything: ordered slots with files, dropped slots, warnings, the resolved generation mode and the PromptIR. purpose=image uses the host image reference limit; purpose=video uses the video provider's capabilities. Call before image.generate / video.generate to see exactly what will be sent.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, purpose: { type: "string", enum: ReferencePackage::PURPOSES }, mode: { type: "string", enum: ReferencePackage::MODES } }, required: %w[dramaID episodeID shotID] }
  },
  {
    name: "drama.select_dialogue_audio",
    description: "Select which generated audio candidate a dialogue line uses.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, lineID: { type: "string" }, candidateID: { type: "string" } }, required: %w[dramaID episodeID shotID lineID candidateID] }
  },
  {
    name: "drama.export_manifest",
    description: "Hand-off manifest for assembling the drama outside the plugin: per episode and shot, the selected frames, completed clip paths, dialogue audio with durations, the reference snapshot each clip was made with, review states and whether synthetic voice was used. Absolute paths are included.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" } }, required: ["dramaID"] }
  },
  {
    name: "voice.generate",
    description: "Synthesize speech with a voice asset. Preview: assetID plus text (result becomes a candidate on the voice). Dialogue: episodeID and shotID (optional lineIDs, at most 20 lines) — each line uses the voice bound in the shot's reference package or the speaker's consented voice, and the audio is attached to the line. presetID picks an expression preset. Requires a configured TTS backend (VIDEO_STUDIO_TTS_API_BASE / VIDEO_STUDIO_TTS_API_KEY) and a voice whose consent is granted. dryRun lists what would be spoken; requestID makes retries safe. For a whole episode use episode.dub. Each generated entry carries charsPerSecond (CJK characters, or words for Latin text, per second); when a voice's clips average below #{SpeechRate::LIMITS['chars'][:slow]} or above #{SpeechRate::LIMITS['chars'][:fast]} characters per second, warnings[] has speech_rate_slow / speech_rate_fast for that voice (normal dialogue is about 3.5 to 5): change providerVoiceID or the preset speed and dub again.#{ASYNC_DESCRIPTION}",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, assetID: { type: "string" }, text: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, lineIDs: { type: "array", items: { type: "string" } }, presetID: { type: "string" }, dryRun: { type: "boolean" }, requestID: { type: "string" }, async: { type: "boolean" } }, required: ["dramaID"] }
  },
  {
    name: "episode.compose_plan",
    description: "Plan the final cut of one episode: per shot the clip that will be used (the selected video, else the newest completed one), whether its dialogue is dubbed separately (tts) or taken from the generated video (video), each line's dub state (ready / missing / stale / no_voice / not_needed), warnings (stale_selection: the selected clip is older than a clip a batch generated later; trim_shorter_than_dialogue: the dub outlasts the trimmed clip and the last kept frame is held; trim_cuts_dialogue_head), " \
                 "the clip's trim (drama.set_clip_trim; with clipDurationMs and keptDurationMs), effectiveDurationMs per shot and estimatedDurationMs (their sum), qaReview as the effective frame verdict (scene_jump issues trimmed away do not count; verdictStatus and trimExcluded keep the original), " \
                 "blockers (also returned as blocking; exactly what episode.compose refuses on with compose_blocked) and composable, the episode's compose settings, ffmpeg / TTS / music availability and the latest compose job.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" } }, required: %w[dramaID episodeID] }
  },
  {
    name: "episode.save_compose_settings",
    description: "Save an episode's compose settings. settings.voiceSource: tts (dub dialogue with TTS, the clip's own audio lowered to originalVolume) or video (keep the dialogue the video model generated). settings.shotVoiceSource maps shotID to tts / video (null follows the episode). settings.originalVolume 0-1 (default 0.3). settings.pacing: picture (use each clip whole; default) or dialogue (cut each shot where its dialogue ends plus pacingTailSeconds, default 0.4; shots without dialogue keep at most pacingSilentMaxSeconds, default 2; shots marked holdFull or whose camera / summary says 留白 / 反应镜头 / 空镜 keep their full length; the plan shows the computed cut as paceTrim and video.generate requests only the needed duration). settings.shotPacing maps shotID to picture / dialogue (null follows the episode). settings.bgm: source none / acestep / file, prompt (instrumental mood), volume 0-1 (default 0.18).",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" }, settings: { type: "object" } }, required: %w[dramaID episodeID settings] }
  },
  {
    name: "episode.compose",
    description: "Start composing one episode in the background: dub missing or stale lines for tts shots, prepare background music, render every shot to one format with the dub mixed in, concatenate them in shot order, mix the music (ducked under speech) and write <output directory>/<drama>/第NN集-<title>.mp4. Returns the job immediately; poll episode.compose_status. Refuses while shots have no completed video; returns the running job instead of starting a second one.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" }, requestID: { type: "string" } }, required: %w[dramaID episodeID] }
  },
  {
    name: "episode.compose_status",
    description: "Compose and music jobs, newest first: state (queued / running / completed / failed / canceled), step, progress 0-100, message, warnings, error, and for completed composes the output path, duration and playback URL. Filter by jobID, dramaID and episodeID.",
    inputSchema: { type: "object", properties: { jobID: { type: "string" }, dramaID: { type: "string" }, episodeID: { type: "string" } } }
  },
  {
    name: "episode.compose_cancel",
    description: "Cancel a queued or running compose / music job and stop its background process.",
    inputSchema: { type: "object", properties: { jobID: { type: "string" } }, required: ["jobID"] }
  },
  {
    name: "episode.generate_music",
    description: "Generate the episode's instrumental background music in the background with the configured music backend (ACE-Step), from settings.bgm.prompt, sized to the episode's estimated length. Sets bgm.source to acestep. Returns a job to poll with episode.compose_status.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" }, requestID: { type: "string" } }, required: %w[dramaID episodeID] }
  },
  {
    name: "episode.import_music",
    description: "Use a local audio file (mp3 / wav / m4a / aac / flac / aiff, up to 200 MB) as the episode's background music (bgm.source becomes file). Pass path when the file is already chosen; without it the macOS file dialog opens.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" },
                                                 path: { type: "string", description: "Absolute path of an already chosen audio file. Omit to pick one with the macOS file dialog." } },
                   required: %w[dramaID episodeID] }
  },
  {
    name: "episode.reveal_output",
    description: "Reveal a composed episode file in Finder.",
    inputSchema: { type: "object", properties: { jobID: { type: "string" } }, required: ["jobID"] }
  },
  {
    name: "video.capabilities",
    description: "What the configured video provider supports (tasks t2va/fl2va/ref2va, reference limits, native audio) plus the image reference limit used when generating frames. Use it to plan a shot's reference package and generation mode.",
    inputSchema: { type: "object", properties: {} }
  },
  {
    name: "review.get_material",
    description: "The content-review material for one object without calling a model: label, text, image/video paths, the basis fingerprint, the review system prompt and the user message. Judge it yourself (look at the images with media.read), then store the verdict with the tool and arguments returned in writeBack. Works in any MCP client; review.run is the WillDeep-host convenience wrapper. For aspect panel (drama or episode: the expert panel) it also returns panel {experts[{name, role, system}], chair {system}} and context; system is a one-reviewer version that plays the whole panel and writes the chair's verdict, so either answer it yourself or run each expert and the chair as sub-agents, then store the verdict (issues may carry raisedBy; the optional panel[] array records each expert's stance).",
    inputSchema: {
      type: "object",
      properties: {
        scope: { type: "string", enum: ReviewRunner::SCOPES.keys }, aspect: { type: "string", enum: ReviewRunner::SCOPES.values.flatten.uniq },
        dramaID: { type: "string" }, characterID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, assetID: { type: "string" }, jobID: { type: "string" }
      },
      required: %w[scope aspect]
    }
  },
  {
    name: "media.read",
    description: "Return an image or audio file from the plugin media directory as an inline MCP content block, so candidates can be looked at without any file-reading tool. Pass fileName, or dramaID plus candidateID. Files above 6 MiB are refused with the path instead.",
    inputSchema: { type: "object", properties: { fileName: { type: "string" }, dramaID: { type: "string" }, candidateID: { type: "string" } } }
  }
].freeze

TOOLS = [
  {
    name: "drama.get_progress",
    description: "Where a short drama stands and what to do next. Returns per-character, per-asset, per-episode and per-shot readiness (adopted content only), review verdicts (reviewDetail on a node gives must / advice counts and whether a warn is acknowledged; the drama and each scripted episode also carry the expert panel verdict under review.panel while video.settings panelReview is on), a `consistency` summary from drama.check_consistency (bannedTerms, unknownSpeakers, facts, visualRules, total), continuity issues per shot (missing or archived package references, unexplained appearance changes, stale or over-long dialogue audio, reference limits), video job state per shot (linked by shotID), and an ordered nextSteps list (review blocks first, then warn verdicts with must-fix issues that nobody acknowledged, then unreviewed or stale content, then fix_consistency when the canon check found issues, then characters, identity images, assets, scripts, storyboards, frame prompts, start frames, continuity fixes, videos). maxSteps raises the default cap of 12. For dramas with more than 6 episodes each episode comes back as a summary; pass episodeOrder (or episodeID) to get one episode with per-shot detail and its own nextSteps. Call this instead of re-reading the whole drama with drama.get.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, maxSteps: { type: "integer", minimum: 1, maximum: DramaProgress::MAX_STEPS_LIMIT }, episodeOrder: { type: "integer", minimum: 1, description: "Focus on one episode: only it keeps per-shot detail and nextSteps are limited to it." }, episodeID: { type: "string", description: "Same as episodeOrder, by ID." }, detail: { type: "string", enum: ["compact", "full"], description: "Without a focus episode, dramas with more than 6 episodes list each episode as a summary (shot counts, frames, videos, QA blocks) unless detail is full." } }, required: ["dramaID"] }
  },
  {
    name: "drama.get_stage_context",
    description: "The writing brief for one creative stage: `system` (stage skill plus the content compliance guard; follow it), `context` (drama, cast, the series canon to obey — numbers, props, visual rules, banned terms, functional speakers — current object, neighbouring episodes or shots, asset catalogue), `canonRev`, `target` (the object with rev, draftRev and any pending draft), and `write` (which tool and fields to use). Stages: planning (dramaID optional), characters (characterID), episodes (optional from/to), script (episodeID), storyboard (episodeID), shot (episodeID, shotID), frames (episodeID, shotID), appearance (characterID or assetID), scene / prop (assetID optional), voice (characterID or assetID), package (episodeID, shotID). `common` adds genre skills from skill.list.",
    inputSchema: {
      type: "object",
      properties: {
        stage: { type: "string", enum: StageContext::STAGES },
        dramaID: { type: "string" }, characterID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, assetID: { type: "string" },
        from: { type: "integer" }, to: { type: "integer" },
        common: { type: "array", items: { type: "string" } }
      },
      required: ["stage"]
    }
  },
  {
    name: "review.run",
    description: "Run the content compliance pre-review on one object with the plugin's review model and store the verdict on it (the plugin page shows the same verdict). scope/aspect: drama/content; character/content or character/images (characterID); episode/content or episode/storyboard (episodeID); shot/images (episodeID, shotID); video/content (jobID, a completed clip after video.prepare_playback). Images and clips are sent to the model; for character/images and asset/images only the selected image is reviewed once one is selected (all candidates, up to 12, otherwise). Returns status pass/warn/block, summary and issues with suggestions; each issue has level must (likely fails platform review) or advice (optional). A block verdict must be fixed or shown to the user before spending more on that object. A warn with only advice issues is done; for a warn with must issues, fix it or, once the user accepts the risk, drama.acknowledge_review — do not re-run the review hoping for a cleaner verdict. This is an AI pre-check, not a platform decision. To review many objects use review.run_batch. " \
                 "aspect panel (drama/panel: the series framework with its canon; episode/panel: one script with its neighbours and canon) is the expert panel from the panel skill: every expert answers once in parallel, then the chair merges them into one verdict of the same shape (issues carry raisedBy, panel[] records each expert's stance). It makes experts + 1 model calls and takes minutes: always pass async: true and follow with jobs.wait. Its verdict settles like any other (advice-only warns are done, must issues can be acknowledged).#{ASYNC_DESCRIPTION}",
    inputSchema: {
      type: "object",
      properties: {
        scope: { type: "string", enum: ReviewRunner::SCOPES.keys },
        aspect: { type: "string", enum: ReviewRunner::SCOPES.values.flatten.uniq },
        dramaID: { type: "string" }, characterID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, assetID: { type: "string" },
        jobID: { type: "string" }, async: { type: "boolean" }, requestID: { type: "string", description: "With async: true, a retry with the same requestID returns the same job." }
      },
      required: %w[scope aspect]
    }
  },
  {
    name: "drama.list",
    description: "List local short-drama projects.",
    inputSchema: { type: "object", properties: {} }
  },
  {
    name: "drama.get",
    description: "Get one short-drama project with characters, episodes, shots, and frame candidates. The read-only `frame` field is the effective canvas: aspect, image (WxH for start/end frames and scene images), video {width,height}, legacy (old 9:16 dramas still render 2:3) and imageModels that can draw it. `canon` is the series canon (facts, props, visualRules, bannedTerms, allowedExtras, rev); write it with drama.save_canon. Image candidates may carry qa (image.qa verdict, with read-only stale); shots carry read-only recommendedStartID / recommendedEndID with startRecommendation / endRecommendation {candidateID, status, score, eligible}, characters and assets recommendedCandidateID (the best fresh, non-blocked candidate; null when none).",
    inputSchema: { type: "object", properties: { id: { type: "string" } }, required: ["id"] }
  },
  {
    name: "drama.confirm_plan",
    description: "Persist a structured short-drama planning card only after the user confirms it. Pass a stable requestID so a retry after a timeout returns the same drama instead of creating a duplicate. Keep tone (writing guidance for the writers, never sent to image or video models) apart from visualStyle (one visual-only sentence — light, colour, texture, lens — used verbatim as the style line of every video prompt).",
    inputSchema: { type: "object", properties: { plan: { type: "object" }, requestID: { type: "string" } }, required: ["plan"] }
  },
  {
    name: "drama.save_metadata",
    description: "Update editable drama metadata used by production planning, including aspect ratio and target duration per episode. Saving switches an old drama to the true ratio (9:16 frames 1080x1920, video 576x1024); see the drama's `frame` field.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, aspectRatio: { type: "string", enum: DramaService::ASPECT_RATIOS }, episodeDurationSeconds: { type: "integer", minimum: 15, maximum: 600 } }, required: %w[dramaID aspectRatio episodeDurationSeconds] }
  },
  {
    name: "drama.save_character",
    description: "Create or update a character identity specification.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, characterID: { type: "string" }, name: { type: "string" }, description: { type: "string" }, visualPrompt: { type: "string" }, identityVersion: { type: "integer" } }, required: ["dramaID", "name"] }
  },
  {
    name: "drama.save_episode",
    description: "Update an episode outline or script.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" }, title: { type: "string" }, summary: { type: "string" }, script: { type: "string" } }, required: ["dramaID", "episodeID"] }
  },
  {
    name: "drama.save_shot",
    description: "Create or update a storyboard shot, including editable dialogue and separate start/end frame prompts.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, order: { type: "integer" }, title: { type: "string" }, summary: { type: "string" }, dialogue: { type: "array", items: { type: "object" } }, actionStart: { type: "string" }, actionEnd: { type: "string" }, cameraIntent: { type: "string" }, duration: { type: "integer", description: "Seconds, 4 to 15 (video provider contract)." }, soundscape: { type: "string", description: "Ambient and action sound only, English, 1-4 sentences." }, music: { type: "string", description: "Off-screen score, or N/A." }, caption: { type: "string", description: "On-screen caption burned in at compose time over the start of the shot (e.g. a day count like 第 3 天), max 60 chars. Keep caption text out of startPrompt/endPrompt: image and video models paint it into the picture and video QA blocks it." }, holdFull: { type: "boolean", description: "A hold shot (empty frame, reaction, a beat of silence): under dialogue pacing the whole clip is kept instead of cutting where the dialogue ends." }, startPrompt: { type: "string" }, endPrompt: { type: "string" } }, required: ["dramaID", "episodeID"] }
  },
  {
    name: "drama.save_draft",
    description: "Write the working draft of one object. Pass expectedDraftRev to detect edits made since the draft was loaded; fields outside the per-scope whitelist are dropped. Pass commit: true to adopt the merged draft into committed content in the same write; the previous committed content is kept in version history. scope=drama accepts visualStyle, the visual-only look that video prompts use.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, scope: { type: "string", enum: DramaService::DRAFT_SCOPES }, characterID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, assetID: { type: "string" }, draft: { type: "object" }, expectedRev: { type: "integer" }, expectedDraftRev: { type: "integer" }, expectedContent: { type: "object", description: "Snapshot of all text fields for atomic conflict detection." }, expectedCanonRev: { type: "integer" }, commit: { type: "boolean" } }, required: %w[dramaID scope draft] }
  },
  {
    name: "drama.write_with_panel",
    description: "Write and revise an existing drama framework or one episode script in the background. Uses the stage skills, cast, canon and neighbouring episodes. An empty script gets a first draft; existing content is reviewed first (regenerate=true explicitly rewrites it). The expert panel reviews each saved revision, mandatory issues are revised up to maxRevisions (default 2, maximum 3), and every save preserves history. Result state ready means no mandatory panel issues remain; needs_human retains the latest text and unresolved verdict for user action. Does not alter cast, episode count, shots, assets or the canon. Existing pending drafts are refused and concurrent edits produce a revision conflict. Cancel with jobs.cancel; use jobs.status or jobs.wait for the result.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, scope: { type: "string", enum: %w[drama episode] }, episodeID: { type: "string" }, brief: { type: "string", maxLength: 4000 }, maxRevisions: { type: "integer", minimum: 0, maximum: 3 }, regenerate: { type: "boolean" }, requestID: { type: "string" } }, required: %w[dramaID scope] }
  },
  {
    name: "drama.record_review",
    description: "Store a content-compliance review verdict on one object without changing its revision. aspect: drama→content|panel; character→content|images; episode→content|storyboard|panel; shot→images; asset→content|images. basis is the fingerprint of the reviewed material (review.get_material returns it) so the page can tell when the review is stale. mediaImages/mediaVideos record how many files the reviewer actually saw. Each issue may carry level: must (likely fails platform review unless fixed) or advice (style, taste, optional); a missing or unknown level becomes must for block and advice for warn. For aspect panel each issue may carry raisedBy (the expert(s) who raised it) and panel is an optional array [{expert, role, state spoke|failed, status, summary, issueCount, mustFix, error}] recording each expert's stance. Storing a verdict replaces the previous one, including any acknowledgement.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, scope: { type: "string", enum: DramaService::DRAFT_SCOPES }, characterID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, assetID: { type: "string" }, aspect: { type: "string", enum: %w[content images storyboard panel] }, review: { type: "object" }, basis: { type: "string" }, model: { type: "string" }, mediaImages: { type: "integer" }, mediaVideos: { type: "integer" }, panel: { type: "array", items: { type: "object" } }, via: { type: "string", description: "Where a panel verdict came from, e.g. host_roundtable." }, roundtable: { type: "object", description: "{reportID, sessionID, rounds} of the host roundtable that produced a panel verdict." } }, required: %w[dramaID scope aspect review] }
  },
  {
    name: "video.record_review",
    description: "Store a content-compliance review verdict on one video job without changing its state. Issues may carry level must|advice (see drama.record_review).",
    inputSchema: { type: "object", properties: { id: { type: "string" }, aspect: { type: "string", enum: %w[content frames] }, review: { type: "object" }, basis: { type: "string" }, model: { type: "string" }, mediaImages: { type: "integer" }, mediaVideos: { type: "integer" } }, required: %w[id review] }
  },
  {
    name: "drama.acknowledge_review",
    description: "Acknowledge a stored warn verdict instead of re-running the review: its risks were read and accepted for now. Stored on the verdict as acknowledgement {acknowledgedBy, note, at, basis}; rev and content are unchanged. Only warn verdicts qualify (a block must be fixed, a pass needs nothing). It lapses on its own when the verdict is replaced by a new review or the content changes (basis differs), and drama.get_progress stops listing resolve_review_warn for it. Use acknowledgedBy=user only when the user said so in this conversation; agent when you accept advisory issues yourself. Pass basis (the verdict's basis you read) so you never acknowledge a newer verdict you have not seen; revoke=true removes the acknowledgement.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, scope: { type: "string", enum: DramaService::DRAFT_SCOPES }, characterID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, assetID: { type: "string" }, aspect: { type: "string", enum: %w[content images storyboard panel] }, acknowledgedBy: { type: "string", enum: ReviewRecord::ACKNOWLEDGERS }, note: { type: "string", maxLength: ReviewRecord::NOTE_LIMIT }, basis: { type: "string" }, revoke: { type: "boolean" } }, required: %w[dramaID scope aspect] }
  },
  {
    name: "video.acknowledge_review",
    description: "Acknowledge a stored warn verdict on one video job (aspect content or frames). Same rules as drama.acknowledge_review; the job state is unchanged.",
    inputSchema: { type: "object", properties: { id: { type: "string" }, aspect: { type: "string", enum: %w[content frames] }, acknowledgedBy: { type: "string", enum: ReviewRecord::ACKNOWLEDGERS }, note: { type: "string", maxLength: ReviewRecord::NOTE_LIMIT }, basis: { type: "string" }, revoke: { type: "boolean" } }, required: %w[id] }
  },
  {
    name: "drama.list_history",
    description: "List earlier committed versions of one object, newest first. Returns previews and lengths only; use drama.read_version for full content. scope=canon lists earlier series canons.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, scope: { type: "string", enum: DramaService::HISTORY_SCOPES }, characterID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, assetID: { type: "string" } }, required: %w[dramaID scope] }
  },
  {
    name: "drama.save_canon",
    description: "Save the drama's series canon (全剧设定账本), the facts every stage must agree on. Partial update: each list you pass replaces that list; lists you omit stay. facts: [{key, label, unit, keywords, values: [{fromDay | fromEpisode, value, note}]}] for numbers that evolve (stock, orders, debt, prices), values in story order; unit (e.g. 只) and keywords (e.g. 存栏, 点数) let drama.check_consistency recognise mentions. props: [{name, description, usage, assetID?}]. visualRules: [string or {rule, forbiddenPhrases}]. bannedTerms: [{term, replacement, reason}] (empty replacement = must not appear). allowedExtras: names of functional speakers without a character sheet (speakers otherwise come from the characters). Pass expectedRev (canon.rev from drama.get or canonRev from drama.get_stage_context); the previous canon goes to history (scope=canon). Change numbers or rules only when the story changed and the user agrees; never to make a script pass.",
    inputSchema: {
      type: "object",
      properties: {
        dramaID: { type: "string" }, expectedRev: { type: "integer" },
        canon: {
          type: "object",
          properties: {
            facts: { type: "array", items: { type: "object" } }, props: { type: "array", items: { type: "object" } },
            visualRules: { type: "array" }, bannedTerms: { type: "array" }, allowedExtras: { type: "array", items: { type: "string" } }
          }
        }
      },
      required: %w[dramaID canon]
    }
  },
  {
    name: "drama.check_consistency",
    description: "Scan the committed plan, characters, assets, episode titles / summaries / scripts and every shot field and dialogue line against the series canon, without calling a model. Returns summary counts and issues grouped by kind, each with a location (scope, episodeOrder, shotOrder, field, line or lineIndex, IDs) and an excerpt: bannedTerms (term found, with its replacement), unknownSpeakers (speaker not in the characters or canon.allowedExtras), facts (a number near a fact's unit or keywords that disagrees with the canon timeline at that story day — heuristic, confidence possible; read the excerpt before changing anything), visualRules (a rule's forbiddenPhrases found). Story days come from 第N天 in the scripts. Fix the content, or update the canon with drama.save_canon when the story really changed.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" } }, required: ["dramaID"] }
  },
  {
    name: "drama.read_version",
    description: "Read the full content of one history version.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, versionID: { type: "string" } }, required: %w[dramaID versionID] }
  },
  {
    name: "drama.restore_version",
    description: "Adopt a history version as committed content. The current committed content is saved to history first, and any pending draft is cleared (reported as discardedDraft). scope=canon restores an earlier series canon (expectedRev is canon.rev).",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, scope: { type: "string", enum: DramaService::HISTORY_SCOPES }, characterID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, assetID: { type: "string" }, versionID: { type: "string" }, expectedRev: { type: "integer" } }, required: %w[dramaID scope versionID] }
  },
  {
    name: "drama.list_shots",
    description: "List one episode's storyboard shots in a model-facing shape: the fields it needs to reason about and reference, without the frame-candidate arrays or selection state. Each shot carries the image-QA recommendation: recommendedStartID / recommendedEndID and startRecommendation / endRecommendation {candidateID, status, score, eligible} (null when nothing fresh and unblocked).",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" } }, required: %w[dramaID episodeID] }
  },
  {
    name: "drama.save_shot_drafts",
    description: "Write drafts for many shots at once, matched by shot order. Atomic: either every listed shot is written or none is. Shots whose order is not found are reported in `skipped` rather than dropped silently.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" }, create: { type: "boolean" }, commit: { type: "boolean" }, expectedEpisodeRev: { type: "integer" }, expectedEpisodeDraftRev: { type: "integer" }, expectedShots: { type: "array", items: { type: "object", properties: { id: { type: "string" }, rev: { type: "integer" }, draftRev: { type: "integer" } }, required: %w[id rev draftRev] } }, shots: { type: "array", items: { type: "object", properties: { order: { type: "integer" }, title: { type: "string" }, summary: { type: "string" }, actionStart: { type: "string" }, actionEnd: { type: "string" }, cameraIntent: { type: "string" }, soundscape: { type: "string" }, music: { type: "string" }, caption: { type: "string", description: "On-screen caption burned in at compose time over the start of the shot (e.g. a day count like 第 3 天), max 60 chars. Keep caption text out of startPrompt/endPrompt: image and video models paint it into the picture and video QA blocks it." }, holdFull: { type: "boolean", description: "Hold shot: keep the whole clip under dialogue pacing." }, startPrompt: { type: "string" }, endPrompt: { type: "string" }, dialogue: { type: "array", items: { type: "object" } }, duration: { type: "integer" } }, required: ["order"] } } }, required: %w[dramaID episodeID shots] }
  },
  {
    name: "drama.save_episode_drafts",
    description: "Write drafts for many episodes at once, matched by episode order. Atomic: either every listed episode is written or none is. Episodes whose order is not found are reported in `skipped` rather than dropped silently.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, commit: { type: "boolean" }, episodes: { type: "array", items: { type: "object", properties: { order: { type: "integer" }, title: { type: "string" }, summary: { type: "string" }, script: { type: "string" } }, required: ["order"] } } }, required: %w[dramaID episodes] }
  },
  {
    name: "drama.commit_episode_drafts",
    description: "Adopt the working drafts of many episodes at once. Atomic: one write for the whole batch. Episodes whose script would still be empty after adopting are left untouched, draft included, and reported in `skipped`. Episodes without a draft are neither touched nor reported. Pass `orders` to limit the batch to those episode numbers.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, orders: { type: "array", items: { type: "integer" } } }, required: %w[dramaID] }
  },
  {
    name: "drama.commit_draft",
    description: "Adopt the working draft into committed content, clear the draft, and bump rev. A second call with the same expectedRev returns revision_conflict, which callers should read as 'already committed'.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, scope: { type: "string", enum: DramaService::DRAFT_SCOPES }, characterID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, assetID: { type: "string" }, expectedRev: { type: "integer" }, expectedDraftRev: { type: "integer" } }, required: %w[dramaID scope] }
  },
  {
    name: "drama.discard_draft",
    description: "Throw away the working draft of one object and leave committed content untouched.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, scope: { type: "string", enum: DramaService::DRAFT_SCOPES }, characterID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, assetID: { type: "string" }, expectedDraftRev: { type: "integer" } }, required: %w[dramaID scope] }
  },
  {
    name: "skill.list",
    description: "List the creative skills that drive each stage's system prompt, plus the optional common skills a user can attach. Returns metadata only; body comes from skill.get.",
    inputSchema: { type: "object", properties: {} }
  },
  {
    name: "skill.get",
    description: "Read one creative skill's markdown body. Falls back to the bundled template and materialises a user copy on first read.",
    inputSchema: { type: "object", properties: { id: { type: "string" } }, required: ["id"] }
  },
  {
    name: "skill.save",
    description: "Overwrite one creative skill's markdown body. The bundled template is never touched, so skill.reset can always restore it.",
    inputSchema: { type: "object", properties: { id: { type: "string" }, body: { type: "string" } }, required: %w[id body] }
  },
  {
    name: "skill.reset",
    description: "Drop the user copy of one creative skill and fall back to the bundled template.",
    inputSchema: { type: "object", properties: { id: { type: "string" } }, required: ["id"] }
  },
  {
    name: "skill.compose",
    description: "Build the system prompt for one stage: the stage skill first, then the selected common skills appended in order.",
    inputSchema: { type: "object", properties: { stage: { type: "string", enum: CreativeSkillStore::STAGE_SKILLS.keys }, common: { type: "array", items: { type: "string" } } }, required: ["stage"] }
  },
  {
    name: "chat.append",
    description: "Append one co-creation chat message for a drama stage. Messages carry the object they were about and the draft revision they were sent against.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, stage: { type: "string", enum: ChatStore::STAGES }, message: { type: "object" } }, required: %w[dramaID stage message] }
  },
  {
    name: "chat.list",
    description: "Read the persisted co-creation chat for a drama stage.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, stage: { type: "string", enum: ChatStore::STAGES } }, required: %w[dramaID stage] }
  },
  {
    name: "chat.clear",
    description: "Clear the persisted co-creation chat for a drama stage.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, stage: { type: "string", enum: ChatStore::STAGES } }, required: %w[dramaID stage] }
  },
  {
    name: "drama.record_image",
    description: "Record an image returned by the host some.im image bridge as a frame candidate.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, kind: { type: "string", enum: %w[start end] }, mediaURL: { type: "string" }, filePath: { type: "string" }, prompt: { type: "string" }, model: { type: "string", enum: DramaService::IMAGE_MODELS } }, required: %w[dramaID episodeID shotID kind filePath prompt model] }
  },
  {
    name: "drama.record_character_image",
    description: "Record a some.im image as a character identity candidate.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, characterID: { type: "string" }, mediaURL: { type: "string" }, filePath: { type: "string" }, prompt: { type: "string" }, model: { type: "string", enum: DramaService::IMAGE_MODELS } }, required: %w[dramaID characterID filePath prompt model] }
  },
  {
    name: "image.generate",
    description: "Generate image candidates and record them on the drama. target=character draws identity candidates from the adopted visualPrompt; target=start/end draws frame candidates from the adopted shot prompt with the shot's compiled reference package (identity images, bound appearances, scene, props; castIDs restricts the cast); target=appearance/scene/sceneVariant/prop (assetID) draws reference images for an asset. Backend: the WillDeep host when it advertises image generation, otherwise the direct image API configured by VIDEO_STUDIO_IMAGE_API_BASE/KEY. At most 4 images per call (models x countPerModel); each image is billed. dryRun returns the prompt and references without generating; a stable requestID makes a retry return the already recorded candidates. Pick a result with drama.select_character_image / drama.select_image / drama.select_asset_media. Start/end frames and scene/sceneVariant images are drawn at the drama's aspect ratio (drama.frame.image) so they match the video; character, appearance and prop images stay 1024x1536. 9:16, 16:9 and 4:5 can only be drawn with nano-banana-2 (drama.frame.imageModels lists what works; old 9:16 dramas stay legacy 2:3); other models return image_model_aspect_unsupported. Character, appearance and prop prompts get a fixed studio framing line appended (full-body front view / single centred subject, plain light-grey seamless background, soft even lighting); dryRun shows the final prompt. Start/end frames, appearance and sceneVariant images that attach references start with a Chinese reference legend (图1 是「name」的长相参考——age/gender excerpt; 图2 the appearance; …) in exactly the order of the reference paths; dryRun returns it as referenceLegend and numbers referenceSummary[].index to match. A shot whose package has an empty cast attaches characters named in the shot text plus the appearance bound to them in the nearest other shot of the episode (warning cast_inferred); pass castIDs: [] for no characters. When a start/end/scene/sceneVariant image comes back in the wrong orientation for drama.frame.image, the candidate is still recorded but flagged aspectMismatch (with actualSize/requestedSize), listed in warnings[] (code aspect_mismatch) and counted in aspectMismatchCount; regenerate those instead of selecting them. Failures: each failed[] entry has code, message and retryable (true for timeouts, gateway 5xx/504, 429; false otherwise). code quota_exhausted means the image quota or balance is used up (upstreamQuota true = the platform's upstream quota, false = this account's own); the remaining images of the call are skipped (stopped.skipped) and, when nothing was generated, the call fails with image_quota_exhausted; do not retry until someone tops up. When nothing was generated for other reasons the call fails with image_generation_failed and error.retryable tells whether retrying later can help. For a whole episode's frames use episode.generate_frames.#{ASYNC_DESCRIPTION}",
    inputSchema: {
      type: "object",
      properties: {
        dramaID: { type: "string" },
        target: { type: "string", enum: ImageGeneration::TARGETS },
        characterID: { type: "string", description: "Required when target is character." },
        episodeID: { type: "string", description: "Required when target is start or end." },
        shotID: { type: "string", description: "Required when target is start or end." },
        assetID: { type: "string", description: "Required when target is an asset kind." },
        models: { type: "array", items: { type: "string", enum: DramaService::IMAGE_MODELS }, description: "Defaults to [\"nano-banana-2\"]." },
        countPerModel: { type: "integer", enum: ImageGeneration::COUNTS, description: "Images per model, default 2." },
        castIDs: { type: "array", items: { type: "string" }, description: "Characters whose identity images to attach to a frame. Omit to use the shot's reference package (or characters named in the shot)." },
        dryRun: { type: "boolean" }, requestID: { type: "string" }, async: { type: "boolean" },
        autoQA: { type: "boolean", description: "Check the new candidates with image.qa (default video.settings imageAutoQA, needs the host model bridge). A synchronous call queues the check as a background job and returns qa {state: queued, jobID}; with async: true the same job checks them and returns qa {checked, recommendedCandidateID}. The result also carries the target's current recommendedCandidateID." },
        extraDirectives: { type: "array", items: { type: "string" }, maxItems: ImageGeneration::EXTRA_DIRECTIVES_LIMIT,
                           description: "Positive sentences added after the prompt body (before the framing line), e.g. to correct a blocked candidate: \"画面中的「许禾」是27岁女性，长相与图1一致\". Negations and words such as text or cut are refused (negative_wording)." }
      },
      required: %w[dramaID target]
    }
  },
  {
    name: "drama.select_character_image",
    description: "Select the canonical identity image for a character.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, characterID: { type: "string" }, candidateID: { type: "string" } }, required: %w[dramaID characterID candidateID] }
  },
  {
    name: "drama.select_image",
    description: "Select a start or end frame candidate for a storyboard shot (records selectedStartAt / selectedStartBy: user, or the End pair). A frame chosen here after an episode.generate_frames batch started is not replaced by that batch's automatic selection.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, kind: { type: "string", enum: %w[start end] }, candidateID: { type: "string" } }, required: %w[dramaID episodeID shotID kind candidateID] }
  },
  {
    name: "drama.select_video",
    description: "Mark one completed video candidate as the selected take for a storyboard shot (records selectedVideoAt and selectedVideoBy: user). " \
                 "A clip chosen here after an episode.generate_videos / remediation batch started is not overridden by that batch; selecting the current clip again acknowledges a stale_selection warning. Selecting a different clip drops the shot's trim (drama.set_clip_trim), which belongs to the previous clip.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, videoID: { type: "string" }, qaOverride: { type: "boolean" }, qaOverrideBy: { type: "string", enum: DramaService::QA_OVERRIDE_SOURCES } }, required: %w[dramaID episodeID shotID videoID] }
  },
  {
    name: "drama.set_clip_trim",
    description: "Trim the clip a storyboard shot uses (its selected video, else its newest completed one): inSeconds and/or outSeconds on that clip's own timeline (omit one to keep the start or the end), or clear: true. " \
                 "The trim is stored on the shot for that clip only (shot.trim {jobID, inSeconds, outSeconds, source: user, reason}) and is dropped when another clip is selected. " \
                 "episode.compose cuts the clip to it (picture and the clip's own sound together; a dialogue track the clip was generated from is cut the same way so lips stay in sync; a longer dub still freezes the last kept frame). " \
                 "For the compose gate and remediation, frame-QA scene_jump issues located in the cut-away part no longer count (the stored verdict itself is unchanged). " \
                 "jobID, when given, must be the clip the shot uses (trim_not_selected otherwise: select it with drama.select_video first). Keeps at least #{ClipTrim::MANUAL_MIN_SECONDS} s; points must fall inside the clip (trim_out_of_range). " \
                 "Returns {trim, clipDurationMs, keptDurationMs, framesQA {verdictStatus, effectiveStatus, excluded}}.",
    inputSchema: { type: "object", properties: { dramaID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" }, jobID: { type: "string" },
                                                 inSeconds: { type: "number", minimum: 0 }, outSeconds: { type: "number", minimum: 0 }, clear: { type: "boolean" },
                                                 reason: { type: "string" } },
                   required: %w[dramaID episodeID shotID] }
  },
  {
    name: "video.status",
    description: "Return the safe Video Studio provider configuration and local job counts. Never returns credentials.",
    inputSchema: { type: "object", properties: {} }
  },
  {
    name: "video.create_draft",
    description: "Save a video idea as a draft. Use this for chat-selection capture before generation.",
    inputSchema: {
      type: "object",
      properties: {
        text: { type: "string", description: "Video idea or prompt." },
        title: { type: "string" },
        referenceImagePath: { type: "string" },
        source: { type: "string" }
      },
      required: ["text"]
    }
  },
  {
    name: "video.list",
    description: "List locally persisted video drafts and asynchronous jobs.",
    inputSchema: {
      type: "object",
      properties: {
        status: { type: "string", enum: %w[all draft active completed failed] },
        query: { type: "string" }
      }
    }
  },
  {
    name: "video.generate",
    description: "Submit an asynchronous video job. With dramaID, episodeID and shotID the shot's reference package is compiled against the provider's capabilities (video.capabilities): the mode is resolved (auto|t2va|fl2va|ref2va), ref2va uses either the bound reference videos or the selected start frame plus the shot's dialogue audio track (lines are concatenated into one WAV), prompt defaults to the compiled MiniMax-H3 structured prompt (a plain sentence you pass is inserted into it; a prompt that already has the section names is sent as is), and the job records the shot and the reference snapshot so drama.get_progress can match it. Without a shot: omitting referenceImagePath uses t2va, providing it uses fl2va. The compiled prompt carries the prevention sentences from the QA lessons store (by default \"One continuous take from a single camera position…\"; skipped when the shot's cameraIntent asks for a cut, transition or multiple cameras) plus any extraDirectives; dryRun lists them in directives / directivesSkipped. dryRun returns the request without submitting; a stable requestID makes a retry return the existing job instead of billing twice.",
    inputSchema: {
      type: "object",
      properties: {
        draftID: { type: "string" },
        dramaID: { type: "string" }, episodeID: { type: "string" }, shotID: { type: "string" },
        mode: { type: "string", enum: ReferencePackage::MODES },
        dryRun: { type: "boolean" }, requestID: { type: "string" },
        extraDirectives: { type: "array", maxItems: VideoService::MAX_EXTRA_DIRECTIVES,
                           items: { anyOf: [{ type: "string" }, { type: "object", properties: { text: { type: "string" }, category: { type: "string" }, id: { type: "string" } }, required: ["text"] }] },
                           description: "Shot path only: extra positive sentences added next to the prevention sentences in the compiled prompt (what video.remediate adds per QA issue). Describe the wanted picture; never negations." },
        title: { type: "string" },
        prompt: { type: "string" },
        negativePrompt: { type: "string" },
        referenceImagePath: { type: "string", description: "Readable local PNG, JPEG, or WebP path." },
        model: { type: "string", description: "Defaults to MiniMax-H3." },
        duration: { type: "integer", minimum: 4, maximum: 15, description: "Seconds, 4 to 15 (provider contract; 4 is the floor)." },
        width: { type: "integer", description: "Multiple of 32." },
        height: { type: "integer", description: "Multiple of 32." },
        inferenceSteps: { type: "integer", minimum: 1, maximum: 100 },
        seed: { type: "integer" },
        source: { type: "string" }
      },
      required: ["prompt"]
    }
  },
  {
    name: "video.refresh",
    description: "Poll one remote video task and download the MP4 when completed and auto-download is enabled.",
    inputSchema: { type: "object", properties: { id: { type: "string" } }, required: ["id"] }
  },
  {
    name: "video.refresh_active",
    description: "Poll up to twenty active video jobs once. It does not create background or scheduled work.",
    inputSchema: { type: "object", properties: {} }
  },
  {
    name: "video.retry",
    description: "Explicitly submit a new attempt using a finished job's stored request. The original job is preserved.",
    inputSchema: { type: "object", properties: { id: { type: "string" } }, required: ["id"] }
  },
  {
    name: "video.download",
    description: "Download a completed video or retry a failed download without creating a new paid generation task.",
    inputSchema: { type: "object", properties: { id: { type: "string" } }, required: ["id"] }
  },
  {
    name: "video.remove",
    description: "Move a non-running video job to the drama recycle bin without deleting its downloaded video file.",
    inputSchema: { type: "object", properties: { id: { type: "string" } }, required: ["id"] }
  },
  {
    name: "video.restore",
    description: "Restore a video job from the recycle bin.",
    inputSchema: { type: "object", properties: { id: { type: "string" } }, required: ["id"] }
  },
  {
    name: "video.settings",
    description: "Read or update creative-assistant routing and default video parameters. API credentials remain in plugin settings and are never returned.",
    inputSchema: {
      type: "object",
      properties: {
        assistantProviderID: { type: ["string", "null"] },
        assistantModel: { type: ["string", "null"] },
        reviewProviderID: { type: ["string", "null"], description: "Review model provider; null follows the assistant routing." },
        reviewModel: { type: ["string", "null"] },
        uiLocale: { type: "string", enum: %w[zh-Hans en], description: "Language the plugin page uses; review material labels follow it." },
        videoModel: { type: "string" },
        duration: { type: "integer", minimum: 4, maximum: 15 },
        width: { type: "integer" },
        height: { type: "integer" },
        inferenceSteps: { type: "integer" },
        autoDownload: { type: "boolean" },
        autoQA: { type: "boolean" },
        autoRemediate: { type: "boolean", description: "With autoQA, episode.generate_videos reviews each finished clip and remediates it (video.remediate) automatically. Default true." },
        remediateMaxRetries: { type: "integer", minimum: 0, maximum: QARemediation::MAX_RETRIES_LIMIT, description: "Retakes per shot for automatic remediation. Default 2." },
        remediateCategories: { type: "array", items: { type: "string", enum: QARemediation::REGENERATE_CATEGORIES }, description: "Frame-QA categories remediated automatically. Default scene_jump, identity, text_overlay." },
        remediateAutoOverride: { type: "boolean", description: "When no take passes, release the best one with qaOverride (by auto) instead of leaving it to a person. Default false." },
        autoTrim: { type: "boolean", description: "Remediation trims a clip just before a frame-QA scene_jump instead of regenerating it, when the part before the cut is long enough and the shot's dialogue fits (or cuts the head when the jump is at the very start). Default true." },
        trimMinSeconds: { type: "number", minimum: ClipTrim::MIN_SECONDS_RANGE.begin, maximum: ClipTrim::MIN_SECONDS_RANGE.end, description: "Shortest clip an automatic trim may leave. Default #{ClipTrim::DEFAULT_MIN_SECONDS}." },
        trimMargin: { type: "number", minimum: ClipTrim::MARGIN_RANGE.begin, maximum: ClipTrim::MARGIN_RANGE.end, description: "Seconds an automatic trim keeps away from the cut. Default #{ClipTrim::DEFAULT_MARGIN}." },
        qaConcurrency: { type: "integer", minimum: 1, maximum: VideoStore::QA_CONCURRENCY_LIMIT,
                         description: "Frame reviews (host model calls) that batch jobs run at the same time across the whole plugin. Default 2." },
        videoConcurrency: { type: "integer", minimum: 1, maximum: VideoStore::VIDEO_CONCURRENCY_LIMIT,
                            description: "Clips that batch jobs (episode.generate_videos, remediation retakes) keep generating at the same time across the whole plugin; a shot waits for a free slot before it is submitted. Default 6 (the provider runs about 7 to 8 per account)." },
        imageAutoQA: { type: "boolean", description: "Check every new image candidate with the review model (image.qa) after image.generate and in episode.generate_frames. Default true. Uses qaConcurrency slots." },
        imageAutoSelect: { type: "boolean", description: "episode.generate_frames selects each shot's recommended new frame when it passed or only has advice notes. Default true." },
        imageRetryOnBlock: { type: "integer", minimum: 0, maximum: VideoStore::IMAGE_RETRY_ON_BLOCK_LIMIT,
                             description: "Extra rounds episode.generate_frames draws for a shot whose candidates were all blocked, with positive remediation sentences. Default 1." },
        panelReview: { type: "boolean", description: "List the expert panel (review aspect panel) for the plan and every scripted episode in drama.get_progress and review.run_batch. Default true. review.run aspect=panel works either way." },
        panelRoundtable: { type: "boolean", description: "When the WillDeep host offers willdeep/roundtable/run (1.412.0 or later), run the expert panel as a real roundtable in the host's roundtable page (each expert speaks in turn, visible to the user; more model calls). Default true. Off: the plugin asks the experts in parallel itself." },
        panelRounds: { type: "integer", minimum: 1, maximum: VideoStore::PANEL_ROUNDS_LIMIT, description: "Discussion rounds for the host roundtable. Default 1." }
      }
    }
  },
  {
    name: "video.pick_reference",
    description: "Open the macOS file picker and return a local PNG, JPEG, or WebP path.",
    inputSchema: { type: "object", properties: {} }
  },
  {
    name: "video.prepare_playback",
    description: "Mirror a completed job's MP4 into the plugin media folder and extract a first-frame poster so the plugin page can play it inline. Idempotent: a job that is already mirrored is returned unchanged.",
    inputSchema: { type: "object", properties: { id: { type: "string" } }, required: ["id"] }
  },
  {
    name: "video.reveal_output",
    description: "Reveal a completed job's downloaded MP4 in Finder.",
    inputSchema: { type: "object", properties: { id: { type: "string" } }, required: ["id"] }
  }
].concat(ASSET_TOOLS).concat(JOB_TOOLS).concat(QA_TOOLS).concat(IMAGE_QA_TOOLS).map { |tool| READ_ONLY_TOOLS.include?(tool[:name]) ? tool.merge(annotations: { readOnlyHint: true }) : tool }
 .map { |tool| ToolI18n.annotate(tool) }.freeze

def async?(arguments)
  arguments["async"] == true
end

# transport：jobs.wait 经 stdio 时等得更短（宿主 10 秒内等不到应答就结束插件进程）。
# drama.* 返回整部剧（drama / dramas）时挂上候选图推荐与过期标记（0.40.0-rc1），读写回包同一个形状。
def call_tool(name, arguments, transport = :stdio)
  result = dispatch_tool(name, arguments, transport)
  name.start_with?("drama.") ? with_image_qa(result) : result
end

def dispatch_tool(name, arguments, transport)
  case name
  when "system.status" then { "ok" => true, "version" => SERVER_VERSION, "packageRoot" => File.realpath(File.expand_path("..", __dir__)), "pid" => Process.pid, "parentPID" => Process.ppid }
  when "image.qa" then BACKGROUND.image_qa(arguments)
  when "episode.accept_recommended_frames" then IMAGE_QA.accept_recommended(arguments)
  when "drama.get_progress" then DRAMA_PROGRESS.get_progress(arguments)
  when "drama.get_stage_context" then STAGE_CONTEXT.get(arguments)
  when "review.run" then async?(arguments) ? BACKGROUND.review_run(arguments) : REVIEW_RUNNER.run(arguments)
  when "review.run_batch" then BACKGROUND.review_batch(arguments)
  when "episode.generate_frames" then BACKGROUND.generate_frames(arguments)
  when "episode.dub" then BACKGROUND.dub(arguments)
  when "episode.generate_videos" then BACKGROUND.generate_videos(arguments)
  when "jobs.status" then BACKGROUND.status(arguments)
  when "jobs.wait" then BACKGROUND.wait(arguments, transport: transport)
  when "jobs.cancel" then BACKGROUND.cancel(arguments)
  when "video.remediate" then BACKGROUND.remediate(arguments)
  when "episode.remediate_videos" then BACKGROUND.remediate_batch(arguments)
  when "qa.lessons" then QA_LESSONS.summary(arguments)
  when "qa.save_lesson" then QA_LESSONS.save(arguments)
  when "drama.list" then DRAMA_SERVICE.list
  when "drama.get" then DRAMA_SERVICE.get(arguments)
  when "drama.confirm_plan" then DRAMA_SERVICE.confirm_plan(arguments)
  when "drama.save_metadata" then DRAMA_SERVICE.save_metadata(arguments)
  when "drama.save_character" then DRAMA_SERVICE.save_character(arguments)
  when "drama.save_episode" then DRAMA_SERVICE.save_episode(arguments)
  when "drama.save_shot" then DRAMA_SERVICE.save_shot(arguments)
  when "drama.save_draft" then DRAMA_SERVICE.save_draft(arguments)
  when "drama.write_with_panel" then BACKGROUND.write_with_panel(arguments)
  when "drama.record_review" then DRAMA_SERVICE.record_review(arguments)
  when "video.record_review" then SERVICE.record_review(arguments)
  when "drama.acknowledge_review" then DRAMA_SERVICE.acknowledge_review(arguments)
  when "video.acknowledge_review" then SERVICE.acknowledge_review(arguments)
  when "drama.list_history" then DRAMA_SERVICE.list_history(arguments)
  when "drama.save_canon" then DRAMA_SERVICE.save_canon(arguments)
  when "drama.check_consistency" then DRAMA_SERVICE.check_consistency(arguments)
  when "drama.read_version" then DRAMA_SERVICE.read_version(arguments)
  when "drama.restore_version" then DRAMA_SERVICE.restore_version(arguments)
  when "drama.list_shots" then list_shots_with_recommendations(arguments)
  when "drama.save_shot_drafts" then DRAMA_SERVICE.save_shot_drafts(arguments)
  when "drama.save_episode_drafts" then DRAMA_SERVICE.save_episode_drafts(arguments)
  when "drama.commit_episode_drafts" then DRAMA_SERVICE.commit_episode_drafts(arguments)
  when "drama.commit_draft" then DRAMA_SERVICE.commit_draft(arguments)
  when "drama.discard_draft" then DRAMA_SERVICE.discard_draft(arguments)
  when "skill.list" then creative_skill_list
  when "skill.get" then creative_skill_get(arguments)
  when "skill.save" then creative_skill_save(arguments)
  when "skill.reset" then creative_skill_reset(arguments)
  when "skill.compose" then creative_skill_compose(arguments)
  when "chat.append" then chat_append(arguments)
  when "chat.list" then chat_list(arguments)
  when "chat.clear" then chat_clear(arguments)
  when "drama.record_image" then DRAMA_SERVICE.record_image(arguments)
  when "drama.select_image" then DRAMA_SERVICE.select_image(arguments)
  when "drama.select_video" then DRAMA_SERVICE.select_video(arguments)
  when "drama.set_clip_trim" then ClipTrim.apply(DRAMA_SERVICE, SERVICE.jobs_with_media, arguments)
  when "drama.record_character_image" then DRAMA_SERVICE.record_character_image(arguments)
  when "drama.select_character_image" then DRAMA_SERVICE.select_character_image(arguments)
  when "image.generate" then async?(arguments) ? BACKGROUND.image_generate(arguments) : BACKGROUND.image_generate_now(arguments)
  when "video.status" then SERVICE.status
  when "video.create_draft" then SERVICE.create_draft(arguments)
  when "video.list" then SERVICE.list(arguments)
  when "video.generate" then SERVICE.generate(arguments)
  when "video.refresh" then SERVICE.refresh(arguments)
  when "video.refresh_active" then SERVICE.refresh_active
  when "video.retry" then SERVICE.retry(arguments)
  when "video.download" then SERVICE.download(arguments)
  when "video.remove" then SERVICE.remove(arguments)
  when "video.restore" then SERVICE.restore(arguments)
  when "video.settings" then SERVICE.settings(arguments)
  when "video.pick_reference" then SERVICE.pick_reference
  when "video.prepare_playback" then SERVICE.prepare_playback(arguments)
  when "video.reveal_output" then SERVICE.reveal_output(arguments)
  when "video.capabilities" then SERVICE.capabilities
  when "drama.list_assets" then DRAMA_SERVICE.list_assets(arguments)
  when "drama.get_asset" then asset_with_image_qa(arguments)
  when "drama.save_asset" then DRAMA_SERVICE.save_asset(arguments)
  when "drama.archive_asset" then DRAMA_SERVICE.archive_asset(arguments)
  when "drama.record_asset_media" then DRAMA_SERVICE.record_asset_media(arguments)
  when "drama.select_asset_media" then DRAMA_SERVICE.select_asset_media(arguments)
  when "drama.set_reference_package" then DRAMA_SERVICE.set_reference_package(arguments)
  when "drama.preview_reference_package" then preview_reference_package(arguments)
  when "drama.select_dialogue_audio" then DRAMA_SERVICE.select_dialogue_audio(arguments)
  when "drama.export_manifest" then export_manifest(arguments)
  when "voice.generate" then async?(arguments) ? BACKGROUND.voice_generate(arguments) : VOICE_GENERATION.generate(arguments)
  when "episode.compose_plan" then EPISODE_COMPOSER.plan(arguments)
  when "episode.save_compose_settings" then EPISODE_COMPOSER.save_settings(arguments)
  when "episode.compose" then EPISODE_COMPOSER.compose(arguments)
  when "episode.compose_status" then EPISODE_COMPOSER.status(arguments)
  when "episode.compose_cancel" then EPISODE_COMPOSER.cancel(arguments)
  when "episode.generate_music" then EPISODE_COMPOSER.generate_music(arguments)
  when "episode.import_music" then EPISODE_COMPOSER.import_music(arguments)
  when "episode.reveal_output" then EPISODE_COMPOSER.reveal_output(arguments)
  when "review.get_material" then REVIEW_RUNNER.material(arguments)
  when "media.read" then MediaReader.read(DRAMA_SERVICE, arguments)
  else { "ok" => false, "error" => { "code" => "unknown_tool", "message" => "Unknown tool #{name}" } }
  end
end

# 工具结果：一律带一条 text 内容块（JSON 载荷）；工具另给了 content（media.read 的
# 图片 / 音频块）时附在前面，载荷里不重复塞 base64。
def tool_result(payload)
  blocks = []
  if payload.is_a?(Hash) && payload["content"].is_a?(Array)
    blocks.concat(payload["content"])
    payload = payload.reject { |key, _| key == "content" }
  end
  blocks << { type: "text", text: JSON.generate(payload) }
  { content: blocks }
end

UnknownMethod = Class.new(StandardError)

def respond_error(request, code, message)
  return unless request.is_a?(Hash) && request["id"]

  HOST.write_line(JSON.generate(jsonrpc: "2.0", id: request["id"], error: { code: code, message: message }))
end

# 主循环经 HOST 取行、经 HOST 写行：stdin 只有 HOST 的读线程在读（反向请求的
# 响应按 id 交回发起方），stdout 只经它加锁写——HTTP 工作线程也会发反向请求。
# 早先的 ARGF.each_line 还会把命令行参数当文件名读，宿主从不传参。
def dispatch_request(request, transport: :stdio)
  result = case request["method"]
           when "initialize"
             # 宿主能力与媒体根只认 stdio 那头的宿主。HTTP 客户端（Claude Code、
             # Codex……）共用同一个进程，它的 initialize 不带宿主扩展；若也记下来，
             # 会清空宿主宣告的反向请求，代管出图 / 审核随即对页面和所有客户端失效。
             if transport == :stdio
               HOST.record_initialize(request["params"])
               # 宿主决定媒体根与 URL 前缀：Web 宿主下切到同源的 plugin-media/。
               MEDIA_HOST.record_initialize(request["params"])
               # 连接文件写在这个宿主的网关读的位置；HTTP 客户端的 initialize 不挪它。
               publish_http_endpoint(request["params"])
             end
             {
               protocolVersion: "2025-06-18",
               capabilities: { tools: {} },
               serverInfo: { name: "video-studio", version: SERVER_VERSION }
             }
           when "tools/list"
             { tools: TOOLS }
           when "tools/call"
             arguments = request.dig("params", "arguments") || {}
             tool_result(call_tool(request.dig("params", "name"), arguments, transport))
           else
             raise UnknownMethod, request["method"].to_s
           end
  { jsonrpc: "2.0", id: request["id"], result: result }
end

def handle_line(line)
  request = JSON.parse(line)
  return unless request["id"]

  HOST.write_line(JSON.generate(dispatch_request(request)))
rescue JSON::ParserError
  warn "video-studio: invalid JSON-RPC request"
rescue UnknownMethod => error
  respond_error(defined?(request) ? request : nil, -32_601, "Method not found: #{error.message}")
rescue VideoStore::Unavailable, DramaStore::Unavailable, ChatStore::Unavailable, CreativeSkillStore::Unavailable, BackgroundJobStore::Unavailable => error
  # 存档读不出来时刻意不静默重建：写入是全量覆写，一次误判就会把用户的
  # 短剧和任务记录抹平。把原因如实告诉调用方，让人去看那个文件。
  warn "video-studio: #{error.class}: #{error.message}"
  respond_error(
    defined?(request) ? request : nil,
    -32_002,
    "Local Video Studio archive could not be read; it was left untouched. #{error.message}"
  )
rescue StandardError => error
  # 只打 error.class 的话，线上报障时 stderr 全部内容就是一行
  # "video-studio: NoMethodError"，没有文件也没有行号。
  warn "video-studio: #{error.class}: #{error.message}"
  Array(error.backtrace).first(5).each { |line| warn "video-studio:   #{line}" }
  respond_error(defined?(request) ? request : nil, -32_000, "Video Studio internal error")
end

def handle_http_request(request)
  payload = JSON.parse(request.body)
  return HTTP_SERVER.complete(request, { status: 202, body: {} }) unless payload.is_a?(Hash) && payload["id"]

  response = dispatch_request(payload, transport: :http)
  session_id = payload["method"] == "initialize" ? SecureRandom.uuid : request.headers["mcp-session-id"]
  headers = session_id ? { "Mcp-Session-Id" => session_id, "MCP-Protocol-Version" => "2025-06-18" } : {}
  HTTP_SERVER.complete(request, { status: 200, body: response, headers: headers }, session_id: session_id)
rescue JSON::ParserError
  HTTP_SERVER.complete(request, { status: 400, body: { "error" => "Invalid JSON-RPC body." } })
rescue UnknownMethod => error
  HTTP_SERVER.complete(request, { status: 200, body: { jsonrpc: "2.0", id: payload && payload["id"], error: { code: -32_601, message: "Method not found: #{error.message}" } } })
rescue StandardError => error
  warn "video-studio: HTTP MCP dispatch failed: #{error.class}: #{error.message}"
  HTTP_SERVER.complete(request, { status: 200, body: { jsonrpc: "2.0", id: payload && payload["id"], error: { code: -32_000, message: "Video Studio internal error" } } })
end

# HTTP 入口的调用在自己的工作线程里逐个执行：出图、审核动辄一两分钟，放在主线程
# 会挡住宿主的请求，而宿主等不到 startup_timeout_sec 就结束插件进程。
HTTP_WORKER = if HTTP_SERVER
                Thread.new do
                  while (request = HTTP_SERVER.pop_request)
                    handle_http_request(request)
                  end
                end
              end

HOST.start
# 起后台工作线程，并把上一个进程没做完的任务标成 interrupted（见 lib/job_runner.rb）。
JOB_RUNNER.start
begin
  while (line = HOST.next_line)
    handle_line(line)
  end
ensure
  HTTP_SERVER&.close
end
