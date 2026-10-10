# frozen_string_literal: true

require "fileutils"
require "json"
require "securerandom"
require "time"

# 分集成片的存档：每集的合成设置与合成任务（设计稿 docs/design/episode-compose.md 第 2 节）。
#
# MCP 主进程与后台合成进程都会读改写这份文件，所以每次读改写都在文件锁内完成，
# 写入走 tmp + rename，与 dramas.json / jobs.json 同一套写法。
class ComposeStore
  VERSION = 1
  MAX_JOBS = 200
  VOICE_SOURCES = %w[tts video].freeze
  BGM_SOURCES = %w[none acestep file].freeze
  ACTIVE_STATES = %w[queued running].freeze
  DEFAULT_ORIGINAL_VOLUME = 0.3
  DEFAULT_BGM_VOLUME = 0.18
  MAX_PROMPT_LENGTH = 1_000
  # 节奏来源（0.43.0-rc1，docs/decisions/0009-dialogue-paced-cut.md）：picture 整条画面用（现状）；
  # dialogue 台词说完加尾留白就切，没台词的镜头最多留 pacingSilentMaxSeconds。
  PACINGS = %w[picture dialogue].freeze
  DEFAULT_PACING_TAIL_SECONDS = 0.4
  PACING_TAIL_RANGE = (0.0..1.5).freeze
  DEFAULT_PACING_SILENT_MAX_SECONDS = 2.0
  PACING_SILENT_MAX_RANGE = (0.5..15.0).freeze

  DEFAULT_SETTINGS = {
    "voiceSource" => "tts",
    "shotVoiceSource" => {},
    "originalVolume" => DEFAULT_ORIGINAL_VOLUME,
    "pacing" => "picture",
    "shotPacing" => {},
    "pacingTailSeconds" => DEFAULT_PACING_TAIL_SECONDS,
    "pacingSilentMaxSeconds" => DEFAULT_PACING_SILENT_MAX_SECONDS,
    "bgm" => { "source" => "none", "prompt" => "", "volume" => DEFAULT_BGM_VOLUME }
  }.freeze

  Unavailable = Class.new(StandardError)

  def initialize(path)
    @path = path
  end

  attr_reader :path

  def self.key(drama_id, episode_id)
    "#{drama_id}:#{episode_id}"
  end

  def settings(drama_id, episode_id)
    entry = data["episodes"][self.class.key(drama_id, episode_id)]
    normalize_settings(entry.is_a?(Hash) ? entry["settings"] : nil)
  end

  # 只收设计稿列出的字段；未给的字段保持原值。返回保存后的完整设置。
  def save_settings(drama_id, episode_id, changes)
    mutate do |current|
      key = self.class.key(drama_id, episode_id)
      entry = current["episodes"][key].is_a?(Hash) ? current["episodes"][key] : {}
      merged = merge_settings(normalize_settings(entry["settings"]), changes.is_a?(Hash) ? changes : {})
      current["episodes"][key] = entry.merge("settings" => merged)
      merged
    end
  end

  # 后台生成音乐完成后写回曲目（不经页面的合法性裁剪，fileName 由服务端生成）。
  def record_music(drama_id, episode_id, fields)
    mutate do |current|
      key = self.class.key(drama_id, episode_id)
      entry = current["episodes"][key].is_a?(Hash) ? current["episodes"][key] : {}
      settings = normalize_settings(entry["settings"])
      settings["bgm"] = settings["bgm"].merge(fields)
      current["episodes"][key] = entry.merge("settings" => settings)
      settings
    end
  end

  def jobs
    data["jobs"]
  end

  def find_job(id)
    jobs.find { |job| job["id"] == id.to_s }
  end

  def active_job(drama_id, episode_id, kind)
    jobs.find do |job|
      job["dramaID"] == drama_id && job["episodeID"] == episode_id && job["kind"] == kind && ACTIVE_STATES.include?(job["state"])
    end
  end

  def create_job(fields)
    mutate do |current|
      job = {
        "id" => SecureRandom.uuid,
        "state" => "queued",
        "progress" => 0,
        "warnings" => [],
        "createdAt" => Time.now.utc.iso8601
      }.merge(fields)
      current["jobs"] = [job] + current["jobs"]
      current["jobs"] = current["jobs"].first(MAX_JOBS)
      job
    end
  end

  def update_job(id, changes)
    mutate do |current|
      index = current["jobs"].index { |job| job["id"] == id.to_s }
      next nil unless index

      current["jobs"][index] = current["jobs"][index].merge(changes)
    end
  end

  private

  def normalize_settings(raw)
    raw = raw.is_a?(Hash) ? raw : {}
    bgm_raw = raw["bgm"].is_a?(Hash) ? raw["bgm"] : {}
    bgm = DEFAULT_SETTINGS["bgm"].merge(bgm_raw.select { |key, _| %w[source prompt volume fileName durationMs generatedFor importedFrom].include?(key) })
    bgm["source"] = "none" unless BGM_SOURCES.include?(bgm["source"])
    bgm["prompt"] = bgm["prompt"].to_s[0, MAX_PROMPT_LENGTH]
    bgm["volume"] = clamp(bgm["volume"], DEFAULT_BGM_VOLUME)
    overrides = raw["shotVoiceSource"].is_a?(Hash) ? raw["shotVoiceSource"].select { |_, value| VOICE_SOURCES.include?(value) } : {}
    pacing_overrides = raw["shotPacing"].is_a?(Hash) ? raw["shotPacing"].select { |_, value| PACINGS.include?(value) } : {}
    {
      "voiceSource" => VOICE_SOURCES.include?(raw["voiceSource"]) ? raw["voiceSource"] : DEFAULT_SETTINGS["voiceSource"],
      "shotVoiceSource" => overrides,
      "originalVolume" => clamp(raw["originalVolume"], DEFAULT_ORIGINAL_VOLUME),
      "pacing" => PACINGS.include?(raw["pacing"]) ? raw["pacing"] : DEFAULT_SETTINGS["pacing"],
      "shotPacing" => pacing_overrides,
      "pacingTailSeconds" => clamp_range(raw["pacingTailSeconds"], DEFAULT_PACING_TAIL_SECONDS, PACING_TAIL_RANGE),
      "pacingSilentMaxSeconds" => clamp_range(raw["pacingSilentMaxSeconds"], DEFAULT_PACING_SILENT_MAX_SECONDS, PACING_SILENT_MAX_RANGE),
      "bgm" => bgm
    }
  end

  # 页面能改的字段：voiceSource、shotVoiceSource（值为 null / "" 表示跟随本集）、
  # originalVolume、pacing / shotPacing / pacingTailSeconds / pacingSilentMaxSeconds（0.43.0-rc1）、
  # bgm.source / prompt / volume。曲目文件名只由服务端写。
  def merge_settings(current, changes)
    merged = Marshal.load(Marshal.dump(current))
    merged["voiceSource"] = changes["voiceSource"] if VOICE_SOURCES.include?(changes["voiceSource"])
    merge_overrides(merged, "shotVoiceSource", changes["shotVoiceSource"], VOICE_SOURCES)
    merged["originalVolume"] = clamp(changes["originalVolume"], merged["originalVolume"]) if changes.key?("originalVolume")
    merged["pacing"] = changes["pacing"] if PACINGS.include?(changes["pacing"])
    merge_overrides(merged, "shotPacing", changes["shotPacing"], PACINGS)
    merged["pacingTailSeconds"] = clamp_range(changes["pacingTailSeconds"], merged["pacingTailSeconds"], PACING_TAIL_RANGE) if changes.key?("pacingTailSeconds")
    if changes.key?("pacingSilentMaxSeconds")
      merged["pacingSilentMaxSeconds"] = clamp_range(changes["pacingSilentMaxSeconds"], merged["pacingSilentMaxSeconds"], PACING_SILENT_MAX_RANGE)
    end
    if changes["bgm"].is_a?(Hash)
      bgm = changes["bgm"]
      merged["bgm"]["source"] = bgm["source"] if BGM_SOURCES.include?(bgm["source"])
      merged["bgm"]["prompt"] = bgm["prompt"].to_s[0, MAX_PROMPT_LENGTH] if bgm.key?("prompt")
      merged["bgm"]["volume"] = clamp(bgm["volume"], merged["bgm"]["volume"]) if bgm.key?("volume")
    end
    merged
  end

  def clamp(value, fallback)
    number = Float(value)
    return fallback unless number.finite?

    [[number, 0.0].max, 1.0].min.round(3)
  rescue ArgumentError, TypeError
    fallback
  end

  # 夹到给定区间（秒），两位小数；不是数就用 fallback。
  def clamp_range(value, fallback, range)
    number = Float(value)
    return fallback unless number.finite?

    number.clamp(range.begin, range.end).round(2)
  rescue ArgumentError, TypeError
    fallback
  end

  # 逐镜覆盖表：值合法就记，null / "" / 非法值表示这一镜改回跟随本集。
  def merge_overrides(merged, key, changes, allowed)
    return unless changes.is_a?(Hash)

    merged[key] = {} unless merged[key].is_a?(Hash)
    changes.each do |shot_id, value|
      next if shot_id.to_s.empty?

      if allowed.include?(value)
        merged[key][shot_id.to_s] = value
      else
        merged[key].delete(shot_id.to_s)
      end
    end
  end

  def mutate
    with_lock do
      current = data
      result = yield current
      write(current)
      result
    end
  end

  def data
    parsed = JSON.parse(File.read(path, encoding: "UTF-8"))
    raise Unavailable, "compose store is not a valid archive" unless parsed.is_a?(Hash)

    {
      "version" => VERSION,
      "episodes" => parsed["episodes"].is_a?(Hash) ? parsed["episodes"] : {},
      "jobs" => parsed["jobs"].is_a?(Array) ? parsed["jobs"].select { |job| job.is_a?(Hash) } : []
    }
  rescue Errno::ENOENT
    { "version" => VERSION, "episodes" => {}, "jobs" => [] }
  rescue JSON::ParserError, EncodingError, SystemCallError, IOError => error
    raise Unavailable, "compose store is unreadable (#{error.class})"
  end

  def write(value)
    FileUtils.mkdir_p(File.dirname(path))
    temporary = "#{path}.#{Process.pid}.#{SecureRandom.hex(4)}.tmp"
    File.write(temporary, JSON.pretty_generate(value))
    File.rename(temporary, path)
  ensure
    File.delete(temporary) if temporary && File.exist?(temporary)
  end

  def with_lock
    FileUtils.mkdir_p(File.dirname(path))
    File.open("#{path}.lock", File::RDWR | File::CREAT, 0o600) do |handle|
      handle.flock(File::LOCK_EX)
      yield
    end
  end
end
