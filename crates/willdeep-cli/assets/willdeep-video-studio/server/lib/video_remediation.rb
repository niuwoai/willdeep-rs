# frozen_string_literal: true

require_relative "qa_remediation"
require_relative "qa_lessons"
require_relative "host_bridge"
require_relative "shot_pipelines"
require_relative "video_selection"
require_relative "clip_trim"
require_relative "episode_plan"
require_relative "media_ref"
require_relative "wav_tools"

# 成片画面质检的自动补救（0.38.0-rc1，docs/decisions/0004-qa-lessons.md）。
#
# 《回村养鸭》第 1 集第 7 镜：H3 生成的片段前半段女主蹲在鸭子旁，中途硬切成鸭子特写、女主远远站着。
# 画面质检判 scene_jump。以前靠插件外的脚本补一句提示词重做；现在插件自己做：
#
#   1. 取这一镜当前的成片（选定的，没选就取最新完成的），确认有一份按当前文件算的画面质检结论，
#      没有或过期就先审一次（review.run scope=video aspect=frames）。
#   2. 结论里有可自动重做的问题（scene_jump / identity / text_overlay …，按设置与参数过滤），就按
#      补救表与经验库为每类问题选一句正面的补救句，经 video.generate 的镜头路径重拍一条
#      （requestID 由源成片与第几次派生，中断后重跑不会重复花钱），等它完成、下载、做播放镜像，
#      再审一次。
#   3. 每次重拍按类别记一条经验（用的哪句、结果 resolved / unresolved），直到通过或用完次数。
#   4. 在原片与各次重拍里选最好的那条（pass 优先，其次 block 级问题少、问题总数少，同分取早的）
#      设为本镜选定成片。都没过时：设置允许自动放行才带 qaOverride，否则留一条给人的下一步。
#
# 花钱与落库只有单个工具那一条路径：提交走 VideoService#generate，审核走 ReviewRunner#run，
# 选片走 DramaService#select_video。
#
# 0.38.0-rc2 起一集里的镜头各走各的（lib/shot_pipelines.rb，docs/decisions/0005-parallel-shot-pipelines.md）：
# 每镜一条流水线并发跑，质检与提交按进程级名额排队，轮询由批量的协调线程统一做。被打断后重跑同一个
# 工具会接着上次的补救链做：重拍的 requestID 由源成片与第几次派生，已经提交过的直接取回，不重复花钱。
class VideoRemediation
  DEFAULT_POLL_SECONDS = 15
  DEFAULT_POLL_LIMIT_SECONDS = 3 * 60 * 60
  STATUS_RANK = { "pass" => 0, "warn" => 1, "block" => 2 }.freeze
  REQUEST_PREFIX = "remediate"
  # 画面质检失败时最多试几次（第一次 + 重试一次）。
  QA_ATTEMPTS = 2
  QA_FAILED_NEXT_STEP = "The frame review of clip %<job>s did not return a verdict (%<message>s), so its quality is unknown, not passed. Run review.run scope=video aspect=frames jobID=%<job>s " \
                        "(or review.get_material + video.record_review), then video.remediate again. %<selection>s"
  SUMMARY_LIMIT = 300

  # 一条补救链最多往上追几层（重拍的 remediation.sourceJobID 都指向链头，一层就够；多给几层防脏数据）。
  CHAIN_DEPTH = 5

  # limits：进程级并发名额（ShotPipelines.default_limits），与 episode.generate_videos 共用同一份。
  def initialize(dramas:, videos:, reviews:, lessons:, settings:, host:, poll_seconds: DEFAULT_POLL_SECONDS,
                 poll_limit_seconds: DEFAULT_POLL_LIMIT_SECONDS, limits: nil)
    @limits = limits || ShotPipelines.default_limits(settings)
    @dramas = dramas
    @videos = videos
    @reviews = reviews
    @lessons = lessons
    @settings = settings
    @host = host
    @poll_seconds = poll_seconds.to_f.positive? ? poll_seconds.to_f : DEFAULT_POLL_SECONDS
    @poll_limit_seconds = poll_limit_seconds.to_f.positive? ? poll_limit_seconds.to_f : DEFAULT_POLL_LIMIT_SECONDS
  end

  # 生效的参数：categories / maxRetries 不给时取设置。
  def options(arguments = {})
    settings = @settings.call
    categories = arguments.key?("categories") ? QARemediation.clean_categories(arguments["categories"], []) : Array(settings["remediateCategories"])
    retries = begin
      Integer(arguments.key?("maxRetries") ? arguments["maxRetries"] : settings["remediateMaxRetries"])
    rescue ArgumentError, TypeError
      QARemediation::DEFAULT_MAX_RETRIES
    end
    { "categories" => categories, "maxRetries" => retries.clamp(0, QARemediation::MAX_RETRIES_LIMIT),
      "autoOverride" => settings["remediateAutoOverride"] == true }
  end

  # episode.generate_videos 跑完要不要接着自动补救：成片自动质检与自动补救都开着、宿主能问模型。
  def auto_enabled?
    settings = @settings.call
    settings["autoQA"] != false && settings["autoRemediate"] != false && @host.supports?(HostBridge::AI_COMPLETE)
  end

  # 一个批量任务的流水线运行时：共用本对象的进程级名额与轮询节奏。
  def pipelines(context)
    ShotPipelines.new(videos: @videos, limits: @limits, context: context, poll_seconds: @poll_seconds, poll_limit_seconds: @poll_limit_seconds)
  end

  # episode.generate_videos 重跑时用：这一镜已有成片，但上一次批量的自动质检 / 补救没做完（进程重启打断）——
  # 当前成片是一条补救链里还没选定的重拍，或是批量生成的成片（requestID 以 `:video:<镜头>` 结尾）还没选定、
  # 结论缺失 / 过期 / 仍有可自动重做的问题。别处来的成片（页面、单个工具）不在这里自动补救。
  def unfinished?(shot, jobs)
    return false unless shot["selectedVideoID"].to_s.empty?

    clip = current_clip(shot, jobs)
    return false unless clip
    return false if shot.dig("qaOverride", "jobID") == clip["id"]
    return true unless chain_root(clip, shot, jobs).equal?(clip)
    return false unless clip["requestID"].to_s.end_with?(":video:#{shot['id']}")

    review = clip.dig("reviews", "frames")
    return true unless review.is_a?(Hash) && review["basis"] == @reviews.frames_basis(clip)

    !QARemediation.remediable_categories(ClipTrim.effective_for(shot, clip), options({})["categories"]).empty?
  end

  # 提交前的检查：宿主能审图，视频后端配好了。通过返回 nil。
  def preflight
    refused = @reviews.preflight("scope" => "video", "aspect" => "frames")
    return refused if refused

    status = @videos.status
    return nil if status["ok"] && status["apiConfigured"]

    failure("video_unconfigured", "The video provider is not configured (API key missing); set it in the plugin settings or VIDEO_STUDIO_API_KEY.")
  end

  # ---- 列项 ----

  # video.remediate：jobID，或 dramaID + episodeID + shotID。episode.remediate_videos：dramaID + episodeID
  # （shotOrders 可选）。返回 {ok, drama, episode, items}；每项是一镜。
  def plan(arguments)
    if !arguments["jobID"].to_s.empty? && arguments["shotID"].to_s.empty?
      job = @videos.jobs_with_media.find { |entry| entry["id"] == arguments["jobID"].to_s }
      return failure("not_found", "Video job was not found.") unless job
      return failure("job_not_linked", "This video job is not linked to a storyboard shot; remediation needs the shot to regenerate it.") if job["shotID"].to_s.empty?

      arguments = arguments.merge("dramaID" => job["dramaID"], "episodeID" => job["episodeID"], "shotID" => job["shotID"])
    end
    loaded = @dramas.get("id" => arguments["dramaID"])
    return loaded unless loaded["ok"]

    drama = loaded["drama"]
    episode = Array(drama["episodes"]).find { |entry| entry["id"] == arguments["episodeID"].to_s }
    return failure("episode_not_found", "Episode was not found.") unless episode

    shots = Array(episode["shots"]).sort_by { |shot| shot["order"].to_i }
    unknown = []
    if !arguments["shotID"].to_s.empty?
      shots = shots.select { |shot| shot["id"] == arguments["shotID"].to_s }
      return failure("shot_not_found", "Storyboard shot was not found.") if shots.empty?
    elsif arguments["shotOrders"].is_a?(Array) && !arguments["shotOrders"].empty?
      wanted = arguments["shotOrders"].map(&:to_i).uniq
      unknown = wanted - shots.map { |shot| shot["order"].to_i }
      shots = shots.select { |shot| wanted.include?(shot["order"].to_i) }
    end
    effective = options(arguments)
    jobs = @videos.jobs_with_media
    items = shots.map { |shot| plan_item(shot, jobs, effective, arguments["jobID"].to_s) }
    { "ok" => true, "drama" => drama, "episode" => episode, "items" => items, "unknownOrders" => unknown, "options" => effective }
  end

  # dryRun：不审、不提交，只说会怎么做。单镜时附上带补救句的提示词预览。
  def preview(arguments)
    planned = plan(arguments)
    return planned unless planned["ok"]

    effective = planned["options"]
    items = planned["items"].map do |item|
      next item unless item["state"] == "pending"

      categories = item["remediableCategories"] || []
      picks = categories.map { |category| @lessons.remediation_for(category) }.compact
      entry = item.merge("wouldAdd" => picks)
      # 自动裁剪能解决的，执行时先裁、不重拍（0.39.0-rc1）。
      shot = Array(planned["episode"]["shots"]).find { |candidate| candidate["id"] == item["shotID"] }
      clip = @videos.jobs_with_media.find { |job| job["id"] == item["jobID"] }
      if shot && clip && !categories.empty?
        trimmed, note = trim_take(shot, clip, ClipTrim.effective_for(shot, clip), effective)
        entry["wouldTrim"] = trimmed[:trim] if trimmed
        entry["trimSkipped"] = note if note
      end
      if planned["items"].length == 1 && !picks.empty?
        dry = @videos.generate(generate_arguments(planned["drama"]["id"], planned["episode"]["id"], item, picks, 1).merge("dryRun" => true))
        entry["promptIR"] = dry["promptIR"] if dry["ok"]
        entry["directives"] = dry["directives"] if dry["ok"]
      end
      entry
    end
    { "ok" => true, "dryRun" => true, "items" => items, "options" => effective, "unknownOrders" => planned["unknownOrders"],
      "note" => "Items whose verdict is missing or stale (needsReview) are reviewed first; they are remediated only if that review finds #{effective['categories'].join(', ')} issues." }
  end

  # ---- 后台执行 ----

  # 批量与单镜共用：每镜一条流水线并发跑（lib/shot_pipelines.rb）。context 是 JobRunner::Context。
  def run(arguments, context)
    planned = plan(arguments)
    return planned unless planned["ok"]

    context.items = planned["items"]
    effective = planned["options"]
    drama_id = planned["drama"]["id"]
    episode_id = planned["episode"]["id"]
    since = VideoSelection.batch_since(context)
    runtime = pipelines(context)
    entries = planned["items"].each_with_index.select { |item, _index| item["state"] == "pending" }
    entries.each { |_item, index| context.update_item(index, "stage" => "queued") }
    crash = ->(entry, error) { context.update_item(entry[1], crash_outcome(error)) }
    finished = runtime.run(entries, on_crash: crash) do |item, index|
      context.update_item(index, "state" => "running", "stage" => "qa")
      outcome = remediate_shot(drama_id, episode_id, item["shotID"], effective, runtime,
                               on_progress: ->(changes) { context.update_item(index, changes) }, job_id: item["jobID"],
                               resume: item["explicit"] != true, since: since, trim_only: item["trimOnly"] == true)
      context.update_item(index, outcome)
      runtime.stop!(outcome.dig("error", "code")) if outcome["stopped"]
    end
    VideoSelection.annotate(context, @dramas, drama_id, episode_id, @videos.jobs_with_media)
    return summary(context, "canceled" => true) if finished == :canceled

    summary(context)
  end

  # 一镜的完整循环，返回逐项结果（state 为 completed / skipped / failed / canceled，stage 为 done /
  # needs_human / failed，带 finishedAt）。episode.generate_videos 的自动补救也调它，必须在
  # runtime.run 的块里调（轮询由 runtime 的协调线程做）。job_id：从哪条成片开始（不给就取这一镜当前的成片）。
  # resume：这条成片是一条还没选定的补救链里的重拍时，从链头接着做（中断后重跑不另起一条链、不重复花钱）。
  # since：批量开始的时间，之后有人另选了别的成片就不覆盖（lib/video_selection.rb）。
  # 结果总带 selectedJobID（这一镜现在实际选定的，没有选定为 nil）与 selectionChanged（0.38.0-rc3）。
  # trim_only：人工放行过的成片（0.39.0-rc1）——只试裁剪，裁不了就不动、不重拍。
  def remediate_shot(drama_id, episode_id, shot_id, effective, runtime, on_progress: ->(_changes) {}, job_id: nil, resume: true, since: nil, trim_only: false)
    before = find_shot(drama_id, episode_id, shot_id)
    result = remediate_shot_steps(drama_id, episode_id, shot_id, effective, runtime, on_progress, job_id, resume, since, trim_only)
    after = find_shot(drama_id, episode_id, shot_id)
    result = result.merge(VideoSelection.item_fields(before && before["selectedVideoID"], after)) if after
    # stage 为 nil 也要写：逐项更新是合并，不写会留着上一步的 qa / retake。
    result.merge("stage" => final_stage(result), "finishedAt" => ShotPipelines.now_iso)
  end

  # ---- 内部 ----

  private

  def remediate_shot_steps(drama_id, episode_id, shot_id, effective, runtime, on_progress, job_id, resume, since, trim_only = false)
    halted = runtime.halt_error
    return halted_outcome(halted) if halted

    shot = find_shot(drama_id, episode_id, shot_id)
    return { "state" => "failed", "error" => { "code" => "shot_not_found", "message" => "Storyboard shot was not found." } } unless shot

    jobs = @videos.jobs_with_media
    source = job_id.to_s.empty? ? current_clip(shot, jobs) : usable(jobs.find { |entry| entry["id"] == job_id.to_s })
    return { "state" => "skipped", "reason" => "no_completed_video" } unless source

    source = chain_root(source, shot, jobs) if resume
    on_progress.call("stage" => "qa", "attempt" => 0)
    verdict, error = fresh_verdict(source, runtime)
    return halted_outcome(error).merge("jobID" => source["id"]) if ShotPipelines.halt?(error)
    if error
      return { "state" => "failed", "jobID" => source["id"], "outcome" => "qa_failed", "error" => error, "stopped" => stop?(error),
               "selectionReason" => "qa_failed", "nextStep" => qa_failed_step(source, error, "The shot's selection was left unchanged.") }
    end

    # 镜头上已有挂在这条成片上的裁剪：裁掉的 scene_jump 不算（0.39.0-rc1，lib/clip_trim.rb）。
    verdict = ClipTrim.effective_for(shot, source.merge("reviews" => { "frames" => verdict })) || verdict
    categories = QARemediation.remediable_categories(verdict, effective["categories"])
    advisory = QARemediation.advisory_categories(verdict)
    takes = [take(source, verdict, 0)]
    base = { "jobID" => source["id"], "initialStatus" => verdict["status"], "initialCategories" => issue_categories(verdict) }
    base["advisory"] = advisory unless advisory.empty?
    # 先试裁剪：切点之前那段够长、台词说得完，就裁到切点前，不重拍（省一次排队与计费）。
    unless categories.empty?
      trimmed, trim_note = trim_take(shot, source, verdict, effective)
      base["trimSkipped"] = trim_note if trim_note
      return trimmed_outcome(drama_id, episode_id, shot, source, verdict, trimmed, since, base) if trimmed
    end
    # 人工放行过的成片只试裁剪，裁不了就照旧不动（不为它重拍）。
    return base.merge("state" => "skipped", "reason" => "qa_override") if trim_only

    if categories.empty?
      # 没什么可补的也要真的选上这条（0.38.0-rc3）：以前只在结果里写 selectedJobID，镜头仍选着旧片。
      adopted = adopt(drama_id, episode_id, shot, source, [source["id"]], since, verdict["status"])
      return base.merge("state" => "completed", "outcome" => verdict["status"] == "pass" ? "passed" : "nothing_to_remediate")
                 .merge(adoption_fields(adopted, source))
    end

    limit = [effective["maxRetries"], categories.map { |category| QARemediation.max_retries(category) }.max.to_i].min
    tried = Hash.new { |hash, key| hash[key] = [] }
    current = verdict
    attempt = 0
    halted = nil
    while attempt < limit
      remaining = QARemediation.remediable_categories(current, effective["categories"]).select { |category| attempt < QARemediation.max_retries(category) }
      break if remaining.empty?
      break halted = runtime.halt_error if runtime.halted?

      attempt += 1
      picks = remaining.map { |category| @lessons.remediation_for(category, tried: tried[category]) }.compact
      on_progress.call("stage" => "retake", "attempt" => attempt, "remediating" => remaining)
      retake, error = regenerate(drama_id, episode_id, shot, source, picks, attempt, current, runtime)
      if error
        halted = error if ShotPipelines.halt?(error)
        base["error"] = error unless halted
        break
      end
      ready_at = ShotPipelines.now_iso
      on_progress.call("stage" => "qa", "attempt" => attempt)
      new_verdict, review_error = fresh_verdict(retake, runtime)
      # 停下（取消 / 别的镜碰到后端故障）时这条重拍没审：不拿它比，按已审过的选。
      break halted = review_error if ShotPipelines.halt?(review_error)
      if review_error
        # 重拍的质量不知道：不拿它比、不选它。把这一镜钉在已审过的最好那条上（多半就是原片），
        # 免得合成按「最新完成的」悄悄用上这条没审过的重拍；留下一步给人。
        best = takes.min_by { |entry| entry[:score] }
        pinned = adopt(drama_id, episode_id, shot, best[:job], takes.map { |entry| entry[:job]["id"] } | [retake["id"]], since, best[:review]["status"])
        return base.merge("state" => "failed", "outcome" => "qa_failed", "error" => review_error, "attempts" => attempt,
                          "unverifiedJobID" => retake["id"], "takes" => takes.map { |entry| take_summary(entry) },
                          "stopped" => stop?(review_error),
                          "nextStep" => qa_failed_step(retake, review_error, "The shot stays on reviewed clip #{best[:job]['id']} until the retake is reviewed."))
                   .merge(adoption_fields(pinned, best[:job]))
      end
      learn(picks, current, new_verdict, retake, shot, attempt, source)
      picks.each { |pick| tried[pick["category"]] << pick["lessonID"] unless resolved?(pick["category"], new_verdict) }
      # 重拍还是在中途切走：能裁就裁这条重拍，不再拍下一条。
      trimmed = nil
      unless QARemediation.remediable_categories(new_verdict, effective["categories"]).empty?
        trimmed, trim_note = trim_take(shot, retake, new_verdict, effective)
        base["trimSkipped"] = trim_note if trim_note
        if trimmed
          learn_trim(shot, retake, new_verdict, trimmed)
          new_verdict = trimmed[:review]
        end
      end
      takes << take(retake, new_verdict, attempt, ready_at).merge(trim: trimmed && trimmed[:trim])
      on_progress.call("stage" => "qa", "attempt" => attempt, "takes" => takes.map { |entry| take_summary(entry) })
      current = new_verdict
      break if QARemediation.remediable_categories(current, effective["categories"]).empty?
    end

    on_progress.call("stage" => "selecting")
    best = takes.min_by { |entry| entry[:score] }
    result = base.merge("attempts" => attempt, "takes" => takes.map { |entry| take_summary(entry) }, "selectedJobID" => best[:job]["id"],
                        "finalStatus" => best[:review]["status"])
    result["canceled"] = true if halted && halted["code"] == "canceled"
    result["stoppedBy"] = halted["reason"] if halted && halted["code"] == "stopped"
    result = result.merge(select_best(drama_id, episode_id, shot, best, effective, categories, takes.map { |entry| entry[:job]["id"] }, since))
    # 重拍没能提交或没跑完（额度、后端故障、轮询超时）：这一项算失败，选片仍按已审过的最好那条。
    result["state"] = "failed" if result["error"]
    result["stopped"] = true if stop?(result["error"])
    result
  end

  # 逐项的 stage：要人看的（没过且没放行、质检失败）记 needs_human。
  def final_stage(result)
    return "needs_human" unless result["nextStep"].to_s.empty?
    return "failed" if result["state"] == "failed"
    # 跳过 / 取消的项由 state 说明，不给 stage。
    return nil unless result["state"] == "completed"

    "done"
  end

  def halted_outcome(error)
    return { "state" => "canceled", "reason" => "canceled" } if error["code"] == "canceled"

    { "state" => "skipped", "reason" => "stopped_#{error['reason']}" }
  end

  def crash_outcome(error)
    { "state" => "failed", "stage" => "failed", "finishedAt" => ShotPipelines.now_iso,
      "error" => { "code" => "internal_error", "message" => "#{error.class}: #{error.message}"[0, SUMMARY_LIMIT] } }
  end

  # 补救链的链头：重拍都记着 remediation.sourceJobID（链头）。这一镜还没选定成片时（上次补救被打断，
  # 或补救前没选过），从链头接着做；已经选定了就以选定的为准（人或上次补救的结论），另起一条链。
  def chain_root(job, shot, jobs)
    return job unless shot["selectedVideoID"].to_s.empty?

    current = job
    CHAIN_DEPTH.times do
      parent_id = current.dig("remediation", "sourceJobID").to_s
      break if parent_id.empty?

      parent = usable(jobs.find { |entry| entry["id"] == parent_id && entry["shotID"] == shot["id"] })
      break unless parent

      current = parent
    end
    current
  end

  def usable(job)
    job && job["state"] == "completed" && File.file?(job["outputPath"].to_s) ? job : nil
  end

  def plan_item(shot, jobs, effective, job_id)
    item = { "key" => shot["id"], "shotID" => shot["id"], "order" => shot["order"], "label" => "第 #{shot['order']} 镜", "state" => "pending" }
             .merge(VideoSelection.plan_fields(shot))
    explicit = !job_id.empty? && shot_has_job?(jobs, shot, job_id)
    clip = explicit ? jobs.find { |job| job["id"] == job_id } : current_clip(shot, jobs)
    return item.merge("state" => "skipped", "reason" => "no_completed_video") unless clip && clip["state"] == "completed" && File.file?(clip["outputPath"].to_s)

    item["jobID"] = clip["id"]
    item["mode"] = clip["generationMode"] if clip["generationMode"]
    # 点名的那条成片就从它做起；否则上次被打断的补救链（重拍还没选定）从链头接着做。
    return item.merge("explicit" => true).merge(verdict_fields(clip, effective, shot)) if explicit
    if shot.dig("qaOverride", "jobID") == clip["id"]
      # 人工放行过的成片不重拍；但质检里的 scene_jump 能靠裁剪解决时先试裁剪（0.39.0-rc1）。
      return item.merge(verdict_fields(clip, effective, shot)).merge("trimOnly" => true) if trim_candidate?(clip, effective, shot)

      return item.merge("state" => "skipped", "reason" => "qa_override")
    end

    root = chain_root(clip, shot, jobs)
    return item.merge("jobID" => root["id"], "resume" => true, "resumeFrom" => clip["id"]) unless root.equal?(clip)

    item.merge(verdict_fields(clip, effective, shot))
  end

  # 放行过的成片值不值得试裁剪：开着自动裁剪、本次要补 scene_jump、结论是新的且（裁剪生效后）仍有 scene_jump。
  def trim_candidate?(clip, effective, shot)
    return false unless @settings.call["autoTrim"] != false
    return false if (effective["categories"] & ClipTrim::CUT_CATEGORIES).empty?

    review = clip.dig("reviews", "frames")
    return false unless review.is_a?(Hash) && review["basis"] == @reviews.frames_basis(clip)

    !(QARemediation.remediable_categories(ClipTrim.effective_for(shot, clip), effective["categories"]) & ClipTrim::CUT_CATEGORIES).empty?
  end

  # 计划里一镜的结论字段：缺失 / 过期要先审（pending），通过或没有可重做的问题就跳过。
  # 镜头上挂在这条成片上的裁剪生效（裁掉的 scene_jump 不算）。
  def verdict_fields(clip, effective, shot = nil)
    item = {}

    review = clip.dig("reviews", "frames")
    fresh = review.is_a?(Hash) && review["basis"] == @reviews.frames_basis(clip)
    unless fresh
      return item.merge("needsReview" => true, "verdict" => review.is_a?(Hash) ? "stale" : "none")
    end

    review = ClipTrim.effective_for(shot, clip) if shot
    item["verdict"] = review["status"]
    categories = QARemediation.remediable_categories(review, effective["categories"])
    return item.merge("state" => "skipped", "reason" => review["status"] == "pass" ? "passed" : "no_remediable_issues") if categories.empty?

    item.merge("remediableCategories" => categories)
  end

  def shot_has_job?(jobs, shot, job_id)
    jobs.any? { |job| job["id"] == job_id && job["shotID"] == shot["id"] }
  end

  def find_shot(drama_id, episode_id, shot_id)
    loaded = @dramas.get("id" => drama_id)
    return nil unless loaded["ok"]

    episode = Array(loaded["drama"]["episodes"]).find { |entry| entry["id"] == episode_id }
    episode && Array(episode["shots"]).find { |entry| entry["id"] == shot_id }
  end

  # 与合成计划同一口径（EpisodePlan.pick_video）：选定的成片，没有就取最新完成的。
  def current_clip(shot, jobs)
    usable = jobs.select { |job| job["shotID"] == shot["id"] && job["state"] == "completed" && File.file?(job["outputPath"].to_s) }
    selected = usable.find { |job| job["id"] == shot["selectedVideoID"].to_s }
    selected || usable.max_by { |job| [job["createdAt"].to_s, job["updatedAt"].to_s] }
  end

  def completed_job(id)
    job = @videos.jobs_with_media.find { |entry| entry["id"] == id.to_s }
    job && job["state"] == "completed" && File.file?(job["outputPath"].to_s) ? job : nil
  end

  # 当前成片文件对应的画面质检结论：有效就用，没有或过期就审一次。返回 [结论, 错误]。
  # 审核失败（ok: false：宿主拒收附件、模型没给出合法结论……）一律当「不知道」，绝不当 pass：
  # 重试一次，仍失败就把错误交回，由调用方停下并留下一步给人。
  # 审核占进程级的质检名额（runtime.with_qa）；停下时返回 canceled / stopped 错误。
  def fresh_verdict(job, runtime)
    job = ensure_playback(job)
    stored = job.dig("reviews", "frames")
    return [stored, nil] if stored.is_a?(Hash) && stored["basis"] && stored["basis"] == @reviews.frames_basis(job)

    reviewed = nil
    QA_ATTEMPTS.times do
      reviewed = runtime.with_qa { @reviews.run("scope" => "video", "aspect" => "frames", "jobID" => job["id"]) }
      return [nil, runtime.halt_error || { "code" => "canceled", "message" => "The job was canceled." }] if reviewed == :halted
      break if reviewed["ok"]
      break if stop?(reviewed["error"])
    end
    return [nil, compact_error(reviewed["error"]).merge("stage" => "frames_review", "jobID" => job["id"])] unless reviewed["ok"]

    refreshed = completed_job(job["id"]) || job
    verdict = refreshed.dig("reviews", "frames")
    return [nil, { "code" => "review_not_stored", "message" => "The frame review ran but its verdict was not stored.", "stage" => "frames_review", "jobID" => job["id"] }] unless verdict.is_a?(Hash)

    [verdict, nil]
  end

  # 审核材料要求成片已镜像进媒体目录。
  def ensure_playback(job)
    return job unless job["mediaFile"].to_s.empty?

    prepared = @videos.prepare_playback("id" => job["id"])
    prepared["ok"] && prepared["job"].is_a?(Hash) ? prepared["job"] : job
  end

  # 提交一条带补救句的重拍（占一个视频名额，等它在远端结束），下载并做播放镜像。返回 [完成的任务, 错误]。
  # 同一 requestID 已经提交过（中断后重跑）时取回那一条，不重复花钱。
  def regenerate(drama_id, episode_id, shot, source, picks, attempt, verdict, runtime)
    item = { "shotID" => shot["id"], "jobID" => source["id"], "mode" => source["generationMode"] }
    arguments = generate_arguments(drama_id, episode_id, item, picks, attempt)
    arguments["remediation"] = { "sourceJobID" => source["id"], "attempt" => attempt, "categories" => picks.map { |pick| pick["category"] },
                                 "lessonIDs" => picks.map { |pick| pick["lessonID"] }, "fromStatus" => verdict["status"] }
    job, error = runtime.submit_video(arguments)
    return [nil, error] if error

    job, error, _warning = runtime.materialize(job)
    error ? [nil, error] : [job, nil]
  end

  def generate_arguments(drama_id, episode_id, item, picks, attempt)
    {
      "dramaID" => drama_id, "episodeID" => episode_id, "shotID" => item["shotID"], "mode" => item["mode"],
      "extraDirectives" => picks.map { |pick| pick.slice("id", "category", "text") },
      "requestID" => "#{REQUEST_PREFIX}:#{item['jobID']}:#{attempt}", "source" => "remediation"
    }.reject { |_key, value| value.nil? }
  end

  def resolved?(category, verdict)
    !issue_categories(verdict).include?(category)
  end

  def issue_categories(verdict)
    Array(verdict && verdict["issues"]).map { |issue| issue.is_a?(Hash) ? issue["category"].to_s : "" }.reject(&:empty?).uniq
  end

  # ---- 裁剪（0.39.0-rc1，lib/clip_trim.rb，docs/decisions/0007-clip-trim.md） ----

  # 这条成片能不能靠裁剪解决本次要补的问题。返回 [{trim:, review:}, nil] 或 [nil, 没裁的原因]（没试时两个都是 nil）。
  # 只在设置开着 autoTrim、本次补 scene_jump、结论里有 scene_jump 时才试；裁完之后结论里仍有别的可补问题
  # （人物错位、叠字……）就不裁，交给重拍——裁剪只解决切点，别的问题重拍时一并修。
  def trim_take(shot, job, verdict, effective)
    settings = @settings.call
    return [nil, nil] if settings["autoTrim"] == false
    return [nil, nil] if (effective["categories"] & ClipTrim::CUT_CATEGORIES).empty?
    return [nil, nil] if (QARemediation.remediable_categories(verdict, effective["categories"]) & ClipTrim::CUT_CATEGORIES).empty?

    dialogue_ms, lip_synced = dialogue_need(shot, job)
    planned = ClipTrim.plan_auto(verdict, clip_ms: ClipTrim.clip_ms(job), dialogue_ms: dialogue_ms, lip_synced: lip_synced,
                                          min_seconds: ClipTrim.min_seconds(settings), margin: ClipTrim.margin(settings))
    return [nil, { "jobID" => job["id"], "reason" => planned["reason"] }] unless planned["ok"]

    trimmed = ClipTrim.effective_review(verdict, planned["trim"].merge("jobID" => job["id"]))
    remaining = QARemediation.remediable_categories(trimmed, effective["categories"])
    return [nil, { "jobID" => job["id"], "reason" => "other_issues_remain", "categories" => remaining }] unless remaining.empty?

    [{ trim: planned["trim"], review: trimmed }, nil]
  end

  # 原片靠裁剪解决：连同裁剪一起选定（闸门照旧：批量开始后有人另选了就不动），记一条经验。
  def trimmed_outcome(drama_id, episode_id, shot, source, verdict, trimmed, since, base)
    adopted = adopt(drama_id, episode_id, shot, source, [source["id"]], since, trimmed[:review]["status"], trim: trimmed[:trim])
    learn_trim(shot, source, verdict, trimmed) unless adopted["kept"] || adopted["error"]
    result = base.merge("state" => "completed", "outcome" => "trimmed", "attempts" => 0, "finalStatus" => trimmed[:review]["status"],
                        "takes" => [take_summary(take(source, trimmed[:review], 0).merge(trim: trimmed[:trim]))])
    result["trim"] = trimmed[:trim] unless adopted["kept"] || adopted["error"]
    result.merge(adoption_fields(adopted, source))
  end

  # 经验库记一条「裁剪」：类别 scene_jump、remediationID trim_before_cut、action trim（与重拍分开去重）。
  def learn_trim(shot, job, verdict, trimmed)
    entry = {
      "category" => "scene_jump", "remediationID" => ClipTrim::REMEDIATION_ID, "lessonID" => ClipTrim::REMEDIATION_ID, "text" => ClipTrim::LESSON_TEXT,
      "action" => "trim", "provider" => @videos.provider_name, "model" => job.dig("request", "model") || job["model"], "traits" => traits(job, shot),
      "outcome" => "resolved", "fromStatus" => verdict["status"], "toStatus" => trimmed[:review]["status"], "attempt" => 0,
      "dramaID" => job["dramaID"], "episodeID" => job["episodeID"], "shotID" => shot["id"], "sourceJobID" => job.dig("remediation", "sourceJobID") || job["id"],
      "jobID" => job["id"], "trim" => trimmed[:trim].slice("inSeconds", "outSeconds", "issueTime")
    }
    @lessons.record([entry])
  end

  # 这一镜台词要占多长（毫秒）与是不是口型对齐的音轨。
  # - 照对白音轨生成的片段（ref2va）：那条音轨从第 0 毫秒起，口型照它对；
  # - 否则按台词选定配音的时长（与合成的排法一致：第一句前 0.3 秒、句间 0.3 秒、末尾 0.3 秒）；
  #   有台词却缺配音时长时返回 nil（算不出，不自动裁）；没有台词为 0。
  def dialogue_need(shot, job)
    track = job["referenceAudioPath"].to_s
    if job["syntheticVoice"] == true && !track.empty? && File.file?(track)
      begin
        return [WavTools.info(track).duration_ms, true]
      rescue WavTools::Error
        return [nil, true]
      end
    end
    lines = Array(shot["dialogue"]).select { |line| line.is_a?(Hash) && !line["text"].to_s.strip.empty? }
    return [0, false] if lines.empty?

    durations = lines.map do |line|
      selected = line["audio"].is_a?(Hash) ? MediaRef.selected(line["audio"], @dramas.media_root) : nil
      selected && selected["durationMs"].to_i
    end
    return [nil, false] if durations.any? { |value| value.nil? || value <= 0 }

    [EpisodePlan.speech_length_ms(durations.map { |value| { "durationMs" => value } }), false]
  end

  def traits(job, shot)
    {
      "mode" => job["generationMode"] || job["mode"], "combo" => job["combo"],
      "hasDialogueAudio" => job["syntheticVoice"] == true || !job["referenceAudioPath"].to_s.empty?,
      "castSize" => Array(shot.dig("package", "cast")).length
    }.reject { |_key, value| value.nil? }
  end

  # 每类问题记一条：用的哪句、重拍后这一类还在不在。
  def learn(picks, before, after, retake, shot, attempt, source)
    traits = {
      "mode" => retake["generationMode"] || retake["mode"], "combo" => retake["combo"],
      "hasDialogueAudio" => retake["syntheticVoice"] == true || !retake["referenceAudioPath"].to_s.empty?,
      "castSize" => Array(shot.dig("package", "cast")).length
    }.reject { |_key, value| value.nil? }
    entries = picks.map do |pick|
      {
        "category" => pick["category"], "remediationID" => pick["id"], "lessonID" => pick["lessonID"], "text" => pick["text"],
        "provider" => @videos.provider_name, "model" => retake.dig("request", "model") || retake["model"],
        "traits" => traits, "outcome" => resolved?(pick["category"], after) ? "resolved" : "unresolved",
        "fromStatus" => before["status"], "toStatus" => after["status"], "attempt" => attempt,
        "dramaID" => retake["dramaID"], "episodeID" => retake["episodeID"], "shotID" => shot["id"],
        "sourceJobID" => source["id"], "jobID" => retake["id"]
      }
    end
    @lessons.record(entries)
  end

  # ready_at：重拍在本次运行里下载好、可以审的时间（原片没有）。
  def take(job, review, attempt, ready_at = nil)
    issues = Array(review["issues"]).select { |issue| issue.is_a?(Hash) }
    blocking = issues.count { |issue| issue["severity"] == "block" }
    { job: job, review: review, attempt: attempt, ready_at: ready_at,
      score: [STATUS_RANK.fetch(review["status"].to_s, 3), blocking, issues.length, attempt] }
  end

  def take_summary(entry)
    { "jobID" => entry[:job]["id"], "attempt" => entry[:attempt], "status" => entry[:review]["status"],
      "categories" => issue_categories(entry[:review]), "summary" => entry[:review]["summary"].to_s[0, SUMMARY_LIMIT] }
      .merge(entry[:ready_at] ? { "readyAt" => entry[:ready_at] } : {})
      .merge(entry[:trim] ? { "trim" => entry[:trim].slice("inSeconds", "outSeconds") } : {})
  end

  # 选片走 VideoSelection.adopt：已经选着这条不重写；批量开始后有人另选了就不动（kept）。
  # status：这条成片的画面质检结论，与当前选定比（新片不比当前选定差才换；当前选定是人工放行的，新片要 pass 才换）。
  # trim：自动裁剪，与选定一起写入（0.39.0-rc1）。
  def adopt(drama_id, episode_id, shot, job, own_ids, since, status, extra = {}, trim: nil)
    current_trim = VideoSelection.shot_trim(@dramas, drama_id, episode_id, shot["id"])
    jobs = @videos.jobs_with_media
    statuses = VideoSelection.statuses_for(jobs, shot["id"], ->(entry) { @reviews.frames_basis(entry) }, current_trim)
    # 口型已过期（台词重配过）的当前选定不参与比较（0.40.0-rc1）。
    stale_ids = VideoSelection.stale_ids_for(@dramas, jobs, drama_id, episode_id, shot["id"])
    VideoSelection.adopt(@dramas, drama_id: drama_id, episode_id: episode_id, shot_id: shot["id"], job_id: job["id"],
                                  by: "remediation", status: status, statuses: statuses, since: since, own_ids: own_ids, extra: extra, trim: trim,
                                  stale_ids: stale_ids)
  end

  # 逐项记为什么选 / 不选（selectionReason）；不换时另记 selectionKept、bestJobID 与比较的两条结论。
  def adoption_fields(adopted, job)
    fields = { "selectionReason" => adopted["reason"] }
    fields["replacedStaleDialogue"] = adopted["replacedStaleDialogue"] if adopted["replacedStaleDialogue"]
    if adopted["kept"]
      fields["selectionKept"] = adopted["kept"]
      fields["bestJobID"] = job["id"]
      fields["selectionCompared"] = adopted["compared"] if adopted["compared"]
    end
    fields["selectError"] = compact_error(adopted["error"]) if adopted["error"]
    fields
  end

  def select_best(drama_id, episode_id, shot, best, effective, categories, own_ids, since)
    status = best[:review]["status"]
    override = status == "block" && effective["autoOverride"]
    extra = override ? { "qaOverride" => true, "qaOverrideBy" => "auto" } : {}
    adopted = adopt(drama_id, episode_id, shot, best[:job], own_ids, since, status, extra, trim: best[:trim])
    result = { "state" => "completed" }.merge(adoption_fields(adopted, best[:job]))
    result["trim"] = best[:trim] if best[:trim] && !adopted["kept"] && !adopted["error"]
    remaining = QARemediation.remediable_categories(best[:review], categories)
    result["outcome"] = if status == "pass" then "resolved"
                        elsif remaining.empty? then "improved"
                        else "unresolved"
                        end
    result["qaOverride"] = true if override && !adopted["kept"] && !adopted["error"]
    if status == "block" && !override
      result["nextStep"] = "No take passed frame QA. Watch the takes and either pick one with drama.select_video qaOverride: true (user decision), " \
                           "revise the shot (camera intent, action, start frame) and run video.remediate again, or regenerate manually."
      result["nextStep"] += " The shot keeps its current clip #{adopted['selectedJobID']} (#{adopted['kept']})." if adopted["kept"]
    end
    result
  end

  def qa_failed_step(job, error, selection)
    format(QA_FAILED_NEXT_STEP, job: job["id"], message: "#{error['code']}: #{error['message']}", selection: selection)
  end

  def stop?(error)
    %w[host_review_unsupported video_unconfigured configuration_error].include?(error && error["code"])
  end

  def compact_error(error)
    return { "code" => "unknown", "message" => "Unknown error." } unless error.is_a?(Hash)

    compact = { "code" => error["code"], "message" => error["message"].to_s[0, SUMMARY_LIMIT] }
    compact["retryable"] = error["retryable"] unless error["retryable"].nil?
    compact
  end

  def summary(context, extra = {})
    items = context.items
    counts = Hash.new(0)
    outcomes = Hash.new(0)
    items.each do |item|
      counts[item["state"]] += 1
      outcomes[item["outcome"]] += 1 if item["outcome"]
    end
    result = { "ok" => true, "counts" => counts, "outcomes" => outcomes }.merge(extra)
    attempted = counts["completed"] + counts["failed"]
    return result unless attempted.positive? && counts["completed"].zero? && extra["canceled"] != true

    first = items.find { |item| item["state"] == "failed" }
    result.merge("ok" => false, "error" => { "code" => "batch_failed", "message" => "Every attempted shot failed. First error: #{first && first.dig('error', 'message')}" })
  end

  def failure(code, message)
    { "ok" => false, "error" => { "code" => code, "message" => message } }
  end
end
