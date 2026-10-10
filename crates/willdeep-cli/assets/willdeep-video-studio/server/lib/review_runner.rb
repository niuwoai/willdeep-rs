# frozen_string_literal: true

require_relative "host_bridge"
require_relative "review_material"
require_relative "review_panel"
require_relative "video_qa"

# 内容预审的两条路（设计稿第 0 节、决策 8）：
#
# - `review.get_material`（只读）：拼好材料、附件路径、指纹与审核用的 system 提示词
#   交给调用方。harness 的模型自己看、自己判，再用 drama.record_review /
#   video.record_review 带同一个 basis 写回。任何 MCP 客户端都能走这条。
# - `review.run`：WillDeep 内的便捷封装。借宿主反向请求 `willdeep/ai/complete`
#   调设置页选定的审核模型，结论经同一个 record_review 落库。宿主没宣告这个方法
#   时返回 host_review_unsupported，并提示改走 get_material。
#
# 两条路用的材料、指纹、技能与输出契约完全一样，页面看到的是同一份结论。
#
# 第三个面 `panel`（专家席审稿，0.42.0-rc1，lib/review_panel.rb）：drama / episode 由多位专家并行审读、
# 主持人归纳，结论形状与前两条路相同；这里只做分发。
class ReviewRunner
  SCOPES = {
    "drama" => %w[content panel], "character" => %w[content images],
    "episode" => %w[content storyboard panel], "shot" => %w[images], "asset" => %w[content images], "video" => %w[content frames]
  }.freeze
  RAW_EXCERPT = 400

  # limits：进程级并发名额（ShotPipelines::Limits），专家席的每位专家发言各占一个 qa 名额；不给就不限。
  def initialize(drama_service:, video_service:, video_store:, skills:, host:, video_qa: nil, limits: nil)
    @dramas = drama_service
    @videos = video_service
    @store = video_store
    @skills = skills
    @host = host
    @video_qa = video_qa
    @panel = ReviewPanel::Runner.new(drama_service: drama_service, video_store: video_store, skills: skills, host: host, limits: limits,
                                     route: ->(request, settings) { route(request, settings) })
  end

  # 材料与请求内容，不调模型。
  def material(arguments)
    scope, aspect, error = scope_and_aspect(arguments)
    return error if error
    return @panel.material(arguments) if aspect == ReviewPanel::ASPECT

    settings = @store.settings
    locale = settings["uiLocale"] == "en" ? "en" : "zh-Hans"
    media = { "images" => true, "videos" => true }
    built, _record = scope == "video" ? video_material(arguments, locale, media) : drama_material(arguments, scope, aspect, locale, media)
    return built if built["ok"] == false

    request = request_for(built, locale, settings)
    result = {
      "ok" => true, "scope" => scope, "aspect" => aspect, "label" => built["label"], "text" => built["text"], "basis" => built["basis"],
      "imagePaths" => built["imagePaths"], "videoPaths" => built["videoPaths"],
      "system" => request["system"], "userMessage" => request.dig("messages", 0, "content"),
      "fence" => ReviewMaterial::FENCE, "locale" => locale,
      "writeBack" => write_back(scope, aspect, arguments, built)
    }
    # 定妆图 / 资产参考图只审选定的那张时，告诉调用方是哪一张（候选 ID）。
    result["selectedCandidateID"] = built["selectedCandidateID"] if built["selectedCandidateID"]
    result
  end

  def run(arguments)
    scope, aspect, error = scope_and_aspect(arguments)
    return error if error
    return @panel.run(arguments) if aspect == ReviewPanel::ASPECT
    unless @host.supports?(HostBridge::AI_COMPLETE)
      return failure("host_review_unsupported", "This host cannot run models for plugin tools. Inside WillDeep update to 1.380.0 or later; elsewhere call review.get_material, judge the material yourself and store the verdict with drama.record_review / video.record_review.")
    end

    settings = @store.settings
    locale = settings["uiLocale"] == "en" ? "en" : "zh-Hans"
    media = { "images" => true, "videos" => true }
    built, record = scope == "video" ? video_material(arguments, locale, media) : drama_material(arguments, scope, aspect, locale, media)
    return built if built["ok"] == false

    response = begin
      @host.request(HostBridge::AI_COMPLETE, request_for(built, locale, settings))
    rescue HostBridge::RequestFailed => error
      return failure("review_model_failed", error.message).merge("label" => built["label"])
    end
    text = response.is_a?(Hash) ? response["text"].to_s : ""
    verdict = ReviewMaterial.parse(text)
    unless verdict
      return failure("review_invalid", "The review model did not return a valid #{ReviewMaterial::FENCE} block.")
             .merge("label" => built["label"], "raw" => text[0, RAW_EXCERPT])
    end

    stored = record.call(verdict, response["model"].to_s, built)
    return stored unless stored["ok"]

    review = stored["review"]
    result = {
      "ok" => true, "label" => built["label"], "status" => review["status"], "summary" => review["summary"],
      "issues" => review["issues"], "mustFix" => ReviewRecord.must_fix_count(review), "model" => review["model"],
      "mediaImages" => review["mediaImages"], "mediaVideos" => review["mediaVideos"], "basis" => review["basis"]
    }
    result["selectedCandidateID"] = built["selectedCandidateID"] if built["selectedCandidateID"]
    result
  end

  # 后台审核提交前的检查（0.36.0-rc1）：范围合法、宿主能问模型。不拼材料——成片质检的
  # 材料要抽帧，放在后台做。通过返回 nil。
  def preflight(arguments)
    _scope, _aspect, error = scope_and_aspect(arguments)
    return error if error
    return nil if @host.supports?(HostBridge::AI_COMPLETE)

    failure("host_review_unsupported", "This host cannot run models for plugin tools. Inside WillDeep update to 1.380.0 or later; elsewhere call review.get_material, judge the material yourself and store the verdict with drama.record_review / video.record_review.")
  end

  # 直接问一次审核模型（0.40.0-rc1，候选图质检 lib/image_qa.rb 用）：与 review.run 同一条路——宿主反向请求
  # willdeep/ai/complete、同一套审核模型路由（单独选了审核模型就用它，否则跟随策划模型）、附件规则一样。
  # 返回 {ok, text, model} 或 {ok: false, error}。
  def ask_model(system:, content:, image_paths: [], writing: false)
    unless @host.supports?(HostBridge::AI_COMPLETE)
      return failure("host_review_unsupported", "This host cannot run models for plugin tools. Inside WillDeep update to 1.380.0 or later.")
    end

    message = { "role" => "user", "content" => content.to_s }
    message["imagePaths"] = image_paths unless image_paths.empty?
    request = { "system" => system.to_s, "messages" => [message] }
    if writing
      settings = @store.settings
      request["provider"] = settings["assistantProviderID"] if settings["assistantProviderID"]
      request["model"] = settings["assistantModel"] if settings["assistantProviderID"] && settings["assistantModel"]
      request["max_output_tokens"] = 4096
    else
      request = route(request, @store.settings)
    end
    response = begin
      @host.request(HostBridge::AI_COMPLETE, request)
    rescue HostBridge::RequestFailed => error
      return failure("review_model_failed", error.message)
    end
    return failure("review_model_failed", "The host returned no answer.") unless response.is_a?(Hash)

    { "ok" => true, "text" => response["text"].to_s, "model" => response["model"].to_s }
  end

  def locale
    @store.settings["uiLocale"] == "en" ? "en" : "zh-Hans"
  end

  # 画面质检结论的指纹（成片文件 + 清单版本），不抽帧。自动补救据此判断已存的结论还算不算数
  # （0.38.0-rc1）。成片文件不在时返回 nil。
  def frames_basis(job)
    path = job["outputPath"].to_s
    return nil unless File.file?(path)

    stat = File.stat(path)
    ReviewMaterial.fingerprint([job["id"], job["mediaFile"], stat.size, stat.mtime.to_f, VideoQA::RUBRIC_VERSION].join("\n"))
  end

  private

  def scope_and_aspect(arguments)
    scope = arguments["scope"].to_s
    aspect = arguments["aspect"].to_s
    return [nil, nil, failure("invalid_scope", "scope must be one of #{SCOPES.keys.join(', ')}.")] unless SCOPES.key?(scope)
    return [nil, nil, failure("invalid_aspect", "#{scope} can be reviewed for: #{SCOPES[scope].join(', ')}.")] unless SCOPES[scope].include?(aspect)
    [scope, aspect, nil]
  end

  # 调用方自己审完以后该怎么写回：工具名与参数原样给出，省得再猜。
  def write_back(scope, aspect, arguments, built)
    if scope == "video"
      { "tool" => "video.record_review", "arguments" => { "id" => arguments["jobID"].to_s, "aspect" => aspect, "basis" => built["basis"], "mediaVideos" => built["videoPaths"].length, "mediaImages" => built["imagePaths"].length } }
    else
      ids = { "dramaID" => arguments["dramaID"].to_s, "scope" => scope, "aspect" => aspect }
      %w[characterID episodeID shotID assetID].each { |key| ids[key] = arguments[key].to_s unless arguments[key].to_s.empty? }
      { "tool" => "drama.record_review", "arguments" => ids.merge("basis" => built["basis"], "mediaImages" => built["imagePaths"].length, "mediaVideos" => 0) }
    end
  end

  def drama_material(arguments, scope, aspect, locale, media)
    loaded = @dramas.get("id" => arguments["dramaID"])
    return [loaded, nil] unless loaded["ok"]

    target = { "scope" => scope, "aspect" => aspect, "characterID" => arguments["characterID"].to_s,
               "episodeID" => arguments["episodeID"].to_s, "shotID" => arguments["shotID"].to_s, "assetID" => arguments["assetID"].to_s }
    material = ReviewMaterial.build(loaded["drama"], target, locale: locale, media: media)
    return [failure("nothing_to_review", "The target does not exist or has no adopted content or images to review yet."), nil] unless material

    record = lambda do |verdict, model, built|
      @dramas.record_review(
        "dramaID" => arguments["dramaID"], "scope" => scope, "aspect" => aspect,
        "characterID" => arguments["characterID"], "episodeID" => arguments["episodeID"], "shotID" => arguments["shotID"], "assetID" => arguments["assetID"],
        "review" => verdict, "basis" => built["basis"], "model" => model,
        "mediaImages" => built["imagePaths"].length, "mediaVideos" => built["videoPaths"].length
      )
    end
    [material, record]
  end

  def video_material(arguments, locale, media)
    job = @videos.jobs_with_media.find { |entry| entry["id"] == arguments["jobID"].to_s }
    return [failure("not_found", "Video job was not found."), nil] unless job

    if arguments["aspect"] == "frames"
      return [failure("video_qa_unavailable", "Frame review is unavailable on this server."), nil] unless @video_qa && media["images"]

      checks = @video_qa.material(job, expected_duration: job["duration"])
      return [checks, nil] if checks["ok"] == false
      material = ReviewMaterial.build_job(job, locale: locale, media: media)
      return [failure("nothing_to_review", "The completed clip has no review material."), nil] unless material
      material["imagePaths"] = checks["imagePaths"]
      material["videoPaths"] = []
      material["autoChecks"] = checks["autoChecks"]
      material["basis"] = frames_basis(job)
      material["text"] = [material["text"], "【本地自动检测】\n#{JSON.generate(checks['autoChecks'])}", shot_review_context(job, locale)].reject(&:empty?).join("\n\n")
      material["systemExtra"] = frame_review_instructions(locale)
    else
      material = ReviewMaterial.build_job(job, locale: locale, media: media)
    end
    unless material
      return [failure("nothing_to_review", "Only completed clips mirrored into the plugin media folder can be reviewed. Call video.prepare_playback first."), nil]
    end

    record = lambda do |verdict, model, built|
      @videos.record_review("id" => job["id"], "aspect" => arguments["aspect"], "review" => verdict, "basis" => built["basis"], "model" => model,
                            "mediaImages" => built["imagePaths"].length, "mediaVideos" => built["videoPaths"].length)
    end
    [material, record]
  end

  def request_for(material, locale, settings)
    skill = @skills.compose(stage: "review", common: [])["system"].to_s
    contract = ReviewMaterial.translate(locale, "reviewContract", "fence" => ReviewMaterial::FENCE)
    # systemExtra 只有成片质检会给；剧、角色、分集、镜头的材料里没有这个键。
    system = [skill, material["systemExtra"].to_s, contract.to_s].reject(&:empty?).join("\n\n")
    has_media = !(material["imagePaths"].empty? && material["videoPaths"].empty?)
    content = [
      ReviewMaterial.translate(locale, "reviewRequest", "label" => material["label"]),
      has_media ? ReviewMaterial.translate(locale, "reviewMediaAttached", "images" => material["imagePaths"].length, "videos" => material["videoPaths"].length) : "",
      material["text"],
      # 不进指纹的上下文（目前只有专家席材料会带）。
      material["context"].to_s
    ].reject(&:empty?).join("\n\n")
    message = { "role" => "user", "content" => content }
    message["imagePaths"] = material["imagePaths"] unless material["imagePaths"].empty?
    message["videoPaths"] = material["videoPaths"] unless material["videoPaths"].empty?
    route({ "system" => system, "messages" => [message] }, settings)
  end

  # 与页面 reviewRouting() 一致：单独选了审核模型就用它，否则跟随策划模型。
  def route(request, settings)
    provider, model = if settings["reviewProviderID"]
                        [settings["reviewProviderID"], settings["reviewModel"]]
                      else
                        [settings["assistantProviderID"], settings["assistantModel"]]
                      end
    request["provider"] = provider if provider
    request["model"] = model if provider && model
    request
  end

  def shot_review_context(job, locale)
    drama_id = job["dramaID"].to_s
    return "" if drama_id.empty?

    loaded = @dramas.get("id" => drama_id)
    return "" unless loaded["ok"]

    drama = loaded["drama"]
    episode = Array(drama["episodes"]).find { |entry| entry["id"] == job["episodeID"].to_s }
    shot = episode && Array(episode["shots"]).find { |entry| entry["id"] == job["shotID"].to_s }
    return "" unless shot

    title = locale == "en" ? "Shot context" : "镜头上下文"
    dialogue = Array(shot["dialogue"]).map { |line| "#{line['speaker']}：#{line['text']}" }.join("\n")
    # 不用 filter_map：/usr/bin/ruby 2.6 没有它（见 video_qa.rb）。
    cast = Array(shot.dig("package", "cast")).map do |member|
      character = Array(drama["characters"]).find { |entry| entry["id"] == member["characterID"] }
      next unless character

      [character["name"], member["screenPosition"], character["visualPrompt"]].map(&:to_s).reject(&:empty?).join("，")
    end.compact.join("\n")
    ["【#{title}】", shot["title"], shot["summary"], shot["actionStart"], shot["actionEnd"], shot["cameraIntent"],
     dialogue, (cast.empty? ? "" : "出场人物：#{cast}")].map(&:to_s).reject(&:empty?).join("\n")
  end

  def frame_review_instructions(locale)
    if locale == "en"
      "Review the attached contact sheets (each cell is one moment; when no timestamp is burned into a cell, derive it from sheetLayout in the local checks) for injury marks, blood or blood-like stains, text burned into the image, wrong or extra identities, and unexplained scene or lighting jumps (a hard switch inside the clip to another camera position, shot size or subject that the shot description does not call for is also scene_jump). Flag continuity and framing problems as warnings. Use categories injury, blood, text_overlay, identity, scene_jump, continuity, framing only. The first five are block severity; continuity and framing are warn. Give an approximate timestamp such as 12.5s in location. Suggestions must use positive descriptions, never negations. Scene-cut detections are inspection clues, not automatic failures."
    else
      "检查附带的抽帧拼图（每格一个时间点；格子上没有时间戳时，按【本地自动检测】里的 sheetLayout 换算）：手腕、手背、指节、掌心、脸颈的伤痕；血迹或血迹状污渍；画面烧录叠字；人物错位或额外人物；分镜无法解释的场景或光线跳变（片段中途硬切到分镜没写的另一个机位、景别或主体，也记 scene_jump）。连续性与构图问题记为 warn。category 只允许 injury、blood、text_overlay、identity、scene_jump、continuity、framing；前五类 severity 为 block，后两类为 warn。location 写近似时间点，例如 12.5s。suggestion 使用正面描述，不写否定句。硬切检测结果是复核线索，不自动判失败。"
    end
  end

  def failure(code, message)
    { "ok" => false, "error" => { "code" => code, "message" => message } }
  end
end
