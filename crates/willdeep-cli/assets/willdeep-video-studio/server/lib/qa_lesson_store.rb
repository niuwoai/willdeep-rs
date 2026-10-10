# frozen_string_literal: true

require "fileutils"
require "json"
require "securerandom"
require "time"

# 经验库存档 qa-lessons.json（0.38.0-rc1，docs/decisions/0004-qa-lessons.md），与 dramas.json 同目录。
#
# 与 background-jobs.json 同一套写法：读改写整段在文件锁内，写入是临时文件 + rename；
# 读不出来就拒绝写，不把坏档当成空档覆写。
#
#   attempts：每一次自动补救的记录（类别、用的哪条补救句、视频后端与模型、镜头特征、
#             结果 resolved / unresolved、时间），最多 MAX_ATTEMPTS 条，最旧的先淘汰。
#   lessons： 人或 Agent 用 qa.save_lesson 加的经验（id 以 lesson_ 开头）。
#   overrides：内置经验（补救表与默认预防句）的开关，{ 经验 ID => { enabled } }。
class QALessonStore
  VERSION = 1
  MAX_ATTEMPTS = 5_000
  MAX_LESSONS = 500

  Unavailable = Class.new(StandardError)

  def initialize(path)
    @path = File.expand_path(path)
  end

  attr_reader :path

  def snapshot
    data
  end

  def record_attempts(entries)
    entries = Array(entries).select { |entry| entry.is_a?(Hash) }
    return [] if entries.empty?

    with_lock do
      current = data
      now = Time.now.utc.iso8601(3)
      # 幂等（0.38.0-rc2）：同一条重拍的同一类问题只记一次。补救被打断后重跑会取回已经审过的重拍，
      # 再记一次就会把解决率算歪。
      # 0.39.0-rc1：裁剪（action: trim）与重拍分开算——同一条重拍复审后又被裁剪，两条都要记。
      recorded = current["attempts"].map { |attempt| [attempt["jobID"], attempt["category"], attempt["action"]] if attempt["jobID"] }.compact
      entries = entries.reject { |entry| entry["jobID"] && recorded.include?([entry["jobID"], entry["category"], entry["action"]]) }
      next [] if entries.empty?

      stored = entries.map { |entry| { "id" => "qa_#{SecureRandom.uuid}", "at" => now }.merge(entry) }
      current["attempts"].concat(stored)
      overflow = current["attempts"].length - MAX_ATTEMPTS
      current["attempts"] = current["attempts"].drop(overflow) if overflow.positive?
      write(current)
      stored
    end
  end

  # 新建或更新一条手动经验；块拿到旧记录（新建时为 nil），返回新记录。
  def upsert_lesson(id)
    with_lock do
      current = data
      index = id ? current["lessons"].index { |lesson| lesson["id"] == id } : nil
      existing = index ? current["lessons"][index] : nil
      updated = yield(existing && deep_copy(existing))
      next nil unless updated

      now = Time.now.utc.iso8601(3)
      if index
        current["lessons"][index] = updated.merge("updatedAt" => now)
      else
        raise Unavailable, "too many lessons (#{MAX_LESSONS})" if current["lessons"].length >= MAX_LESSONS

        updated = { "id" => "lesson_#{SecureRandom.uuid}", "createdAt" => now }.merge(updated).merge("updatedAt" => now)
        current["lessons"] << updated
      end
      write(current)
      index ? current["lessons"][index] : updated
    end
  end

  def set_override(id, enabled)
    with_lock do
      current = data
      current["overrides"][id.to_s] = { "enabled" => enabled == true, "updatedAt" => Time.now.utc.iso8601(3) }
      write(current)
      current["overrides"][id.to_s]
    end
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
    raise Unavailable, "qa lesson store is not a valid archive" unless parsed.is_a?(Hash)

    {
      "version" => VERSION,
      "attempts" => Array(parsed["attempts"]).select { |entry| entry.is_a?(Hash) },
      "lessons" => Array(parsed["lessons"]).select { |entry| entry.is_a?(Hash) && !entry["id"].to_s.empty? },
      "overrides" => parsed["overrides"].is_a?(Hash) ? parsed["overrides"] : {}
    }
  rescue Errno::ENOENT
    { "version" => VERSION, "attempts" => [], "lessons" => [], "overrides" => {} }
  rescue JSON::ParserError, EncodingError, SystemCallError, IOError => error
    raise Unavailable, "qa lesson store is unreadable (#{error.class})"
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
