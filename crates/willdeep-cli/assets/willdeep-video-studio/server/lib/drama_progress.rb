# frozen_string_literal: true

# `drama.get_progress`：告诉聊天里的 Agent 这部剧做到哪了、下一步做什么。
#
# 没有它，Agent 只能反复 `drama.get` 读回整部剧自己推断：一部十集的剧 JSON
# 几十 KB，每读一次就吃掉一大块上下文，还容易漏掉某一镜没选首帧。
#
# 判据只看**已采用**的内容，与出图、出视频的前置条件一致：草稿随时会被丢，
# 以草稿算「已完成」会让 Agent 在一份不作数的文字上去花钱出图。
#
# 下一步按生产顺序排：审核拦下的先处理 → 带必改项且没被「已知悉」的 warn → 待审 → 角色设计 → 定妆图 → 资产（造型、场景、
# 道具、声音）→ 分集正文 → 分镜 → 首帧提示词 → 首帧图 → 连续性检查 → 视频。
#
# 0.26.0（设计稿 5.1、Phase 5）：视频任务优先按 shotID 关联，旧任务仍按参考图路径
# 反查；连续性检查并入这里，页面与 Agent 看到同一份结果。
require_relative "review_material"
require_relative "review_record"
require_relative "reference_package"
require_relative "media_ref"
require_relative "consistency_check"
require_relative "qa_remediation"
require_relative "video_selection"
require_relative "clip_trim"

class DramaProgress
  MAX_STEPS = 12
  # 调用方可以要更多步（maxSteps），上限在这里：一部剧的待办不会超过这个数。
  MAX_STEPS_LIMIT = 200
  # 超过这么多集、又没指定集时，各集只给摘要（detail: "full" 仍给全部逐镜明细）。
  COMPACT_EPISODES_OVER = 6
  ACTIVE_VIDEO_STATES = %w[submitting queued in_progress].freeze
  MEDIA = { "images" => true, "videos" => true }.freeze
  ASPECT_NAMES = { "content" => "内容", "images" => "画面", "storyboard" => "分镜", "panel" => "专家席" }.freeze
  KIND_NAMES = { "appearance" => "造型", "scene" => "场景", "sceneVariant" => "场景变体", "prop" => "道具", "voice" => "声音" }.freeze
  # 专家席审稿（0.42.0-rc1）要调多次模型，待审步骤的工具提示写明走后台。
  REVIEW_TOOL_HINT = "review.run (or review.get_material, then drama.record_review)"
  PANEL_TOOL_HINT = "review.run async=true (expert panel: several model calls, minutes; or review.get_material, play the panel yourself, then drama.record_review)"

  # jobs 是一个返回视频任务数组的 lambda：进度服务不持有视频存档，测试里也
  # 不必起一个真的视频服务。settings 同样是 lambda：审核材料的标签随页面语言走。
  # capabilities：Provider 能力声明，参考包上限检查要用。
  # background_jobs：给定剧 ID，返回这部剧还在排队或在跑的后台任务（0.36.0-rc1）。
  # annotate：给读出的剧挂候选图推荐（ImageQA#annotate，0.40.0-rc1）；不给时没有推荐。
  def initialize(drama_service:, jobs: -> { [] }, settings: -> { {} }, capabilities: -> { {} }, background_jobs: ->(_drama_id) { [] },
                 annotate: ->(drama) { drama })
    @annotate = annotate
    @dramas = drama_service
    @jobs = jobs
    @settings = settings
    @capabilities = capabilities
    @background_jobs = background_jobs
  end

  # 下一步里能用按集批量工具一次做完的几类（0.36.0-rc1，复盘 4.1）。
  BATCH_ACTIONS = {
    "generate_start_frame" => "episode.generate_frames",
    # 候选图质检给出了可直接用的推荐（0.40.0-rc1）：一次 episode.accept_recommended_frames 全部选上。
    "accept_recommended_frame" => "episode.accept_recommended_frames",
    "run_review" => "review.run_batch",
    "generate_video" => "episode.generate_videos",
    "refresh_video" => "episode.generate_videos",
    "remediate_video" => "episode.remediate_videos",
    "regenerate_dialogue_audio" => "episode.dub"
  }.freeze
  REVIEW_BATCH_SCOPES = { "drama" => "drama", "character" => "characters", "asset" => "assets", "episode" => "episodes", "shot" => "shots" }.freeze

  def get_progress(arguments)
    loaded = @dramas.get("id" => arguments["dramaID"])
    return loaded unless loaded["ok"]

    drama = @annotate.call(loaded["drama"]) || loaded["drama"]
    settings = @settings.call
    @locale = settings["uiLocale"] == "en" ? "en" : "zh-Hans"
    # 专家席审稿（0.42.0-rc1）：设置关掉时进度不列 panel 面，批量审核（按进度排项）随之不列。
    @panel = settings["panelReview"] != false
    jobs = Array(@jobs.call)
    characters = drama["characters"].map { |character| character_state(drama, character) }
    assets = Array(drama["assets"]).map { |asset| asset_state(drama, asset) }
    episodes = drama["episodes"].sort_by { |episode| episode["order"].to_i }.map { |episode| episode_state(drama, episode, jobs) }
    drama_details = {}
    drama_targets = { "content" => { "scope" => "drama", "aspect" => "content" } }
    drama_targets["panel"] = { "scope" => "drama", "aspect" => "panel" } if @panel
    drama_review = review_states(drama, drama, drama_targets, drama_details)
    consistency = consistency_summary(drama)
    steps = next_steps(drama_review, drama_details, characters, assets, episodes, consistency)
    limit = begin
      Integer(arguments["maxSteps"])
    rescue ArgumentError, TypeError
      MAX_STEPS
    end
    limit = MAX_STEPS unless limit.positive?
    limit = [limit, MAX_STEPS_LIMIT].min
    focus = focus_episode(drama, arguments)
    return focus if focus.is_a?(Hash)

    # 只看一集（0.41.0-rc5）：下一步只留这一集的；其余集与整剧层面的步骤由不带参数的调用给出。
    steps = steps.select { |entry| entry.dig("target", "episodeID") == focus } if focus
    background = Array(@background_jobs.call(drama["id"]))
    steps.each { |entry| attach_batch(entry, drama["id"]) }
    compact = focus || (arguments["detail"] != "full" && episodes.length > COMPACT_EPISODES_OVER)
    # 只看一集时其余集不列（整剧计数在 summary 里）；不指定集的长剧各集只给摘要。
    listed = if focus
               episodes.select { |episode| episode["id"] == focus }
             elsif compact
               episodes.map { |episode| compact_episode(episode) }
             else
               episodes
             end
    {
      "ok" => true,
      "drama" => with_review_detail({
        "id" => drama["id"], "title" => drama["title"], "status" => drama["status"],
        "episodeCount" => drama["episodes"].length, "hasDraft" => draft?(drama),
        "review" => drama_review
      }, drama_details),
      "summary" => summary(characters, assets, episodes),
      "consistency" => consistency,
      "characters" => characters,
      "assets" => assets,
      "episodes" => listed,
      "focusEpisodeID" => focus,
      "episodesCompact" => compact ? true : nil,
      "nextSteps" => steps.first(limit),
      "remainingSteps" => [steps.length - limit, 0].max,
      "batchSteps" => batch_steps(drama, steps, focus ? listed : episodes, background),
      "backgroundJobs" => background
    }.reject { |_key, value| value.nil? }
  end

  # episodeOrder / episodeID 指定的那一集的 ID；都没给返回 nil；给了却找不到返回失败回包。
  def focus_episode(drama, arguments)
    id = arguments["episodeID"].to_s.strip
    order = arguments["episodeOrder"]
    return nil if id.empty? && order.nil?

    episode = drama["episodes"].find { |entry| (!id.empty? && entry["id"] == id) || (id.empty? && entry["order"].to_i == order.to_i) }
    return episode["id"] if episode

    { "ok" => false, "error" => { "code" => "episode_not_found", "message" => "Episode #{id.empty? ? "order #{order}" : id} was not found." } }
  end

  # 一集的摘要：去掉逐镜明细，只留计数（0.41.0-rc5）。24 集的剧逐镜全给有 200 KB，主会话的模型吃不下也用不上。
  def compact_episode(episode)
    shots = Array(episode["shots"])
    episode.reject { |key, _value| key == "shots" }.merge(
      "shotCount" => shots.length,
      "shotsWithStartFrame" => shots.count { |shot| shot["startSelected"] },
      "shotsWithVideo" => shots.count { |shot| shot["video"] && shot["video"]["state"] == "completed" },
      "videosActive" => shots.count { |shot| shot["video"] && ACTIVE_VIDEO_STATES.include?(shot["video"]["state"]) },
      "framesQABlocked" => shots.count { |shot| shot.dig("framesQA", "status") == "block" && !shot.dig("framesQA", "qaOverride") },
      "continuityIssues" => shots.sum { |shot| Array(shot["continuity"]).length },
      "shotsOmitted" => true
    )
  end

  private

  # 能用批量工具做的那一步带上 batch: {tool, arguments}，一次调用覆盖整集同类的步骤。
  def attach_batch(entry, drama_id)
    tool = BATCH_ACTIONS[entry["action"]]
    return unless tool

    target = entry["target"]
    arguments = { "dramaID" => drama_id }
    if tool == "review.run_batch"
      scope = REVIEW_BATCH_SCOPES[target["scope"]]
      return unless scope

      arguments["scope"] = [scope]
      arguments["episodeID"] = target["episodeID"] if target["episodeID"]
    else
      return if target["episodeID"].to_s.empty?

      arguments["episodeID"] = target["episodeID"]
      arguments["target"] = "start" if %w[episode.generate_frames episode.accept_recommended_frames].include?(tool)
    end
    entry["batch"] = { "tool" => tool, "arguments" => arguments }
  end

  # 按（工具，集）汇总：每条写清一次批量调用能覆盖多少步；同类批量任务正在跑时给出它的
  # jobID，别再提交一次。另补一类 nextSteps 里没有的：有已授权声音、但台词还没配音的集。
  def batch_steps(drama, steps, episodes, background)
    grouped = {}
    steps.each do |entry|
      batch = entry["batch"]
      next unless batch

      key = [batch["tool"], batch["arguments"]["episodeID"].to_s]
      grouped[key] ||= { "tool" => batch["tool"], "arguments" => batch["arguments"].dup, "covers" => 0, "actions" => [] }
      if batch["tool"] == "review.run_batch"
        scopes = (grouped[key]["arguments"]["scope"] + batch["arguments"]["scope"]).uniq
        grouped[key]["arguments"]["scope"] = scopes
      end
      grouped[key]["covers"] += 1
      grouped[key]["actions"] |= [entry["action"]]
    end
    if Array(drama["assets"]).any? { |asset| asset["kind"] == "voice" && !asset["archived"] && ReferencePackage.consent_granted?(asset) }
      episodes.each do |episode|
        missing = episode["shots"].sum { |shot| shot["dialogueLines"] - shot["dialogueWithAudio"] }
        next unless missing.positive?

        key = ["episode.dub", episode["id"]]
        grouped[key] ||= { "tool" => "episode.dub", "arguments" => { "dramaID" => drama["id"], "episodeID" => episode["id"] }, "covers" => 0, "actions" => [] }
        grouped[key]["dialogueLinesWithoutAudio"] = missing
      end
    end
    grouped.values.map do |entry|
      running = background.find do |job|
        job["kind"] == entry["tool"] && (entry["arguments"]["episodeID"].to_s.empty? || job["episodeID"] == entry["arguments"]["episodeID"])
      end
      entry["runningJobID"] = running["id"] if running
      entry["reason"] = batch_reason(entry, episodes)
      entry
    end
  end

  def batch_reason(entry, episodes)
    episode = episodes.find { |item| item["id"] == entry["arguments"]["episodeID"] }
    where = episode ? "第 #{episode['order']} 集" : "全剧"
    case entry["tool"]
    when "episode.generate_frames" then "#{where}有 #{entry['covers']} 镜还没有首帧候选图，一次 episode.generate_frames 全部出完。"
    when "episode.accept_recommended_frames" then "#{where}有 #{entry['covers']} 镜的首帧候选质检有可直接用的推荐、还没选定，一次 episode.accept_recommended_frames 全部选上。"
    when "episode.generate_videos" then "#{where}有 #{entry['covers']} 镜待提交或在生成视频，一次 episode.generate_videos 提交并轮询下载。"
    when "episode.remediate_videos" then "#{where}有 #{entry['covers']} 镜的成片画面质检发现可自动重做的问题，一次 episode.remediate_videos 加补救句重拍、复审并选最好的一条（每次重拍都计费）。"
    when "episode.dub"
      count = entry["dialogueLinesWithoutAudio"] || entry["covers"]
      "#{where}有 #{count} 句台词待配音或音频过期，一次 episode.dub 全部配完。"
    else "#{where}有 #{entry['covers']} 项待审或审核已过期，一次 review.run_batch 审完（跳过结论仍有效的）。"
    end
  end

  def character_state(drama, character)
    candidates = Array(character["candidates"])
    base = { "scope" => "character", "characterID" => character["id"] }
    details = {}
    with_review_detail({
      "id" => character["id"], "name" => character["name"],
      "designed" => filled?(character["description"]) && filled?(character["visualPrompt"]),
      "hasDraft" => draft?(character),
      "candidates" => candidates.length,
      "identityLocked" => candidates.any? { |entry| entry["id"] == character["selectedCandidateID"] },
      "review" => review_states(drama, character,
                                { "content" => base.merge("aspect" => "content"), "images" => base.merge("aspect" => "images") }, details)
    }, details)
  end

  def asset_state(drama, asset)
    candidates = Array(asset["candidates"])
    base = { "scope" => "asset", "assetID" => asset["id"] }
    targets = { "content" => base.merge("aspect" => "content") }
    targets["images"] = base.merge("aspect" => "images") unless asset["kind"] == "voice"
    details = {}
    state = {
      "id" => asset["id"], "kind" => asset["kind"], "name" => asset["name"], "archived" => asset["archived"] == true,
      "designed" => filled?(asset["prompt"]), "hasDraft" => draft?(asset),
      "candidates" => candidates.length,
      "selected" => candidates.any? { |entry| entry["id"] == asset["selectedCandidateID"] },
      "review" => review_states(drama, asset, targets, details)
    }
    state["characterID"] = asset["characterID"] if asset.key?("characterID")
    state["consentGranted"] = ReferencePackage.consent_granted?(asset) if asset["kind"] == "voice"
    with_review_detail(state, details)
  end

  def episode_state(drama, episode, jobs)
    shots = Array(episode["shots"]).sort_by { |shot| shot["order"].to_i }
    states = []
    shots.each_with_index { |shot, index| states << shot_state(drama, episode, shot, jobs, index.zero? ? nil : shots[index - 1]) }
    details = {}
    with_review_detail({
      "id" => episode["id"], "order" => episode["order"], "title" => episode["title"],
      "hasSummary" => filled?(episode["summary"]), "hasScript" => filled?(episode["script"]),
      "hasDraft" => draft?(episode),
      "review" => review_states(drama, episode, episode_review_targets(episode), details),
      "shots" => states
    }, details)
  end

  def episode_review_targets(episode)
    base = { "scope" => "episode", "episodeID" => episode["id"] }
    targets = { "content" => base.merge("aspect" => "content"), "storyboard" => base.merge("aspect" => "storyboard") }
    targets["panel"] = base.merge("aspect" => "panel") if @panel
    targets
  end

  def shot_state(drama, episode, shot, jobs, previous)
    start = Array(shot["startCandidates"]).find { |entry| entry["id"] == shot["selectedStartID"] }
    video = latest_video(jobs, shot, start)
    video_review, video_detail = video ? video_review_of(video) : [nil, nil]
    details = {}
    state = {
      "id" => shot["id"], "order" => shot["order"], "title" => shot["title"],
      "hasDraft" => draft?(shot),
      "hasStartPrompt" => filled?(shot["startPrompt"]),
      "startCandidates" => Array(shot["startCandidates"]).length,
      "startSelected" => !start.nil?,
      # 候选图质检的推荐（0.40.0-rc1）：最好的、结论新鲜且没被拦截的那张；eligible 表示通过或只剩建议。
      "startRecommendation" => shot["startRecommendation"],
      "startQA" => frame_qa_counts(shot["startCandidates"]),
      "hasEndPrompt" => filled?(shot["endPrompt"]),
      "endCandidates" => Array(shot["endCandidates"]).length,
      "hasPackage" => shot["package"].is_a?(Hash),
      "dialogueLines" => Array(shot["dialogue"]).length,
      "dialogueWithAudio" => Array(shot["dialogue"]).count { |line| line.is_a?(Hash) && line["audio"].is_a?(Hash) && line["audio"]["selectedCandidateID"] },
      "review" => review_states(drama, shot,
                                { "images" => { "scope" => "shot", "aspect" => "images", "episodeID" => episode["id"], "shotID" => shot["id"] } }, details),
      "continuity" => continuity(drama, episode, shot, previous),
      "video" => video && { "jobID" => video["id"], "state" => video["state"], "mode" => video["generationMode"] || video["mode"],
                            "review" => video_review, "reviewDetail" => video_detail }.compact
    }
    qa = frames_qa(shot, jobs)
    state["framesQA"] = qa if qa
    stale = VideoSelection.stale(shot, jobs)
    state["staleSelection"] = stale if stale
    # 台词重配后，这一镜现在用的成片口型对的还是旧配音（0.40.0-rc1）。
    clip_id = qa && qa["jobID"]
    clip = clip_id && jobs.find { |job| job["id"] == clip_id }
    dialogue_stale = clip ? VideoSelection.dialogue_stale(shot, clip) : nil
    state["dialogueStale"] = dialogue_stale if dialogue_stale
    with_review_detail(state, details)
  end

  # 这一镜当前成片（选定的，没选就取最新完成的，与合成计划同一口径）的画面质检摘要（0.38.0-rc1）。
  # remediable：设置里自动补救的类别中，结论里出现了的；人工放行过这条成片的不算。
  def frames_qa(shot, jobs)
    usable = jobs.select { |job| job["shotID"] == shot["id"] && job["state"] == "completed" && !job["outputPath"].to_s.empty? }
    clip = usable.find { |job| job["id"] == shot["selectedVideoID"].to_s } || usable.max_by { |job| [job["createdAt"].to_s, job["updatedAt"].to_s] }
    return nil unless clip

    review = clip.dig("reviews", "frames")
    return { "jobID" => clip["id"], "status" => "none" } unless review.is_a?(Hash)

    # 裁剪裁掉的 scene_jump 不算（0.39.0-rc1，lib/clip_trim.rb）：状态与可补救类别都按生效结论。
    review = ClipTrim.effective_for(shot, clip)
    categories = Array(@settings.call["remediateCategories"] || QARemediation::DEFAULT_AUTO_CATEGORIES)
    overridden = shot.dig("qaOverride", "jobID") == clip["id"]
    trim = ClipTrim.for_job(shot, clip["id"])
    {
      "jobID" => clip["id"], "status" => review["status"],
      "categories" => Array(review["issues"]).map { |issue| issue.is_a?(Hash) ? issue["category"].to_s : "" }.reject(&:empty?).uniq,
      "remediable" => overridden ? [] : QARemediation.remediable_categories(review, categories),
      "qaOverride" => overridden,
      "trim" => trim && trim.slice("inSeconds", "outSeconds", "source")
    }.reject { |_key, value| value.nil? }
  end

  # 与页面 reviewState 同一口径：没审过 none；审过但材料指纹变了 stale；否则是结论。
  # 没有可审材料的一面（没出过图、没写正文）不列出来。
  #
  # 状态值保持 none / stale / pass / warn / block 不变（对外契约）。warn 的分量另记在
  # details[aspect]：必改几条、建议几条、是否已知悉（0.35.0-rc1）。
  def review_states(drama, node, targets, details = {})
    stored = node["reviews"].is_a?(Hash) ? node["reviews"] : {}
    targets.each_with_object({}) do |(aspect, target), result|
      material = ReviewMaterial.build(drama, target, locale: @locale, media: MEDIA)
      next unless material

      result[aspect] = state_of(stored[aspect], material)
      detail = warn_detail(stored[aspect], material, result[aspect])
      details[aspect] = detail if detail
    end
  end

  def video_review_of(job)
    material = ReviewMaterial.build_job(job, locale: @locale, media: MEDIA)
    return [nil, nil] unless material

    state = state_of(job["review"], material)
    [state, warn_detail(job["review"], material, state)]
  end

  def state_of(record, material)
    return "none" unless record.is_a?(Hash)
    return "stale" if record["basis"] != material["basis"]

    record["status"]
  end

  def warn_detail(record, material, state)
    return nil unless state == "warn"

    must = ReviewRecord.must_fix_count(record)
    detail = { "must" => must, "advice" => Array(record["issues"]).length - must,
               "acknowledged" => ReviewRecord.acknowledged?(record, material["basis"]) }
    detail["acknowledgedBy"] = record.dig("acknowledgement", "acknowledgedBy") if detail["acknowledged"]
    detail
  end

  # 节点上附 reviewDetail（只在有 warn 时出现，免得几百个空对象撑大返回值）。
  def with_review_detail(state, details)
    details.empty? ? state : state.merge("reviewDetail" => details)
  end

  # 0.26.0 起视频任务记 shotID，按它取该镜最新一次提交；旧任务没有 shotID 时按
  # 「提交时的参考图就是该镜选定首帧」反查。
  def latest_video(jobs, shot, start)
    by_id = jobs.select { |job| job["shotID"] == shot["id"] }.max_by { |job| job["createdAt"].to_s }
    return by_id if by_id
    return nil if start.nil? || start["filePath"].to_s.empty?

    jobs.select { |job| job["shotID"].to_s.empty? && job["referenceImagePath"] == start["filePath"] }.max_by { |job| job["createdAt"].to_s }
  end

  # 连续性检查（设计稿 Phase 5）。每条带 code / message / 可选的 assetID、characterID、lineID。
  def continuity(drama, _episode, shot, previous)
    issues = []
    assets = Array(drama["assets"])
    package = shot["package"].is_a?(Hash) ? shot["package"] : nil
    has_scenes = assets.any? { |asset| asset["kind"] == "scene" && !asset["archived"] }
    if package.nil?
      issues << issue("no_package", "还没有参考包：场景、造型与声音都没绑定。") if has_scenes || assets.any? { |asset| !asset["archived"] }
      return issues + audio_issues(shot)
    end

    issues << issue("scene_missing", "本镜没有绑定场景。") if has_scenes && package["sceneID"].to_s.empty?
    check = lambda do |id, label, kind|
      next if id.to_s.empty?
      asset = assets.find { |entry| entry["id"] == id.to_s }
      if asset.nil?
        issues << issue("asset_missing", "#{label}引用了不存在的#{KIND_NAMES.fetch(kind, kind)}。", "assetID" => id.to_s)
      elsif asset["archived"]
        issues << issue("asset_archived", "#{label}引用的#{KIND_NAMES.fetch(kind, kind)}「#{asset['name']}」已归档。", "assetID" => asset["id"])
      elsif kind != "voice" && Array(asset["candidates"]).none? { |entry| entry["id"] == asset["selectedCandidateID"] }
        issues << issue("asset_no_image", "#{label}引用的#{KIND_NAMES.fetch(kind, kind)}「#{asset['name']}」还没有选定参考图。", "assetID" => asset["id"])
      elsif kind == "voice" && !ReferencePackage.consent_granted?(asset)
        issues << issue("voice_consent_missing", "声音「#{asset['name']}」还没有确认授权。", "assetID" => asset["id"])
      end
    end
    check.call(package["sceneID"], "场景", "scene")
    check.call(package["sceneVariantID"], "场景变体", "sceneVariant")
    Array(package["propIDs"]).each { |id| check.call(id, "道具", "prop") }
    previous_cast = previous && previous["package"].is_a?(Hash) ? Array(previous["package"]["cast"]) : []
    Array(package["cast"]).each do |binding|
      next unless binding.is_a?(Hash)
      character = drama["characters"].find { |entry| entry["id"] == binding["characterID"] }
      name = character ? character["name"] : binding["characterID"]
      check.call(binding["appearanceID"], "「#{name}」", "appearance")
      check.call(binding["voiceID"], "「#{name}」", "voice")
      before = previous_cast.find { |entry| entry.is_a?(Hash) && entry["characterID"] == binding["characterID"] }
      next unless before && before["appearanceID"].to_s != binding["appearanceID"].to_s && binding["appearanceChange"].to_s.strip.empty?

      issues << issue("appearance_change_unexplained", "「#{name}」的造型与上一镜不同，但没有写换装说明。", "characterID" => binding["characterID"])
    end

    capabilities = @capabilities.call
    unless capabilities.empty?
      compiled = ReferencePackage.compile(drama, nil, shot, purpose: "image", capabilities: capabilities, media_root: @dramas.media_root)
      if compiled["ok"]
        issues << issue("over_limit", "参考图超过出图上限，已有 #{compiled['dropped'].length} 项会被去掉。") unless compiled["dropped"].empty?
        issues << issue("over_limit_required", "必带的身份图超过出图上限，请精简出场角色。") if compiled["overLimit"]
      end
    end
    issues + audio_issues(shot)
  end

  def audio_issues(shot)
    issues = []
    Array(shot["dialogue"]).each do |line|
      next unless line.is_a?(Hash) && line["audio"].is_a?(Hash)
      selected = Array(line["audio"]["candidates"]).find { |entry| entry["id"] == line["audio"]["selectedCandidateID"] }
      next unless selected
      current = ReviewMaterial.fingerprint(line["text"].to_s.strip)
      if !selected["textFingerprint"].to_s.empty? && selected["textFingerprint"] != current
        issues << issue("audio_stale", "「#{line['speaker']}」的这句台词改过了，音频需要重新生成。", "lineID" => line["id"])
      end
      duration = selected["durationMs"].to_i
      limit = shot["duration"].to_i * 1000
      issues << issue("audio_too_long", "「#{line['speaker']}」的这句音频 #{duration} 毫秒，超过镜头时长 #{shot['duration']} 秒。", "lineID" => line["id"]) if duration.positive? && limit.positive? && duration > limit
    end
    issues
  end

  # 全剧一致性（0.37.0-rc1，lib/consistency_check.rb）：只给计数，明细走 drama.check_consistency。
  def consistency_summary(drama)
    checked = ConsistencyCheck.run(drama)
    checked["summary"].merge("canonRev" => checked["canonRev"], "canonEmpty" => checked["canonEmpty"])
  end

  # 一致性问题排一步、放在审核之后、生产之前：禁用词和前后矛盾的数一路带进分镜和
  # 出图，越往后改越贵。数字冲突只是「可能」，原因里写明，交给 Agent 核对。
  def consistency_step(consistency)
    return nil unless consistency["total"].to_i.positive?

    parts = []
    parts << "禁用词 #{consistency['bannedTerms']} 处" if consistency["bannedTerms"].to_i.positive?
    parts << "未建档的说话人 #{consistency['unknownSpeakers']} 个" if consistency["unknownSpeakers"].to_i.positive?
    parts << "可能与设定账本冲突的数字 #{consistency['facts']} 处" if consistency["facts"].to_i.positive?
    parts << "违反视觉口径 #{consistency['visualRules']} 处" if consistency["visualRules"].to_i.positive?
    step("fix_consistency", "全剧一致性检查发现：#{parts.join('，')}。",
         "drama.check_consistency for the locations, then revise those fields (drama.save_draft commit=true), add the speaker as a character or to canon.allowedExtras, or update the canon with drama.save_canon if the story really changed",
         {})
  end

  def next_steps(drama_review, drama_details, characters, assets, episodes, consistency = { "total" => 0 })
    steps = []
    blocked, warned, pending = review_steps(drama_review, drama_details, characters, assets, episodes)
    steps.concat(blocked)
    steps.concat(warned)
    steps.concat(pending)
    fix = consistency_step(consistency)
    steps << fix if fix

    characters.each do |character|
      target = { "characterID" => character["id"] }
      if !character["designed"]
        steps << step("design_character", "「#{character['name']}」还没有人物小传或身份视觉提示词。",
                      "drama.get_stage_context stage=characters, then drama.save_draft scope=character commit=true", target)
      elsif character["candidates"].zero?
        steps << step("generate_identity_images", "「#{character['name']}」还没有定妆候选图。", "image.generate target=character", target)
      elsif !character["identityLocked"]
        steps << step("select_identity_image", "「#{character['name']}」有 #{character['candidates']} 张候选图，还没选定形象。",
                      "drama.select_character_image", target)
      end
    end
    cast_ready = characters.all? { |character| character["identityLocked"] }

    assets.reject { |asset| asset["archived"] }.each do |asset|
      target = { "assetID" => asset["id"] }
      label = "#{KIND_NAMES.fetch(asset['kind'], asset['kind'])}「#{asset['name']}」"
      if !asset["designed"]
        steps << step("design_asset", "#{label}还没有描述提示词。", "drama.get_stage_context stage=#{asset['kind'] == 'sceneVariant' ? 'scene' : asset['kind']}, then drama.save_draft scope=asset commit=true", target)
      elsif asset["kind"] == "voice"
        steps << step("confirm_voice_consent", "#{label}还没有确认授权，不能用于生成。", "drama.save_asset consent={status: granted, grantedBy}", target) unless asset["consentGranted"]
      elsif asset["candidates"].zero?
        steps << step("generate_asset_images", "#{label}还没有参考图候选。", "image.generate target=#{asset['kind']}", target)
      elsif !asset["selected"]
        steps << step("select_asset_image", "#{label}有 #{asset['candidates']} 张候选图，还没选定。", "drama.select_asset_media", target)
      end
    end

    episodes.each do |episode|
      target = { "episodeID" => episode["id"] }
      label = "第 #{episode['order']} 集"
      unless episode["hasScript"]
        steps << step("write_episode", "#{label}还没有剧本正文。",
                      "drama.get_stage_context stage=script, then drama.save_draft scope=episode commit=true", target)
        next
      end
      if episode["shots"].empty?
        steps << step("storyboard_episode", "#{label}有正文但还没拆分镜。",
                      "drama.get_stage_context stage=storyboard, then drama.save_shot_drafts commit=true", target)
        next
      end
      episode["shots"].each do |shot|
        steps.concat(shot_steps(label, episode, shot, cast_ready))
        remediation = remediation_step(label, episode, shot)
        steps << remediation if remediation
        stale = stale_selection_step(label, episode, shot)
        steps << stale if stale
        redub = dialogue_stale_step(label, episode, shot)
        steps << redub if redub
      end
    end
    steps
  end

  def shot_steps(label, episode, shot, cast_ready)
    target = { "episodeID" => episode["id"], "shotID" => shot["id"] }
    name = "#{label}第 #{shot['order']} 镜"
    if shot["hasDraft"]
      return [step("resolve_shot_draft", "#{name}有未采用的草稿，出图前要先采用或丢弃。", "drama.commit_draft or drama.discard_draft scope=shot", target)]
    end
    unless shot["hasStartPrompt"]
      return [step("write_frame_prompts", "#{name}还没有首帧提示词。",
                   "drama.get_stage_context stage=frames, then drama.save_draft scope=shot commit=true", target)]
    end
    continuity = continuity_steps(name, shot, target)
    if shot["startCandidates"].zero?
      # 角色没锁形象时出首帧，人物长相会和定妆图对不上。只提示，不硬拦：
      # 空镜、纯景物的镜头本来就不需要角色参照。
      reason = cast_ready ? "#{name}还没有首帧候选图。" : "#{name}还没有首帧候选图（仍有角色没选定形象，出图前先确认本镜不涉及他们）。"
      return continuity + [step("generate_start_frame", reason, "image.generate target=start (dryRun first to see the reference package)", target)]
    end
    return continuity + [select_start_step(name, shot, target)] unless shot["startSelected"]

    video = shot["video"]
    return continuity + [step("generate_video", "#{name}已选定首帧，还没提交视频。", "video.generate dramaID episodeID shotID (dryRun first)", target)] unless video
    return [step("refresh_video", "#{name}的视频还在生成。", "video.refresh", target.merge("jobID" => video["jobID"]))] if ACTIVE_VIDEO_STATES.include?(video["state"])
    return [step("fix_failed_video", "#{name}的视频任务失败了。", "video.list to read the error, then video.retry", target.merge("jobID" => video["jobID"]))] if video["state"] == "failed"

    continuity
  end

  # 有候选、没选定首帧（0.40.0-rc1）：质检推荐可直接用（通过或只剩建议）时排「接受推荐」（批量提示
  # episode.accept_recommended_frames）；推荐有必改项、全被拦截、或还没质检时仍是人来选，理由写明。
  def select_start_step(name, shot, target)
    recommendation = shot["startRecommendation"]
    counts = shot["startQA"] || {}
    if recommendation && recommendation["eligible"]
      return step("accept_recommended_frame",
                  "#{name}有 #{shot['startCandidates']} 张首帧候选，质检推荐 #{recommendation['candidateID']}（#{recommendation['status']}，#{recommendation['score']} 分），还没选定。",
                  "episode.accept_recommended_frames dramaID episodeID (or drama.select_image kind=start candidateID=#{recommendation['candidateID']})",
                  target.merge("candidateID" => recommendation["candidateID"]))
    end
    reason = if recommendation then "#{name}的首帧质检推荐 #{recommendation['candidateID']} 有必改项，看过再选。"
             elsif counts["block"].to_i.positive? && counts["block"] == counts["checked"] && counts["unchecked"].to_i.zero?
               "#{name}的 #{counts['block']} 张首帧候选质检全被拦截，改提示词或参考包后重出，或人工挑一张。"
             else "#{name}有首帧候选图，还没选定。"
             end
    how = counts["unchecked"].to_i.positive? ? "image.qa target=start (check the candidates), then drama.select_image kind=start" : "drama.select_image kind=start"
    step("select_start_frame", reason, how, target)
  end

  # 一组候选的质检计数：checked（结论新鲜）/ block / unchecked（没审或过期）。
  def frame_qa_counts(candidates)
    list = Array(candidates)
    fresh = list.select { |entry| entry["qa"].is_a?(Hash) && entry["qa"]["stale"] == false }
    { "checked" => fresh.length, "block" => fresh.count { |entry| entry["qa"]["status"] == "block" }, "unchecked" => list.length - fresh.length }
  end

  # 已有成片的画面质检发现可自动重做的问题（0.38.0-rc1）：不论这一镜前面的生产步骤到了哪里，
  # 成片已经在那儿了，就排一步补救（批量提示 episode.remediate_videos）。
  def remediation_step(label, episode, shot)
    qa = shot["framesQA"]
    return nil if shot["hasDraft"] || qa.nil? || Array(qa["remediable"]).empty?

    name = "#{label}第 #{shot['order']} 镜"
    step("remediate_video", "#{name}的成片画面质检发现 #{qa['remediable'].join('、')}，可以加补救句自动重拍。",
         "video.remediate jobID (dryRun first; every retake is billed)", { "episodeID" => episode["id"], "shotID" => shot["id"], "jobID" => qa["jobID"] })
  end

  # 台词重配后，这一镜用的成片是照旧配音对口型生成的（0.40.0-rc1）：重新生成这一镜的视频。
  def dialogue_stale_step(label, episode, shot)
    stale = shot["dialogueStale"]
    return nil unless stale

    name = "#{label}第 #{shot['order']} 镜"
    step("regenerate_dialogue_stale_video",
         "#{name}的台词重新配过音，现在用的成片 #{stale['jobID']} 口型对的还是旧配音。",
         "episode.generate_videos dramaID episodeID shotOrders=[#{shot['order']}] force=true onlyMissing=false (the new take replaces it)",
         { "episodeID" => episode["id"], "shotID" => shot["id"], "jobID" => stale["jobID"] })
  end

  # 选定的成片比批量为这一镜新生成的旧（0.38.0-rc3，lib/video_selection.rb）：合成会用旧片，
  # 排一步让人或 Agent 明确选一条——选新片，或再选一次旧片表示就要它（选定时间晚于新片后不再提醒）。
  def stale_selection_step(label, episode, shot)
    stale = shot["staleSelection"]
    return nil unless stale

    name = "#{label}第 #{shot['order']} 镜"
    step("review_stale_selection",
         "#{name}选定的成片 #{stale['selectedJobID']} 比批量后来生成的 #{stale['newerJobID']} 旧，合成会用旧的那条。",
         "drama.select_video videoID=#{stale['newerJobID']} (or select #{stale['selectedJobID']} again to keep it on purpose)",
         { "episodeID" => episode["id"], "shotID" => shot["id"], "jobID" => stale["newerJobID"], "selectedJobID" => stale["selectedJobID"] })
  end

  # 连续性问题里会让出图或出片结果出错的那几类排成步骤；纯提示（没绑场景、
  # 没有参考包）只留在 continuity 列表里，不排步骤。
  def continuity_steps(name, shot, target)
    Array(shot["continuity"]).map do |entry|
      case entry["code"]
      when "asset_missing", "asset_archived", "asset_no_image"
        step("fix_package_reference", "#{name}：#{entry['message']}", "drama.set_reference_package or fix the asset", target.merge("assetID" => entry["assetID"]))
      when "voice_consent_missing"
        step("confirm_voice_consent", "#{name}：#{entry['message']}", "drama.save_asset consent={status: granted, grantedBy}", target.merge("assetID" => entry["assetID"]))
      when "appearance_change_unexplained"
        step("confirm_appearance_change", "#{name}：#{entry['message']}", "drama.set_reference_package with appearanceChange on that cast binding", target.merge("characterID" => entry["characterID"]))
      when "over_limit_required"
        step("trim_reference_package", "#{name}：#{entry['message']}", "drama.set_reference_package with fewer cast or excluded slots", target)
      when "audio_stale"
        step("regenerate_dialogue_audio", "#{name}：#{entry['message']}", "voice.generate episodeID shotID lineIDs", target.merge("lineID" => entry["lineID"]))
      when "audio_too_long"
        step("fit_dialogue_audio", "#{name}：#{entry['message']}", "shorten the line or lengthen the shot, then voice.generate", target.merge("lineID" => entry["lineID"]))
      end
    end.compact
  end

  # 返回 [不通过的, 有必改项的 warn, 没审过或已过期的]。不通过排在一切之前：在一份
  # 会被拦下的内容上继续出图出片是白花钱。待审排在生产步骤之前，理由相同——合规
  # 审核是设计里每个阶段完成后的固定一步，不是可选项。
  #
  # warn（0.35.0-rc1）：只有带 must 级问题、且没被「已知悉」的才排一步；只剩 advice
  # 的、或用户 / Agent 已经确认过这一版结论的，算做完——否则每次复审冒出几条新的
  # 小意见，这一步永远消不掉。
  def review_steps(drama_review, drama_details, characters, assets, episodes)
    blocked = []
    warned = []
    pending = []
    collect = lambda do |states, details, name, target, fix, ready = ->(_aspect) { true }|
      states.each do |aspect, state|
        next if %w[none stale].include?(state) && !ready.call(aspect)

        review_target = target.merge("aspect" => aspect)
        if state == "block"
          blocked << step("resolve_review_block", "#{name}（#{ASPECT_NAMES.fetch(aspect)}）审核结论为「不通过」。", fix, review_target)
        elsif state == "warn"
          detail = details[aspect]
          next unless detail && detail["must"].positive? && !detail["acknowledged"]

          ack_tool = target["scope"] == "video" ? "video.acknowledge_review" : "drama.acknowledge_review"
          warned << step("resolve_review_warn", "#{name}（#{ASPECT_NAMES.fetch(aspect)}）审核有 #{detail['must']} 处必改问题。",
                         "#{fix}; or, once the user accepts the risk, #{ack_tool} acknowledgedBy=user", review_target)
        elsif %w[none stale].include?(state)
          reason = state == "none" ? "#{name}（#{ASPECT_NAMES.fetch(aspect)}）还没审核。" : "#{name}（#{ASPECT_NAMES.fetch(aspect)}）改过了，审核结论已过期。"
          pending << step("run_review", reason, aspect == "panel" ? PANEL_TOOL_HINT : REVIEW_TOOL_HINT, review_target)
        end
      end
    end

    collect.call(drama_review, drama_details, "策划", { "scope" => "drama" }, "revise with drama.save_draft scope=drama commit=true, then review.run")
    characters.each do |character|
      collect.call(character["review"], character["reviewDetail"] || {}, "「#{character['name']}」", { "scope" => "character", "characterID" => character["id"] },
                   "revise the character or regenerate / reselect its image, then review.run",
                   ->(aspect) { aspect == "content" ? character["designed"] : character["candidates"].positive? })
    end
    assets.reject { |asset| asset["archived"] }.each do |asset|
      collect.call(asset["review"], asset["reviewDetail"] || {}, "#{KIND_NAMES.fetch(asset['kind'], asset['kind'])}「#{asset['name']}」", { "scope" => "asset", "assetID" => asset["id"] },
                   "revise the asset or regenerate / reselect its image, then review.run",
                   ->(aspect) { aspect == "content" ? asset["designed"] : asset["candidates"].positive? })
    end
    episodes.each do |episode|
      collect.call(episode["review"], episode["reviewDetail"] || {}, "第 #{episode['order']} 集", { "scope" => "episode", "episodeID" => episode["id"] },
                   "revise the episode or its shots, then review.run",
                   ->(aspect) { aspect == "storyboard" ? !episode["shots"].empty? : episode["hasScript"] })
      episode["shots"].each do |shot|
        name = "第 #{episode['order']} 集第 #{shot['order']} 镜"
        collect.call(shot["review"], shot["reviewDetail"] || {}, name, { "scope" => "shot", "episodeID" => episode["id"], "shotID" => shot["id"] },
                     "rewrite the frame prompt and regenerate or reselect the frame, then review.run",
                     ->(_aspect) { (shot["startCandidates"] + shot["endCandidates"]).positive? })
        video_state = shot.dig("video", "review")
        next unless video_state

        video_details = shot["video"]["reviewDetail"] ? { "content" => shot["video"]["reviewDetail"] } : {}
        collect.call({ "content" => video_state }, video_details, "#{name}的成片", { "scope" => "video", "jobID" => shot["video"]["jobID"] },
                     "regenerate the clip with a revised prompt, then review.run")
      end
    end
    [blocked, warned, pending]
  end

  def summary(characters, assets, episodes)
    shots = episodes.flat_map { |episode| episode["shots"] }
    live = assets.reject { |asset| asset["archived"] }
    {
      "characters" => characters.length,
      "charactersDesigned" => characters.count { |character| character["designed"] },
      "charactersLocked" => characters.count { |character| character["identityLocked"] },
      "assets" => live.length,
      "assetsReady" => live.count { |asset| asset["kind"] == "voice" ? asset["consentGranted"] : asset["selected"] },
      "episodesWithScript" => episodes.count { |episode| episode["hasScript"] },
      "episodesStoryboarded" => episodes.count { |episode| !episode["shots"].empty? },
      "shots" => shots.length,
      "shotsWithPackage" => shots.count { |shot| shot["hasPackage"] },
      "shotsWithStartFrame" => shots.count { |shot| shot["startSelected"] },
      "shotsWithVideo" => shots.count { |shot| shot["video"] && shot["video"]["state"] == "completed" },
      "continuityIssues" => shots.sum { |shot| Array(shot["continuity"]).length },
      "staleSelections" => shots.count { |shot| shot["staleSelection"] }
    }
  end

  def step(action, reason, how, target)
    { "action" => action, "reason" => reason, "how" => how, "target" => target }
  end

  def issue(code, message, extra = {})
    { "code" => code, "message" => message }.merge(extra)
  end

  def draft?(node)
    node["draft"].is_a?(Hash) && !node["draft"].empty?
  end

  def filled?(value)
    !value.to_s.strip.empty?
  end
end
