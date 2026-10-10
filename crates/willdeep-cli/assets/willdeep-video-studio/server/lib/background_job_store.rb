# frozen_string_literal: true

require "fileutils"
require "json"
require "securerandom"
require "time"

# 后台任务台账（0.36.0-rc1，决策见 docs/decisions/0003-background-jobs.md）。
#
# 与 jobs.json（视频任务）、compose.json（合成任务）同一套写法：读改写整段在文件锁内，
# 写入是临时文件 + rename；读不出来就拒绝写，不把坏档当成空档覆写。
#
# 一条记录：
#   id（bg_ 前缀，与视频任务的 UUID 区分）、kind（image.generate / review.run /
#   voice.generate / episode.generate_frames / episode.dub / episode.generate_videos /
#   review.run_batch）、state、dramaID、episodeID、requestID、dedupeKey、label、
#   arguments（调用参数，不含 async）、items（逐项进度）、progress {done,total,percent}、
#   result、error、cancelRequested、ownerPID、createdAt / startedAt / finishedAt / updatedAt。
#
# ownerPID：哪个进程在跑它。宿主装新版时旧进程可能还活着（复盘 2.4），新进程启动时
# 只把「主人已经不在」的在途任务标成 interrupted，不去抢活着的进程手里的任务。
class BackgroundJobStore
  VERSION = 1
  # 超出上限时只淘汰已结束的记录，最旧的先走；在途的一律保留。
  MAX_JOBS = 500
  ACTIVE_STATES = %w[queued running].freeze
  TERMINAL_STATES = %w[completed failed canceled interrupted].freeze
  STATES = (ACTIVE_STATES + TERMINAL_STATES).freeze

  Unavailable = Class.new(StandardError)

  def initialize(path)
    @path = File.expand_path(path)
  end

  attr_reader :path

  def jobs
    data["jobs"]
  end

  def find(id)
    token = id.to_s
    return nil if token.empty?

    jobs.find { |job| job["id"] == token }
  end

  def add(attributes)
    with_lock do
      current = data
      now = Time.now.utc.iso8601(3)
      job = {
        "id" => "bg_#{SecureRandom.uuid}",
        "state" => "queued",
        "items" => [],
        "progress" => { "done" => 0, "total" => 0, "percent" => 0 },
        "result" => nil,
        "error" => nil,
        "cancelRequested" => false,
        "ownerPID" => Process.pid,
        "createdAt" => now,
        "updatedAt" => now,
        "startedAt" => nil,
        "finishedAt" => nil
      }.merge(attributes)
      current["jobs"].unshift(job)
      current["jobs"] = self.class.trim(current["jobs"])
      write(current)
      job
    end
  end

  # 在锁内取出一条记录的副本交给块修改，块的返回值不管；返回改后的记录，找不到返回 nil。
  def update(id)
    with_lock do
      current = data
      index = current["jobs"].index { |job| job["id"] == id.to_s }
      next nil unless index

      job = deep_copy(current["jobs"][index])
      yield job
      job["updatedAt"] = Time.now.utc.iso8601(3)
      current["jobs"][index] = job
      write(current)
      job
    end
  end

  # 进程启动时调用：主人已经不在的 queued / running 记录标成 interrupted，未完成的逐项
  # 标 skipped（reason server_restarted）。返回被标记的条数。
  def recover(alive: ->(pid) { self.class.process_alive?(pid) })
    # 还没有台账就什么都不碰（不建目录、不建锁文件）。
    return 0 unless File.exist?(path)

    with_lock do
      current = data
      now = Time.now.utc.iso8601(3)
      count = 0
      current["jobs"].each do |job|
        next unless ACTIVE_STATES.include?(job["state"])

        owner = job["ownerPID"].to_i
        next if owner.positive? && owner != Process.pid && alive.call(owner)

        count += 1
        job["state"] = "interrupted"
        job["error"] = { "code" => "server_restarted",
                         "message" => "The plugin server stopped before this job finished. Work already recorded is kept; call the same tool again to continue (finished items are skipped)." }
        Array(job["items"]).each do |item|
          next unless item.is_a?(Hash) && %w[pending running waiting].include?(item["state"])

          item["state"] = "skipped"
          item["reason"] = "server_restarted"
        end
        job["finishedAt"] = now
        job["updatedAt"] = now
      end
      write(current) if count.positive?
      count
    end
  end

  def self.process_alive?(pid)
    Process.kill(0, pid)
    true
  rescue Errno::ESRCH
    false
  rescue Errno::EPERM
    true
  end

  # jobs 按新到旧排列。超出 limit 时从最旧的一端删已结束的记录，删够为止。
  def self.trim(jobs, limit = MAX_JOBS)
    overflow = jobs.length - limit
    return jobs if overflow <= 0

    doomed = {}
    jobs.reverse_each do |job|
      break if doomed.length >= overflow

      doomed[job.object_id] = true if TERMINAL_STATES.include?(job["state"].to_s)
    end
    jobs.reject { |job| doomed[job.object_id] }
  end

  private

  def with_lock
    FileUtils.mkdir_p(File.dirname(path))
    File.open("#{path}.lock", File::RDWR | File::CREAT, 0o600) do |handle|
      handle.flock(File::LOCK_EX)
      yield
    end
  end

  def data
    parsed = JSON.parse(File.read(path, encoding: "UTF-8"))
    raise Unavailable, "background job store is not a valid archive" unless parsed.is_a?(Hash) && parsed["jobs"].is_a?(Array)

    { "version" => VERSION, "jobs" => parsed["jobs"].select { |job| job.is_a?(Hash) && !job["id"].to_s.empty? } }
  rescue Errno::ENOENT
    { "version" => VERSION, "jobs" => [] }
  rescue JSON::ParserError, EncodingError, SystemCallError, IOError => error
    raise Unavailable, "background job store is unreadable (#{error.class})"
  end

  def deep_copy(value)
    JSON.parse(JSON.generate(value))
  end

  def write(payload)
    FileUtils.mkdir_p(File.dirname(path))
    temporary = "#{path}.#{Process.pid}.#{Thread.current.object_id}.tmp"
    File.write(temporary, JSON.pretty_generate(payload), perm: 0o600, encoding: "UTF-8")
    File.rename(temporary, path)
    temporary = nil
  ensure
    File.delete(temporary) if temporary && File.exist?(temporary)
  end
end
