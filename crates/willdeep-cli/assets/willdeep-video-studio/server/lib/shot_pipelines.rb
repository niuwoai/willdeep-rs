# frozen_string_literal: true

require "time"
require_relative "host_bridge"

# 进程级并发名额（0.38.0-rc2）：同时在跑的画面质检（宿主 willdeep/ai/complete）与同时在生成的视频。
#
# 先来先得：排在前面的先拿名额（一集里靠前的镜头先提交），后来的在条件变量上等，不轮询。
# 容量从设置里读（video.settings qaConcurrency / videoConcurrency），读设置要解析 jobs.json，
# 所以缓存一秒；改了设置，一秒内对正在跑的批量生效。
class ConcurrencyLimiter
  WAIT_SLICE = 0.25
  CAPACITY_CACHE_SECONDS = 1

  attr_reader :name

  def initialize(name, capacity)
    @name = name
    @capacity = capacity.respond_to?(:call) ? capacity : -> { capacity }
    @lock = Mutex.new
    @changed = ConditionVariable.new
    @in_use = 0
    @peak = 0
    @queue = []
    @cached = nil
    @cached_at = nil
  end

  def capacity
    now = Process.clock_gettime(Process::CLOCK_MONOTONIC)
    return @cached if @cached && @cached_at && now - @cached_at < CAPACITY_CACHE_SECONDS

    value = begin
      Integer(@capacity.call)
    rescue StandardError
      1
    end
    @cached_at = now
    @cached = [value, 1].max
  end

  def in_use
    @lock.synchronize { @in_use }
  end

  # 本进程启动以来同时占用的最多名额（诊断用）。
  def peak
    @lock.synchronize { @peak }
  end

  def waiting
    @lock.synchronize { @queue.length }
  end

  # 排队拿一个名额。halt 返回真时放弃排队，返回 false。
  def acquire(halt = -> { false })
    ticket = Object.new
    @lock.synchronize { @queue << ticket }
    loop do
      limit = capacity
      granted = @lock.synchronize do
        if @queue.first.equal?(ticket) && @in_use < limit
          @queue.shift
          @in_use += 1
          @peak = @in_use if @in_use > @peak
          @changed.broadcast
          true
        else
          @changed.wait(@lock, WAIT_SLICE)
          false
        end
      end
      return true if granted
      next unless halt.call

      @lock.synchronize do
        @queue.delete(ticket)
        @changed.broadcast
      end
      return false
    end
  end

  def release
    @lock.synchronize do
      @in_use -= 1 if @in_use.positive?
      @changed.broadcast
    end
  end
end

# 一个批量任务里逐镜并发的流水线（0.38.0-rc2，docs/decisions/0005-parallel-shot-pipelines.md）。
#
# 《回村养鸭》第 1 集 11 镜：视频并行提交了，质检与补救却一镜一镜地串行——一镜要重拍时，整批等它
# 10～15 分钟再去审下一镜，70 分钟只审完 7 镜。现在每镜一条流水线（生成 → 质检 → 重拍 → 复审 →
# 选片），各走各的：
#
# - 每镜一个轻量线程跑自己的流水线（沿用原来逐镜的那段线性逻辑），JobRunner 的工作线程只做协调：
#   起线程、按间隔统一轮询本批所有在生成的视频、等它们都结束。流水线线程不睡觉轮询，而是在
#   `await_video` 上等轮询结果。
# - 花钱的地方都要先拿名额：画面质检拿 qa 名额，提交视频拿 video 名额（直到这条视频在远端结束才还）。
#   名额是进程级的（ConcurrencyLimiter），同一进程里所有批量共享。
# - 取消：排队拿名额的放弃排队，等视频的立刻返回 canceled（远端照常跑），正在进行的一次审核 / 提交不打断。
# - 停止：某镜碰到「后端没了」类错误（stop!），其余镜不再拿新名额（不再花钱），已经在生成的照常等完。
class ShotPipelines
  # 逐项 stage：页面与 Agent 看并发进度用。
  STAGES = %w[queued generating qa retake selecting done needs_human failed].freeze
  FAILED_VIDEO_STATES = %w[failed cancelled canceled].freeze
  CANCEL_CHECK_SECONDS = 0.5
  IDLE_SLICE = 0.1
  SUMMARY_LIMIT = 300

  # 设置里没有时的名额（与 VideoStore::DEFAULT_SETTINGS 一致）。
  FALLBACK_QA_CONCURRENCY = 2
  FALLBACK_VIDEO_CONCURRENCY = 6

  Limits = Struct.new(:qa, :video)

  # settings：返回插件设置的 lambda（qaConcurrency / videoConcurrency，校验在 VideoStore）。
  def self.default_limits(settings)
    Limits.new(ConcurrencyLimiter.new("qa", -> { settings.call["qaConcurrency"] || FALLBACK_QA_CONCURRENCY }),
               ConcurrencyLimiter.new("video", -> { settings.call["videoConcurrency"] || FALLBACK_VIDEO_CONCURRENCY }))
  end

  def self.now_iso
    Time.now.utc.iso8601(3)
  end

  def initialize(videos:, limits:, context:, poll_seconds:, poll_limit_seconds:, logger: ->(line) { warn(line) })
    @videos = videos
    @limits = limits
    @context = context
    @poll_seconds = poll_seconds.to_f.positive? ? poll_seconds.to_f : 15.0
    @poll_limit_seconds = poll_limit_seconds.to_f.positive? ? poll_limit_seconds.to_f : 3 * 60 * 60
    @logger = logger
    @lock = Mutex.new
    @watches = {}
    @sequence = 0
    @stopped = nil
    @canceled = false
    @checked_at = nil
  end

  attr_reader :stopped

  # entries 的每一项各起一个线程跑块（块拿到这一项）；本线程统一轮询视频，直到所有线程结束。
  # 块抛出的异常交给 on_crash（这一项记失败），不影响别的镜。返回 :done 或 :canceled。
  def run(entries, on_crash: ->(_entry, _error) {})
    timeout = Thread.current[HostBridge::TIMEOUT_KEY]
    threads = entries.map do |entry|
      Thread.new do
        Thread.current[HostBridge::TIMEOUT_KEY] = timeout
        begin
          yield entry
        rescue StandardError => error
          @logger.call("video-studio: shot pipeline raised #{error.class}: #{error.message}")
          Array(error.backtrace).first(5).each { |line| @logger.call("video-studio:   #{line}") }
          begin
            on_crash.call(entry, error)
          rescue StandardError => nested
            @logger.call("video-studio: shot pipeline crash not recorded (#{nested.class}: #{nested.message})")
          end
        end
      end
    end
    coordinate(threads)
    threads.each(&:join)
    canceled? ? :canceled : :done
  end

  # ---- 停止与取消 ----

  def stop!(code)
    @lock.synchronize { @stopped ||= code.to_s }
  end

  # 读台账判断取消：最多每半秒读一次（几十个线程同时问时不反复解析台账）。
  def canceled?
    return true if @canceled

    now = monotonic
    return false if @checked_at && now - @checked_at < CANCEL_CHECK_SECONDS

    @checked_at = now
    @canceled = true if @context.canceled?
    @canceled
  end

  def halted?
    canceled? || !@stopped.nil?
  end

  # 停下的原因（canceled / stopped），没停返回 nil。
  def halt_error
    return { "code" => "canceled", "message" => "The job was canceled." } if canceled?
    return { "code" => "stopped", "reason" => @stopped, "message" => "Skipped after another shot hit #{@stopped}." } if @stopped

    nil
  end

  def self.halt?(error)
    error.is_a?(Hash) && %w[canceled stopped].include?(error["code"])
  end

  # ---- 花钱的两类动作 ----

  # 拿一个质检名额跑块。停下时不跑，返回 :halted。
  def with_qa
    return :halted unless @limits.qa.acquire(-> { halted? })

    begin
      yield
    ensure
      @limits.qa.release
    end
  end

  # 拿一个视频名额提交，等到这条视频在远端结束（完成 / 失败）再还名额。返回 [远端结束时的任务, 错误]。
  # requestID 去重：同一 requestID 返回已有任务（中断后重跑不重复花钱），已完成的立即返回。
  # on_slot.call：拿到名额、提交之前。on_submitted.call(job, deduplicated)：提交成功后、开始等之前。
  def submit_video(arguments, on_slot: nil, on_submitted: nil, on_update: nil)
    halted = halt_error
    return [nil, halted] if halted
    return [nil, halt_error || { "code" => "canceled", "message" => "The job was canceled." }] unless @limits.video.acquire(-> { halted? })

    begin
      on_slot&.call
      submitted = @videos.generate(arguments)
      unless submitted["ok"] && submitted["job"].is_a?(Hash)
        error = compact_error(submitted["error"]).merge("stage" => "submit")
        error["jobID"] = submitted.dig("job", "id") if submitted.dig("job", "id")
        return [nil, error]
      end
      on_submitted&.call(submitted["job"], submitted["deduplicated"] == true)
      await_video(submitted["job"], on_update: on_update)
    ensure
      @limits.video.release
    end
  end

  # 等一条已经提交的视频结束（不占名额：它不是这一批提交的，或名额已经由 submit_video 拿着）。
  def await_video(job, on_update: nil)
    state = job["state"].to_s
    return [job, nil] if state == "completed"
    return [nil, video_error(job)] if FAILED_VIDEO_STATES.include?(state)
    return [nil, halt_error] if canceled?

    waiter = HostBridge::Waiter.new
    key = @lock.synchronize do
      @sequence += 1
      @watches[@sequence] = { job_id: job["id"], waiter: waiter, on_update: on_update, deadline: monotonic + @poll_limit_seconds,
                              last: [job["state"], job["progress"]] }
      @sequence
    end
    result = waiter.pop
    @lock.synchronize { @watches.delete(key) }
    result
  end

  # 完成的视频：没下载就下载，没做播放镜像就做（审核材料要求成片已镜像进媒体目录）。
  # 返回 [任务, 错误, 播放镜像的警告]。
  def materialize(job)
    if job["outputPath"].to_s.empty?
      downloaded = @videos.download("id" => job["id"])
      job = downloaded["job"] if downloaded["job"].is_a?(Hash)
      if job["outputPath"].to_s.empty?
        error = downloaded["error"] || { "code" => "download_failed", "message" => "The clip finished but could not be downloaded; retry with video.download." }
        return [nil, compact_error(error).merge("jobID" => job["id"]), nil]
      end
    end
    return [job, nil, nil] unless job["mediaFile"].to_s.empty?

    playback = @videos.prepare_playback("id" => job["id"])
    return [playback["job"], nil, nil] if playback["ok"] && playback["job"].is_a?(Hash)

    [job, nil, compact_error(playback["error"])]
  end

  def compact_error(error)
    return { "code" => "unknown", "message" => "Unknown error." } unless error.is_a?(Hash)

    compact = { "code" => error["code"], "message" => error["message"].to_s[0, SUMMARY_LIMIT] }
    compact["retryable"] = error["retryable"] unless error["retryable"].nil?
    compact
  end

  private

  def monotonic
    Process.clock_gettime(Process::CLOCK_MONOTONIC)
  end

  # 协调循环：每 poll_seconds 统一轮询一遍本批所有在等的视频；其间每 0.1 秒看一眼线程是否都结束了。
  def coordinate(threads)
    next_poll = monotonic + @poll_seconds
    loop do
      return if threads.none?(&:alive?)

      if canceled?
        resolve_all(halt_error)
      elsif monotonic >= next_poll
        poll_once
        next_poll = monotonic + @poll_seconds
      end
      Kernel.sleep(IDLE_SLICE)
    end
  end

  def poll_once
    watches = @lock.synchronize { @watches.to_a }
    watches.each do |key, watch|
      begin
        poll_watch(key, watch)
      rescue StandardError => error
        # 一次轮询出错不让整批停下：下一轮再试，总时长上限兜底。
        @logger.call("video-studio: polling #{watch[:job_id]} failed (#{error.class}: #{error.message})")
      end
    end
  end

  def poll_watch(key, watch)
    refreshed = @videos.refresh("id" => watch[:job_id])
    job = refreshed["job"]
    return resolve(key, [nil, compact_error(refreshed["error"]).merge("jobID" => watch[:job_id])]) unless job.is_a?(Hash)

    state = job["state"].to_s
    return resolve(key, [job, nil]) if state == "completed"
    return resolve(key, [nil, video_error(job)]) if FAILED_VIDEO_STATES.include?(state)

    if monotonic > watch[:deadline]
      return resolve(key, [nil, { "code" => "poll_timeout", "message" => "Stopped waiting for the clip; it keeps running remotely. Use video.refresh or the generation queue, or run the same tool again later.",
                                  "retryable" => true, "jobID" => watch[:job_id] }])
    end
    marker = [job["state"], job["progress"]]
    return if marker == watch[:last]

    watch[:last] = marker
    watch[:on_update]&.call(job)
  end

  def resolve(key, value)
    watch = @lock.synchronize { @watches.delete(key) }
    watch[:waiter] << value if watch
  end

  def resolve_all(error)
    keys = @lock.synchronize { @watches.keys }
    keys.each { |key| resolve(key, [nil, error.merge("message" => "Canceled while waiting for the clip; it keeps running remotely.")]) }
  end

  def video_error(job)
    compact_error(job["error"] || { "code" => "video_failed", "message" => "The video job failed." }).merge("jobID" => job["id"])
  end
end
