# frozen_string_literal: true

require "open3"
require_relative "media_mirror"
require_relative "review_record"
require_relative "reference_package"
require_relative "h3_prompt"
require_relative "wav_tools"
require_relative "aspect_ratios"
require_relative "qa_remediation"
require_relative "episode_plan"
require "uri"

class VideoService
  ACTIVE_STATES = %w[submitting queued in_progress].freeze
  FILTER_STATES = %w[all draft active completed failed recycle].freeze
  MAX_PROMPT_BYTES = 12_000
  MAX_TITLE_BYTES = 300

  # dramas：DramaService，给带 shotID 的提交展开参考包用。可以不给（旧调用方与
  # 一部分测试），那时 video.generate 只认显式参数。
  # directives：返回成片提示词预防句的 lambda（经验库 QALessons#prevention_directives）；
  # 不给时参考包编译用内置默认（0.38.0-rc1）。
  # pacing：(dramaID, episodeID) → 这一集的成片设置（ComposeStore#settings），配音节奏下请求时长由台词反推（0.43.0-rc1）；
  # 不给时时长只看分镜。
  def initialize(store:, adapter_factory:, media_mirror: nil, dramas: nil, directives: nil, pacing: nil)
    @store = store
    @adapter_factory = adapter_factory
    @media_mirror = media_mirror
    @dramas = dramas
    @directives = directives
    @pacing = pacing
    @store.jobs.each do |job|
      next unless job["state"] == "submitting" && job["remoteID"].to_s.empty?
      @store.update_job(job["id"], "state" => "failed", "ambiguousSubmission" => true,
                        "error" => { "message" => "Submission was interrupted. Verify the provider before retrying." })
    end
  end

  def status
    adapter = @adapter_factory.call
    {
      "ok" => true,
      "provider" => "openai-videos-async",
      "apiBase" => adapter.base_url,
      "apiConfigured" => adapter.configured?,
      "outputDirectory" => adapter.output_directory,
      "storePath" => @store.path,
      "settings" => @store.settings,
      "counts" => counts(@store.jobs)
    }
  rescue VideoProviderError => error
    { "ok" => false, "error" => error.as_json, "settings" => @store.settings }
  end

  def create_draft(arguments)
    prompt = clipped(arguments["text"] || arguments["prompt"], MAX_PROMPT_BYTES)
    return failure("empty_prompt", "A video idea is required.") if prompt.empty?
    title = clipped(arguments["title"], MAX_TITLE_BYTES)
    title = prompt[0, 72] if title.empty?
    job = @store.add_job(
      "title" => title,
      "prompt" => prompt,
      "negativePrompt" => nil,
      "referenceImagePath" => optional_string(arguments["referenceImagePath"]),
      "mode" => optional_string(arguments["referenceImagePath"]) ? "image_to_video" : "text_to_video",
      "source" => optional_string(arguments["source"]) || "manual"
    )
    { "ok" => true, "job" => job }
  end

  def list(arguments)
    filter = arguments["status"].to_s
    filter = "all" unless FILTER_STATES.include?(filter)
    query = clipped(arguments["query"], 300).downcase
    jobs = @store.jobs.select { |job| matches_filter?(job, filter) }
    jobs = jobs.select { |job| searchable_text(job).downcase.include?(query) } unless query.empty?
    {
      "ok" => true,
      "jobs" => jobs.map { |job| with_media_path(job) },
      "counts" => counts(@store.jobs),
      "settings" => @store.settings,
      "provider" => safe_provider_status,
      # 已完成却还没下载到本地的条数：页面据此补下载、Agent 据此决定要不要调 video.download。
      "pendingDownloads" => undownloaded_jobs.length
    }
  end

  # 全部任务，补上 mediaPath。给进度与审核用：它们要知道成片镜像在哪，却不需要
  # list 顺带算的计数与 provider 状态。
  def jobs_with_media
    @store.jobs.reject { |job| !job["deletedAt"].to_s.empty? }.map { |job| with_media_path(job) }
  end

  def settings(arguments)
    changes = {}
    copy_optional_string(arguments, changes, "assistantProviderID", 300)
    copy_optional_string(arguments, changes, "assistantModel", 300)
    copy_optional_string(arguments, changes, "reviewProviderID", 300)
    copy_optional_string(arguments, changes, "reviewModel", 300)
    changes["uiLocale"] = arguments["uiLocale"].to_s if arguments.key?("uiLocale")
    copy_optional_string(arguments, changes, "videoModel", 160)
    copy_integer(arguments, changes, "duration")
    copy_integer(arguments, changes, "width")
    copy_integer(arguments, changes, "height")
    copy_integer(arguments, changes, "inferenceSteps")
    changes["autoDownload"] = boolean(arguments["autoDownload"]) if arguments.key?("autoDownload")
    changes["autoQA"] = boolean(arguments["autoQA"]) if arguments.key?("autoQA")
    # 自动补救（0.38.0-rc1）：校验与默认值在 VideoStore#normalize_settings。
    changes["autoRemediate"] = boolean(arguments["autoRemediate"]) if arguments.key?("autoRemediate")
    changes["remediateAutoOverride"] = boolean(arguments["remediateAutoOverride"]) if arguments.key?("remediateAutoOverride")
    copy_integer(arguments, changes, "remediateMaxRetries")
    # 批量逐镜并发的名额（0.38.0-rc2）：截断在 VideoStore#normalize_settings。
    copy_integer(arguments, changes, "qaConcurrency")
    copy_integer(arguments, changes, "videoConcurrency")
    changes["remediateCategories"] = QARemediation.clean_categories(arguments["remediateCategories"], []) if arguments.key?("remediateCategories")
    # 自动裁剪（0.39.0-rc1）：夹取在 VideoStore#normalize_settings（ClipTrim.min_seconds / margin）。
    changes["autoTrim"] = boolean(arguments["autoTrim"]) if arguments.key?("autoTrim")
    %w[trimMinSeconds trimMargin].each { |key| changes[key] = arguments[key] if arguments.key?(key) && arguments[key].is_a?(Numeric) }
    # 候选图质检（0.40.0-rc1）：夹取在 VideoStore#normalize_settings。
    %w[imageAutoQA imageAutoSelect].each { |key| changes[key] = boolean(arguments[key]) if arguments.key?(key) }
    copy_integer(arguments, changes, "imageRetryOnBlock")
    # 专家席审稿（0.42.0-rc1）：进度与批量是否列出 panel 面。宿主圆桌（0.43.0-rc1）：开关与轮数。
    changes["panelReview"] = boolean(arguments["panelReview"]) if arguments.key?("panelReview")
    changes["panelRoundtable"] = boolean(arguments["panelRoundtable"]) if arguments.key?("panelRoundtable")
    copy_integer(arguments, changes, "panelRounds")
    saved = changes.empty? ? @store.settings : @store.update_settings(changes)
    status.merge("settings" => saved)
  end

  # 提交一条视频任务。
  #
  # 0.26.0 起可以带 dramaID / episodeID / shotID：服务端按镜头的参考包展开（设计稿
  # 5.2），把当时实际发出去的参考写成 referenceSnapshot，进度按 shotID 关联视频，
  # 不再靠参考图路径反查。prompt 没给时用编译出来的 PromptIR。
  # dryRun: true 只返回将要发的请求与展开结果，不建任务、不花钱。
  # requestID：同一个 ID 的重复提交返回上一次建的任务。
  def generate(arguments)
    request_id = optional_string(arguments["requestID"])
    if request_id
      existing = @store.jobs.find { |job| job["requestID"] == request_id }
      return { "ok" => true, "job" => with_media_path(existing), "deduplicated" => true } if existing
    end

    linkage = shot_linkage(arguments)
    return linkage if linkage["ok"] == false

    merged = arguments.merge({})
    # 带镜头时提示词默认用编译出来的 H3 结构化提示词；调用方传了已含段名的提示词
    # 就原样用它，传了一句自然语言则包进三段式 / 六段式的对应位置。
    if linkage["mode"]
      custom = optional_string(merged["prompt"], MAX_PROMPT_BYTES)
      merged["prompt"] = custom.nil? || H3Prompt.structured?(custom) ? (custom || linkage["promptIR"]) : H3Prompt.insert_into_shot(linkage["promptIR"], custom)
      merged["task"] = linkage["mode"]
      merged["combo"] = linkage["combo"]
      # 页面一直按镜头时长提交；只传 shotID 的 Agent 此前落到全局默认 4 秒，8 秒的戏被压成一半。
      merged["duration"] = linkage["duration"] if merged["duration"].nil? && linkage["duration"]
      merged["referenceVideoPaths"] = linkage["referenceVideoPaths"]
      merged["referenceAudioPath"] = linkage["referenceAudioPath"]
      merged["referenceImagePath"] = linkage["mode"] == "t2va" || linkage["combo"] == "videos" ? nil : (optional_string(merged["referenceImagePath"]) || linkage["referenceImagePath"])
    end
    request = normalized_request(merged)
    validation = validate_request(request)
    return validation if validation

    if arguments["dryRun"] == true
      return { "ok" => true, "dryRun" => true, "request" => request, "mode" => linkage["mode"] || request["mode"],
               "referenceSnapshot" => linkage["referenceSnapshot"] || [], "warnings" => linkage["warnings"] || [],
               "dropped" => linkage["dropped"] || [], "promptIR" => linkage["promptIR"],
               "directives" => linkage["directives"] || [], "directivesSkipped" => linkage["directivesSkipped"] || [] }
    end

    extra = linkage.select { |key, _| %w[dramaID episodeID shotID generationMode combo referenceSnapshot syntheticVoice directives dialogueAudio].include?(key) }
    extra["requestID"] = request_id if request_id
    # 自动补救提交的重拍（lib/video_remediation.rb）：记下是哪条成片的第几次重做、因为什么。
    extra["remediation"] = arguments["remediation"] if linkage["mode"] && arguments["remediation"].is_a?(Hash)
    job = prepare_job(arguments["draftID"], request, extra)
    submit(job)
  rescue VideoProviderError => error
    failure("configuration_error", error.message, error.as_json)
  end

  # Provider 能力声明与图片参照上限，给 Agent 做预算与选模式（设计稿 7.1）。
  def capabilities
    { "ok" => true, "video" => adapter_capabilities, "imageReferenceLimit" => ReferencePackage::IMAGE_REFERENCE_LIMIT,
      "modes" => ReferencePackage::MODES }
  end

  def refresh(arguments)
    job = @store.find_job(arguments["id"])
    return failure("not_found", "Video job was not found.") unless job
    return failure("missing_remote_id", "This draft has not been submitted.") if job["remoteID"].to_s.empty?
    refresh_job(job)
  end

  def refresh_active
    jobs = @store.jobs.select { |job| ACTIVE_STATES.include?(job["state"]) && !job["remoteID"].to_s.empty? }
    # 每个任务单独兜底：refresh_job 只 rescue 了 VideoProviderError，一个任务
    # 抛别的异常会让这一整批 20 个刷新全废。
    refreshed = jobs.first(20).map do |job|
      begin
        refresh_job(job)["job"] || job
      rescue StandardError => error
        warn "video-studio: refresh skipped for #{job['id']} (#{error.class}: #{error.message})"
        job
      end
    end
    # 已完成却没下载到本地的任务（进程在下载途中被宿主结束、机器休眠都会造成）
    # 此前永远停在「已完成、无文件、无错误」：轮询只看活动任务，页面上就是一块
    # 空白。自动下载开着时在这里顺手补上，一次最多补几条，别让一次刷新卡太久。
    recovered = pending_downloads.first(MAX_RECOVERED_DOWNLOADS).map do |job|
      begin
        download_if_ready(job) || job
      rescue StandardError => error
        warn "video-studio: download recovery skipped for #{job['id']} (#{error.class}: #{error.message})"
        job
      end
    end
    { "ok" => true, "jobs" => refreshed + recovered, "counts" => counts(@store.jobs), "pendingDownloads" => undownloaded_jobs.length }
  end

  MAX_RECOVERED_DOWNLOADS = 3
  # 预签名直链失效（Hub 的直链 7 天有效）时上游回这几种状态；重新查一次任务就能拿到新链接。
  REFRESHABLE_DOWNLOAD_STATUSES = [400, 401, 403].freeze

  # 已完成、有直链、本地没文件、也没记过下载错误的任务。
  def undownloaded_jobs
    @store.jobs.select do |job|
      job["state"] == "completed" && !job["outputURL"].to_s.empty? && job["outputPath"].to_s.empty? &&
        job["downloadError"].nil? && job["deletedAt"].to_s.empty?
    end
  end

  def pending_downloads
    @store.settings["autoDownload"] ? undownloaded_jobs : []
  end

  def retry(arguments)
    original = @store.find_job(arguments["id"])
    return failure("not_found", "Video job was not found.") unless original
    request = original["request"]
    return failure("missing_request", "The original request is unavailable.") unless request.is_a?(Hash)
    return failure("still_active", "An active job cannot be retried.") if ACTIVE_STATES.include?(original["state"])

    job = @store.add_job(job_attributes(request).merge("retryOf" => original["id"], "qaEligible" => true, "state" => "submitting"))
    submit(job)
  end

  def remove(arguments)
    job = @store.find_job(arguments["id"])
    return failure("not_found", "Video job was not found.") unless job
    return failure("active_job", "An active video cannot be moved to the recycle bin.") if ACTIVE_STATES.include?(job["state"])

    updated = @store.update_job(job["id"], "deletedAt" => Time.now.utc.iso8601)
    { "ok" => true, "job" => with_media_path(updated || job), "counts" => counts(@store.jobs) }
  end

  def restore(arguments)
    job = @store.find_job(arguments["id"])
    return failure("not_found", "Video job was not found.") unless job

    updated = @store.update_job(job["id"], "deletedAt" => nil)
    { "ok" => true, "job" => with_media_path(updated || job), "counts" => counts(@store.jobs) }
  end

  def download(arguments)
    job = @store.find_job(arguments["id"])
    return failure("not_found", "Video job was not found.") unless job
    return failure("not_completed", "The video is not completed yet.") unless job["state"] == "completed"
    updated = download_if_ready(job, force: true)
    return { "ok" => false, "job" => updated, "error" => updated["downloadError"] } if updated["downloadError"]
    return failure("missing_output", "The provider has not supplied an output URL.") if updated["outputPath"].to_s.empty?
    { "ok" => true, "job" => updated }
  end

  def pick_reference
    override = ENV["VIDEO_STUDIO_PICK_REFERENCE"].to_s.strip
    return selected_reference(override) unless override.empty?

    script = 'POSIX path of (choose file with prompt "Choose a reference image" of type {"public.png", "public.jpeg", "org.webmproject.webp"})'
    output, error, status = Open3.capture3("/usr/bin/osascript", "-e", script)
    return failure("selection_cancelled", "No reference image was selected.") unless status.success?
    selected_reference(output.strip)
  rescue Errno::ENOENT
    failure("picker_unavailable", "The macOS file picker is unavailable.")
  end

  # 让页面能就地播放这一条成片：在宿主放行的媒体目录里给成片做一份镜像，
  # 顺带抽一张首帧当封面。页面在渲染完成的任务时按需调用，一条任务只会真正
  # 做一次——已经镜像过的直接返回原样。
  def prepare_playback(arguments)
    return failure("playback_unavailable", "Playback mirroring is not configured.") unless @media_mirror

    job = @store.find_job(arguments["id"])
    return failure("not_found", "Video job was not found.") unless job
    return failure("not_completed", "The video is not downloaded yet.") unless job["state"] == "completed"

    path = job["outputPath"].to_s
    return failure("missing_output", "This job has no downloaded output.") unless File.file?(path)

    fields = @media_mirror.publish(id: job["id"], path: path)
    { "ok" => true, "job" => with_media_path(@store.update_job(job["id"], fields) || job.merge(fields)) }
  rescue MediaMirror::Unavailable => error
    failure("playback_failed", error.message)
  rescue SystemCallError => error
    failure("playback_failed", "Unable to prepare playback: #{error.class}")
  end

  # 记一次成片内容审核的结论。不改任务状态；basis 与短剧对象上的审核同义，
  # 是页面对「审的是哪一份成片」算的指纹。
  def record_review(arguments)
    job = @store.find_job(arguments["id"])
    return failure("not_found", "Video job was not found.") unless job

    stored = ReviewRecord.normalize(
      arguments["review"], basis: arguments["basis"], model: arguments["model"],
      media: { "images" => arguments["mediaImages"], "videos" => arguments["mediaVideos"] }
    )
    return failure("invalid_review", "Review fields do not match creative-v1.") unless stored

    aspect = arguments["aspect"].to_s == "frames" ? "frames" : "content"
    changes = if aspect == "frames"
                reviews = job["reviews"].is_a?(Hash) ? job["reviews"].dup : {}
                reviews[aspect] = stored
                { "reviews" => reviews }
              else
                { "review" => stored }
              end
    updated = @store.update_job(job["id"], changes)
    { "ok" => true, "job" => with_media_path(updated || job.merge(changes)), "review" => stored }
  end

  # 成片结论的「已知悉」，规则同 DramaService#acknowledge_review。不改任务状态。
  def acknowledge_review(arguments)
    job = @store.find_job(arguments["id"])
    return failure("not_found", "Video job was not found.") unless job

    by = arguments["acknowledgedBy"].to_s.empty? ? "agent" : arguments["acknowledgedBy"].to_s
    return failure("invalid_acknowledged_by", "acknowledgedBy must be user or agent.") unless ReviewRecord::ACKNOWLEDGERS.include?(by)

    frames = arguments["aspect"].to_s == "frames"
    current = frames ? (job["reviews"].is_a?(Hash) ? job["reviews"]["frames"] : nil) : job["review"]
    record = ReviewRecord.apply_acknowledgement(current.is_a?(Hash) ? current.dup : nil, arguments, by)
    changes = frames ? { "reviews" => job["reviews"].merge("frames" => record) } : { "review" => record }
    updated = @store.update_job(job["id"], changes)
    { "ok" => true, "job" => with_media_path(updated || job.merge(changes)), "review" => record }
  rescue ReviewRecord::Refused => error
    failure(error.code, error.message)
  end

  def reveal_output(arguments)
    job = @store.find_job(arguments["id"])
    return failure("not_found", "Video job was not found.") unless job
    path = job["outputPath"].to_s
    return failure("missing_output", "This job has no downloaded output.") unless File.file?(path)
    pid = Process.spawn("/usr/bin/open", "-R", path, out: File::NULL, err: File::NULL)
    Process.detach(pid)
    { "ok" => true, "path" => path }
  rescue SystemCallError => error
    failure("reveal_failed", "Unable to reveal the output: #{error.class}")
  end

  private

  # 0.20.0 之前镜像过的任务只记了 mediaFile。按当前媒体根补上绝对路径，只在
  # 文件确实还在时给——给一个不存在的路径，审核请求会在宿主那边报读不出来。
  def with_media_path(job)
    return job unless job.is_a?(Hash) && @media_mirror
    name = job["mediaFile"].to_s
    return job if name.empty? || name.include?("/")
    # 文件可能在另一个媒体根里（换过宿主）——path_for 会按需镜像到活跃根。
    path = @media_mirror.host.path_for(name)
    merged = path ? job.merge("mediaPath" => path) : job.reject { |key, _| key == "mediaPath" }
    # 旧存档记的可能是另一个宿主的前缀，按当前宿主重写后再交给页面。
    @media_mirror.host.rewrite_urls(merged)
  end

  def safe_provider_status
    adapter = @adapter_factory.call
    {
      "id" => "openai-videos-async",
      "apiBase" => adapter.base_url,
      "apiConfigured" => adapter.configured?,
      "outputDirectory" => adapter.output_directory
    }
  rescue VideoProviderError => error
    { "id" => "openai-videos-async", "apiConfigured" => false, "error" => error.as_json }
  end

  def normalized_request(arguments)
    settings = @store.settings
    dimensions = default_dimensions(arguments, settings)
    reference = optional_string(arguments["referenceImagePath"])
    requested_model = clipped(arguments["model"], 160)
    task = optional_string(arguments["task"])
    task = (reference ? "fl2va" : "t2va") if task.nil?
    {
      "title" => clipped(arguments["title"], MAX_TITLE_BYTES),
      "prompt" => clipped(arguments["prompt"], MAX_PROMPT_BYTES),
      "negativePrompt" => clipped(arguments["negativePrompt"], 4_000),
      "referenceImagePath" => reference,
      "task" => task,
      "combo" => optional_string(arguments["combo"]),
      "referenceVideoPaths" => Array(arguments["referenceVideoPaths"]).map(&:to_s).reject(&:empty?).first(OpenAIVideosAsyncAdapter::MAX_VIDEO_REFERENCES),
      "referenceAudioPath" => optional_string(arguments["referenceAudioPath"], 4_000),
      "mode" => task == "ref2va" ? "reference_to_video" : (reference ? "image_to_video" : "text_to_video"),
      "model" => requested_model.empty? ? settings["videoModel"] : requested_model,
      "duration" => integer_or(arguments["duration"], settings["duration"]),
      "width" => integer_or(arguments["width"], dimensions["width"]),
      "height" => integer_or(arguments["height"], dimensions["height"]),
      "inferenceSteps" => integer_or(arguments["inferenceSteps"], settings["inferenceSteps"]),
      "seed" => optional_integer(arguments["seed"]),
      "source" => optional_string(arguments["source"]) || "manual"
    }
  end

  # 带 dramaID 时成片默认宽高取剧的有效画幅（schemas/aspect-ratios-v1.json，
  # 与首尾帧出图同一档），否则竖版首帧会配出横版成片、被上游裁切或拉伸。
  # 独立 video.generate（不带 dramaID）沿用全局设置；显式 width/height 仍优先。
  def default_dimensions(arguments, settings)
    return settings unless arguments["dramaID"] && @dramas

    loaded = @dramas.get("id" => arguments["dramaID"])
    return settings unless loaded["ok"]

    AspectRatios.video_size(@dramas.effective_aspect(loaded["drama"]))
  rescue StandardError => error
    warn "video-studio: drama aspect lookup failed for #{arguments['dramaID']} (#{error.class}: #{error.message}); using settings"
    settings
  end

  def validate_request(request)
    return failure("empty_prompt", "A video prompt is required.") if request["prompt"].empty?
    unless request["duration"].between?(OpenAIVideosAsyncAdapter::MIN_DURATION, OpenAIVideosAsyncAdapter::MAX_DURATION)
      return failure("invalid_duration", "Duration must be between #{OpenAIVideosAsyncAdapter::MIN_DURATION} and #{OpenAIVideosAsyncAdapter::MAX_DURATION} seconds.")
    end
    return failure("invalid_width", "Width must be a multiple of 32 between 32 and 4096.") unless valid_dimension?(request["width"])
    return failure("invalid_height", "Height must be a multiple of 32 between 32 and 4096.") unless valid_dimension?(request["height"])
    ratio = request["width"].to_f / request["height"]
    return failure("invalid_aspect_ratio", "Aspect ratio must be between 1:4 and 4:1.") unless ratio.between?(0.25, 4.0)
    return failure("invalid_steps", "Inference steps must be between 1 and 100.") unless request["inferenceSteps"].between?(1, 100)
    nil
  end

  def valid_dimension?(value)
    value.between?(32, 4096) && (value % 32).zero?
  end

  # 镜头自己的时长，夹到 Provider 的 4–15 秒。镜头没写时长时交回全局默认。
  def shot_duration(shot)
    value = Integer(shot["duration"], exception: false)
    return nil unless value&.positive?

    value.clamp(OpenAIVideosAsyncAdapter::MIN_DURATION, OpenAIVideosAsyncAdapter::MAX_DURATION)
  end

  # 配音节奏下的请求时长（0.43.0-rc1，docs/decisions/0009-dialogue-paced-cut.md）：合成时尾巴会按台词说完的时刻剪掉，
  # 生成太长等于白付钱。有对白音轨：音轨长度 + 首留白 + 尾留白向上取整；没台词：分镜时长与无台词上限取短；
  # 留白镜头、画面节奏、台词还没配音：照分镜时长。下限 4 秒是 Provider 的合同。
  def paced_duration(drama, episode, shot, track)
    base = shot_duration(shot)
    settings = @pacing && @pacing.call(drama["id"], episode["id"])
    return base unless settings.is_a?(Hash) && EpisodePlan.pacing_for(settings, shot["id"]) == "dialogue"
    return base if EpisodePlan.hold_full?(shot)

    seconds = if !EpisodePlan.dialogue?(shot)
                [settings["pacingSilentMaxSeconds"].to_f, (base || settings["pacingSilentMaxSeconds"].to_f)].min
              elsif track && track["durationMs"].to_i.positive?
                (track["durationMs"].to_i + EpisodePlan::LEAD_IN_MS) / 1000.0 + settings["pacingTailSeconds"].to_f
              else
                return base
              end
    seconds.ceil.clamp(OpenAIVideosAsyncAdapter::MIN_DURATION, OpenAIVideosAsyncAdapter::MAX_DURATION)
  end

  # 带 shotID 的提交：找到镜头，按 Provider 能力编译参考包。返回失败时原样交回。
  def shot_linkage(arguments)
    shot_id = optional_string(arguments["shotID"])
    return { "ok" => true } if shot_id.nil?
    return failure("drama_service_unavailable", "This server was started without the drama store; omit shotID.") unless @dramas

    loaded = @dramas.get("id" => arguments["dramaID"])
    return loaded unless loaded["ok"]
    drama = loaded["drama"]
    episode = drama["episodes"].find { |entry| entry["id"] == arguments["episodeID"].to_s }
    return failure("episode_not_found", "Episode was not found.") unless episode
    shot = episode["shots"].find { |entry| entry["id"] == shot_id }
    return failure("shot_not_found", "Storyboard shot was not found.") unless shot
    if shot["draft"].is_a?(Hash) && !shot["draft"].empty?
      return failure("shot_has_draft", "This shot has an unadopted draft. Adopt or discard it before generating video.")
    end

    compiled = ReferencePackage.compile(drama, episode, shot, purpose: "video", capabilities: adapter_capabilities,
                                                             mode: optional_string(arguments["mode"]), media_root: @dramas.media_root, jobs: jobs_with_media,
                                                             directives: prevention_directives, extra_directives: extra_directives(arguments))
    return compiled unless compiled["ok"]

    frame = compiled["slots"].find { |slot| slot["semanticType"] == "frame" }
    audio = compiled["slots"].find { |slot| slot["semanticType"] == "audio" }
    videos = compiled["slots"].select { |slot| slot["semanticType"] == "video" }
    audio_path = nil
    if audio
      track = compiled["audioTrack"]
      if track["needsConcat"]
        # 多句对白拼成一条音轨，落在媒体目录里；dryRun 也拼，好让快照里有文件名。
        built = WavTools.concat(track["lines"].map { |line| line["filePath"] }, File.join(@dramas.media_root, WavTools.track_name(shot["id"])), gap_ms: ReferencePackage::DIALOGUE_GAP_MS)
        audio["fileName"] = File.basename(built["filePath"])
        audio["filePath"] = built["filePath"]
      end
      audio_path = audio["filePath"]
    end
    {
      "ok" => true, "dramaID" => drama["id"], "episodeID" => episode["id"], "shotID" => shot["id"],
      "mode" => compiled["mode"], "combo" => compiled["combo"], "generationMode" => compiled["mode"],
      "referenceSnapshot" => ReferencePackage.snapshot(compiled["slots"]),
      "promptIR" => compiled["promptIR"], "warnings" => compiled["warnings"], "dropped" => compiled["dropped"],
      "directives" => compiled["directives"], "directivesSkipped" => compiled["directivesSkipped"],
      "referenceImagePath" => frame && frame["filePath"], "referenceVideoPaths" => videos.map { |slot| slot["filePath"] }, "referenceAudioPath" => audio_path,
      "syntheticVoice" => !audio.nil?, "duration" => paced_duration(drama, episode, shot, compiled["audioTrack"]),
      # 照对白音轨生成（口型对的就是这几条配音）时记下每句用的配音候选（0.40.0-rc1）：之后台词重配了，
      # 这条成片的口型就对不上了，自动选片不再偏向它（VideoSelection.dialogue_stale）。
      "dialogueAudio" => audio ? Array(compiled.dig("audioTrack", "lines")).map { |line| { "lineID" => line["lineID"], "candidateID" => line["candidateID"] } } : nil
    }.reject { |key, value| key == "dialogueAudio" && value.nil? }
  rescue WavTools::Error => error
    failure("audio_track_failed", error.message)
  end

  # 预防句：经验库给的那份；没接经验库时 nil（参考包编译用内置默认）。
  def prevention_directives
    @directives&.call
  end

  # video.generate 的 extraDirectives：字符串或 {text, category, id}，最多 MAX_EXTRA_DIRECTIVES 句，
  # 每句不超过 MAX_DIRECTIVE_CHARS。否定句照收——这是调用方明确要加的话，规则写在工具说明里。
  MAX_EXTRA_DIRECTIVES = 8
  MAX_DIRECTIVE_CHARS = 500
  def extra_directives(arguments)
    Array(arguments["extraDirectives"]).first(MAX_EXTRA_DIRECTIVES).map do |entry|
      normalized = QARemediation.normalize(entry)
      normalized && normalized.merge("text" => normalized["text"][0, MAX_DIRECTIVE_CHARS])
    end.compact
  end

  # 视频后端的名字（记进经验库）：API 地址的主机名。
  def provider_name
    URI(@adapter_factory.call.base_url.to_s).host || "openai-videos-async"
  rescue StandardError
    "openai-videos-async"
  end
  public :provider_name

  def adapter_capabilities
    @adapter_factory.call.capabilities
  rescue VideoProviderError
    OpenAIVideosAsyncAdapter.capabilities
  end

  def prepare_job(draft_id, request, extra = {})
    draft = @store.find_job(draft_id)
    attributes = job_attributes(request).merge(extra).merge("qaEligible" => true, "state" => "submitting")
    if draft && draft["state"] == "draft"
      @store.update_job(draft["id"], attributes)
    else
      @store.add_job(attributes)
    end
  end

  def job_attributes(request)
    request.merge(
      "title" => request["title"].empty? ? request["prompt"][0, 72] : request["title"],
      "request" => request,
      "progress" => 0,
      "error" => nil,
      "outputURL" => nil,
      "outputPath" => nil,
      "downloadError" => nil,
      "ambiguousSubmission" => false
    )
  end

  def submit(job)
    result = @adapter_factory.call.create(job["request"])
    unless result["remoteID"]
      error = VideoProviderError.new("Video provider accepted the request without returning a task id.")
      return failed_submission(job, error)
    end
    updated = @store.update_job(job["id"], result.merge("ambiguousSubmission" => false))
    updated = download_if_ready(updated)
    { "ok" => true, "job" => updated }
  rescue VideoProviderError => error
    failed_submission(job, error)
  end

  def failed_submission(job, error)
    updated = @store.update_job(
      job["id"],
      "state" => "failed",
      "error" => error.as_json,
      "ambiguousSubmission" => error.ambiguous_submission
    )
    { "ok" => false, "job" => updated, "error" => error.as_json }
  end

  def refresh_job(job)
    next_poll = job["nextPollAt"].to_s
    return { "ok" => true, "job" => job } if !next_poll.empty? && Time.parse(next_poll) > Time.now
    result = @adapter_factory.call.retrieve(job["remoteID"])
    updated = @store.update_job(job["id"], result.merge("lastPollError" => nil, "nextPollAt" => nil))
    updated = download_if_ready(updated)
    { "ok" => true, "job" => updated }
  rescue VideoProviderError => error
    delay = if error.retry_after.to_s.match?(/\A\d+\z/)
              error.retry_after.to_i
            else
              begin
                [Time.httpdate(error.retry_after.to_s) - Time.now, 0].max
              rescue ArgumentError
                5
              end
            end
    updated = @store.update_job(job["id"], "lastPollError" => error.as_json,
                                "nextPollAt" => (Time.now + [delay, 5].max).utc.iso8601)
    { "ok" => false, "job" => updated, "error" => error.as_json }
  end

  def download_if_ready(job, force: false)
    return job unless job["state"] == "completed"
    return job if job["outputURL"].to_s.empty?
    return job unless force || @store.settings["autoDownload"]
    # 手动重下时要认「文件其实已经被用户删了」这种情况，否则只看 outputPath
    # 非空就返回成功，用户拿到的是一条指向不存在文件的路径。
    existing = job["outputPath"].to_s
    return job unless existing.empty? || (force && !File.exist?(existing))

    adapter = @adapter_factory.call
    url = job["outputURL"]
    path = begin
      adapter.download(url, job["remoteID"] || job["id"])
    rescue VideoProviderError => error
      # 直链是预签名地址，过期后上游回 403 之类；重新 GET 任务能拿到新链接（Hub 教程第 6 节）。
      raise error unless REFRESHABLE_DOWNLOAD_STATUSES.include?(error.http_status) && !job["remoteID"].to_s.empty?
      fresh = adapter.retrieve(job["remoteID"])
      raise error if fresh["outputURL"].to_s.empty? || fresh["outputURL"] == url
      @store.update_job(job["id"], "outputURL" => fresh["outputURL"])
      adapter.download(fresh["outputURL"], job["remoteID"] || job["id"])
    end
    @store.update_job(job["id"], "outputPath" => path, "downloadError" => nil)
  rescue VideoProviderError => error
    @store.update_job(job["id"], "downloadError" => error.as_json)
  rescue StandardError => error
    # 下载阶段抛了别的异常也要留痕。此前这类异常穿到 refresh_active 的兜底里，任务
    # 就停在「已完成、无文件、无错误」，没有任何入口能把它救回来。
    warn "video-studio: download failed for #{job['id']} (#{error.class}: #{error.message})"
    @store.update_job(job["id"], "downloadError" => { "message" => "Download failed: #{error.class}" })
  end

  def selected_reference(path)
    expanded = File.expand_path(path.to_s)
    return failure("invalid_reference", "The selected reference image is unavailable.") unless File.file?(expanded)
    extension = File.extname(expanded).downcase
    return failure("invalid_reference", "Choose a PNG, JPEG, or WebP image.") unless %w[.png .jpg .jpeg .webp].include?(extension)
    { "ok" => true, "path" => expanded }
  end

  def matches_filter?(job, filter)
    deleted = !job["deletedAt"].to_s.empty?
    return deleted if filter == "recycle"
    return false if deleted
    case filter
    when "draft" then job["state"] == "draft"
    when "active" then ACTIVE_STATES.include?(job["state"])
    when "completed" then job["state"] == "completed"
    when "failed" then job["state"] == "failed"
    else true
    end
  end

  def searchable_text(job)
    [job["title"], job["prompt"], job["model"], job["remoteID"], job["state"]].join("\n")
  end

  def counts(jobs)
    visible = jobs.reject { |job| !job["deletedAt"].to_s.empty? }
    {
      "all" => visible.length,
      "recycle" => jobs.length - visible.length,
      "draft" => visible.count { |job| job["state"] == "draft" },
      "active" => visible.count { |job| ACTIVE_STATES.include?(job["state"]) },
      "completed" => visible.count { |job| job["state"] == "completed" },
      "failed" => visible.count { |job| job["state"] == "failed" }
    }
  end

  def copy_optional_string(source, destination, key, limit)
    return unless source.key?(key)
    destination[key] = optional_string(source[key], limit)
  end

  def copy_integer(source, destination, key)
    destination[key] = Integer(source[key]) if source.key?(key)
  rescue ArgumentError, TypeError
    nil
  end

  def boolean(value)
    value == true || value.to_s == "true"
  end

  def integer_or(value, fallback)
    Integer(value)
  rescue ArgumentError, TypeError
    fallback.to_i
  end

  def optional_integer(value)
    token = value.to_s.strip
    token.empty? ? nil : Integer(token)
  rescue ArgumentError, TypeError
    nil
  end

  def clipped(value, limit)
    token = value.to_s
    return token.strip if token.bytesize <= limit
    token.byteslice(0, limit).scrub("").strip
  end

  def optional_string(value, limit = 1_000)
    token = clipped(value, limit)
    token.empty? ? nil : token
  end

  def failure(code, message, details = nil)
    error = { "code" => code, "message" => message }
    error["details"] = details if details
    { "ok" => false, "error" => error }
  end
end
