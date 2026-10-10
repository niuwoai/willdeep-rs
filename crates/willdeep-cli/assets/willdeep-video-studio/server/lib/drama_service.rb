# frozen_string_literal: true

require "json"
require "securerandom"
require "time"
require_relative "creative_schema"
require_relative "review_record"
require_relative "media_host"
require_relative "media_ref"
require_relative "reference_package"
require_relative "aspect_ratios"
require_relative "drama_canon"
require_relative "consistency_check"
require_relative "video_selection"
require_relative "speech_rate"

class DramaService
  IMAGE_MODELS = %w[gpt-image-2 nano-banana-2].freeze
  FRAME_KINDS = %w[start end].freeze
  QA_OVERRIDE_SOURCES = %w[user agent auto].freeze
  # 首尾帧选定的来源与字段（0.40.0-rc1）：user（drama.select_image）、batch（episode.generate_frames 自动选定）、
  # recommendation（episode.accept_recommended_frames）。
  FRAME_SELECTION_SOURCES = %w[user batch recommendation].freeze
  FRAME_SELECTION_KEYS = {
    "start" => { id: "selectedStartID", at: "selectedStartAt", by: "selectedStartBy" },
    "end" => { id: "selectedEndID", at: "selectedEndAt", by: "selectedEndBy" }
  }.freeze
  # 候选上记的出图尺寸（aspectMismatch 时的 actualSize / requestedSize）。
  MEDIA_SIZE_PATTERN = /\A\d{1,5}x\d{1,5}\z/.freeze
  # 可选画幅（9:16 16:9 1:1 4:5）。取自 schemas/aspect-ratios-v1.json，旧剧专用的 2:3 不在其中。
  ASPECT_RATIOS = AspectRatios::SELECTABLE_IDS
  # 早于画幅字段的旧剧，format 写着横屏时的画幅。
  LANDSCAPE_ASPECT = "16:9"

  # 资产类型（设计稿 4.1）。角色不是资产：它留在 characters 里，字段不迁移。
  ASSET_KINDS = %w[appearance scene sceneVariant prop voice].freeze
  APPEARANCE_CATEGORIES = %w[wardrobe hair makeup age state].freeze
  CONSENT_STATUSES = %w[pending granted].freeze
  MAX_ASSETS = 300
  MAX_PRESETS = 20

  # 一次批量最多写多少集。二十集是「一批」的合理规模，也够一次请求的输出
  # 预算装下；更多的话模型会开始偷工，每集只剩一句空话。
  MAX_BATCH_EPISODES = 30

  # 一次批量最多写多少镜。一集二十镜上下，三十给足余量。
  MAX_BATCH_SHOTS = 40

  # 找不到目标时抛出，message 即对外错误码。刻意不复用 ArgumentError：
  # positive / integer 这类转换也会抛 ArgumentError，混在一起会把
  # "参数格式不对" 误报成 "找不到对象"。
  NotFound = Class.new(StandardError)

  # 自动流程选片时用（select_video 的 gate / skip_if_same）：抛出即放弃这次写入。
  # SelectionKept 的 message 是不换的原因码。
  SelectionKept = Class.new(StandardError)
  SelectionUnchanged = Class.new(StandardError)
  TrimRefused = Class.new(StandardError)

  # 版本对不上。跨调用（先 get 再 save）没有锁能拦，只能靠对象自己的修订号。
  # 带上当前值，前端才好告诉用户「你手上这版已经旧了」而不是干抛一句失败。
  class Conflict < StandardError
    attr_reader :expected, :actual, :field

    def initialize(field, expected, actual)
      @field = field
      @expected = expected
      @actual = actual
      super("revision_conflict")
    end
  end

  # 允许走草稿的字段，以及各自的长度上限。白名单在这里而不是靠提示词：
  # 模型返回整个对象，超出这张表的键一律丢掉，它就改不动候选图、ID 和
  # 状态这些不该由文字生成来动的东西。
  DRAFT_TEXT_FIELDS = {
    "drama" => {
      "title" => 160, "genre" => 80, "format" => 80, "logline" => 1_200,
      "coreConflict" => 1_200, "audience" => 300, "tone" => 300, "arc" => 4_000,
      # 画面风格：成片提示词风格行的唯一来源（lib/reference_package.rb#style_line）。
      "visualStyle" => 300
    }.freeze,
    "character" => { "name" => 120, "description" => 2_000, "visualPrompt" => 8_000 }.freeze,
    "episode" => { "title" => 160, "summary" => 2_000, "script" => 20_000 }.freeze,
    "shot" => {
      "title" => 160, "summary" => 2_000, "actionStart" => 2_000, "actionEnd" => 2_000,
      "cameraIntent" => 1_000, "startPrompt" => 8_000, "endPrompt" => 8_000,
      # H3 三段式 / 六段式提示词的后两段：环境与动作音、画外配乐（0.26.0）。
      "soundscape" => 1_000, "music" => 500,
      # 画面字幕（0.41.0-rc1）：合成时叠在这一镜开头的字（如「第 3 天」），不进出图 / 视频提示词。
      "caption" => 60
    }.freeze,
    # 资产的文字字段：名称、生成用描述、连续性规则。与角色一样走草稿与历史；
    # 候选、选定、归档、绑定关系不归文字生成管。
    "asset" => { "name" => 120, "prompt" => 8_000, "notes" => 4_000 }.freeze
  }.freeze

  DRAFT_SCOPES = DRAFT_TEXT_FIELDS.keys.freeze

  NOT_FOUND_MESSAGES = {
    "drama_not_found" => "Short drama was not found.",
    "episode_not_found" => "Episode was not found.",
    "shot_not_found" => "Storyboard shot was not found.",
    "character_not_found" => "Character was not found.",
    "candidate_not_found" => "Image candidate was not found.",
    "asset_not_found" => "Asset was not found.",
    "line_not_found" => "Dialogue line was not found.",
    "version_not_found" => "That history version was not found for this object."
  }.freeze

  # 各作用域找不到对象时的错误码。
  NOT_FOUND_KEYS = {
    "drama" => "drama_not_found", "character" => "character_not_found",
    "episode" => "episode_not_found", "shot" => "shot_not_found", "asset" => "asset_not_found",
    "canon" => "drama_not_found"
  }.freeze

  ASSET_DEFAULTS = {
    "kind" => "", "name" => "", "prompt" => "", "notes" => "", "archived" => false,
    "candidates" => [], "selectedCandidateID" => nil,
    "rev" => 0, "draftRev" => 0, "draft" => nil
  }.freeze

  # 各类资产的专有字段与默认值。
  ASSET_KIND_DEFAULTS = {
    "appearance" => { "characterID" => "", "category" => "wardrobe" },
    "scene" => { "parentSceneID" => nil },
    "sceneVariant" => { "sceneID" => "", "lighting" => "" },
    "prop" => {},
    "voice" => {
      "characterID" => "", "language" => "zh-CN", "dialect" => "", "referenceTranscript" => "",
      "providerVoiceID" => "", "presets" => [],
      "consent" => { "status" => "pending", "grantedBy" => "", "confirmedAt" => nil, "note" => "" }
    }
  }.freeze

  CONFLICT_MESSAGE = "This object changed since you loaded it. Reload and review before saving again."


  CHARACTER_DEFAULTS = {
    "name" => "", "description" => "", "visualPrompt" => "",
    "identityVersion" => 1, "candidates" => [], "selectedCandidateID" => nil,
    "rev" => 0, "draftRev" => 0, "draft" => nil
  }.freeze

  SHOT_DEFAULTS = {
    "order" => 1, "title" => "", "summary" => "", "dialogue" => [],
    "actionStart" => "", "actionEnd" => "", "cameraIntent" => "", "duration" => 4,
    "soundscape" => "", "music" => "", "caption" => "",
    # 留白镜头（0.43.0-rc1）：配音节奏下保留全长，不按台词说完的时刻剪。
    "holdFull" => false,
    "startPrompt" => "", "endPrompt" => "", "startCandidates" => [], "endCandidates" => [],
    "selectedStartID" => nil, "selectedEndID" => nil, "selectedVideoID" => nil,
    "rev" => 0, "draftRev" => 0, "draft" => nil
  }.freeze

  # 宿主生成的图片只会落在这个目录里（AgentPluginGeneratedMediaResolver）。
  # 模型递上来的 filePath 一律收敛到这里，否则它可以把用户任意一张本地
  # 图片当作 referenceImagePath 送去第三方视频接口。
  #
  # history 可以不给（旧调用方和一部分测试就不给），给了才会在每次覆盖正式
  # 内容之前留一版快照。
  def initialize(store:, media_root:, history: nil)
    @store = store
    # 给了 MediaHost 就用它的活跃根；给字符串（旧调用方、单测）按 macOS 媒体根算。
    @media_host = media_root.respond_to?(:root) ? media_root : MediaHost.new(desktop_root: media_root)
    @history = history
  end

  attr_reader :media_host

  # 活跃媒体根：Web 宿主下切到 plugin-media/，见 lib/media_host.rb。
  def media_root
    @media_host.root
  end

  def list
    { "ok" => true, "dramas" => @store.list.map { |drama| decorate(drama) } }
  end

  def get(arguments)
    drama = @store.find(arguments["id"])
    drama ? { "ok" => true, "drama" => decorate(drama) } : failure("drama_not_found")
  end

  # 一部剧实际生效的画幅 id（画幅表里的一档）。旧剧（没有 aspectSpec）的 9:16
  # 按 2:3 算，与升级前出图 1024x1536、成片 768x1152 一致。
  def effective_aspect(drama)
    return AspectRatios::DEFAULT_ID unless drama.is_a?(Hash)

    AspectRatios.effective_id(stored_aspect_ratio(drama), drama["aspectSpec"])
  end

  # 附在剧上的只读字段 `frame`：首尾帧 / 场景图出图尺寸、成片尺寸、能出这一档的
  # 出图模型。页面与 Agent 都读它，不各算一遍。
  def frame(drama)
    aspect = effective_aspect(drama)
    {
      "aspect" => aspect,
      "image" => AspectRatios.image_size(aspect),
      "video" => AspectRatios.video_size(aspect),
      "legacy" => aspect == AspectRatios::LEGACY_ID,
      "imageModels" => AspectRatios.image_models(aspect)
    }
  end

  def save_metadata(arguments)
    guarded do
      ratio = arguments["aspectRatio"].to_s
      return failure("invalid_aspect_ratio", "Aspect ratio must be one of #{ASPECT_RATIOS.join(', ')}.") unless ASPECT_RATIOS.include?(ratio)

      duration = Integer(arguments["episodeDurationSeconds"])
      return failure("invalid_episode_duration", "Episode duration must be between 15 and 600 seconds.") unless duration.between?(15, 600)

      drama = mutate_drama(arguments) do |item|
        # 只有画幅真的换了才按真实比例解释。页面保存时画幅和时长一起提交，旧剧只改
        # 时长也会把原来的 9:16 原样带回来——这时写 aspectSpec，会把整部剧已画好的
        # 2:3 首尾帧一下子判成「比例不对」，违背「旧剧保持 2:3」。
        item["aspectSpec"] = AspectRatios::SPEC_VERSION if ratio != stored_aspect_ratio(item)
        item["aspectRatio"] = ratio
        item["episodeDurationSeconds"] = duration
      end
      { "ok" => true, "drama" => drama }
    rescue ArgumentError, TypeError
      failure("invalid_episode_duration", "Episode duration must be between 15 and 600 seconds.")
    end
  end

  # ---- 资产（设计稿第 4、10 节）----

  # 资产摘要列表。带引用镜头数：归档前要知道有没有镜头还在用它。
  def list_assets(arguments)
    guarded do
      drama = @store.find(arguments["dramaID"])
      raise NotFound, "drama_not_found" unless drama
      decorate(drama)
      references = asset_references(drama)
      kind = arguments["kind"].to_s
      character_id = arguments["characterID"].to_s
      scene_id = arguments["sceneID"].to_s
      include_archived = arguments["includeArchived"] == true
      assets = Array(drama["assets"]).select do |asset|
        next false if !kind.empty? && asset["kind"] != kind
        next false if !character_id.empty? && asset["characterID"] != character_id
        next false if !scene_id.empty? && asset["sceneID"] != scene_id && asset["parentSceneID"] != scene_id
        include_archived || !asset["archived"]
      end
      { "ok" => true, "assets" => assets.map { |asset| asset_summary(asset, references) } }
    end
  end

  def get_asset(arguments)
    guarded do
      drama = @store.find(arguments["dramaID"])
      raise NotFound, "drama_not_found" unless drama
      decorate(drama)
      asset = Array(drama["assets"]).find { |entry| entry["id"] == arguments["assetID"].to_s }
      raise NotFound, "asset_not_found" unless asset
      { "ok" => true, "asset" => asset, "referencedBy" => asset_references(drama).fetch(asset["id"], []) }
    end
  end

  # 建或改一个资产。文字字段直接写正式内容并留历史（与 save_character 同一条路），
  # 关系字段校验目标存在；kind 建后不可改。
  #
  # requestID：新建时去重——模型超时重试不该多出一个一模一样的资产。
  def save_asset(arguments)
    guarded do
      request_id = clipped(arguments["requestID"], 200)
      pending = []
      warnings = []
      created = nil
      drama = mutate_drama(arguments) do |item|
        item["assets"] = [] unless item["assets"].is_a?(Array)
        unless request_id.empty? || !arguments["assetID"].to_s.empty?
          existing = item["assets"].find { |entry| entry["requestID"] == request_id }
          if existing
            created = existing
            next
          end
        end
        asset = find_asset_or_build(item, arguments, request_id)
        created = asset
        check_revision(asset, arguments["expectedRev"])
        pending << version_of("asset", asset)
        assign(asset, "name", arguments["name"], 120)
        assign(asset, "prompt", arguments["prompt"], 8_000)
        assign(asset, "notes", arguments["notes"], 4_000)
        apply_asset_fields(item, asset, arguments, warnings)
        raise NotFound, "invalid_asset" if asset["name"].to_s.strip.empty?
        asset["updatedAt"] = Time.now.utc.iso8601
      end
      remember(arguments["dramaID"], pending)
      asset = drama["assets"].find { |entry| entry["id"] == created["id"] }
      { "ok" => true, "drama" => drama, "asset" => asset, "warnings" => warnings,
        "deduplicated" => !request_id.empty? && arguments["assetID"].to_s.empty? && pending.empty? }
    end
  end

  # 归档或恢复。不提供删除：被镜头引用的资产删了，参考包就指向一个不存在的 ID。
  # 归档后仍能被已有的参考包解析，只是选择器里不再出现；进度会报出来。
  def archive_asset(arguments)
    guarded do
      archived = arguments["archived"] != false
      referenced = []
      drama = mutate_drama(arguments) do |item|
        asset = find_asset(item, arguments["assetID"])
        asset["archived"] = archived
        asset["updatedAt"] = Time.now.utc.iso8601
        referenced = asset_references(item).fetch(asset["id"], [])
      end
      { "ok" => true, "drama" => drama, "referencedBy" => referenced }
    end
  end

  # 登记一条已经生成好的图片或音频为资产候选。声音资产只收音频，其它只收图片。
  def record_asset_media(arguments)
    file_path = generated_media_path(arguments["filePath"])
    return failure("invalid_media_path", media_path_message) unless file_path

    guarded do
      candidate = nil
      drama = mutate_drama(arguments) do |item|
        asset = find_asset(item, arguments["assetID"])
        media_type = MediaRef.type_of(file_path)
        raise NotFound, "invalid_media_type" if (asset["kind"] == "voice") != (media_type == "audio")
        candidate = MediaRef.build(file_path, media_root, prompt: clipped(arguments["prompt"], 8_000), model: clipped(arguments["model"], 160),
                                   media_type: media_type, source: arguments["source"].to_s.empty? ? "generated" : arguments["source"],
                                   extra: { "durationMs" => positive_or_nil(arguments["durationMs"]), "requestID" => request_token(arguments) }
                                            .merge(aspect_flags(arguments)).merge(speech_rate_fields(arguments)))
        asset["candidates"] = [] unless asset["candidates"].is_a?(Array)
        asset["candidates"] << candidate
        asset["updatedAt"] = Time.now.utc.iso8601
      end
      { "ok" => true, "drama" => drama, "candidate" => MediaRef.decorate!(candidate, media_root) }
    end
  end

  def select_asset_media(arguments)
    guarded do
      drama = mutate_drama(arguments) do |item|
        asset = find_asset(item, arguments["assetID"])
        selected = Array(asset["candidates"]).find { |entry| entry["id"] == arguments["candidateID"].to_s }
        raise NotFound, "candidate_not_found" unless selected
        asset["selectedCandidateID"] = selected["id"]
        asset["updatedAt"] = Time.now.utc.iso8601
      end
      { "ok" => true, "drama" => drama }
    end
  end

  # 写镜头参考包（设计稿 5.1）。只存绑定与排除项；引用的资产必须存在且类型对得上。
  # 结构性写入：不动 rev、不进历史，与 select_image 一致。
  def set_reference_package(arguments)
    guarded do
      raw = arguments["package"]
      return failure("invalid_package", "package must be an object.") unless raw.is_a?(Hash)

      warnings = []
      drama = mutate_shot(arguments) do |shot, item|
        shot["package"] = normalize_package(item, raw, warnings)
      end
      { "ok" => true, "drama" => drama, "warnings" => warnings }
    end
  end

  def select_dialogue_audio(arguments)
    guarded do
      drama = mutate_shot(arguments) do |shot|
        line = Array(shot["dialogue"]).find { |entry| entry.is_a?(Hash) && entry["id"] == arguments["lineID"].to_s }
        raise NotFound, "line_not_found" unless line
        audio = line["audio"].is_a?(Hash) ? line["audio"] : {}
        selected = Array(audio["candidates"]).find { |entry| entry["id"] == arguments["candidateID"].to_s }
        raise NotFound, "candidate_not_found" unless selected
        audio["selectedCandidateID"] = selected["id"]
        line["audio"] = audio
      end
      { "ok" => true, "drama" => drama }
    end
  end

  # 把一条已生成的音频挂到台词上（voice.generate 用）。
  def record_dialogue_audio(arguments)
    file_path = generated_media_path(arguments["filePath"])
    return failure("invalid_media_path", media_path_message) unless file_path

    guarded do
      candidate = nil
      drama = mutate_shot(arguments) do |shot|
        line = Array(shot["dialogue"]).find { |entry| entry.is_a?(Hash) && entry["id"] == arguments["lineID"].to_s }
        raise NotFound, "line_not_found" unless line
        candidate = MediaRef.build(file_path, media_root, prompt: clipped(line["text"], 2_000), model: clipped(arguments["model"], 160), media_type: "audio",
                                   extra: { "voiceAssetID" => arguments["voiceAssetID"].to_s, "presetID" => arguments["presetID"].to_s,
                                            "textFingerprint" => clipped(arguments["textFingerprint"], 64), "durationMs" => positive_or_nil(arguments["durationMs"]),
                                            "requestID" => request_token(arguments) }.merge(speech_rate_fields(arguments)))
        audio = line["audio"].is_a?(Hash) ? line["audio"] : { "candidates" => [], "selectedCandidateID" => nil }
        audio["candidates"] = [] unless audio["candidates"].is_a?(Array)
        audio["candidates"] << candidate
        # 第一条自动选定：批量生成一集对白时没人会逐句点。
        audio["selectedCandidateID"] ||= candidate["id"]
        line["audio"] = audio
      end
      { "ok" => true, "drama" => drama, "candidate" => MediaRef.decorate!(candidate, media_root) }
    end
  end

  def confirm_plan(arguments)
    plan = arguments["plan"]
    return failure("invalid_plan", "A structured short-drama plan is required.") unless plan.is_a?(Hash)
    return failure("missing_title", "The short drama title is required.") if plan["title"].to_s.strip.empty?
    return failure("invalid_plan", "Plan fields do not match creative-v1.") unless CreativeSchema.valid?("plan", plan)
    # 模型超时重试不该多出一部一模一样的短剧。requestID 由调用方给，
    # 没给就退回原来的「每次都新建」。
    request_id = clipped(arguments["requestID"], 200)
    unless request_id.empty?
      existing = @store.find_by_request_id(request_id)
      return { "ok" => true, "drama" => existing, "deduplicated" => true } if existing
    end
    { "ok" => true, "drama" => @store.create(plan, request_id: request_id) }
  end

  def save_character(arguments)
    guarded do
      pending = []
      drama = mutate_drama(arguments) do |item|
        character = find_or_build(item["characters"], arguments["characterID"],
                                  CHARACTER_DEFAULTS, "character_not_found")
        check_revision(character, arguments["expectedRev"])
        pending << version_of("character", character)
        assign(character, "name", arguments["name"], 120)
        assign(character, "description", arguments["description"], 2_000)
        assign(character, "visualPrompt", arguments["visualPrompt"], 8_000)
        unless arguments["identityVersion"].nil?
          character["identityVersion"] = [arguments["identityVersion"].to_i, 1].max
        end
      end
      remember(arguments["dramaID"], pending)
      { "ok" => true, "drama" => drama }
    end
  end

  def save_episode(arguments)
    guarded do
      pending = []
      drama = mutate_episode(arguments) do |episode|
        check_revision(episode, arguments["expectedRev"])
        pending << version_of("episode", episode)
        assign(episode, "title", arguments["title"], 160)
        assign(episode, "summary", arguments["summary"], 2_000)
        assign(episode, "script", arguments["script"], 20_000)
      end
      remember(arguments["dramaID"], pending)
      { "ok" => true, "drama" => drama }
    end
  end

  # 一次写多集的草稿。
  #
  # 批量丰富十到二十集时，逐集调 saveDraft 有两个问题：每次都是「读全量、改、
  # 全量覆写」加一次文件锁，二十集就是二十轮；而且中途失败会留下一半写进去
  # 一半没写的状态，用户看不出哪几集是新的。这里在一次 mutate 里写完，
  # 要么全成要么全不成。
  #
  # 按 order 对齐而不是按 episodeID：模型手上只有集号，让它记住二十个 UUID
  # 既不现实也没必要。集号找不到的条目跳过并在结果里报出来，不静默丢——
  # 静默丢会让用户以为二十集都写好了。
  #
  # `commit` 为真时写完就采用（0.20.0 起前端的 AI 批量出稿走这条路）：格式已经
  # 校验过，再让用户逐集点一次「采用草稿」只是重复劳动。采用前会把旧正文留一
  # 版历史，退路在历史记录里，不在那颗按钮上。
  def save_episode_drafts(arguments)
    guarded do
      entries = arguments["episodes"]
      return failure("invalid_episodes", "An array of episode drafts is required.") unless entries.is_a?(Array)
      return failure("empty_episodes", "No episode drafts were provided.") if entries.empty?
      return failure("invalid_episodes", "Episode fields do not match creative-v1.") unless entries.all? { |entry| CreativeSchema.valid?("episode", entry) }

      commit = arguments["commit"] == true
      applied = []
      committed = []
      skipped = []
      pending = []
      drama = mutate_drama(arguments) do |item|
        by_order = {}
        item["episodes"].each { |episode| by_order[episode["order"].to_i] = episode }

        entries.first(MAX_BATCH_EPISODES).each do |entry|
          next skipped << { "reason" => "not_an_object" } unless entry.is_a?(Hash)
          order = entry["order"].to_i
          episode = by_order[order]
          next skipped << { "order" => order, "reason" => "episode_not_found" } unless episode

          draft = episode["draft"].is_a?(Hash) ? deep_copy(episode["draft"]) : {}
          touched = false
          DRAFT_TEXT_FIELDS.fetch("episode").each do |key, limit|
            next unless entry.key?(key)
            draft[key] = clipped(entry[key], limit)
            touched = true
          end
          next skipped << { "order" => order, "reason" => "no_writable_field" } unless touched

          episode["draft"] = draft
          episode["draftRev"] = episode["draftRev"].to_i + 1
          applied << order

          next unless commit
          # 采用后正文会变空的集留在草稿态，理由与 commit_episode_drafts 一致：
          # 那种采用等于把这一集唯一一份内容清掉。
          resulting = draft.key?("script") ? draft["script"] : episode["script"]
          next skipped << { "order" => order, "reason" => "script_would_be_empty" } if resulting.to_s.strip.empty?

          pending << version_of("episode", episode)
          adopt_draft(episode, "episode", item)
          committed << order
        end
      end
      remember(arguments["dramaID"], pending)
      { "ok" => true, "drama" => drama, "applied" => applied, "committed" => committed, "skipped" => skipped }
    end
  end

  # 一次采用多集的草稿。
  #
  # 逐集点「采用草稿」在二十四集的工程里是二十四次切集加二十四次点击，而
  # 每一次都是一轮「读全量、改、全量覆写」。这里一把锁里走完，要么全写要么
  # 全不写。
  #
  # 正文会变空的集**不采用**：草稿里没有 script、这一集的正式正文也还是空的
  # 时候，采用下去的效果是正式内容照样没有正文，而那份草稿被一并清掉——用户
  # 手上就此什么都不剩。这不是保守，是这条路真的吃掉过一集正文。跳过的集连
  # 草稿一起留在原地，并在 `skipped` 里报出集号，让调用方说得出是哪几集。
  #
  # 没有草稿的集既不动也不报：二十四集里二十三集没草稿是常态，逐条报出来
  # 只会把真正需要看的那几行埋掉。
  def commit_episode_drafts(arguments)
    guarded do
      orders = arguments["orders"]
      return failure("invalid_orders", "Orders must be an array of episode numbers.") unless orders.nil? || orders.is_a?(Array)
      wanted = orders&.map { |value| value.to_i }

      applied = []
      skipped = []
      pending = []
      drama = mutate_drama(arguments) do |item|
        candidates = item["episodes"].select do |episode|
          draft = episode["draft"]
          next false unless draft.is_a?(Hash) && !draft.empty?
          wanted.nil? || wanted.include?(episode["order"].to_i)
        end

        candidates.first(MAX_BATCH_EPISODES).each do |episode|
          order = episode["order"].to_i
          draft = episode["draft"]
          # 采用后的正文：草稿给了就是草稿那份，没给就还是正式那份。
          resulting = draft.key?("script") ? draft["script"] : episode["script"]
          next skipped << { "order" => order, "reason" => "script_would_be_empty" } if resulting.to_s.strip.empty?

          pending << version_of("episode", episode)
          adopt_draft(episode, "episode", item)
          applied << order
        end
      end
      remember(arguments["dramaID"], pending)
      { "ok" => true, "drama" => drama, "applied" => applied, "skipped" => skipped }
    end
  end

  def save_shot(arguments)
    guarded do
      pending = []
      drama = mutate_episode(arguments) do |episode, item|
        # 第一个分镜落库就算进入制作。早先 status 只会是 planning，首页那两个
        # 状态筛选点了必然是空的。
        item["status"] = "production" if item["status"] == "planning"
        shot = find_or_build(episode["shots"], arguments["shotID"], SHOT_DEFAULTS, "shot_not_found")
        check_revision(shot, arguments["expectedRev"])
        pending << version_of("shot", shot)
        shot["order"] = positive(arguments["order"], episode["shots"].length) unless arguments["order"].nil?
        assign(shot, "title", arguments["title"], 160)
        assign(shot, "summary", arguments["summary"], 2_000)
        assign(shot, "actionStart", arguments["actionStart"], 2_000)
        assign(shot, "actionEnd", arguments["actionEnd"], 2_000)
        assign(shot, "cameraIntent", arguments["cameraIntent"], 1_000)
        assign(shot, "startPrompt", arguments["startPrompt"], 8_000)
        assign(shot, "endPrompt", arguments["endPrompt"], 8_000)
        assign(shot, "soundscape", arguments["soundscape"], 1_000)
        assign(shot, "music", arguments["music"], 500)
        assign(shot, "caption", arguments["caption"], 60)
        shot["dialogue"] = normalize_dialogue(arguments["dialogue"], shot["dialogue"]) unless arguments["dialogue"].nil?
        shot["duration"] = positive(arguments["duration"], 4) unless arguments["duration"].nil?
        shot["holdFull"] = arguments["holdFull"] == true unless arguments["holdFull"].nil?
        episode["shots"].sort_by! { |entry| entry["order"].to_i }
      end
      remember(arguments["dramaID"], pending)
      { "ok" => true, "drama" => drama }
    end
  end

  # 写工作草稿。手改和 AI 结果都走这条路：正式内容一步不动，用户随时
  # 可以反悔。合并是局部的——只认调用方真的传了的键，其余保持原样，
  # 这样「AI 只改一个字段」不会把别的字段抹成空串。
  #
  # `commit` 为真时在同一把锁里把草稿并进正式内容（0.20.0 起 AI 出稿走这条
  # 路）。刻意不让前端「先 saveDraft 再 commitDraft」两跳：中间隔着一次往返，
  # 期间用户的手改会把 rev 顶掉，于是刚生成的那一版卡在草稿里等人点确认——
  # 而那颗确认按钮正是这次要去掉的东西。
  def save_draft(arguments)
    guarded do
      scope = draft_scope(arguments["scope"])
      return failure("invalid_scope", "Unknown draft scope.") unless scope

      commit = arguments["commit"] == true
      pending = []
      drama = mutate_scope(scope, arguments) do |node, item|
        check_revision(node, arguments["expectedRev"])
        check_revision(node, arguments["expectedDraftRev"], "draftRev")
        if arguments["expectedContent"].is_a?(Hash)
          actual = DRAFT_TEXT_FIELDS.fetch(scope).keys.to_h { |key| [key, node[key].to_s] }
          raise Conflict.new("content", arguments["expectedContent"], actual) unless arguments["expectedContent"] == actual
        end
        check_revision(item["canon"] || {}, arguments["expectedCanonRev"]) if item && arguments.key?("expectedCanonRev")
        patch = arguments["draft"]
        raise NotFound, "invalid_draft" unless patch.is_a?(Hash)
        if %w[character episode shot].include?(scope) && !CreativeSchema.valid?(scope, patch)
          raise NotFound, "invalid_draft"
        end

        node["draft"] = merge_draft(scope, node, patch)
        node["draftRev"] = node["draftRev"].to_i + 1
        next unless commit
        pending << version_of(scope, node)
        adopt_draft(node, scope, item)
      end
      remember(arguments["dramaID"], pending)
      { "ok" => true, "drama" => drama, "committed" => commit }
    end
  end

  # 定稿。把草稿并进正式内容，清空草稿，rev 自增。
  #
  # 重复点确认靠 expectedRev 天然挡住：第二次进来时 rev 已经变了，返回的是
  # 冲突而不是又写一遍——前端把这个冲突当成「刚才那次已经成了」就行，
  # 不必再单独存一份请求 ID 去重。
  def commit_draft(arguments)
    guarded do
      scope = draft_scope(arguments["scope"])
      return failure("invalid_scope", "Unknown draft scope.") unless scope

      pending = []
      drama = mutate_scope(scope, arguments) do |node, item|
        check_revision(node, arguments["expectedRev"])
        check_revision(node, arguments["expectedDraftRev"], "draftRev")
        draft = node["draft"]
        raise NotFound, "empty_draft" unless draft.is_a?(Hash) && !draft.empty?

        pending << version_of(scope, node)
        adopt_draft(node, scope, item)
      end
      remember(arguments["dramaID"], pending)
      { "ok" => true, "drama" => drama }
    end
  end

  # 每种对象能审哪几面。content 是文字设定本身；images 是出图用的提示词
  # （宿主的对话接口只收文字，图片本身审不了）；storyboard 是一集的整套分镜；
  # panel 是专家席审稿（0.42.0-rc1，lib/review_panel.rb）：主框架与一集剧本的剧作品质。
  REVIEW_ASPECTS = {
    "drama" => %w[content panel],
    "character" => %w[content images],
    "episode" => %w[content storyboard panel],
    "shot" => %w[images],
    "asset" => %w[content images]
  }.freeze

  # 记一次内容审核的结论。
  #
  # 刻意不动 rev、不进历史：审核是对内容的判断，不是内容的变更，动 rev 会让
  # 在跑的 AI 请求回来时被误判成冲突。
  #
  # basis 是前端对「审的是哪一份内容」算的指纹，服务端只存不解读。内容之后
  # 被改过，指纹对不上，页面就显示「待复审」——比记 rev 可靠：出图审核看的
  # 是候选图的提示词，那些变化根本不走 rev。
  def record_review(arguments)
    guarded do
      scope = draft_scope(arguments["scope"])
      return failure("invalid_scope", "Unknown review scope.") unless scope
      aspect = arguments["aspect"].to_s
      return failure("invalid_aspect", "Unknown review aspect for this scope.") unless REVIEW_ASPECTS.fetch(scope).include?(aspect)
      stored = ReviewRecord.normalize(
        arguments["review"], basis: arguments["basis"], model: arguments["model"],
        media: { "images" => arguments["mediaImages"], "videos" => arguments["mediaVideos"] },
        panel: arguments["panel"], via: arguments["via"], roundtable: arguments["roundtable"]
      )
      return failure("invalid_review", "Review fields do not match creative-v1.") unless stored

      drama = mutate_scope(scope, arguments) do |node|
        node["reviews"] = {} unless node["reviews"].is_a?(Hash)
        node["reviews"][aspect] = stored
      end
      { "ok" => true, "drama" => drama, "review" => stored }
    end
  end

  # 「已知悉」（0.35.0-rc1）：看过这份 warn 结论、决定先不改。写在结论上，不动 rev、
  # 不进历史，和 record_review 一样只是对内容的判断。
  #
  # 只认 warn：block 是「不改无法上线」，不能靠点一下放过；pass 没什么可知悉的。
  # 复审会整份替换结论，确认随之消失；内容改了、指纹对不上，确认也不再算数
  # （ReviewRecord.acknowledged?）。basis 传调用方读到的那份结论的指纹，防止确认到
  # 一份它没看过的新结论上；revoke: true 撤销。
  def acknowledge_review(arguments)
    guarded do
      scope = draft_scope(arguments["scope"])
      return failure("invalid_scope", "Unknown review scope.") unless scope
      aspect = arguments["aspect"].to_s
      return failure("invalid_aspect", "Unknown review aspect for this scope.") unless REVIEW_ASPECTS.fetch(scope).include?(aspect)
      by = arguments["acknowledgedBy"].to_s.empty? ? "agent" : arguments["acknowledgedBy"].to_s
      return failure("invalid_acknowledged_by", "acknowledgedBy must be user or agent.") unless ReviewRecord::ACKNOWLEDGERS.include?(by)

      updated = nil
      drama = mutate_scope(scope, arguments) do |node|
        record = node["reviews"].is_a?(Hash) ? node["reviews"][aspect] : nil
        updated = ReviewRecord.apply_acknowledgement(record, arguments, by)
      end
      { "ok" => true, "drama" => drama, "review" => updated }
    end
  rescue ReviewRecord::Refused => error
    failure(error.code, error.message)
  end

  # ---- 全剧设定账本（0.37.0-rc1，lib/drama_canon.rb）----

  # 部分更新：canon 里给了哪几块（facts / props / visualRules / bannedTerms /
  # allowedExtras）就整块替换哪几块。expectedRev 对的是 canon.rev，不是剧的 rev：
  # 改账本不该让在跑的策划请求回来时撞上冲突。不走草稿；上一版进历史（scope=canon）。
  def save_canon(arguments)
    guarded do
      previous = nil
      drama = mutate_drama(arguments) do |item|
        current = DramaCanon.read(item)
        check_revision(current, arguments["expectedRev"])
        updated = DramaCanon.merge(current, arguments["canon"])
        previous = current
        item["canon"] = stamp_canon(updated, current)
      end
      remember_canon(arguments["dramaID"], previous)
      { "ok" => true, "drama" => drama, "canon" => drama["canon"] }
    end
  rescue DramaCanon::Invalid => error
    failure("invalid_canon", error.message)
  end

  # drama.check_consistency：只读，不调模型。
  def check_consistency(arguments)
    drama = @store.find(arguments["dramaID"] || arguments["id"])
    return failure("drama_not_found") unless drama

    ConsistencyCheck.run(drama)
  end

  # 历史能翻的作用域：草稿那五种，加上账本。
  HISTORY_SCOPES = (DRAFT_SCOPES + %w[canon]).freeze

  # 一个对象的历史版本列表（摘要），新的在前。
  def list_history(arguments)
    guarded do
      scope = history_scope(arguments["scope"])
      return failure("invalid_scope", "Unknown draft scope.") unless scope
      return failure("history_unavailable", "Version history is not configured.") unless @history
      object_id = history_object_id(scope, arguments)
      raise NotFound, NOT_FOUND_KEYS.fetch(scope) if object_id.to_s.empty?
      { "ok" => true, "versions" => @history.list(arguments["dramaID"], scope: scope, object_id: object_id) }
    end
  end

  # 一个历史版本的完整内容。列表只给摘要，查看全文时才走这里。
  def read_version(arguments)
    guarded do
      return failure("history_unavailable", "Version history is not configured.") unless @history
      version = @history.read(arguments["dramaID"], arguments["versionID"])
      raise NotFound, "version_not_found" unless version
      { "ok" => true, "version" => version }
    end
  end

  # 采用一个历史版本。
  #
  # 当前正式内容先留一版再覆盖，所以回滚本身也能再滚回来。草稿一并清掉：
  # 编辑区显示的是「草稿优先」，留着草稿的话采用完页面上看起来什么都没变。
  # 清掉草稿会丢用户没采用的手改，所以前端在有草稿时先确认；这里把
  # `discardedDraft` 如实回过去。
  def restore_version(arguments)
    guarded do
      scope = history_scope(arguments["scope"])
      return failure("invalid_scope", "Unknown draft scope.") unless scope
      return failure("history_unavailable", "Version history is not configured.") unless @history
      version = @history.read(arguments["dramaID"], arguments["versionID"])
      raise NotFound, "version_not_found" unless version
      fields = version["fields"]
      unless version["scope"] == scope && version["objectID"] == history_object_id(scope, arguments) && fields.is_a?(Hash)
        raise NotFound, "version_not_found"
      end
      # 账本：把那一版的五块整份写回，当前这版先进历史。
      if scope == "canon"
        restored = save_canon("dramaID" => arguments["dramaID"], "expectedRev" => arguments["expectedRev"],
                              "canon" => DramaCanon.snapshot(fields))
        return restored.merge("discardedDraft" => false)
      end

      pending = []
      discarded = false
      drama = mutate_scope(scope, arguments) do |node|
        check_revision(node, arguments["expectedRev"])
        pending << version_of(scope, node)
        discarded = node["draft"].is_a?(Hash) && !node["draft"].empty?
        apply_draft(scope, node, fields)
        node["draft"] = nil
        node["rev"] = node["rev"].to_i + 1
        node["draftRev"] = node["draftRev"].to_i + 1
      end
      remember(arguments["dramaID"], pending)
      { "ok" => true, "drama" => drama, "discardedDraft" => discarded }
    end
  end

  def discard_draft(arguments)
    guarded do
      scope = draft_scope(arguments["scope"])
      return failure("invalid_scope", "Unknown draft scope.") unless scope

      if scope == "shot"
        drama = mutate_episode(arguments) do |episode|
          node = episode["shots"].find { |entry| entry["id"] == arguments["shotID"] }
          raise NotFound, "shot_not_found" unless node
          check_revision(node, arguments["expectedDraftRev"], "draftRev")
          if node["draftOnly"]
            episode["shots"].delete(node)
          else
            node["draft"] = nil
            node["draftRev"] = node["draftRev"].to_i + 1
          end
        end
        return { "ok" => true, "drama" => drama }
      end

      drama = mutate_scope(scope, arguments) do |node|
        check_revision(node, arguments["expectedDraftRev"], "draftRev")
        node["draft"] = nil
        node["draftRev"] = node["draftRev"].to_i + 1
      end
      { "ok" => true, "drama" => drama }
    end
  end

  # 一集的分镜清单。给模型看的那份：只带它需要判断和引用的字段，不带候选图
  # 与选定状态。
  #
  # 分镜的完整对象里塞着每一镜的首尾帧候选图数组，一集二十镜就是几十条
  # 候选记录。整份丢给模型既撑爆预算，又让它以为那些是可以改的东西。
  def list_shots(arguments)
    guarded do
      drama = @store.find(arguments["dramaID"])
      raise NotFound, "drama_not_found" unless drama
      episode = drama["episodes"].find { |entry| entry["id"] == arguments["episodeID"].to_s }
      raise NotFound, "episode_not_found" unless episode

      shots = episode["shots"].sort_by { |entry| entry["order"].to_i }.map do |shot|
        shot = shot.merge(shot["draft"].is_a?(Hash) ? shot["draft"] : {})
        {
          "id" => shot["id"],
          "order" => shot["order"],
          "title" => shot["title"],
          "summary" => shot["summary"],
          "dialogue" => shot["dialogue"],
          "actionStart" => shot["actionStart"],
          "actionEnd" => shot["actionEnd"],
          "cameraIntent" => shot["cameraIntent"],
          "duration" => shot["duration"],
          "rev" => shot["rev"].to_i,
          "draftRev" => shot["draftRev"].to_i,
          "hasDraft" => shot["draft"].is_a?(Hash) && !shot["draft"].empty?,
          "startCandidateCount" => Array(shot["startCandidates"]).length,
          "endCandidateCount" => Array(shot["endCandidates"]).length
        }
      end
      { "ok" => true, "episodeID" => episode["id"], "order" => episode["order"], "shots" => shots }
    end
  end

  # 一次写多镜的草稿，按镜号对齐。理由与分集批量相同：逐镜调用是多轮全量
  # 覆写，中途失败会留下一半写进去的状态；而模型手上只有镜号。
  def save_shot_drafts(arguments)
    guarded do
      entries = arguments["shots"]
      return failure("invalid_shots", "An array of shot drafts is required.") unless entries.is_a?(Array)
      return failure("empty_shots", "No shot drafts were provided.") if entries.empty?
      return failure("invalid_shots", "Shot fields do not match creative-v1.") unless entries.all? { |entry| CreativeSchema.valid?("shot", entry) }
      return failure("invalid_shots", "Too many storyboard shots.") if entries.length > MAX_BATCH_SHOTS
      orders = entries.map { |entry| entry.is_a?(Hash) ? entry["order"] : nil }
      unless orders.all? { |order| order.is_a?(Integer) && order.positive? } && orders.uniq.length == orders.length
        return failure("invalid_shots", "Shot orders must be unique positive integers.")
      end

      commit = arguments["commit"] == true
      applied = []
      skipped = []
      pending = []
      drama = mutate_episode(arguments) do |episode, item|
        check_revision(episode, arguments["expectedEpisodeRev"])
        check_revision(episode, arguments["expectedEpisodeDraftRev"], "draftRev")
        creating = arguments["create"] == true
        if creating && !episode["shots"].empty?
          raise Conflict.new("shots", 0, episode["shots"].length)
        end
        if creating && entries.any? { |entry| entry["summary"].to_s.strip.empty? }
          raise NotFound, "invalid_draft"
        end
        by_order = {}
        episode["shots"].each { |shot| by_order[shot["order"].to_i] = shot }
        # Check every base revision before writing any member of the batch.
        expected = arguments["expectedShots"]
        if arguments.key?("expectedShots") && (!expected.is_a?(Array) || !expected.all? { |value| value.is_a?(Hash) && value["id"].is_a?(String) && value["rev"].is_a?(Integer) && value["draftRev"].is_a?(Integer) })
          raise NotFound, "invalid_shots"
        end
        if expected.is_a?(Array)
          entries.each do |entry|
            node = by_order[entry["order"]]
            base = expected.find { |value| value["id"] == node&.fetch("id") }
            raise Conflict.new("shots", entries.length, expected.length) unless node && base
            check_revision(node, base["rev"])
            check_revision(node, base["draftRev"], "draftRev")
          end
        end

        entries.first(MAX_BATCH_SHOTS).each do |entry|
          next skipped << { "reason" => "not_an_object" } unless entry.is_a?(Hash)
          order = entry["order"].to_i
          shot = by_order[order]
          if !shot && creating
            shot = deep_copy(SHOT_DEFAULTS).merge("id" => SecureRandom.uuid, "order" => order, "draftOnly" => true)
            episode["shots"] << shot
          end
          next skipped << { "order" => order, "reason" => "shot_not_found" } unless shot

          draft = shot["draft"].is_a?(Hash) ? deep_copy(shot["draft"]) : {}
          touched = false
          DRAFT_TEXT_FIELDS.fetch("shot").each do |key, limit|
            next unless entry.key?(key)
            draft[key] = clipped(entry[key], limit)
            touched = true
          end
          if entry.key?("dialogue")
            draft["dialogue"] = normalize_dialogue(entry["dialogue"])
            touched = true
          end
          if entry.key?("duration")
            draft["duration"] = positive(entry["duration"], shot["duration"].to_i)
            touched = true
          end
          if entry.key?("holdFull")
            draft["holdFull"] = entry["holdFull"] == true
            touched = true
          end
          next skipped << { "order" => order, "reason" => "no_writable_field" } unless touched

          shot["draft"] = draft
          shot["draftRev"] = shot["draftRev"].to_i + 1
          applied << order
          next unless commit

          pending << version_of("shot", shot)
          adopt_draft(shot, "shot", item)
        end
        episode["shots"].sort_by! { |entry| entry["order"].to_i } if commit
      end
      remember(arguments["dramaID"], pending)
      { "ok" => true, "drama" => drama, "applied" => applied, "committed" => commit ? applied : [], "skipped" => skipped }
    end
  end

  def record_image(arguments)
    kind = arguments["kind"].to_s
    return failure("invalid_frame_kind", "Frame kind must be start or end.") unless FRAME_KINDS.include?(kind)
    model = arguments["model"].to_s
    return failure("invalid_image_model", "Unsupported image model.") unless IMAGE_MODELS.include?(model)
    file_path = generated_media_path(arguments["filePath"])
    return failure("invalid_media_path", media_path_message) unless file_path

    guarded do
      drama = mutate_shot(arguments) do |shot|
        key = kind == "start" ? "startCandidates" : "endCandidates"
        shot[key] ||= []
        shot[key] << candidate(arguments, model, file_path)
      end
      { "ok" => true, "drama" => drama }
    end
  end

  def record_character_image(arguments)
    model = arguments["model"].to_s
    return failure("invalid_image_model", "Unsupported image model.") unless IMAGE_MODELS.include?(model)
    file_path = generated_media_path(arguments["filePath"])
    return failure("invalid_media_path", media_path_message) unless file_path

    guarded do
      drama = mutate_character(arguments) do |character|
        character["candidates"] ||= []
        character["candidates"] << candidate(arguments, model, file_path)
      end
      { "ok" => true, "drama" => drama }
    end
  end

  def select_character_image(arguments)
    guarded do
      drama = mutate_character(arguments) do |character|
        candidates = character["candidates"] || []
        selected = candidates.find { |entry| entry["id"] == arguments["candidateID"].to_s }
        raise NotFound, "candidate_not_found" unless selected
        character["selectedCandidateID"] = selected["id"]
      end
      { "ok" => true, "drama" => drama }
    end
  end

  # by：谁选的（MCP 调用一律 user；episode.generate_frames 自动选定 batch、episode.accept_recommended_frames
  # 记 recommendation，0.40.0-rc1）。每次选定记 selectedStartAt / selectedStartBy（尾帧 selectedEndAt / selectedEndBy），
  # 批量据此不覆盖批量开始之后的人工选择。gate：自动选定时在文件锁内拿当前镜头判断能不能换，返回原因码就不写
  # （selection_kept）。skip_if_same：已经选着这条就不写（不刷新选定时间）。
  def select_image(arguments, by: "user", gate: nil, skip_if_same: false)
    kind = arguments["kind"].to_s
    return failure("invalid_frame_kind", "Frame kind must be start or end.") unless FRAME_KINDS.include?(kind)

    keys = FRAME_SELECTION_KEYS.fetch(kind)
    current = nil
    guarded do
      drama = mutate_shot(arguments) do |shot|
        candidates = shot[kind == "start" ? "startCandidates" : "endCandidates"] || []
        selected = candidates.find { |entry| entry["id"] == arguments["candidateID"].to_s }
        raise NotFound, "candidate_not_found" unless selected

        current = shot[keys[:id]].to_s
        raise SelectionUnchanged if skip_if_same && current == selected["id"]

        refused = gate&.call(shot)
        raise SelectionKept, refused.to_s if refused

        shot[keys[:id]] = selected["id"]
        shot[keys[:at]] = VideoSelection.now_iso
        shot[keys[:by]] = FRAME_SELECTION_SOURCES.include?(by.to_s) ? by.to_s : "user"
      end
      { "ok" => true, "drama" => drama, "changed" => true }
    end
  rescue SelectionUnchanged
    { "ok" => true, "changed" => false }
  rescue SelectionKept => error
    failure("selection_kept", "The shot keeps its current frame (#{error.message}).")
      .merge("reason" => error.message, "selectedCandidateID" => current.to_s.empty? ? nil : current)
  end

  # 候选图质检结论（0.40.0-rc1，lib/image_qa.rb）写在候选上：candidate.qa。结构性写入，不动 rev、不进历史。
  # target：character | start | end | appearance | scene | sceneVariant | prop；按 target 用 characterID、
  # episodeID + shotID 或 assetID 找候选。qa 由 ImageQA 规范化，这里只存。
  def record_candidate_qa(arguments)
    target = arguments["target"].to_s
    qa = arguments["qa"]
    return failure("invalid_arguments", "qa must be an object.") unless qa.is_a?(Hash)

    guarded do
      drama = mutate_drama(arguments) do |item|
        list = candidate_list(item, target, arguments)
        candidate = list.find { |entry| entry["id"] == arguments["candidateID"].to_s }
        raise NotFound, "candidate_not_found" unless candidate

        candidate["qa"] = qa
      end
      { "ok" => true, "drama" => drama }
    end
  end

  # by：谁选的（MCP 调用一律 user；按集批量 batch、自动补救 remediation 由服务内部传）。
  # gate：自动流程选片时传（0.38.0-rc3，lib/video_selection.rb）：在文件锁内拿当前镜头判断能不能换，
  # 返回原因码（newer_selection / user_qa_override / worse_qa）就不写，返回 selection_kept。
  # skip_if_same：已经选着这条就不写（不刷新选定时间）。
  # trim：自动补救裁剪成片时传（0.39.0-rc1，lib/clip_trim.rb），与选定在同一次写入里落库。
  # 不传时：选的是另一条成片就作废原来的裁剪（裁剪只对它那条成片有效），还是同一条就保留。
  def select_video(arguments, by: "user", gate: nil, skip_if_same: false, trim: nil)
    current = nil
    guarded do
      drama = mutate_shot(arguments) do |shot|
        video_id = arguments["videoID"].to_s
        raise NotFound, "video_not_found" if video_id.empty?

        current = shot["selectedVideoID"].to_s
        raise SelectionUnchanged if skip_if_same && current == video_id

        refused = gate&.call(shot)
        raise SelectionKept, refused.to_s if refused

        shot["selectedVideoID"] = video_id
        shot["selectedVideoAt"] = VideoSelection.now_iso
        shot["selectedVideoBy"] = VideoSelection::SOURCES.include?(by.to_s) ? by.to_s : "user"
        if arguments["qaOverride"] == true
          # qaOverrideBy：自动补救在设置允许时放行记 auto（0.38.0-rc1）；人工放行仍是 user。
          override_by = QA_OVERRIDE_SOURCES.include?(arguments["qaOverrideBy"].to_s) ? arguments["qaOverrideBy"].to_s : "user"
          shot["qaOverride"] = { "jobID" => video_id, "by" => override_by, "at" => Time.now.utc.iso8601 }
        elsif shot.dig("qaOverride", "jobID") != video_id
          shot.delete("qaOverride")
        end
        if trim.is_a?(Hash)
          shot["trim"] = trim.merge("jobID" => video_id, "at" => VideoSelection.now_iso)
        elsif shot.dig("trim", "jobID") != video_id
          shot.delete("trim")
        end
      end
      { "ok" => true, "drama" => drama, "changed" => true }
    end
  rescue SelectionUnchanged
    { "ok" => true, "changed" => false }
  rescue SelectionKept => error
    failure("selection_kept", "The shot keeps its current clip (#{error.message}).")
      .merge("reason" => error.message, "selectedVideoID" => current.to_s.empty? ? nil : current)
  end

  # drama.set_clip_trim 的落库（0.39.0-rc1，lib/clip_trim.rb）：参数校验与读片长在 ClipTrim.apply。
  # job_id 是这一镜现在要用的成片（选定的，没选就是最新完成的那条）；锁里再核一次选定，
  # 期间被换选了就拒绝（trim_not_selected）。trim 为 nil 时清掉裁剪。结构性写入，不动 rev、不进历史。
  def set_clip_trim(arguments, job_id:, trim:)
    guarded do
      drama = mutate_shot(arguments) do |shot|
        current = shot["selectedVideoID"].to_s
        raise TrimRefused, "trim_not_selected" unless current.empty? || current == job_id.to_s

        if trim.nil?
          shot.delete("trim")
        else
          shot["trim"] = trim.merge("jobID" => job_id.to_s, "at" => VideoSelection.now_iso)
        end
      end
      { "ok" => true, "drama" => drama }
    end
  rescue TrimRefused
    failure("trim_not_selected", "The shot's selected clip changed; read the shot again and trim the clip it uses now.")
  end

  private

  def guarded
    yield
  rescue NotFound => error
    failure(error.message)
  rescue Conflict => error
    {
      "ok" => false,
      "error" => {
        "code" => "revision_conflict", "message" => CONFLICT_MESSAGE,
        "field" => error.field, "expected" => error.expected, "actual" => error.actual
      }
    }
  end

  # expectedRev 没传就当调用方不关心版本——旧插件和图片候选那几条路径都
  # 这样。传了就必须对上，对不上宁可让这次保存失败：静默覆盖丢的是用户
  # 刚敲进去的字。
  def check_revision(node, expected, field = "rev")
    return if expected.nil?
    actual = node[field].to_i
    raise Conflict.new(field, Integer(expected), actual) unless Integer(expected) == actual
  rescue ArgumentError, TypeError
    raise Conflict.new(field, expected, node[field].to_i)
  end

  def draft_scope(value)
    token = value.to_s.strip
    DRAFT_SCOPES.include?(token) ? token : nil
  end

  def history_scope(value)
    token = value.to_s.strip
    HISTORY_SCOPES.include?(token) ? token : nil
  end

  # 账本每次写入 rev + 1、记时间。内容没变也照样 + 1：调用方显式存了一次。
  def stamp_canon(updated, current)
    updated.merge("rev" => current["rev"].to_i + 1, "updatedAt" => Time.now.utc.iso8601)
  end

  # 被覆盖的那一版进历史；空账本不记（回滚到「什么都没有」没有意义）。
  def remember_canon(drama_id, previous)
    return if previous.nil? || DramaCanon.blank?(previous)

    remember(drama_id, [{ "scope" => "canon", "objectID" => drama_id.to_s, "rev" => previous["rev"].to_i,
                          "fields" => DramaCanon.snapshot(previous) }])
  end

  # 四种作用域共用一条定位路径，省得每个方法各写一遍找对象的代码。
  def mutate_scope(scope, arguments, &block)
    case scope
    when "drama" then mutate_drama(arguments) { |drama| block.call(drama, drama) }
    when "character" then mutate_character(arguments, &block)
    when "episode" then mutate_episode(arguments, &block)
    when "shot" then mutate_shot(arguments, &block)
    when "asset" then mutate_asset(arguments, &block)
    end
  end

  # 已有草稿上叠新的一层，而不是整份替换：模型这轮只回了三个字段时，
  # 上一轮生成、用户已经看过的其他字段不该跟着消失。
  def merge_draft(scope, node, patch)
    merged = node["draft"].is_a?(Hash) ? deep_copy(node["draft"]) : {}
    DRAFT_TEXT_FIELDS.fetch(scope).each do |key, limit|
      next unless patch.key?(key)
      merged[key] = clipped(patch[key], limit)
    end
    if scope == "shot"
      merged["dialogue"] = normalize_dialogue(patch["dialogue"]) if patch.key?("dialogue")
      merged["duration"] = positive(patch["duration"], node["duration"].to_i) if patch.key?("duration")
      merged["holdFull"] = patch["holdFull"] == true if patch.key?("holdFull")
    end
    merged
  end

  # 草稿并进正式内容、清空草稿、rev 自增。定稿、批量采用和「写完即采用」
  # 共用这一段，三处各写一遍迟早有一处漏掉 draftOnly 或 draftRev。
  def adopt_draft(node, scope, item)
    draft = node["draft"]
    return false unless draft.is_a?(Hash) && !draft.empty?

    apply_draft(scope, node, draft)
    node.delete("draftOnly") if scope == "shot"
    item["status"] = "production" if scope == "shot" && item.is_a?(Hash) && item["status"] == "planning"
    node["draft"] = nil
    node["rev"] = node["rev"].to_i + 1
    # 草稿清空本身也是一次草稿变更。draftRev 继续往前走，在跑的那条
    # 请求回来时才认得出「基准已经不是你出发时那份了」。
    node["draftRev"] = node["draftRev"].to_i + 1
    true
  end

  # 覆盖之前的正式内容快照。只拍草稿白名单里的字段（分镜再加台词与时长）：
  # 候选图、选定状态这些不归文字生成管，回滚也不该动它们。
  def version_of(scope, node)
    fields = {}
    DRAFT_TEXT_FIELDS.fetch(scope).each_key { |key| fields[key] = node[key].to_s }
    if scope == "shot"
      fields["dialogue"] = Array(node["dialogue"])
      fields["duration"] = node["duration"].to_i
      fields["holdFull"] = node["holdFull"] == true
    end
    # 新建的分镜空壳（draftOnly）没有正式内容，留一版空白没有意义。
    { "scope" => scope, "objectID" => node["id"].to_s, "rev" => node["rev"].to_i,
      "fields" => fields, "skip" => scope == "shot" && node["draftOnly"] == true }
  end

  # 在主存档写成功之后才记历史：写失败的那一版不该出现在列表里。
  #
  # 历史写不进去不回滚主写入——正式内容已经落盘，这时候报失败会让用户以为
  # AI 那一版没保存上、再点一次。只往 stderr 记一行，让排障时看得见。
  def remember(drama_id, snapshots)
    return unless @history
    snapshots.each do |snapshot|
      next if snapshot["skip"]
      next if snapshot["scope"] == "shot" && shot_fields_blank?(snapshot["fields"])
      @history.record(drama_id, scope: snapshot["scope"], object_id: snapshot["objectID"],
                                rev: snapshot["rev"], fields: snapshot["fields"])
    end
  rescue StandardError => error
    warn "video-studio: history record failed (#{error.class}: #{error.message})"
  end

  # 分镜的 duration 恒为数字、dialogue 恒为数组，HistoryStore 的「全空不记」
  # 对它不成立，这里单独判一次。
  def shot_fields_blank?(fields)
    fields.all? do |key, value|
      next true if key == "duration" || key == "holdFull"
      value.is_a?(Array) ? value.empty? : value.to_s.strip.empty?
    end
  end

  def history_object_id(scope, arguments)
    case scope
    when "drama", "canon" then arguments["dramaID"].to_s
    when "character" then arguments["characterID"].to_s
    when "episode" then arguments["episodeID"].to_s
    when "shot" then arguments["shotID"].to_s
    when "asset" then arguments["assetID"].to_s
    end
  end

  def apply_draft(scope, node, draft)
    DRAFT_TEXT_FIELDS.fetch(scope).each do |key, limit|
      next unless draft.key?(key)
      node[key] = clipped(draft[key], limit)
    end
    return unless scope == "shot"
    node["dialogue"] = normalize_dialogue(draft["dialogue"], node["dialogue"]) if draft.key?("dialogue")
    node["duration"] = positive(draft["duration"], node["duration"].to_i) if draft.key?("duration")
    node["holdFull"] = draft["holdFull"] == true if draft.key?("holdFull")
  end

  # 写入后的返回值同样派生路径与 URL：页面拿写入工具的回包更新状态，与 get 读到的
  # 必须是同一个形状。
  def mutate_drama(arguments)
    drama = @store.update(arguments["dramaID"]) do |item|
      item["assets"] = [] unless item["assets"].is_a?(Array)
      yield item
    end
    raise NotFound, "drama_not_found" unless drama
    decorate(drama)
  end

  # 按出图 target 找候选数组（找不到对象抛 NotFound）。
  def candidate_list(drama, target, arguments)
    case target
    when "character"
      character = Array(drama["characters"]).find { |entry| entry["id"] == arguments["characterID"].to_s }
      raise NotFound, "character_not_found" unless character

      character["candidates"] ||= []
    when "start", "end"
      episode = Array(drama["episodes"]).find { |entry| entry["id"] == arguments["episodeID"].to_s }
      raise NotFound, "episode_not_found" unless episode

      shot = Array(episode["shots"]).find { |entry| entry["id"] == arguments["shotID"].to_s }
      raise NotFound, "shot_not_found" unless shot

      shot[target == "start" ? "startCandidates" : "endCandidates"] ||= []
    when "appearance", "scene", "sceneVariant", "prop"
      asset = find_asset(drama, arguments["assetID"])
      asset["candidates"] ||= []
    else
      raise NotFound, "candidate_not_found"
    end
  end

  def mutate_asset(arguments)
    mutate_drama(arguments) do |drama|
      asset = find_asset(drama, arguments["assetID"])
      yield asset, drama
    end
  end

  # 读出边界：补 assets 数组，给每一条媒体引用派生 fileName / filePath / mediaURL。
  def decorate(drama)
    return drama unless drama.is_a?(Hash)
    drama["aspectRatio"] = stored_aspect_ratio(drama)
    drama["frame"] = frame(drama)
    drama["assets"] = [] unless drama["assets"].is_a?(Array)
    # 旧剧没有账本：读出时补成空账本，形状固定（lib/drama_canon.rb）。
    drama["canon"] = DramaCanon.read(drama)
    # 声音资产的语速（0.40.0-rc1，lib/speech_rate.rb）：只读字段 speechRate，不落盘。
    SpeechRate.annotate!(drama)
    MediaRef.decorate_drama!(drama, media_root)
  end

  # 存档里的 aspectRatio；缺失或认不出时按 format 推断（早于画幅字段的剧）。
  # 「landscape」一并认：升级前视频尺寸就是这么判横屏的，旧剧成片不能因此变竖。
  def stored_aspect_ratio(drama)
    value = drama["aspectRatio"].to_s
    return value if ASPECT_RATIOS.include?(value)

    format = drama["format"].to_s.downcase
    format.include?("横") || format.include?("landscape") ? LANDSCAPE_ASPECT : AspectRatios::DEFAULT_ID
  end

  def find_asset(drama, id)
    asset = Array(drama["assets"]).find { |entry| entry["id"] == id.to_s }
    raise NotFound, "asset_not_found" unless asset
    asset
  end

  def find_asset_or_build(drama, arguments, request_id)
    token = arguments["assetID"].to_s
    return find_asset(drama, token) unless token.empty?

    kind = arguments["kind"].to_s
    raise NotFound, "invalid_asset_kind" unless ASSET_KINDS.include?(kind)
    raise NotFound, "too_many_assets" if drama["assets"].length >= MAX_ASSETS
    now = Time.now.utc.iso8601
    asset = deep_copy(ASSET_DEFAULTS).merge(deep_copy(ASSET_KIND_DEFAULTS.fetch(kind)))
                                     .merge("id" => SecureRandom.uuid, "kind" => kind, "createdAt" => now, "updatedAt" => now)
    asset["requestID"] = request_id unless request_id.empty?
    drama["assets"] << asset
    asset
  end

  # 各类资产的关系与专有字段。目标不存在直接报错，别让参考包指向空气。
  def apply_asset_fields(drama, asset, arguments, warnings)
    kind = asset["kind"]
    if !arguments["kind"].nil? && arguments["kind"].to_s != kind
      raise NotFound, "asset_kind_immutable"
    end
    case kind
    when "appearance", "voice"
      unless arguments["characterID"].nil?
        token = arguments["characterID"].to_s
        raise NotFound, "character_not_found" unless drama["characters"].any? { |entry| entry["id"] == token }
        asset["characterID"] = token
      end
      raise NotFound, "character_required" if asset["characterID"].to_s.empty?
    end
    case kind
    when "appearance"
      unless arguments["category"].nil?
        raise NotFound, "invalid_category" unless APPEARANCE_CATEGORIES.include?(arguments["category"].to_s)
        asset["category"] = arguments["category"].to_s
      end
    when "scene"
      unless arguments["parentSceneID"].nil?
        parent = arguments["parentSceneID"].to_s
        if parent.empty?
          asset["parentSceneID"] = nil
        else
          target = drama["assets"].find { |entry| entry["id"] == parent && entry["kind"] == "scene" }
          raise NotFound, "scene_not_found" unless target
          raise NotFound, "scene_cycle" if parent == asset["id"] || ReferencePackage.ancestors(drama["assets"], target).any? { |node| node["id"] == asset["id"] }
          asset["parentSceneID"] = parent
        end
      end
    when "sceneVariant"
      unless arguments["sceneID"].nil?
        token = arguments["sceneID"].to_s
        raise NotFound, "scene_not_found" unless drama["assets"].any? { |entry| entry["id"] == token && entry["kind"] == "scene" }
        asset["sceneID"] = token
      end
      raise NotFound, "scene_required" if asset["sceneID"].to_s.empty?
      assign(asset, "lighting", arguments["lighting"], 300)
    when "voice"
      assign(asset, "language", arguments["language"], 40)
      assign(asset, "dialect", arguments["dialect"], 80)
      assign(asset, "referenceTranscript", arguments["referenceTranscript"], 2_000)
      assign(asset, "providerVoiceID", arguments["providerVoiceID"], 200)
      asset["presets"] = normalize_presets(arguments["presets"]) unless arguments["presets"].nil?
      apply_consent(asset, arguments["consent"]) unless arguments["consent"].nil?
      warnings << { "code" => "voice_consent_missing", "message" => "Voice consent is not confirmed; generation stays disabled." } unless ReferencePackage.consent_granted?(asset)
    end
  end

  # 授权：status=granted 必须带授权人，确认时间由服务端盖；撤回时清掉确认时间。
  def apply_consent(asset, raw)
    raise NotFound, "invalid_consent" unless raw.is_a?(Hash)
    current = asset["consent"].is_a?(Hash) ? asset["consent"] : deep_copy(ASSET_KIND_DEFAULTS["voice"]["consent"])
    status = raw.key?("status") ? raw["status"].to_s : current["status"]
    raise NotFound, "invalid_consent" unless CONSENT_STATUSES.include?(status)
    granted_by = raw.key?("grantedBy") ? clipped(raw["grantedBy"], 120) : current["grantedBy"].to_s
    note = raw.key?("note") ? clipped(raw["note"], 1_000) : current["note"].to_s
    raise NotFound, "consent_incomplete" if status == "granted" && granted_by.empty?
    confirmed = status == "granted" ? (current["status"] == "granted" && current["confirmedAt"] ? current["confirmedAt"] : Time.now.utc.iso8601) : nil
    asset["consent"] = { "status" => status, "grantedBy" => granted_by, "confirmedAt" => confirmed, "note" => note }
  end

  def normalize_presets(value)
    Array(value).map do |entry|
      next nil unless entry.is_a?(Hash)
      name = clipped(entry["name"], 60)
      next nil if name.empty?
      { "id" => entry["id"].to_s.empty? ? SecureRandom.uuid : entry["id"].to_s, "name" => name,
        "instruction" => clipped(entry["instruction"], 500), "speed" => entry["speed"].nil? ? nil : entry["speed"].to_f }
    end.compact.first(MAX_PRESETS)
  end

  # 参考包的校验与裁剪。归档的资产允许绑定但报 warning：旧镜头不该因为归档就坏掉。
  def normalize_package(drama, raw, warnings)
    assets = Array(drama["assets"])
    lookup = lambda do |id, kind|
      token = id.to_s
      next nil if token.empty?
      asset = assets.find { |entry| entry["id"] == token }
      raise NotFound, "asset_not_found" unless asset && asset["kind"] == kind
      warnings << { "code" => "asset_archived", "assetID" => asset["id"], "message" => "#{kind} \"#{asset['name']}\" is archived." } if asset["archived"]
      asset
    end
    scene = lookup.call(raw["sceneID"], "scene")
    variant = lookup.call(raw["sceneVariantID"], "sceneVariant")
    if variant
      raise NotFound, "scene_mismatch" if scene && variant["sceneID"] != scene["id"]
      scene ||= assets.find { |entry| entry["id"] == variant["sceneID"] }
    end
    cast = Array(raw["cast"]).first(20).map do |binding|
      next nil unless binding.is_a?(Hash)
      character_id = binding["characterID"].to_s
      raise NotFound, "character_not_found" unless drama["characters"].any? { |entry| entry["id"] == character_id }
      appearance = lookup.call(binding["appearanceID"], "appearance")
      raise NotFound, "appearance_mismatch" if appearance && appearance["characterID"] != character_id
      voice = lookup.call(binding["voiceID"], "voice")
      raise NotFound, "voice_mismatch" if voice && voice["characterID"] != character_id
      role = ReferencePackage::ROLES.include?(binding["role"].to_s) ? binding["role"].to_s : "supporting"
      position = ReferencePackage::POSITIONS.include?(binding["screenPosition"].to_s) ? binding["screenPosition"].to_s : ""
      { "characterID" => character_id, "appearanceID" => appearance && appearance["id"], "voiceID" => voice && voice["id"],
        "role" => role, "screenPosition" => position, "appearanceChange" => clipped(binding["appearanceChange"], 500) }
    end.compact
    raise NotFound, "duplicate_cast" if cast.map { |entry| entry["characterID"] }.uniq.length != cast.length
    props = Array(raw["propIDs"]).first(20).map { |id| lookup.call(id, "prop") }.compact.map { |prop| prop["id"] }.uniq
    mode = raw["generationMode"].to_s.empty? ? "auto" : raw["generationMode"].to_s
    raise NotFound, "invalid_mode" unless ReferencePackage::MODES.include?(mode)
    # 参考视频：前几镜的成片任务 ID（ref2va 组合一）。任务是否存在、成片在不在，
    # 由编译时按任务表判断并告警，这里只留 ID。
    video_refs = Array(raw["videoRefs"]).map do |entry|
      id = entry.is_a?(Hash) ? clipped(entry["jobID"], 120) : clipped(entry, 120)
      id.empty? ? nil : { "jobID" => id }
    end.compact.uniq.first(ReferencePackage::MAX_VIDEO_REFS)
    retention = raw["audioRetention"].to_s.empty? ? "fully_copy" : raw["audioRetention"].to_s
    raise NotFound, "invalid_audio_retention" unless ReferencePackage::AUDIO_RETENTIONS.include?(retention)
    {
      "sceneID" => scene && scene["id"], "sceneVariantID" => variant && variant["id"], "cast" => cast, "propIDs" => props,
      "generationMode" => mode, "excluded" => Array(raw["excluded"]).map { |key| clipped(key, 120) }.reject(&:empty?).uniq.first(50),
      "videoRefs" => video_refs, "audioRetention" => retention
    }
  end

  # 每个资产被哪些镜头引用：{ assetID => [{episodeID, shotID, order}] }。
  def asset_references(drama)
    references = Hash.new { |hash, key| hash[key] = [] }
    Array(drama["episodes"]).each do |episode|
      Array(episode["shots"]).each do |shot|
        ReferencePackage.referenced_asset_ids(shot).each do |id|
          references[id] << { "episodeID" => episode["id"], "episodeOrder" => episode["order"], "shotID" => shot["id"], "shotOrder" => shot["order"] }
        end
        Array(shot["dialogue"]).each do |line|
          next unless line.is_a?(Hash) && line["audio"].is_a?(Hash)
          Array(line["audio"]["candidates"]).map { |entry| entry["voiceAssetID"].to_s }.reject(&:empty?).uniq.each do |id|
            references[id] << { "episodeID" => episode["id"], "episodeOrder" => episode["order"], "shotID" => shot["id"], "shotOrder" => shot["order"], "lineID" => line["id"] }
          end
        end
      end
    end
    references
  end

  def asset_summary(asset, references)
    selected = Array(asset["candidates"]).find { |entry| entry["id"] == asset["selectedCandidateID"] }
    summary = {
      "id" => asset["id"], "kind" => asset["kind"], "name" => asset["name"], "prompt" => asset["prompt"], "notes" => asset["notes"],
      "archived" => asset["archived"] == true, "candidates" => Array(asset["candidates"]).length,
      "selectedCandidateID" => asset["selectedCandidateID"], "selected" => selected,
      "rev" => asset["rev"].to_i, "draftRev" => asset["draftRev"].to_i,
      "hasDraft" => asset["draft"].is_a?(Hash) && !asset["draft"].empty?,
      "reviews" => asset["reviews"].is_a?(Hash) ? asset["reviews"] : {},
      "referencedBy" => references.fetch(asset["id"], []), "updatedAt" => asset["updatedAt"]
    }
    %w[characterID category parentSceneID sceneID lighting language dialect providerVoiceID].each { |key| summary[key] = asset[key] if asset.key?(key) }
    summary["consent"] = asset["consent"] if asset["kind"] == "voice"
    summary["presets"] = asset["presets"] if asset["kind"] == "voice"
    if asset["kind"] == "voice" && asset["speechRate"].is_a?(Hash)
      summary["speechRate"] = asset["speechRate"]
      warning = SpeechRate.warning(asset, asset["speechRate"])
      summary["warnings"] = [warning] if warning
    end
    summary
  end

  def positive_or_nil(value)
    number = Integer(value)
    number.positive? ? number : nil
  rescue ArgumentError, TypeError
    nil
  end

  def mutate_episode(arguments)
    mutate_drama(arguments) do |drama|
      episode = drama["episodes"].find { |entry| entry["id"] == arguments["episodeID"].to_s }
      raise NotFound, "episode_not_found" unless episode
      yield episode, drama
    end
  end

  def mutate_shot(arguments)
    mutate_episode(arguments) do |episode, drama|
      shot = episode["shots"].find { |entry| entry["id"] == arguments["shotID"].to_s }
      raise NotFound, "shot_not_found" unless shot
      yield shot, drama
    end
  end

  def mutate_character(arguments)
    mutate_drama(arguments) do |drama|
      character = drama["characters"].find { |entry| entry["id"] == arguments["characterID"].to_s }
      raise NotFound, "character_not_found" unless character
      yield character
    end
  end

  # 0.26.0 起只持久化文件名（设计稿 4.3）；filePath / mediaURL 在读出时派生。
  def candidate(arguments, model, file_path)
    MediaRef.build(file_path, media_root, prompt: clipped(arguments["prompt"], 8_000), model: model, media_type: "image",
                                           extra: { "requestID" => request_token(arguments) }.merge(aspect_flags(arguments)))
  end

  # image.generate 发现图片方向与请求尺寸相反时传进来的标记（0.35.0-rc2）。图照记，
  # 候选上留 aspectMismatch 与两个尺寸，页面显示「画幅不符」，Agent 据此重抽。
  def aspect_flags(arguments)
    return {} unless arguments["aspectMismatch"] == true

    sizes = [arguments["actualSize"], arguments["requestedSize"]].map { |value| value.to_s.match?(MEDIA_SIZE_PATTERN) ? value.to_s : nil }
    { "aspectMismatch" => true, "actualSize" => sizes[0], "requestedSize" => sizes[1] }
  end

  # 配音的语速（0.40.0-rc1，voice.generate 算好传进来）：每秒字数（或词数）与单位。
  def speech_rate_fields(arguments)
    rate = arguments["charsPerSecond"]
    return {} unless rate.is_a?(Numeric) && rate.positive? && rate < 100

    { "charsPerSecond" => rate.to_f.round(1), "speechUnit" => arguments["speechUnit"].to_s == "words" ? "words" : "chars" }
  end

  # 计费工具的去重键，记在候选上；空就不记。
  def request_token(arguments)
    token = clipped(arguments["requestID"], 200)
    token.empty? ? nil : token
  end

  # 收敛到宿主的生成目录内，并拒绝符号链接——否则目录里放一个指向别处的
  # 链接就能把收敛绕过去。返回 nil 表示这个路径不可信。
  def generated_media_path(value)
    raw = value.to_s.strip
    return nil if raw.empty? || raw.length > 4_000
    expanded = File.expand_path(raw)
    return nil unless media_host.within?(expanded)
    return nil if File.symlink?(expanded)
    expanded
  end

  def media_path_message
    "Image files must live in the plugin's generated-images directory."
  end

  # 只有调用方真的传了这个字段才改。模型常常只带想改的那一项，
  # 无条件 merge 会把人物小传、台词、首尾帧提示词一起清成空串。
  def assign(target, key, value, limit)
    return if value.nil?
    target[key] = clipped(value, limit)
  end

  def find_or_build(collection, id, defaults, missing_code)
    token = id.to_s
    unless token.empty?
      found = collection.find { |entry| entry["id"] == token }
      # 传了 ID 却找不到，多半是模型拿着上一轮的旧 ID 重试。静默新建
      # 会让用户的分镜列表里堆出一串空壳。
      raise NotFound, missing_code unless found
      return found
    end
    created = deep_copy(defaults).merge("id" => SecureRandom.uuid)
    collection << created
    created
  end

  # macOS 自带的 /usr/bin/ruby 是 2.6，没有 filter_map；mcp.json 钉死了这个
  # 解释器，所以这里只用 2.6 就有的 API。
  #
  # existing：当前正式内容里的台词。同一 id 的台词改文字时，挂在它上面的音频候选
  # 原样带过来——文字生成不归音频管，改一个字就把配好的音丢掉说不过去；台词改了
  # 音频过期这件事由进度里的指纹检查报出来。
  def normalize_dialogue(value, existing = [])
    previous = {}
    Array(existing).each { |line| previous[line["id"].to_s] = line["audio"] if line.is_a?(Hash) && line["audio"].is_a?(Hash) }
    Array(value).map do |entry|
      next nil unless entry.is_a?(Hash)
      text = clipped(entry["text"], 2_000)
      next nil if text.empty?
      id = entry["id"].to_s.empty? ? SecureRandom.uuid : entry["id"].to_s
      line = { "id" => id, "speaker" => clipped(entry["speaker"], 120), "text" => text }
      line["audio"] = previous[id] if previous[id]
      line
    end.compact.first(50)
  end

  def deep_copy(value)
    JSON.parse(JSON.generate(value))
  end

  def clipped(value, limit)
    value.to_s.strip[0, limit]
  end

  def positive(value, fallback)
    number = Integer(value)
    number.positive? ? number : fallback
  rescue ArgumentError, TypeError
    fallback
  end

  def failure(code, message = nil)
    { "ok" => false, "error" => { "code" => code, "message" => message || NOT_FOUND_MESSAGES.fetch(code, code) } }
  end
end
