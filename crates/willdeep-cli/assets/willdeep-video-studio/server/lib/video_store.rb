# frozen_string_literal: true

require "fileutils"
require "json"
require "securerandom"
require "time"
require_relative "qa_remediation"
require_relative "clip_trim"

class VideoStore
  VERSION = 1
  # 超出上限时只淘汰可有可无的记录（失败、取消），最旧的先走；完成的、在途的、草稿一律
  # 保留——镜头的「已选成片」、整集合成与导出都靠这些记录找到片段。此前按新旧一刀切
  # 截到 500 条：2026-09-27 一部 24 集的剧经过几轮服务商故障与排队重试，500 条里 325 条
  # 是失败记录，早期完成的成片记录连同另一部剧的全部任务被挤掉，合成时找不到片段。
  MAX_JOBS = 2_000
  DISPOSABLE_STATES = %w[failed cancelled canceled].freeze

  # jobs 按新到旧排列。超出 limit 时从最旧的一端删可淘汰记录，删够为止；
  # 其余状态不删，所以结果可能仍多于 limit。
  def self.trim(jobs, limit = MAX_JOBS)
    overflow = jobs.length - limit
    return jobs if overflow <= 0

    doomed = {}
    jobs.reverse_each do |job|
      break if doomed.length >= overflow

      doomed[job.object_id] = true if DISPOSABLE_STATES.include?(job["state"].to_s)
    end
    jobs.reject { |job| doomed[job.object_id] }
  end

  DEFAULT_SETTINGS = {
    "assistantProviderID" => nil,
    "assistantModel" => nil,
    # 审核模型与界面语言由页面同步过来（0.23.0）。页面把审核模型存在宿主给页面
    # 的存储里，MCP 进程读不到；review.run 要用同一个模型、按同一种语言拼材料，
    # 算出来的内容指纹才和页面一致。
    "reviewProviderID" => nil,
    "reviewModel" => nil,
    "uiLocale" => "zh-Hans",
    "videoModel" => "MiniMax-H3",
    "duration" => 4,
    "width" => 832,
    "height" => 480,
    "inferenceSteps" => 20,
    "autoDownload" => true,
    "autoQA" => true,
    # 自动补救（0.38.0-rc1，docs/decisions/0004-qa-lessons.md）：成片质检发现这几类问题时，
    # episode.generate_videos 跑完自动加补救句重做，最多 remediateMaxRetries 次；都没过时
    # remediateAutoOverride 决定是否自动放行最好的那条（默认否，留给人决定）。
    "autoRemediate" => true,
    "remediateMaxRetries" => 2,
    "remediateCategories" => %w[scene_jump identity text_overlay],
    "remediateAutoOverride" => false,
    # 批量里逐镜并发（0.38.0-rc2，docs/decisions/0005-parallel-shot-pipelines.md）：整个插件进程里同时在跑的
    # 画面质检（宿主 ai.complete）与同时在生成的视频（批量提交的，含补救重拍）各最多几条。
    "qaConcurrency" => 2,
    "videoConcurrency" => 6,
    # 自动裁剪（0.39.0-rc1，docs/decisions/0007-clip-trim.md）：scene_jump 的切点之前那段够长、台词说得完时，
    # 补救先把成片裁到切点前（让出 trimMargin 秒），不重拍；裁完至少还剩 trimMinSeconds 秒。
    "autoTrim" => true,
    "trimMinSeconds" => ClipTrim::DEFAULT_MIN_SECONDS,
    "trimMargin" => ClipTrim::DEFAULT_MARGIN,
    # 候选图质检（0.40.0-rc1，docs/decisions/0008-candidate-image-qa.md）：新出的候选图逐张质检（imageAutoQA），
    # episode.generate_frames 自动选定推荐图（imageAutoSelect），一镜的候选全被拦截时带补救句重抽几轮（imageRetryOnBlock）。
    "imageAutoQA" => true,
    "imageAutoSelect" => true,
    "imageRetryOnBlock" => 1,
    # 专家席审稿（0.42.0-rc1，docs/decisions/0010-expert-panel-review.md）：进度与批量审核是否列出
    # 主框架与每一集的 panel 面。关掉后 review.run aspect=panel 仍可手动调。
    "panelReview" => true,
    # 宿主圆桌（0.43.0-rc1）：WillDeep 1.412.0-rc1 起专家席可以在宿主的圆桌页里开（看得见过程，调用更多）；
    # 关掉就走插件自己并行问专家的路。panelRounds 是宿主圆桌的讨论轮数（1～3）。
    "panelRoundtable" => true,
    "panelRounds" => 1
  }.freeze
  PANEL_ROUNDS_LIMIT = 3
  IMAGE_RETRY_ON_BLOCK_LIMIT = 2
  REMEDIATE_MAX_RETRIES_LIMIT = 5
  QA_CONCURRENCY_LIMIT = 4
  # 视频后端（Tsingfly Hub）单账号同时生成约 7～8 条、待处理任务上限约 100；一个进程最多占 20 条，
  # 页面与单个工具提交的不在这个名额里。
  VIDEO_CONCURRENCY_LIMIT = 20

  # 存档存在但读不出来。所有写入都是「读全量 → 改 → 全量覆写」，
  # 把读失败当成空存档会在下一次保存时抹掉全部任务记录。
  Unavailable = Class.new(StandardError)

  def initialize(path)
    @path = File.expand_path(path)
    @settings_lock = Mutex.new
    @settings_cache = nil
  end

  attr_reader :path

  def data
    parsed = JSON.parse(File.read(path, encoding: "UTF-8"))
    raise Unavailable, "video store is not a valid archive" unless parsed.is_a?(Hash)

    {
      "version" => VERSION,
      "settings" => normalize_settings(parsed["settings"]),
      "jobs" => normalize_jobs(parsed["jobs"])
    }
  rescue Errno::ENOENT
    empty_data
  rescue JSON::ParserError, EncodingError, SystemCallError, IOError => error
    raise Unavailable, "video store is unreadable (#{error.class})"
  end

  # 设置只是存档里一小块，但读它要解析整个 jobs.json（《回村养鸭》做到第 9 集时 11 MB、约 40 毫秒）。
  # drama.get_progress 每镜都要读一次设置，24 集 309 镜光这一项就是 13 秒（0.41.0-rc5）。
  # 按文件指纹（修改时间、大小、inode）缓存：任何一次写入（原子替换换 inode）都会让缓存失效。
  # 返回副本，调用方改了也不会污染缓存。
  def settings
    key = settings_fingerprint
    @settings_lock.synchronize do
      @settings_cache = [key, data["settings"]] unless key && @settings_cache && @settings_cache[0] == key
      Marshal.load(Marshal.dump(@settings_cache[1]))
    end
  end

  def settings_fingerprint
    stat = File.stat(path)
    [stat.mtime.to_r, stat.size, stat.ino]
  rescue SystemCallError
    nil
  end

  def update_settings(changes)
    with_lock do
      current = data
      current["settings"] = normalize_settings(current["settings"].merge(changes))
      write(current)
      current["settings"]
    end
  end

  def jobs
    data["jobs"]
  end

  def find_job(id)
    token = id.to_s.strip
    jobs.find { |job| job["id"] == token || job["remoteID"] == token }
  end

  def add_job(attributes)
    with_lock do
      current = data
      now = Time.now.utc.iso8601
      job = {
        "id" => SecureRandom.uuid,
        "remoteID" => nil,
        "provider" => "openai-videos-async",
        "state" => "draft",
        "progress" => 0,
        "createdAt" => now,
        "updatedAt" => now,
        "error" => nil,
        "outputURL" => nil,
        "outputPath" => nil,
        "downloadError" => nil,
        "ambiguousSubmission" => false
      }.merge(attributes)
      current["jobs"].unshift(job)
      current["jobs"] = self.class.trim(current["jobs"])
      write(current)
      job
    end
  end

  def update_job(id, changes)
    with_lock do
      current = data
      index = current["jobs"].index { |job| job["id"] == id || job["remoteID"] == id }
      next nil if index.nil?

      current["jobs"][index] = current["jobs"][index].merge(changes).merge("updatedAt" => Time.now.utc.iso8601)
      write(current)
      current["jobs"][index]
    end
  end

  def remove_job(id)
    with_lock do
      current = data
      removed = current["jobs"].find { |job| job["id"] == id || job["remoteID"] == id }
      next nil unless removed

      current["jobs"].reject! { |job| job["id"] == removed["id"] }
      write(current)
      removed
    end
  end

  private

  # 读-改-写整段互斥。写入本身是 tmp + rename 所以文件不会写坏，但两个并发的
  # 轮询会各拿一份陈旧快照，后写的那个把前一个的进度整段丢掉。
  def with_lock
    FileUtils.mkdir_p(File.dirname(path))
    File.open("#{path}.lock", File::RDWR | File::CREAT, 0o600) do |handle|
      handle.flock(File::LOCK_EX)
      yield
    end
  end

  def empty_data
    { "version" => VERSION, "settings" => DEFAULT_SETTINGS.dup, "jobs" => [] }
  end

  def normalize_settings(raw)
    value = raw.is_a?(Hash) ? DEFAULT_SETTINGS.merge(raw) : DEFAULT_SETTINGS.dup
    value["assistantProviderID"] = optional_string(value["assistantProviderID"])
    value["assistantModel"] = optional_string(value["assistantModel"])
    value["reviewProviderID"] = optional_string(value["reviewProviderID"])
    value["reviewModel"] = optional_string(value["reviewModel"])
    value["uiLocale"] = value["uiLocale"] == "en" ? "en" : "zh-Hans"
    value["videoModel"] = clean_string(value["videoModel"], "MiniMax-H3", 160)
    # Hub 教程的合同区间是 4~15 秒（2026-09-17 核实）；低于 4 秒不受支持。
    value["duration"] = bounded_integer(value["duration"], 4, 15, 4)
    value["width"] = dimension(value["width"], 832)
    value["height"] = dimension(value["height"], 480)
    value["inferenceSteps"] = bounded_integer(value["inferenceSteps"], 1, 100, 20)
    value["autoDownload"] = value["autoDownload"] != false
    value["autoQA"] = value["autoQA"] != false
    value["autoRemediate"] = value["autoRemediate"] != false
    value["remediateMaxRetries"] = bounded_integer(value["remediateMaxRetries"], -1_000, 1_000, 2).clamp(0, REMEDIATE_MAX_RETRIES_LIMIT)
    categories = value["remediateCategories"]
    value["remediateCategories"] = if categories.is_a?(Array)
                                     categories.map(&:to_s).select { |name| QARemediation.regenerate?(name) }.uniq
                                   else
                                     DEFAULT_SETTINGS["remediateCategories"].dup
                                   end
    value["remediateAutoOverride"] = value["remediateAutoOverride"] == true
    value["qaConcurrency"] = bounded_integer(value["qaConcurrency"], -1_000, 1_000, DEFAULT_SETTINGS["qaConcurrency"]).clamp(1, QA_CONCURRENCY_LIMIT)
    value["videoConcurrency"] = bounded_integer(value["videoConcurrency"], -1_000, 1_000, DEFAULT_SETTINGS["videoConcurrency"]).clamp(1, VIDEO_CONCURRENCY_LIMIT)
    value["autoTrim"] = value["autoTrim"] != false
    value["trimMinSeconds"] = ClipTrim.min_seconds(value)
    value["trimMargin"] = ClipTrim.margin(value)
    value["imageAutoQA"] = value["imageAutoQA"] != false
    value["imageAutoSelect"] = value["imageAutoSelect"] != false
    value["imageRetryOnBlock"] = bounded_integer(value["imageRetryOnBlock"], -1_000, 1_000, DEFAULT_SETTINGS["imageRetryOnBlock"])
                                 .clamp(0, IMAGE_RETRY_ON_BLOCK_LIMIT)
    value["panelReview"] = value["panelReview"] != false
    value["panelRoundtable"] = value["panelRoundtable"] != false
    value["panelRounds"] = bounded_integer(value["panelRounds"], -1_000, 1_000, DEFAULT_SETTINGS["panelRounds"]).clamp(1, PANEL_ROUNDS_LIMIT)
    value
  end

  def normalize_jobs(raw)
    return [] unless raw.is_a?(Array)

    self.class.trim(raw.select { |job| job.is_a?(Hash) && !job["id"].to_s.empty? })
  end

  def optional_string(value)
    token = value.to_s.strip[0, 300]
    token.empty? ? nil : token
  end

  def clean_string(value, fallback, limit)
    token = value.to_s.strip[0, limit]
    token.empty? ? fallback : token
  end

  def allowed_integer(value, allowed, fallback)
    number = Integer(value)
    allowed.include?(number) ? number : fallback
  rescue ArgumentError, TypeError
    fallback
  end

  def bounded_integer(value, minimum, maximum, fallback)
    number = Integer(value)
    number.between?(minimum, maximum) ? number : fallback
  rescue ArgumentError, TypeError
    fallback
  end

  def dimension(value, fallback)
    number = bounded_integer(value, 32, 4096, fallback)
    normalized = number - (number % 32)
    normalized < 32 ? fallback : normalized
  end

  def write(payload)
    FileUtils.mkdir_p(File.dirname(path))
    temporary = nil
    temporary = "#{path}.#{Process.pid}.tmp"
    # 显式钉死编码，理由同 DramaStore#write。
    File.write(temporary, JSON.pretty_generate(payload), perm: 0o600, encoding: "UTF-8")
    File.rename(temporary, path)
    temporary = nil
    payload
  ensure
    # 早先写的是 `defined?(temporary) && File.exist?(temporary)`：赋值语句一旦
    # 被解析器看见，defined? 就返回真，于是异常路径上会拿 nil 去调 File.exist?
    # 抛 TypeError，把真正的错误顶掉。
    File.delete(temporary) if temporary && File.exist?(temporary)
  end
end
