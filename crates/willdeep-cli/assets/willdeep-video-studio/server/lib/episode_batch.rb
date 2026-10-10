# frozen_string_literal: true

require_relative "image_generation"
require_relative "voice_generation"
require_relative "review_material"
require_relative "reference_package"
require_relative "shot_pipelines"
require_relative "video_selection"

# 按集批量（0.36.0-rc1，复盘 4.1）：一集的首 / 尾帧、台词配音、视频、按范围批量审核。
#
# 每种批量分两步：
# - plan_*：在提交时同步跑，只读存档，列出逐项（镜头或审核对象）以及哪些会被跳过、为什么。
#   参数不对、剧或集找不到、后端不可用都在这一步当场报错，不建任务。
# - run_*：在后台工作线程里跑。开头按当前存档**重新**列一遍（提交到开工之间、或进程
#   重启后重跑时，已经做完的项会被跳过），再逐项调用与单个工具完全相同的服务方法
#   （ImageGeneration#generate、VoiceGeneration#generate、VideoService#generate、
#   ReviewRunner#run），花钱与落库只有那一条路径。
#
# 幂等：每项带派生的 requestID（批量的 requestID，没有时用任务 ID，加上项的键），
# 同一项重做时由单个工具自己的 requestID 去重，不会重复花钱。
class EpisodeBatch
  FRAME_TARGETS = %w[start end].freeze
  REVIEW_SCOPES = %w[drama characters assets episodes shots].freeze
  FRESH_REVIEW_STATES = %w[pass warn block].freeze
  ASPECT_NAMES = { "content" => "内容", "images" => "画面", "storyboard" => "分镜", "panel" => "专家席" }.freeze
  REVIEW_ASPECTS = ASPECT_NAMES.keys.freeze
  # 碰到这些错误码，剩下的项不再请求：后端没了，或额度用完。
  STOP_CODES = [ImageGeneration::QUOTA_ERROR_CODE, "host_image_unsupported", "tts_unavailable", "host_review_unsupported", "configuration_error"].freeze
  VOICE_CHUNK = VoiceGeneration::MAX_LINES_PER_CALL
  DEFAULT_POLL_SECONDS = 15
  # 视频轮询的总时长上限：超过后停止轮询（远端任务照常跑，video.refresh / 生成队列可继续）。
  DEFAULT_POLL_LIMIT_SECONDS = 3 * 60 * 60
  ACTIVE_VIDEO_STATES = %w[submitting queued in_progress].freeze
  REQUEST_BASE_LIMIT = 120
  SUMMARY_LIMIT = 300

  # remediator：VideoRemediation，episode.generate_videos 每镜成片出来后的自动质检与补救用；不给就不做。
  # limits：进程级并发名额（ShotPipelines.default_limits），与 remediator 共用同一份。
  # image_qa：ImageQA（0.40.0-rc1），episode.generate_frames 每镜出图后的候选图质检、重抽与自动选定用；不给就只出图。
  def initialize(dramas:, images:, voices:, reviews:, videos:, progress:, poll_seconds: DEFAULT_POLL_SECONDS,
                 poll_limit_seconds: DEFAULT_POLL_LIMIT_SECONDS, remediator: nil, limits: nil, image_qa: nil)
    @remediator = remediator
    @image_qa = image_qa
    @limits = limits || ShotPipelines.default_limits(-> { {} })
    @dramas = dramas
    @images = images
    @voices = voices
    @reviews = reviews
    @videos = videos
    @progress = progress
    @poll_seconds = poll_seconds.to_f.positive? ? poll_seconds.to_f : DEFAULT_POLL_SECONDS
    @poll_limit_seconds = poll_limit_seconds.to_f
  end

  # ---- 首 / 尾帧 ----

  def plan_frames(arguments)
    target = (arguments["target"] || "start").to_s
    return failure("invalid_target", "target must be start or end.") unless FRAME_TARGETS.include?(target)

    loaded, episode = episode_of(arguments)
    return loaded unless episode

    shots, unknown = select_shots(episode, arguments["shotOrders"])
    only_missing = only_missing?(arguments)
    key = target == "start" ? "startCandidates" : "endCandidates"
    prompt_key = target == "start" ? "startPrompt" : "endPrompt"
    items = shots.map do |shot|
      item = shot_item(shot)
      reason = if draft?(shot) then "shot_has_draft"
               elsif shot[prompt_key].to_s.strip.empty? then "prompt_missing"
               elsif only_missing && !Array(shot[key]).empty? then "already_has_candidates"
               end
      reason ? item.merge("state" => "skipped", "reason" => reason, "candidates" => Array(shot[key]).length) : item
    end
    planned(loaded["drama"], episode, items, unknown).merge("target" => target)
  end

  # 每镜一条流水线（0.40.0-rc1，lib/image_qa.rb）：出图 → 候选图质检 → 全被拦截时带补救句重抽 → 选定推荐图。
  # 出图仍一镜一张地排队（本批共用一把锁，与以前逐镜串行相同的节奏与额度停止规则）；质检占进程级质检名额，
  # 一镜在质检时下一镜已经在出图。质检没开（imageAutoQA 关、autoQA: false 或宿主不能问模型）时只出图，与以前相同。
  def run_frames(arguments, context)
    plan = plan_frames(arguments)
    return plan unless plan["ok"]

    context.items = plan["items"]
    base = request_base(arguments, context)
    target = plan["target"]
    qa = image_qa_options(arguments)
    since = VideoSelection.batch_since(context)
    runtime = ShotPipelines.new(videos: @videos, limits: @limits, context: context, poll_seconds: @poll_seconds, poll_limit_seconds: @poll_limit_seconds)
    lock = Mutex.new
    entries = plan["items"].each_with_index.select { |item, _index| item["state"] == "pending" }
    entries.each { |_item, index| context.update_item(index, "stage" => "queued") }
    crash = lambda do |entry, error|
      context.update_item(entry[1], "state" => "failed", "stage" => "failed",
                                    "error" => { "code" => "internal_error", "message" => "#{error.class}: #{error.message}"[0, SUMMARY_LIMIT] })
    end
    finished = runtime.run(entries, on_crash: crash) do |item, index|
      frame_pipeline(arguments, target, base, item, ->(changes) { context.update_item(index, changes) }, runtime, lock, qa, since)
    end
    return summary(context, "canceled" => true) if finished == :canceled

    extra = { "target" => target, "generated" => context.items.sum { |item| item["generated"].to_i } }
    extra["imageQA"] = frames_qa_summary(context.items, qa) if qa
    summary(context, extra)
  end

  # 一镜：出图（拿本批的出图锁）→ 开着质检时交给 ImageQA#run_frame_shot（质检、重抽、选定）。
  def frame_pipeline(arguments, target, base, item, update, runtime, lock, qa, since)
    halted = runtime.halt_error
    return update.call(halted_changes(halted)) if halted

    request = frame_request(arguments, target, item["shotID"], "#{base}:#{target}:#{item['shotID']}")
    result = lock.synchronize do
      next :halted if runtime.halted?

      update.call("state" => "running", "stage" => "generating")
      @images.generate(request)
    end
    return update.call(halted_changes(runtime.halt_error)) if result == :halted
    unless result["ok"]
      update.call("state" => "failed", "stage" => "failed", "error" => compact_error(result["error"]))
      code = result.dig("error", "code")
      runtime.stop!(code) if STOP_CODES.include?(code)
      return
    end
    own_ids = Array(result["generated"]).map { |entry| entry["candidateID"] }
    changes = { "generated" => own_ids.length, "candidateIDs" => own_ids, "failedImages" => Array(result["failed"]).length,
                "aspectMismatchCount" => result["aspectMismatchCount"].to_i, "deduplicated" => result["deduplicated"] == true }
    return update.call(changes.merge("state" => "completed", "stage" => "done")) unless qa

    update.call(changes.merge("stage" => "qa"))
    retry_generate = lambda do |sentences, attempt|
      retry_request = frame_request(arguments, target, item["shotID"], "#{base}:#{target}:#{item['shotID']}:retry#{attempt}")
                      .merge("extraDirectives" => sentences)
      lock.synchronize do
        next :halted if runtime.halted?

        generated = @images.generate(retry_request)
        runtime.stop!(generated.dig("error", "code")) if !generated["ok"] && STOP_CODES.include?(generated.dig("error", "code"))
        generated
      end
    end
    outcome = @image_qa.run_frame_shot(drama_id: arguments["dramaID"], episode_id: arguments["episodeID"], shot_id: item["shotID"], target: target,
                                       own_ids: own_ids, options: qa, runtime: runtime, since: since, generate: retry_generate, update: update)
    update.call(outcome)
  end

  # 本批的质检选项；质检没开或不可用时为 nil。
  def image_qa_options(arguments)
    return nil if @image_qa.nil?

    effective = @image_qa.options(arguments)
    effective["autoQA"] && @image_qa.available? ? effective : nil
  end

  def frames_qa_summary(items, qa)
    reasons = Hash.new(0)
    items.each { |item| reasons[item["selectionReason"]] += 1 if item["selectionReason"] }
    { "autoSelect" => qa["autoSelect"], "retryOnBlock" => qa["retryOnBlock"], "selectionReasons" => reasons,
      "needsHuman" => items.count { |item| item["stage"] == "needs_human" },
      "retries" => items.sum { |item| item.dig("imageQA", "retries").to_i } }
  end

  # ---- 台词配音 ----

  def plan_dub(arguments)
    loaded, episode = episode_of(arguments)
    return loaded unless episode

    shots, unknown = select_shots(episode, arguments["shotOrders"])
    force = arguments["force"] == true
    items = shots.map do |shot|
      item = shot_item(shot)
      next item.merge("state" => "skipped", "reason" => "shot_has_draft") if draft?(shot)

      spoken = Array(shot["dialogue"]).select { |line| line.is_a?(Hash) && !line["text"].to_s.strip.empty? }
      next item.merge("state" => "skipped", "reason" => "no_dialogue") if spoken.empty?

      lines = lines_to_dub(spoken, force)
      next item.merge("state" => "skipped", "reason" => "already_dubbed", "lines" => spoken.length) if lines.empty?

      item.merge("lineIDs" => lines, "lines" => spoken.length)
    end
    planned(loaded["drama"], episode, items, unknown)
  end

  def run_dub(arguments, context)
    plan = plan_dub(arguments)
    return plan unless plan["ok"]

    context.items = plan["items"]
    base = request_base(arguments, context)
    stopped = nil
    plan["items"].each_with_index do |item, index|
      next unless item["state"] == "pending"
      return summary(context, "canceled" => true) if context.canceled?
      next context.update_item(index, "state" => "skipped", "reason" => "stopped_#{stopped}") if stopped

      context.update_item(index, "state" => "running")
      outcome = dub_shot(arguments, item, base)
      context.update_item(index, outcome)
      code = outcome.dig("error", "code")
      stopped = code if outcome["state"] == "failed" && STOP_CODES.include?(code)
    end
    summary(context, "generatedLines" => context.items.sum { |item| item["generatedLines"].to_i })
  end

  # ---- 视频 ----

  def plan_videos(arguments)
    mode = arguments["mode"].to_s
    return failure("invalid_mode", "mode must be one of #{ReferencePackage::MODES.join(', ')}.") unless mode.empty? || ReferencePackage::MODES.include?(mode)

    loaded, episode = episode_of(arguments)
    return loaded unless episode

    shots, unknown = select_shots(episode, arguments["shotOrders"])
    only_missing = only_missing?(arguments)
    jobs = Array(@videos.jobs_with_media)
    auto = auto_remediation?
    items = shots.map do |shot|
      item = shot_item(shot).merge(VideoSelection.plan_fields(shot))
      next item.merge("state" => "skipped", "reason" => "shot_has_draft") if draft?(shot)

      latest = latest_video(jobs, shot)
      if only_missing && latest
        if latest["state"] == "completed"
          # 上一次批量被打断时这一镜的自动质检 / 补救没做完：不重新生成，只接着做质检与补救。
          next item.merge("jobID" => latest["id"], "remediateOnly" => true) if auto && @remediator.unfinished?(shot, jobs)

          next item.merge("state" => "skipped", "reason" => "already_has_video", "jobID" => latest["id"])
        end
        # 还在生成的那条不重复提交，只接着轮询到完成。
        next item.merge("jobID" => latest["id"], "existing" => true) if ACTIVE_VIDEO_STATES.include?(latest["state"])
      end
      needs_frame = mode.empty? || %w[auto fl2va].include?(mode)
      next item.merge("state" => "skipped", "reason" => "no_start_frame") if needs_frame && !start_selected?(shot)

      item
    end
    planned(loaded["drama"], episode, items, unknown).merge("mode" => mode.empty? ? nil : mode)
  end

  # 每镜一条流水线并发跑（lib/shot_pipelines.rb，0.38.0-rc2）：拿视频名额提交 → 本批统一轮询到完成 →
  # 下载、播放镜像 → 成片自动质检与自动补救都开着时（video.settings autoQA / autoRemediate），这一镜的成片
  # 一出来就接着质检、重拍、选片（lib/video_remediation.rb），不等别的镜。补救结果记在该项的 remediation 上；
  # 补救失败不改变「视频已生成」这一项的状态，stage 写明要不要人看。
  def run_videos(arguments, context)
    plan = plan_videos(arguments)
    return plan unless plan["ok"]

    context.items = plan["items"]
    base = request_base(arguments, context)
    effective = auto_remediation? ? @remediator.options({}) : nil
    since = VideoSelection.batch_since(context)
    runtime = ShotPipelines.new(videos: @videos, limits: @limits, context: context, poll_seconds: @poll_seconds, poll_limit_seconds: @poll_limit_seconds)
    entries = plan["items"].each_with_index.select { |item, _index| item["state"] == "pending" }
    entries.each do |item, index|
      context.update_item(index, item["existing"] ? { "state" => "waiting", "stage" => "generating" } : { "stage" => "queued" })
    end
    crash = lambda do |entry, error|
      context.update_item(entry[1], "state" => "failed", "stage" => "failed",
                                    "error" => { "code" => "internal_error", "message" => "#{error.class}: #{error.message}"[0, SUMMARY_LIMIT] })
    end
    finished = runtime.run(entries, on_crash: crash) do |item, index|
      video_pipeline(arguments, plan["mode"], base, item, ->(changes) { context.update_item(index, changes) }, runtime, effective, since)
    end
    VideoSelection.annotate(context, @dramas, arguments["dramaID"], arguments["episodeID"], @videos.jobs_with_media)
    return summary(context, "canceled" => true) if finished == :canceled

    extra = { "completedVideos" => context.items.count { |item| item["state"] == "completed" } }
    extra["autoRemediation"] = remediation_summary(context.items, effective) if effective
    summary(context, extra)
  end

  # 一镜：生成（或接着等已在生成的那条）→ 下载 → 自动质检与补救 → 选片。update 改这一项。
  #
  # 选片（0.38.0-rc3，lib/video_selection.rb）：新成片就绪后设为这一镜的选定，不论 force 与否、这一镜原来选没选。
  # 开着自动质检与补救时由补救在本次产出的几条里选最好的（质检没给出结论时不选没审过的片，留 nextStep）；
  # 没开时下载好就选。不换的情况（逐项 selectionReason）：批量开始之后有人另选了别的成片（newer_selection）、
  # 当前选定是人工放行的而新片不是 pass（user_qa_override）、新片画面质检比当前选定差（worse_qa）。
  def video_pipeline(arguments, mode, base, item, update, runtime, effective, since = nil)
    job = nil
    unless item["remediateOnly"]
      job, error = generate_one(arguments, mode, base, item, update, runtime)
      if error
        return update.call(halted_changes(error)) if ShotPipelines.halt?(error)

        update.call("state" => "failed", "stage" => "failed", "jobID" => error["jobID"] || item["jobID"], "error" => error)
        runtime.stop!(error["code"]) if error["stage"] == "submit" && STOP_CODES.include?(error["code"])
        return
      end
      job, error, warning = runtime.materialize(job)
      if error
        return update.call("state" => "failed", "stage" => "failed", "remoteState" => "completed", "jobID" => error["jobID"], "error" => error)
      end

      changes = { "state" => effective ? "running" : "completed", "stage" => effective ? "qa" : "done", "remoteState" => "completed",
                  "jobID" => job["id"], "outputPath" => job["outputPath"] }
      changes["warning"] = warning if warning
      changes.merge!(adopt_take(arguments, item, job, since)) unless effective
      update.call(changes)
    end
    return unless effective

    update.call("state" => "running", "stage" => "qa", "remediation" => { "state" => "running" }) if item["remediateOnly"]
    progress = ->(changes) { update.call(changes.slice("stage", "attempt").merge("remediation" => { "state" => "running" }.merge(changes))) }
    outcome = @remediator.remediate_shot(arguments["dramaID"], arguments["episodeID"], item["shotID"], effective, runtime,
                                         on_progress: progress, job_id: job && job["id"], since: since)
    update.call({ "state" => "completed", "stage" => outcome["stage"], "remediation" => outcome }
                  .merge(outcome.slice("selectionReason", "selectionKept", "selectionCompared", "replacedStaleDialogue")))
    runtime.stop!(outcome.dig("error", "code")) if outcome["stopped"]
  end

  # 提交（拿视频名额）或接着等已在生成的那条，返回 [远端结束时的任务, 错误]。
  def generate_one(arguments, mode, base, item, update, runtime)
    remote = ->(job) { update.call("remoteState" => job["state"], "remoteProgress" => job["progress"].to_i) }
    return runtime.await_video({ "id" => item["jobID"], "state" => "in_progress" }, on_update: remote) if item["existing"]

    request = { "dramaID" => arguments["dramaID"], "episodeID" => arguments["episodeID"], "shotID" => item["shotID"],
                "mode" => mode, "requestID" => "#{base}:video:#{item['shotID']}" }.reject { |_key, value| value.nil? }
    submitted = lambda do |job, _deduplicated|
      update.call("state" => "waiting", "stage" => "generating", "jobID" => job["id"], "remoteState" => job["state"])
    end
    runtime.submit_video(request, on_slot: -> { update.call("state" => "running", "stage" => "generating") }, on_submitted: submitted, on_update: remote)
  end

  # 质检没开时：新成片下载好就选上（批量开始后有人另选了就不动）。
  # 新片多半还没有结论（没审），只有当前选定也没有结论时才换（不比当前选定差，见 lib/video_selection.rb）。
  def adopt_take(arguments, item, job, since)
    basis = ->(entry) { @reviews.frames_basis(entry) }
    jobs = @videos.jobs_with_media
    trim = VideoSelection.shot_trim(@dramas, arguments["dramaID"], arguments["episodeID"], item["shotID"])
    statuses = VideoSelection.statuses_for(jobs, item["shotID"], basis, trim)
    adopted = VideoSelection.adopt(@dramas, drama_id: arguments["dramaID"], episode_id: arguments["episodeID"], shot_id: item["shotID"],
                                            job_id: job["id"], by: "batch", status: statuses[job["id"]], statuses: statuses,
                                            since: since, own_ids: [job["id"]],
                                            stale_ids: VideoSelection.stale_ids_for(@dramas, jobs, arguments["dramaID"], arguments["episodeID"], item["shotID"]))
    changes = { "selectionReason" => adopted["reason"] }
    changes["replacedStaleDialogue"] = adopted["replacedStaleDialogue"] if adopted["replacedStaleDialogue"]
    changes["selectionKept"] = adopted["kept"] if adopted["kept"]
    changes["selectionCompared"] = adopted["compared"] if adopted["kept"] && adopted["compared"]
    changes["selectError"] = compact_error(adopted["error"]) if adopted["error"]
    changes
  end

  def halted_changes(error)
    return { "state" => "canceled", "reason" => "canceled", "stage" => nil } if error["code"] == "canceled"

    { "state" => "skipped", "reason" => "stopped_#{error['reason']}", "stage" => nil }
  end

  def auto_remediation?
    !@remediator.nil? && @remediator.auto_enabled?
  end

  def remediation_summary(items, effective)
    outcomes = Hash.new(0)
    items.each do |item|
      outcome = item["remediation"]
      next unless outcome.is_a?(Hash) && outcome["state"] != "running"

      outcomes[outcome["outcome"] || outcome["reason"] || outcome["state"]] += 1
    end
    { "categories" => effective["categories"], "maxRetries" => effective["maxRetries"], "outcomes" => outcomes }
  end

  # ---- 批量审核 ----

  def plan_reviews(arguments)
    scopes = review_scopes(arguments["scope"])
    return scopes if scopes.is_a?(Hash)
    # aspects（0.42.0-rc1）：只审列出的方面，例如只补专家席 ["panel"]；不给就是进度列出的全部。
    aspects = review_aspects(arguments["aspects"])
    return aspects if aspects.is_a?(Hash)

    loaded = @dramas.get("id" => arguments["dramaID"])
    return loaded unless loaded["ok"]

    episode_filter = arguments["episodeID"].to_s
    if !episode_filter.empty? && loaded["drama"]["episodes"].none? { |entry| entry["id"] == episode_filter }
      return failure("episode_not_found", "Episode was not found.")
    end
    progress = @progress.call.get_progress("dramaID" => arguments["dramaID"], "maxSteps" => 1)
    return progress unless progress["ok"]

    skip_fresh = arguments["skipFresh"] != false && arguments["force"] != true
    items = []
    # ready：与 drama.get_progress 排「待审」时同一口径——没定稿的文字、没出过图的画面不审。
    add = lambda do |states, scope, ids, name, ready = ->(_aspect) { true }|
      (states || {}).each do |aspect, state|
        next unless ready.call(aspect)
        next if aspects && !aspects.include?(aspect)

        item ={ "key" => ([scope] + ids.values + [aspect]).join(":"), "scope" => scope, "aspect" => aspect, "label" => "#{name}（#{ASPECT_NAMES.fetch(aspect, aspect)}）",
                 "state" => "pending", "reviewState" => state }.merge(ids)
        item = item.merge("state" => "skipped", "reason" => "fresh") if skip_fresh && FRESH_REVIEW_STATES.include?(state)
        items << item
      end
    end
    add.call(progress["drama"]["review"], "drama", {}, "策划") if scopes.include?("drama")
    designed_or_drawn = ->(entry) { ->(aspect) { aspect == "content" ? entry["designed"] : entry["candidates"].positive? } }
    if scopes.include?("characters")
      progress["characters"].each { |entry| add.call(entry["review"], "character", { "characterID" => entry["id"] }, "「#{entry['name']}」", designed_or_drawn.call(entry)) }
    end
    if scopes.include?("assets")
      progress["assets"].reject { |entry| entry["archived"] }.each do |entry|
        add.call(entry["review"], "asset", { "assetID" => entry["id"] }, "「#{entry['name']}」", designed_or_drawn.call(entry))
      end
    end
    progress["episodes"].each do |episode|
      next unless episode_filter.empty? || episode["id"] == episode_filter

      if scopes.include?("episodes")
        add.call(episode["review"], "episode", { "episodeID" => episode["id"] }, "第 #{episode['order']} 集",
                 ->(aspect) { aspect == "storyboard" ? !episode["shots"].empty? : episode["hasScript"] })
      end
      next unless scopes.include?("shots")

      episode["shots"].each do |shot|
        add.call(shot["review"], "shot", { "episodeID" => episode["id"], "shotID" => shot["id"] }, "第 #{episode['order']} 集第 #{shot['order']} 镜",
                 ->(_aspect) { (shot["startCandidates"] + shot["endCandidates"]).positive? })
      end
    end
    { "ok" => true, "drama" => loaded["drama"], "items" => items, "scopes" => scopes, "unknownOrders" => [] }
  end

  def run_reviews(arguments, context)
    plan = plan_reviews(arguments)
    return plan unless plan["ok"]

    context.items = plan["items"]
    stopped = nil
    plan["items"].each_with_index do |item, index|
      next unless item["state"] == "pending"
      return summary(context, "canceled" => true) if context.canceled?
      next context.update_item(index, "state" => "skipped", "reason" => "stopped_#{stopped}") if stopped

      context.update_item(index, "state" => "running")
      request = { "dramaID" => arguments["dramaID"], "scope" => item["scope"], "aspect" => item["aspect"] }
      %w[characterID assetID episodeID shotID].each { |key| request[key] = item[key] if item[key] }
      result = @reviews.run(request)
      if result["ok"]
        context.update_item(index, "state" => "completed", "status" => result["status"], "mustFix" => result["mustFix"].to_i,
                                   "summary" => result["summary"].to_s[0, SUMMARY_LIMIT])
      else
        context.update_item(index, "state" => "failed", "error" => compact_error(result["error"]))
        code = result.dig("error", "code")
        stopped = code if STOP_CODES.include?(code)
      end
    end
    verdicts = Hash.new(0)
    context.items.each { |item| verdicts[item["status"]] += 1 if item["state"] == "completed" }
    summary(context, "verdicts" => verdicts)
  end

  def failure(code, message)
    { "ok" => false, "error" => { "code" => code, "message" => message } }
  end

  # 单镜出图的参数，与 image.generate 的入参同形；没给的键不带（让它用自己的默认值）。
  def frame_request(arguments, target, shot_id, request_id)
    { "dramaID" => arguments["dramaID"], "target" => target, "episodeID" => arguments["episodeID"], "shotID" => shot_id,
      "models" => arguments["models"], "countPerModel" => arguments["countPerModel"], "castIDs" => arguments["castIDs"],
      "requestID" => request_id }.reject { |_key, value| value.nil? }
  end

  private

  def episode_of(arguments)
    loaded = @dramas.get("id" => arguments["dramaID"])
    return [loaded, nil] unless loaded["ok"]

    episode = loaded["drama"]["episodes"].find { |entry| entry["id"] == arguments["episodeID"].to_s }
    return [failure("episode_not_found", "Episode was not found."), nil] unless episode

    [loaded, episode]
  end

  def select_shots(episode, orders)
    shots = Array(episode["shots"]).sort_by { |shot| shot["order"].to_i }
    return [shots, []] unless orders.is_a?(Array) && !orders.empty?

    wanted = orders.map(&:to_i).uniq
    [shots.select { |shot| wanted.include?(shot["order"].to_i) }, wanted - shots.map { |shot| shot["order"].to_i }]
  end

  def planned(drama, episode, items, unknown)
    { "ok" => true, "drama" => drama, "episode" => episode, "items" => items, "unknownOrders" => unknown }
  end

  def shot_item(shot)
    { "key" => shot["id"], "shotID" => shot["id"], "order" => shot["order"], "label" => "第 #{shot['order']} 镜", "state" => "pending" }
  end

  def only_missing?(arguments)
    arguments["onlyMissing"] != false && arguments["force"] != true
  end

  def draft?(node)
    node["draft"].is_a?(Hash) && !node["draft"].empty?
  end

  def start_selected?(shot)
    Array(shot["startCandidates"]).any? { |entry| entry["id"] == shot["selectedStartID"] }
  end

  # 每项派生 requestID 的前缀：批量自己的 requestID，没有时用任务 ID。
  def request_base(arguments, context)
    token = arguments["requestID"].to_s.strip
    (token.empty? ? context.job_id : token)[0, REQUEST_BASE_LIMIT]
  end

  # 没有选定音频、或选定的音频是按旧台词配的，都要（重新）配；force 时全部重配。
  def lines_to_dub(lines, force)
    lines.select do |line|
      next true if force

      audio = line["audio"].is_a?(Hash) ? line["audio"] : {}
      selected = Array(audio["candidates"]).find { |entry| entry["id"] == audio["selectedCandidateID"] }
      next true unless selected

      fingerprint = selected["textFingerprint"].to_s
      !fingerprint.empty? && fingerprint != ReviewMaterial.fingerprint(line["text"].to_s.strip)
    end.map { |line| line["id"] }
  end

  def dub_shot(arguments, item, base)
    generated = []
    failed = []
    skipped = []
    error = nil
    item["lineIDs"].each_slice(VOICE_CHUNK).with_index do |chunk, part|
      result = @voices.generate({ "dramaID" => arguments["dramaID"], "episodeID" => arguments["episodeID"], "shotID" => item["shotID"],
                                  "lineIDs" => chunk, "presetID" => arguments["presetID"],
                                  "requestID" => "#{base}:dub:#{item['shotID']}:#{part}" }.reject { |_key, value| value.nil? })
      skipped.concat(Array(result["skipped"]))
      failed.concat(Array(result["failed"]))
      generated.concat(Array(result["generated"]))
      error ||= result["error"] unless result["ok"]
    end
    # 重配的台词要换成新音频：record_dialogue_audio 只在台词还没有选定音频时自动选。
    generated.each do |entry|
      next if entry["candidateID"].to_s.empty?

      @dramas.select_dialogue_audio("dramaID" => arguments["dramaID"], "episodeID" => arguments["episodeID"], "shotID" => item["shotID"],
                                    "lineID" => entry["lineID"], "candidateID" => entry["candidateID"])
    end
    outcome = { "generatedLines" => generated.length, "failedLines" => failed.map { |entry| compact_error(entry) },
                "skippedLines" => skipped.map { |entry| entry.slice("lineID", "reason", "speaker") } }
    return outcome.merge("state" => "completed") unless generated.empty? && !error.nil?
    return outcome.merge("state" => "skipped", "reason" => "no_usable_voice") if error["code"] == "nothing_to_generate"

    outcome.merge("state" => "failed", "error" => compact_error(error))
  end

  def latest_video(jobs, shot)
    jobs.select { |job| job["shotID"] == shot["id"] && job["deletedAt"].to_s.empty? }.max_by { |job| job["createdAt"].to_s }
  end

  def review_scopes(value)
    requested = case value
                when nil, "" then REVIEW_SCOPES
                when Array then value.map(&:to_s)
                else value.to_s.split(/[|,\s]+/)
                end
    requested = requested.map(&:strip).reject(&:empty?).uniq
    unknown = requested - REVIEW_SCOPES
    return failure("invalid_scope", "scope must be a list of #{REVIEW_SCOPES.join(', ')}.") unless unknown.empty? && !requested.empty?

    requested
  end

  # 不给（nil / 空）返回 nil，表示不过滤。
  def review_aspects(value)
    requested = case value
                when nil, "" then return nil
                when Array then value.map(&:to_s)
                else value.to_s.split(/[|,\s]+/)
                end
    requested = requested.map(&:strip).reject(&:empty?).uniq
    return nil if requested.empty?

    unknown = requested - REVIEW_ASPECTS
    return failure("invalid_aspect", "aspects must be a list of #{REVIEW_ASPECTS.join(', ')}.") unless unknown.empty?

    requested
  end

  def compact_error(error)
    return { "code" => "unknown", "message" => "Unknown error." } unless error.is_a?(Hash)

    compact = { "code" => error["code"], "message" => error["message"].to_s[0, SUMMARY_LIMIT] }
    compact["retryable"] = error["retryable"] unless error["retryable"].nil?
    compact["lineID"] = error["lineID"] if error["lineID"]
    compact
  end

  # 批量任务的结果：逐项状态计数。所有尝试过的项都失败时整个任务算失败。
  def summary(context, extra = {})
    items = context.items
    counts = Hash.new(0)
    items.each { |item| counts[item["state"]] += 1 }
    attempted = counts["completed"] + counts["failed"]
    result = { "ok" => true, "counts" => counts }.merge(extra)
    return result unless attempted.positive? && counts["completed"].zero? && extra["canceled"] != true

    first = items.find { |item| item["state"] == "failed" }
    result.merge("ok" => false, "error" => { "code" => "batch_failed",
                                             "message" => "Every attempted item failed. First error: #{first && first.dig('error', 'message')}" })
  end
end
