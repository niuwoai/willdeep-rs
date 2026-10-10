# frozen_string_literal: true

require "fileutils"
require "json"
require "open3"
require "rbconfig"
require "securerandom"
require "time"

require_relative "compose_store"
require_relative "episode_plan"
require_relative "ffmpeg_tool"

# 「成片」阶段的 MCP 工具（设计稿 docs/design/episode-compose.md 第 3 节）。
#
# 这里只建任务、起后台进程、读进度：宿主对 stdio 每个请求只等 startup_timeout_sec
# （10 秒），而一集的配音加转码要几十秒到几分钟。真正的活在 server/compose_worker.rb
# 里由 ComposePipeline 做。
class EpisodeComposer
  MUSIC_EXTENSIONS = %w[.mp3 .wav .m4a .aac .flac .aiff .aif].freeze
  MAX_MUSIC_BYTES = 200 * 1024 * 1024

  # worker_spec：后台进程需要的路径（dramaStore、historyDirectory、videoStore、
  # desktopMediaRoot、outputDirectory），由 video_studio.rb 按当前配置给出。
  def initialize(store:, dramas:, video_jobs:, media_host:, worker_spec:, tts_backend:, music_backend:,
                 worker_script: File.expand_path("../compose_worker.rb", __dir__), env: ENV)
    @store = store
    @dramas = dramas
    @video_jobs = video_jobs
    @media_host = media_host
    @worker_spec = worker_spec
    @tts_backend = tts_backend
    @music_backend = music_backend
    @worker_script = worker_script
    @env = env
  end

  def plan(arguments)
    context = load_episode(arguments)
    return context unless context["ok"]

    settings = @store.settings(context["drama"]["id"], context["episode"]["id"])
    built = EpisodePlan.build(drama: context["drama"], episode: context["episode"], jobs: @video_jobs.call,
                              settings: settings, media_root: @dramas.media_root)
    capabilities = self.capabilities
    blocking = gate(built, capabilities)
    shots = built["shots"].map do |shot|
      page = shot.merge("lines" => shot["lines"].map { |line| line.reject { |key, _| key == "filePath" } })
      page["dialogueTrack"] = shot["dialogueTrack"].reject { |key, _| key == "filePath" } if shot["dialogueTrack"]
      page
    end
    {
      "ok" => true,
      "episode" => { "id" => context["episode"]["id"], "order" => context["episode"]["order"], "title" => context["episode"]["title"] },
      "settings" => decorate_settings(settings),
      "capabilities" => capabilities,
      "shots" => shots,
      # blockers 与 blocking 是同一份（0.38.0-rc3 起两个名字都给）：episode.compose 拒绝合成时用的就是它。
      "blocking" => blocking,
      "blockers" => blocking,
      "composable" => blocking.empty?,
      "estimatedDurationMs" => built["estimatedDurationMs"],
      "latest" => latest_job(context["drama"]["id"], context["episode"]["id"])
    }
  end

  # 合成闸门（0.38.0-rc3）：episode.compose_plan 报出的 blockers 与 episode.compose 拒绝合成的依据是同一个函数、
  # 同一次计算——compose 先算一遍计划，再只看计划里的 blockers。以前两边口径一致但字段叫 blocking，
  # 读 blockers 的调用方拿到的是空的。
  def gate(built, capabilities)
    blockers = built["blocking"].dup
    blockers.unshift("code" => "ffmpeg_missing", "message" => "没有找到 ffmpeg，请先安装（brew install ffmpeg）。") unless capabilities["ffmpeg"]
    blockers
  end

  def capabilities
    tts = safe_call { @tts_backend.call }
    music = safe_call { @music_backend.call }
    {
      "ffmpeg" => FFmpegTool.available?(env: @env),
      "tts" => !tts.nil?,
      "ttsBackend" => tts && tts.name,
      "music" => !music.nil?,
      "musicBackend" => music && music.name,
      # 配置了但本机服务没起：页面据此提示「ACE-Step 没有启动」，而不是等生成失败。
      "musicReachable" => music && music.respond_to?(:reachable?) ? music.reachable? : nil
    }
  end

  def save_settings(arguments)
    context = load_episode(arguments)
    return context unless context["ok"]

    saved = @store.save_settings(context["drama"]["id"], context["episode"]["id"], arguments["settings"])
    { "ok" => true, "settings" => decorate_settings(saved) }
  end

  def compose(arguments)
    planned = plan(arguments)
    return planned unless planned["ok"]

    drama_id = arguments["dramaID"].to_s
    episode_id = arguments["episodeID"].to_s
    running = refresh(@store.active_job(drama_id, episode_id, "compose"), persist: true)
    return { "ok" => true, "job" => present(running), "alreadyRunning" => true } if running && running["state"] != "failed"
    unless planned["blockers"].empty?
      return failure("compose_blocked", planned["blockers"].map { |entry| entry["message"] }.join(" "),
                     "blockers" => planned["blockers"], "blocking" => planned["blockers"])
    end

    start("compose", drama_id, episode_id, arguments["requestID"])
  end

  def generate_music(arguments)
    context = load_episode(arguments)
    return context unless context["ok"]
    return failure("music_unavailable", "没有可用的背景音乐服务：请在插件设置里选择 ACE-Step 并确认本机服务已启动。") unless safe_call { @music_backend.call }
    return failure("ffmpeg_missing", "没有找到 ffmpeg，请先安装（brew install ffmpeg）。") unless FFmpegTool.available?(env: @env)

    drama_id = context["drama"]["id"]
    episode_id = context["episode"]["id"]
    running = refresh(@store.active_job(drama_id, episode_id, "music"), persist: true)
    return { "ok" => true, "job" => present(running), "alreadyRunning" => true } if running && running["state"] != "failed"

    @store.save_settings(drama_id, episode_id, "bgm" => { "source" => "acestep" })
    start("music", drama_id, episode_id, arguments["requestID"])
  end

  def status(arguments)
    jobs = @store.jobs
    jobs = jobs.select { |job| job["id"] == arguments["jobID"].to_s } unless arguments["jobID"].to_s.empty?
    jobs = jobs.select { |job| job["dramaID"] == arguments["dramaID"].to_s } unless arguments["dramaID"].to_s.empty?
    jobs = jobs.select { |job| job["episodeID"] == arguments["episodeID"].to_s } unless arguments["episodeID"].to_s.empty?
    { "ok" => true, "jobs" => jobs.first(50).map { |job| present(refresh(job)) } }
  end

  def cancel(arguments)
    job = @store.find_job(arguments["jobID"])
    return failure("job_not_found", "合成任务不存在。") unless job
    return { "ok" => true, "job" => present(job) } unless ComposeStore::ACTIVE_STATES.include?(job["state"])

    signal_group(job["pid"]) if job["pid"]
    updated = @store.update_job(job["id"], "state" => "canceled", "message" => "", "finishedAt" => Time.now.utc.iso8601)
    { "ok" => true, "job" => present(updated) }
  end

  def import_music(arguments)
    context = load_episode(arguments)
    return context unless context["ok"]

    source = music_source(arguments)
    return source unless source.is_a?(String)
    extension = File.extname(source).downcase
    return failure("unsupported_music", "只支持 mp3、wav、m4a、aac、flac、aiff 音频文件。") unless MUSIC_EXTENSIONS.include?(extension)
    return failure("music_too_large", "音乐文件超过 200 MB。") if File.size(source) > MAX_MUSIC_BYTES

    root = @dramas.media_root
    FileUtils.mkdir_p(root)
    name = "music-#{SecureRandom.hex(12)}#{extension}"
    FileUtils.cp(source, File.join(root, name))
    info = FFmpegTool.probe(File.join(root, name), env: @env) || {}
    settings = @store.record_music(context["drama"]["id"], context["episode"]["id"],
                                   "source" => "file", "fileName" => name, "durationMs" => info["durationMs"],
                                   "generatedFor" => nil, "importedFrom" => File.basename(source))
    { "ok" => true, "settings" => decorate_settings(settings) }
  rescue SystemCallError => error
    failure("music_import_failed", "音乐文件导入失败：#{error.class}")
  end

  def reveal_output(arguments)
    job = @store.find_job(arguments["jobID"])
    return failure("job_not_found", "合成任务不存在。") unless job
    path = job["outputPath"].to_s
    return failure("missing_output", "这一集的成片文件不见了。") unless File.file?(path)

    pid = Process.spawn("/usr/bin/open", "-R", path, out: File::NULL, err: File::NULL)
    Process.detach(pid)
    { "ok" => true, "path" => path }
  rescue SystemCallError => error
    failure("reveal_failed", "无法在访达中显示：#{error.class}")
  end

  private

  def load_episode(arguments)
    loaded = @dramas.get("id" => arguments["dramaID"])
    return loaded unless loaded["ok"]

    episode = Array(loaded["drama"]["episodes"]).find { |entry| entry["id"] == arguments["episodeID"].to_s }
    return failure("episode_not_found", "这一集不存在。") unless episode

    { "ok" => true, "drama" => loaded["drama"], "episode" => episode }
  end

  def start(kind, drama_id, episode_id, request_id)
    job = @store.create_job("kind" => kind, "dramaID" => drama_id, "episodeID" => episode_id,
                            "requestID" => request_id.to_s[0, 200], "step" => kind == "music" ? "music" : "dub")
    spec_path = File.join(File.dirname(@store.path), "compose-work", "#{job['id']}.json")
    FileUtils.mkdir_p(File.dirname(spec_path))
    File.write(spec_path, JSON.generate(@worker_spec.merge("jobID" => job["id"], "composeStore" => @store.path)))
    env = { "VIDEO_STUDIO_HOST_MODE" => @media_host.mode }
    pid = Process.spawn(env, RbConfig.ruby, @worker_script, spec_path,
                        in: File::NULL, out: File::NULL, err: [log_path(job), "w"], pgroup: true)
    Process.detach(pid)
    { "ok" => true, "job" => present(@store.update_job(job["id"], "pid" => pid)) }
  rescue SystemCallError => error
    updated = job && @store.update_job(job["id"], "state" => "failed", "error" => { "code" => "worker_start_failed", "message" => error.message })
    failure("worker_start_failed", "后台合成进程没能启动：#{error.class}", "job" => updated && present(updated))
  end

  def log_path(job)
    File.join(File.dirname(@store.path), "compose-work", "#{job['id']}.log")
  end

  # 后台进程已不在而任务还挂着 queued/running：按失败呈现，不让页面一直转圈。
  # 只读工具（compose_plan / compose_status）只改返回值不落盘；发起新任务时才写回存档。
  def refresh(job, persist: false)
    return job unless job && ComposeStore::ACTIVE_STATES.include?(job["state"]) && job["pid"]
    return job if alive?(job["pid"])

    job = @store.find_job(job["id"]) || job
    return job unless ComposeStore::ACTIVE_STATES.include?(job["state"])

    detail = File.file?(log_path(job)) ? File.read(log_path(job)).to_s.lines.last(3).join.strip : ""
    message = detail.empty? ? "后台合成进程意外退出。" : "后台合成进程意外退出：#{detail[0, 400]}"
    changes = { "state" => "failed", "finishedAt" => Time.now.utc.iso8601, "error" => { "code" => "worker_exited", "message" => message } }
    persist ? @store.update_job(job["id"], changes) : job.merge(changes)
  end

  def alive?(pid)
    Process.kill(0, pid.to_i)
    true
  rescue Errno::ESRCH
    false
  rescue Errno::EPERM
    true
  end

  def signal_group(pid)
    Process.kill("TERM", -pid.to_i)
  rescue Errno::ESRCH, Errno::EPERM
    begin
      Process.kill("TERM", pid.to_i)
    rescue Errno::ESRCH, Errno::EPERM
      nil
    end
  end

  def latest_job(drama_id, episode_id)
    candidates = @store.jobs.select { |job| job["dramaID"] == drama_id && job["episodeID"] == episode_id && job["kind"] == "compose" }
    active = candidates.find { |job| ComposeStore::ACTIVE_STATES.include?(job["state"]) }
    done = candidates.find { |job| job["state"] == "completed" && File.file?(job["outputPath"].to_s) }
    chosen = refresh(active) || done || candidates.first
    chosen && present(chosen).merge("previous" => active && done ? present(done) : nil).reject { |_, value| value.nil? }
  end

  # 存档里的播放地址是合成那一刻的宿主前缀，按当前宿主重写；pid、内部路径不交给页面。
  def present(job)
    return nil unless job

    shown = job.reject { |key, _| %w[pid mediaPath].include?(key) }
    @media_host.rewrite_urls(shown)
  end

  def decorate_settings(settings)
    bgm = settings["bgm"]
    name = bgm["fileName"].to_s
    return settings if name.empty? || name.include?("/")

    settings.merge("bgm" => bgm.merge("mediaURL" => @media_host.url_for(name)))
  end

  # 调用方已经选好文件时（Web 宿主：浏览器选、上传到宿主后把服务端路径放进
  # `path`）直接用它；没带才弹 macOS 选择框。扩展名与大小的检查对两条路一样。
  def music_source(arguments)
    path = arguments["path"].to_s.strip
    return pick_music_file if path.empty?

    File.file?(path) ? path : failure("music_not_found", "选中的音乐文件读不到。")
  end

  def pick_music_file
    override = @env["VIDEO_STUDIO_PICK_MUSIC"].to_s.strip
    return override unless override.empty?

    script = 'POSIX path of (choose file with prompt "选择背景音乐" of type {"public.audio"})'
    output, _error, status = Open3.capture3("/usr/bin/osascript", "-e", script)
    return failure("selection_cancelled", "没有选择音乐文件。") unless status.success?

    path = output.strip
    File.file?(path) ? path : failure("music_not_found", "选中的音乐文件读不到。")
  rescue Errno::ENOENT
    failure("picker_unavailable", "当前环境无法弹出选文件框。")
  end

  def safe_call
    yield
  rescue StandardError
    nil
  end

  def failure(code, message, extra = {})
    { "ok" => false, "error" => { "code" => code, "message" => message } }.merge(extra)
  end
end
