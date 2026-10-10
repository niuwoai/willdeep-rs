# frozen_string_literal: true

require_relative "background_job_store"

# 后台任务执行器（0.36.0-rc1，决策见 docs/decisions/0003-background-jobs.md）。
#
# 出图、审核、配音一次要几十秒到几分钟。以前它们在请求循环里同步跑，stdio 主线程或
# HTTP 工作线程被占住，期间 drama.get 之类的读也排队（复盘 2.4）。现在 `async: true`
# 与按集批量工具只建任务、立即返回 jobID，活由这里的工作线程跑。
#
# - 线程而不是子进程：宿主代管的出图（willdeep/images/generate）与审核
#   （willdeep/ai/complete）只能经本进程的 HostBridge 发反向请求，子进程够不着宿主。
#   HostBridge 本来就允许多个线程同时发请求（按字符串 id 配对、stdout 加锁写），HTTP
#   工作线程早已这样用。
# - 存储不另加串行化：dramas.json、jobs.json 等每次读改写都在 flock 内，flock 认的是
#   打开的文件描述，同一进程的不同线程各自 open 也互斥；合成子进程一直在并发写同一份档。
# - 工作线程数有上限（默认 3，VIDEO_STUDIO_JOB_WORKERS 可调），排队中的任务也有上限，
#   超出返回 jobs_queue_full，不无限堆积。
# - 取消是协作式的：排队中的直接取消；运行中的在两项之间检查，正在进行的那一次出图 /
#   审核 / 视频提交不会被打断。
class JobRunner
  DEFAULT_WORKERS = 3
  MAX_WORKERS = 8
  MAX_QUEUED = 100
  # 只认「已经开始做或做完了」的同 requestID 任务；失败、中断、取消的允许用同一个
  # requestID 重新来过（逐项的 requestID 保证已经花过钱的那几项不会重复花）。
  REUSABLE_STATES = %w[queued running completed].freeze

  QueueFull = Class.new(StandardError)
  UnknownKind = Class.new(StandardError)

  # 交给任务处理块的上下文：报告逐项进度、检查取消。
  class Context
    attr_reader :job_id

    def initialize(runner, job_id)
      @runner = runner
      @job_id = job_id
    end

    def canceled?
      @runner.cancel_requested?(@job_id)
    end

    # 整个逐项列表（处理块开头重新核对计划后调用）。
    def items=(items)
      @runner.update_job(@job_id) do |job|
        job["items"] = items
        job["progress"] = JobRunner.progress_of(items)
      end
    end

    def items
      job = @runner.find(@job_id)
      job ? Array(job["items"]) : []
    end

    # 任务建立的时间（毫秒 ISO 8601）：批量选片时用来判断「批量开始之后有没有人另选」（0.38.0-rc3）。
    def started_at
      job = @runner.find(@job_id)
      job && (job["createdAt"] || job["startedAt"])
    end

    def update_item(index, changes)
      @runner.update_job(@job_id) do |job|
        items = Array(job["items"])
        next unless items[index].is_a?(Hash)

        items[index] = items[index].merge(changes)
        job["items"] = items
        job["progress"] = JobRunner.progress_of(items)
      end
    end

    def message=(text)
      @runner.update_job(@job_id) { |job| job["message"] = text.to_s[0, 500] }
    end

    # 可被取消打断的等待（视频轮询用）。
    def sleep(seconds)
      @runner.pause(seconds) { canceled? }
    end
  end

  ITEM_DONE_STATES = %w[completed failed skipped canceled].freeze

  def self.progress_of(items)
    total = items.length
    done = items.count { |item| item.is_a?(Hash) && ITEM_DONE_STATES.include?(item["state"]) }
    { "done" => done, "total" => total, "percent" => total.zero? ? 0 : (done * 100 / total) }
  end

  def initialize(store:, workers: DEFAULT_WORKERS, max_queued: MAX_QUEUED, logger: ->(line) { warn(line) })
    @store = store
    @worker_count = workers.to_i.clamp(1, MAX_WORKERS)
    @max_queued = max_queued
    @logger = logger
    @handlers = {}
    @queue = Queue.new
    @lock = Mutex.new
    @changed = ConditionVariable.new
    @canceled = {}
    @threads = []
    @started = false
  end

  attr_reader :store

  def register(kind, &handler)
    @handlers[kind.to_s] = handler
    self
  end

  def kinds
    @handlers.keys
  end

  # 进程启动时调一次：起工作线程，再把上一个进程留下的在途任务标成中断（工作线程只从
  # 本进程的队列取活，此时队列还是空的）。幂等。台账读不出来时只记一行，工具调用会把
  # 原因报给调用方。
  def start
    @lock.synchronize do
      return self if @started

      @started = true
    end
    @threads = Array.new(@worker_count) { |index| Thread.new { work_loop(index) } }
    begin
      recovered = @store.recover
      @logger.call("video-studio: marked #{recovered} background job(s) from a previous run as interrupted") if recovered.positive?
    rescue StandardError => error
      @logger.call("video-studio: background job recovery skipped (#{error.class}: #{error.message})")
    end
    self
  end

  def find(id)
    @store.find(id)
  end

  # 返回 [job, how]，how 为 :created、:deduplicated（同 requestID）或 :already_running
  # （同 dedupeKey 的任务还在跑）。
  def submit(kind:, arguments:, label: "", drama_id: nil, episode_id: nil, request_id: nil, dedupe_key: nil, items: nil)
    raise UnknownKind, kind unless @handlers.key?(kind)

    # 先起（并完成启动时的中断标记），再登记新任务：反过来会把刚登记的任务也标成中断。
    start
    jobs = @store.jobs
    token = request_id.to_s.strip[0, 200]
    unless token.empty?
      existing = jobs.find { |job| job["kind"] == kind && job["requestID"] == token && REUSABLE_STATES.include?(job["state"]) }
      return [existing, :deduplicated] if existing
    end
    if dedupe_key
      running = jobs.find { |job| job["dedupeKey"] == dedupe_key && BackgroundJobStore::ACTIVE_STATES.include?(job["state"]) }
      return [running, :already_running] if running
    end
    queued = jobs.count { |job| job["state"] == "queued" }
    raise QueueFull, "#{queued} background jobs are already waiting" if queued >= @max_queued

    initial = Array(items)
    job = @store.add(
      "kind" => kind, "label" => label.to_s[0, 300], "dramaID" => drama_id, "episodeID" => episode_id,
      "requestID" => token.empty? ? nil : token, "dedupeKey" => dedupe_key, "arguments" => arguments,
      "items" => initial, "progress" => self.class.progress_of(initial)
    )
    @queue << job["id"]
    notify
    [job, :created]
  end

  # 排队中的直接取消；运行中的记下请求，由处理块在下一项之前停下。别的进程在跑的任务
  # 只能记下请求（那个进程的处理块读同一份台账）。
  def cancel(id)
    job = @store.find(id)
    return nil unless job
    return job if BackgroundJobStore::TERMINAL_STATES.include?(job["state"])

    @lock.synchronize { @canceled[job["id"]] = true }
    updated = @store.update(job["id"]) do |record|
      record["cancelRequested"] = true
      if record["state"] == "queued"
        record["state"] = "canceled"
        record["finishedAt"] = Time.now.utc.iso8601(3)
        mark_open_items(record, "canceled")
      end
    end
    notify
    updated
  end

  def cancel_requested?(id)
    return true if @lock.synchronize { @canceled[id] }

    job = @store.find(id)
    job && job["cancelRequested"] == true
  end

  def update_job(id, &block)
    updated = @store.update(id, &block)
    notify
    updated
  end

  # 等到这些任务都结束或超时。返回 [jobs, done]。
  def wait(ids, timeout)
    deadline = monotonic + timeout.to_f
    loop do
      jobs = ids.map { |id| @store.find(id) }
      done = jobs.compact.all? { |job| BackgroundJobStore::TERMINAL_STATES.include?(job["state"]) }
      remaining = deadline - monotonic
      return [jobs, done] if done || remaining <= 0

      # 本进程的任务变化会唤醒；别的进程的任务只能靠定时重读。
      @lock.synchronize { @changed.wait(@lock, [remaining, 0.5].min) }
    end
  end

  # 等 seconds 秒，期间块返回真就提前结束。返回是否被打断。
  def pause(seconds)
    deadline = monotonic + seconds.to_f
    loop do
      return true if block_given? && yield

      remaining = deadline - monotonic
      return false if remaining <= 0

      Kernel.sleep([remaining, 0.25].min)
    end
  end

  def worker_count
    @worker_count
  end

  private

  def monotonic
    Process.clock_gettime(Process::CLOCK_MONOTONIC)
  end

  def notify
    @lock.synchronize { @changed.broadcast }
  end

  def work_loop(index)
    while (id = @queue.pop)
      begin
        run(id)
      rescue StandardError => error
        # 台账写不进去（Unavailable 等）也不能让工作线程死掉。
        @logger.call("video-studio: background worker #{index} could not run #{id} (#{error.class}: #{error.message})")
      end
    end
  end

  def run(id)
    job = @store.find(id)
    return unless job && job["state"] == "queued"

    if cancel_requested?(id)
      finish(id, "canceled", nil, nil)
      return
    end
    @store.update(id) do |record|
      record["state"] = "running"
      record["startedAt"] = Time.now.utc.iso8601(3)
    end
    notify
    context = Context.new(self, id)
    result = begin
      @handlers.fetch(job["kind"]).call(job["arguments"] || {}, context)
    rescue StandardError => error
      @logger.call("video-studio: background job #{id} (#{job['kind']}) raised #{error.class}: #{error.message}")
      Array(error.backtrace).first(5).each { |line| @logger.call("video-studio:   #{line}") }
      { "ok" => false, "error" => { "code" => "internal_error", "message" => "#{error.class}: #{error.message}"[0, 500] } }
    end
    result = { "ok" => false, "error" => { "code" => "internal_error", "message" => "The job returned no result." } } unless result.is_a?(Hash)
    # 处理块因取消提前停下时在结果里写 canceled: true；单项任务在取消前已做完的照常算完成。
    state = if result["canceled"] == true
              "canceled"
            elsif result["ok"] == false
              "failed"
            else
              "completed"
            end
    finish(id, state, result, result["ok"] == false ? result["error"] : nil)
  end

  def finish(id, state, result, error)
    @store.update(id) do |record|
      record["state"] = state
      record["result"] = result
      record["error"] = error
      record["finishedAt"] = Time.now.utc.iso8601(3)
      mark_open_items(record, state == "canceled" ? "canceled" : "skipped")
      record["progress"] = self.class.progress_of(Array(record["items"]))
    end
    @lock.synchronize { @canceled.delete(id) }
    notify
  end

  def mark_open_items(record, state)
    Array(record["items"]).each do |item|
      next unless item.is_a?(Hash) && %w[pending running waiting].include?(item["state"])

      item["state"] = state
      item["reason"] ||= state == "canceled" ? "canceled" : "not_reached"
    end
  end
end
