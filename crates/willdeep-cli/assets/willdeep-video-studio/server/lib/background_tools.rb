# frozen_string_literal: true

require_relative "host_bridge"
require_relative "job_runner"
require_relative "episode_batch"
require_relative "image_qa"

# MCP 入口：`async: true` 的 image.generate / review.run / voice.generate、四个按集批量工具、
# jobs.status / jobs.wait / jobs.cancel（0.36.0-rc1，决策见 docs/decisions/0003-background-jobs.md）。
#
# 这里只做三件事：提交前的同步检查（参数、对象、后端是否可用——不合格当场报错、不建
# 任务）；把任务交给 JobRunner；把台账记录整理成对外的样子。真正干活的仍是单个工具的
# 那几个服务对象。
class BackgroundTools
  # 后台任务里的单个宿主反向请求最多等这么久（宿主自己对出图的上限是 10 分钟）。
  HOST_REQUEST_TIMEOUT = 15 * 60
  WAIT_LIMIT = 60
  # 宿主对 stdio 上的单个请求只等 startup_timeout_sec，等不到就结束插件进程（0.41.0-rc3 起 mcp.json 写 180 秒，
  # 以前是 10 秒）。经 stdio 调 jobs.wait 仍只等这么久：宿主对一个插件的 stdio 请求是排队的，一次长等会把
  # 插件页面的请求也挡住；经 HTTP 时才放到 60 秒。
  STDIO_WAIT_LIMIT = 8
  DEFAULT_WAIT = 30
  STATUS_LIMIT = 20
  MAX_STATUS_LIMIT = 100
  MAX_WAIT_IDS = 20
  # image.qa：候选图质检（0.40.0-rc1，lib/image_qa.rb），async: true 或 image.generate 之后自动排队。
  SINGLE_KINDS = %w[image.generate review.run voice.generate image.qa drama.write_with_panel].freeze
  BATCH_KINDS = %w[episode.generate_frames episode.dub episode.generate_videos review.run_batch].freeze
  # 画面质检自动补救（0.38.0-rc1，docs/decisions/0004-qa-lessons.md）。
  REMEDIATION_KINDS = %w[video.remediate episode.remediate_videos].freeze
  KINDS = (SINGLE_KINDS + BATCH_KINDS + REMEDIATION_KINDS).freeze
  STATE_FILTERS = (%w[active finished] + BackgroundJobStore::STATES).freeze

  def initialize(runner:, batch:, images:, voices:, reviews:, videos:, remediator: nil, image_qa: nil, script_creation: nil)
    @script_creation = script_creation
    @image_qa = image_qa
    @runner = runner
    @batch = batch
    @images = images
    @voices = voices
    @reviews = reviews
    @videos = videos
    @remediator = remediator
    register_handlers
  end

  # ---- 画面质检自动补救 ----

  # video.remediate：一镜（jobID，或 dramaID + episodeID + shotID）。dryRun 当场返回计划与提示词预览。
  def remediate(arguments)
    request = without_async(arguments)
    return @remediator.preview(request) if request["dryRun"] == true

    plan = @remediator.plan(request)
    return plan unless plan["ok"]
    refused = @remediator.preflight
    return refused if refused

    item = plan["items"][0]
    request = request.merge("dramaID" => plan["drama"]["id"], "episodeID" => plan["episode"]["id"], "shotID" => item["shotID"])
    label = "第 #{plan['episode']['order']} 集第 #{item['order']} 镜"
    submit_batch("video.remediate", request, plan, extra_key: item["shotID"], label: label)
  end

  # episode.remediate_videos：一集（shotOrders 可选）。
  def remediate_batch(arguments)
    request = without_async(arguments)
    return @remediator.preview(request) if request["dryRun"] == true
    return failure("invalid_arguments", "Pass dramaID and episodeID (use video.remediate for one shot).") if request["episodeID"].to_s.empty?

    plan = @remediator.plan(request.reject { |key, _| %w[shotID jobID].include?(key) })
    return plan unless plan["ok"]
    refused = @remediator.preflight
    return refused if refused

    submit_batch("episode.remediate_videos", request.reject { |key, _| %w[shotID jobID].include?(key) }, plan)
  end

  # ---- 候选图质检（0.40.0-rc1） ----

  # 同步的 image.generate：出完图后，开着候选图质检时把新候选的质检排成后台任务（image.qa），结果里带 qa {state: queued, jobID}，
  # 不让出图调用多等一两分钟。dryRun、去重返回、没出图时不排。
  def image_generate_now(arguments)
    result = @images.generate(arguments)
    return result unless @image_qa && result["ok"] && result["dryRun"] != true

    ids = ImageQA.ids_for(arguments["target"], arguments)
    recommendation = @image_qa.current_recommendation(arguments["dramaID"], arguments["target"], ids)
    result = result.merge("recommendedCandidateID" => recommendation && recommendation["candidateID"])
    return result if result["deduplicated"] == true || Array(result["generated"]).empty? || !@image_qa.auto?(arguments)

    request = { "dramaID" => arguments["dramaID"], "target" => arguments["target"],
                "candidateIDs" => Array(result["generated"]).map { |entry| entry["candidateID"] } }
                .merge(ImageQA.ids_for(arguments["target"], arguments))
    request["castIDs"] = arguments["castIDs"] if arguments["castIDs"].is_a?(Array)
    queued = submit("image.qa", request, label: "qa #{arguments['target']} #{request['shotID'] || request['characterID'] || request['assetID']}",
                                         request_id: nil, items: [single_item("image.qa")])
    result.merge("qa" => queued["ok"] ? { "state" => "queued", "jobID" => queued["jobID"] } : { "state" => "not_queued", "error" => queued["error"] })
  end

  # image.qa：同步跑，或 async: true 排成后台任务。
  def image_qa(arguments)
    return failure("host_review_unsupported", "Candidate image QA is not available in this server.") unless @image_qa
    return @image_qa.run_tool(arguments) unless arguments["async"] == true

    request = without_async(arguments)
    target = request["target"].to_s
    return failure("invalid_target", "target must be one of #{ImageQA::TARGETS.join(', ')}.") unless ImageQA::TARGETS.include?(target)
    return failure("host_review_unsupported", "Candidate image QA needs the WillDeep host's model bridge (willdeep/ai/complete).") unless @image_qa.available?

    label = "qa #{target} #{request['shotID'] || request['characterID'] || request['assetID']}"
    submit("image.qa", request, label: label, request_id: request["requestID"], items: [single_item(label)])
  end

  # ---- 单个工具的 async: true ----

  def image_generate(arguments)
    request = without_async(arguments)
    checked = @images.generate(request.merge("dryRun" => true))
    return checked unless checked["ok"]
    # 同一个 requestID 已经出过：直接把上次的结果交回，不建任务。
    return checked if checked["deduplicated"]
    unless @images.available?
      return failure("host_image_unsupported", "No image backend is available. Inside WillDeep, update the host to 1.378.0 or later; elsewhere set VIDEO_STUDIO_IMAGE_API_BASE and VIDEO_STUDIO_IMAGE_API_KEY.")
    end

    label = [request["target"], request["characterID"] || request["shotID"] || request["assetID"]].compact.join(" ")
    submit("image.generate", request, label: label, request_id: request["requestID"], items: [single_item(label)])
  end

  def review_run(arguments)
    request = without_async(arguments)
    refused = @reviews.preflight(request)
    return refused if refused

    label = "#{request['scope']}/#{request['aspect']}"
    submit("review.run", request, label: label, request_id: request["requestID"], items: [single_item(label)])
  end

  def write_with_panel(arguments)
    request = without_async(arguments)
    refused = @script_creation.preflight(request)
    return refused if refused
    label = "#{request['scope']} #{request['episodeID']}"
    submit("drama.write_with_panel", request, label: label, request_id: request["requestID"],
           dedupe_key: "write-panel|#{request['dramaID']}|#{request['episodeID']}", items: [single_item(label)])
  end

  def voice_generate(arguments)
    request = without_async(arguments)
    checked = @voices.generate(request.merge("dryRun" => true))
    return checked unless checked["ok"]
    return checked if checked["deduplicated"]
    unless @voices.available?
      return failure("tts_unavailable", "No TTS backend is configured. Set VIDEO_STUDIO_TTS_API_KEY to a DashScope (Bailian) API key, or also set VIDEO_STUDIO_TTS_PROVIDER=openai with VIDEO_STUDIO_TTS_API_BASE for an OpenAI-compatible service.")
    end
    return failure("nothing_to_generate", "No dialogue line has a usable voice.").merge("skipped" => checked["skipped"]) if Array(checked["items"]).empty?

    label = request["assetID"] ? "preview #{request['assetID']}" : "shot #{request['shotID']}"
    submit("voice.generate", request, label: label, request_id: request["requestID"], items: [single_item(label)])
  end

  # ---- 按集批量 ----

  def generate_frames(arguments)
    request = without_async(arguments)
    plan = @batch.plan_frames(request)
    return plan unless plan["ok"]
    unless @images.available?
      return failure("host_image_unsupported", "No image backend is available. Inside WillDeep, update the host to 1.378.0 or later; elsewhere set VIDEO_STUDIO_IMAGE_API_BASE and VIDEO_STUDIO_IMAGE_API_KEY.")
    end
    first = plan["items"].find { |item| item["state"] == "pending" }
    if first
      # 张数、模型、画幅不合适时在花钱前报出来：拿第一镜做一次 dryRun。
      checked = @images.generate(@batch.frame_request(request, plan["target"], first["shotID"], nil).merge("dryRun" => true))
      return checked unless checked["ok"]
    end
    submit_batch("episode.generate_frames", request, plan, extra_key: plan["target"])
  end

  def dub(arguments)
    request = without_async(arguments)
    plan = @batch.plan_dub(request)
    return plan unless plan["ok"]
    unless @voices.available?
      return failure("tts_unavailable", "No TTS backend is configured. Set VIDEO_STUDIO_TTS_API_KEY (DashScope) or VIDEO_STUDIO_TTS_PROVIDER=openai with VIDEO_STUDIO_TTS_API_BASE / _KEY.")
    end

    submit_batch("episode.dub", request, plan)
  end

  def generate_videos(arguments)
    request = without_async(arguments)
    plan = @batch.plan_videos(request)
    return plan unless plan["ok"]

    status = @videos.status
    unless status["ok"] && status["apiConfigured"]
      return failure("video_unconfigured", "The video provider is not configured (API key missing); set it in the plugin settings or VIDEO_STUDIO_API_KEY.")
    end
    submit_batch("episode.generate_videos", request, plan, extra_key: plan["mode"])
  end

  def review_batch(arguments)
    request = without_async(arguments)
    refused = @reviews.preflight("scope" => "drama", "aspect" => "content")
    return refused if refused

    plan = @batch.plan_reviews(request)
    return plan unless plan["ok"]

    submit_batch("review.run_batch", request, plan, extra_key: plan["scopes"].join(","), label: "review #{plan['scopes'].join(',')}")
  end

  # ---- 查询 ----

  def status(arguments)
    ids = requested_ids(arguments)
    jobs = @runner.store.jobs
    jobs = jobs.select { |job| ids.include?(job["id"]) } unless ids.empty?
    %w[dramaID episodeID kind].each do |key|
      value = arguments[key].to_s
      jobs = jobs.select { |job| job[key].to_s == value } unless value.empty?
    end
    state = arguments["state"].to_s
    unless state.empty?
      return failure("invalid_state", "state must be one of #{STATE_FILTERS.join(', ')}.") unless STATE_FILTERS.include?(state)

      jobs = jobs.select { |job| state_matches?(job, state) }
    end
    counts = Hash.new(0)
    jobs.each { |job| counts[job["state"]] += 1 }
    limit = bounded(arguments["limit"], 1, MAX_STATUS_LIMIT, STATUS_LIMIT)
    include_items = arguments["includeItems"] != false
    result = { "ok" => true, "jobs" => jobs.first(limit).map { |job| view(job, include_items: include_items) }, "counts" => counts,
               "total" => jobs.length, "workers" => @runner.worker_count }
    missing = ids - jobs.map { |job| job["id"] }
    result["missing"] = missing unless missing.empty?
    result
  end

  def wait(arguments, transport: :stdio)
    ids = requested_ids(arguments)
    return failure("invalid_arguments", "Pass jobIDs (1 to #{MAX_WAIT_IDS}).") if ids.empty? || ids.length > MAX_WAIT_IDS

    limit = transport == :stdio ? STDIO_WAIT_LIMIT : WAIT_LIMIT
    requested = begin
      Float(arguments["timeoutSeconds"] || DEFAULT_WAIT)
    rescue ArgumentError, TypeError
      DEFAULT_WAIT
    end
    timeout = requested.clamp(0, limit)
    started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
    jobs, done = @runner.wait(ids, timeout)
    missing = ids.each_with_index.reject { |_id, index| jobs[index] }.map(&:first)
    return failure("not_found", "No background job has these IDs: #{missing.join(', ')}.") if missing.length == ids.length

    result = { "ok" => true, "done" => done, "timedOut" => !done, "timeoutSeconds" => timeout,
               "waitedSeconds" => (Process.clock_gettime(Process::CLOCK_MONOTONIC) - started).round(2),
               "jobs" => jobs.compact.map { |job| view(job, include_items: arguments["includeItems"] != false) } }
    result["capped"] = true if requested > limit
    result["missing"] = missing unless missing.empty?
    result
  end

  def cancel(arguments)
    id = arguments["jobID"].to_s
    job = @runner.find(id)
    return failure("not_found", "Background job was not found.") unless job
    return { "ok" => true, "alreadyFinished" => true, "job" => view(job) } if BackgroundJobStore::TERMINAL_STATES.include?(job["state"])

    updated = @runner.cancel(id)
    result = { "ok" => true, "job" => view(updated || job) }
    if updated && updated["state"] == "running"
      result["note"] = "The job stops before its next item; the image, review or video submission already in progress finishes and is kept. Submitted video jobs keep running remotely."
    end
    result
  end

  # 台账记录 → 对外形状。
  def view(job, include_items: true)
    items = Array(job["items"])
    counts = Hash.new(0)
    items.each { |item| counts[item["state"]] += 1 if item.is_a?(Hash) }
    shown = job.reject { |key, _| %w[dedupeKey ownerPID items].include?(key) }
    shown["itemCounts"] = counts
    stages = stages_of(items)
    shown["stages"] = stages unless stages.empty?
    shown["items"] = items if include_items
    shown
  end

  # 逐镜并发的批量（视频、补救）每项带 stage：给页面和 Agent 一份不含结果明细的精简列表。
  def stages_of(items)
    items.select { |item| item.is_a?(Hash) && item.key?("stage") }
         .map { |item| item.slice("key", "order", "label", "state", "stage", "attempt") }
  end

  # drama.get_progress 用：这部剧还在排队或在跑的后台任务。
  def active_for(drama_id)
    @runner.store.jobs.select { |job| job["dramaID"] == drama_id.to_s && BackgroundJobStore::ACTIVE_STATES.include?(job["state"]) }
           .map { |job| job.slice("id", "kind", "state", "episodeID", "label", "progress") }
  end

  private

  def register_handlers
    if @script_creation
      @runner.register("drama.write_with_panel") { |arguments, context| single(context) { @script_creation.run(arguments, context) } }
    end
    @runner.register("image.generate") { |arguments, context| single(context) { generate_and_check(arguments) } }
    @runner.register("image.qa") { |arguments, context| single(context) { @image_qa ? @image_qa.run_tool(arguments) : failure("host_review_unsupported", "Candidate image QA is not available.") } }
    @runner.register("review.run") { |arguments, context| single(context) { @reviews.run(arguments) } }
    @runner.register("voice.generate") { |arguments, context| single(context) { @voices.generate(arguments) } }
    @runner.register("episode.generate_frames") { |arguments, context| host_bound { @batch.run_frames(arguments, context) } }
    @runner.register("episode.dub") { |arguments, context| host_bound { @batch.run_dub(arguments, context) } }
    @runner.register("episode.generate_videos") { |arguments, context| host_bound { @batch.run_videos(arguments, context) } }
    @runner.register("review.run_batch") { |arguments, context| host_bound { @batch.run_reviews(arguments, context) } }
    return unless @remediator

    @runner.register("video.remediate") { |arguments, context| host_bound { @remediator.run(arguments, context) } }
    @runner.register("episode.remediate_videos") { |arguments, context| host_bound { @remediator.run(arguments, context) } }
  end

  # 后台的 image.generate：出完图在同一个任务里接着质检新候选（已经在后台，不另排任务）。
  def generate_and_check(arguments)
    result = @images.generate(arguments)
    return result unless @image_qa && result["ok"] && result["deduplicated"] != true && @image_qa.auto?(arguments)

    checked = @image_qa.after_generate(arguments, result)
    checked ? result.merge("qa" => checked, "recommendedCandidateID" => checked["recommendedCandidateID"]) : result
  end

  def host_bound(&block)
    HostBridge.with_timeout(HOST_REQUEST_TIMEOUT, &block)
  end

  def single(context)
    context.update_item(0, "state" => "running")
    result = host_bound { yield }
    if result["ok"]
      context.update_item(0, "state" => "completed")
    else
      context.update_item(0, "state" => "failed", "error" => result["error"])
    end
    result
  end

  def single_item(label)
    { "key" => "call", "label" => label.to_s, "state" => "pending" }
  end

  def submit_batch(kind, request, plan, extra_key: nil, label: nil)
    episode = plan["episode"]
    label ||= episode ? "第 #{episode['order']} 集" : ""
    dedupe = [kind, request["dramaID"], request["episodeID"], extra_key].map(&:to_s).join("|")
    pending = plan["items"].count { |item| item["state"] == "pending" }
    extra = { "planned" => { "items" => plan["items"].length, "pending" => pending, "skipped" => plan["items"].length - pending } }
    extra["unknownOrders"] = plan["unknownOrders"] unless Array(plan["unknownOrders"]).empty?
    submit(kind, request, label: label, request_id: request["requestID"], items: plan["items"], dedupe_key: dedupe, extra: extra)
  end

  def submit(kind, request, label:, request_id:, items:, dedupe_key: nil, extra: {})
    job, how = @runner.submit(kind: kind, arguments: request, label: label, drama_id: request["dramaID"], episode_id: request["episodeID"],
                              request_id: request_id, dedupe_key: dedupe_key, items: items)
    result = { "ok" => true, "async" => true, "jobID" => job["id"], "state" => job["state"], "job" => view(job) }.merge(extra)
    result["deduplicated"] = true if how == :deduplicated
    result["alreadyRunning"] = true if how == :already_running
    result
  rescue JobRunner::QueueFull => error
    failure("jobs_queue_full", "Too many background jobs are waiting (#{error.message}). Wait for some to finish with jobs.wait, then retry.")
  end

  def without_async(arguments)
    arguments.reject { |key, _| key == "async" }
  end

  def requested_ids(arguments)
    (Array(arguments["jobIDs"]) + [arguments["jobID"]]).map(&:to_s).reject(&:empty?).uniq
  end

  def state_matches?(job, filter)
    case filter
    when "active" then BackgroundJobStore::ACTIVE_STATES.include?(job["state"])
    when "finished" then BackgroundJobStore::TERMINAL_STATES.include?(job["state"])
    else job["state"] == filter
    end
  end

  def bounded(value, minimum, maximum, fallback)
    number = Integer(value)
    number.clamp(minimum, maximum)
  rescue ArgumentError, TypeError
    fallback
  end

  def failure(code, message)
    { "ok" => false, "error" => { "code" => code, "message" => message } }
  end
end
