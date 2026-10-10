# frozen_string_literal: true

require "fileutils"
require "json"
require "securerandom"
require "time"
require_relative "aspect_ratios"

class DramaStore
  VERSION = 1
  MAX_DRAMAS = 100

  # 存档存在但读不出来（半截文件、权限、磁盘故障）。这时候必须停手：
  # 所有写入都是「读全量 → 改 → 全量覆写」，把读失败当成空存档会在下一次
  # 保存时把用户全部短剧抹掉。
  Unavailable = Class.new(StandardError)

  def initialize(path)
    @path = File.expand_path(path)
  end

  attr_reader :path

  def list
    data["dramas"]
  end

  def find(id)
    list.find { |drama| drama["id"] == id.to_s }
  end

  def find_by_request_id(request_id)
    token = request_id.to_s
    return nil if token.empty?
    list.find { |drama| drama["requestID"] == token }
  end

  def create(plan, request_id: "")
    with_lock do
      current = data
      now = Time.now.utc.iso8601
      drama = {
        "id" => SecureRandom.uuid,
        "requestID" => request_id.to_s,
        "title" => text(plan["title"], 160, "未命名短剧"),
        "genre" => text(plan["genre"], 80, "剧情"),
        "format" => text(plan["format"], 80, "竖屏短剧"),
        "aspectRatio" => aspect_ratio(plan["aspectRatio"], plan["format"]),
        # 新剧按画幅表的真实比例出图出片；没有这个标记的旧剧 9:16 仍按 2:3。
        "aspectSpec" => AspectRatios::SPEC_VERSION,
        "episodeCount" => integer(plan["episodeCount"], 1, 100, 12),
        "episodeDurationSeconds" => integer(plan["episodeDurationSeconds"], 15, 600, 90),
        "logline" => text(plan["logline"], 1_200, ""),
        "coreConflict" => text(plan["coreConflict"], 1_200, ""),
        "audience" => text(plan["audience"], 300, ""),
        "tone" => text(plan["tone"], 300, ""),
        # 画面风格（0.32.0-rc1）：只写画面，原样进每条成片提示词。tone 是写给编剧的。
        "visualStyle" => text(plan["visualStyle"], 300, ""),
        "arc" => text(plan["arc"], 4_000, ""),
        "status" => "planning",
        "rev" => 0,
        "draftRev" => 0,
        "draft" => nil,
        "coverURL" => nil,
        "characters" => build_characters(plan),
        "episodes" => build_episodes(plan),
        # 资产（造型、场景、场景变体、道具、声音）与角色同档，见设计稿 4.1。
        "assets" => [],
        "createdAt" => now,
        "updatedAt" => now
      }
      current["dramas"].unshift(drama)
      current["dramas"] = current["dramas"].first(MAX_DRAMAS)
      write(current)
      drama
    end
  end

  def update(drama_id)
    with_lock do
      current = data
      index = current["dramas"].index { |entry| entry["id"] == drama_id.to_s }
      next nil unless index

      drama = deep_copy(current["dramas"][index])
      yield drama
      drama["updatedAt"] = Time.now.utc.iso8601
      current["dramas"][index] = drama
      write(current)
      drama
    end
  end

  private

  # 读-改-写整段互斥：`data` 在锁内读，`write` 在锁内写，所以并发保存不会
  # 各拿一份陈旧快照。跨调用（先 get 再 save）的覆盖不归它管，那一层靠
  # 对象自己的 rev 拦。
  def with_lock
    FileUtils.mkdir_p(File.dirname(path))
    File.open("#{path}.lock", File::RDWR | File::CREAT, 0o600) do |handle|
      handle.flock(File::LOCK_EX)
      yield
    end
  end

  def data
    parsed = JSON.parse(File.read(path, encoding: "UTF-8"))
    raise Unavailable, "drama store is not a valid archive" unless parsed.is_a?(Hash) && parsed["dramas"].is_a?(Array)
    { "version" => VERSION, "dramas" => parsed["dramas"].first(MAX_DRAMAS) }
  rescue Errno::ENOENT
    empty_data
  rescue JSON::ParserError, EncodingError, SystemCallError, IOError => error
    raise Unavailable, "drama store is unreadable (#{error.class})"
  end

  def empty_data
    { "version" => VERSION, "dramas" => [] }
  end

  def aspect_ratio(value, format)
    token = value.to_s.strip
    return token if AspectRatios::SELECTABLE_IDS.include?(token)
    format.to_s.include?("横") ? "16:9" : "9:16"
  end

  # 策划里带出来的角色骨架：只有名字和一句话定位，小传与视觉提示词留空，
  # 等角色阶段单独出第一稿。视觉提示词要求很具体（年龄、脸型、发型、体态、
  # 标志服饰、不可改变项），塞进策划那一轮会两边都写不细。
  def build_characters(plan)
    requested = plan["characters"]
    return [] unless requested.is_a?(Array)

    seen = {}
    requested.map do |source|
      next nil unless source.is_a?(Hash)
      name = text(source["name"], 120, "")
      next nil if name.empty?
      # 模型偶尔会把同一个角色写两遍，或者把「路人」这类泛称当角色。
      # 重名直接丢后一个，别在角色表里堆出两个同名条目。
      next nil if seen[name]
      seen[name] = true
      {
        "id" => SecureRandom.uuid,
        "name" => name,
        "description" => text(source["role"] || source["description"], 2_000, ""),
        "visualPrompt" => "",
        "identityVersion" => 1,
        "candidates" => [],
        "selectedCandidateID" => nil,
        "rev" => 0,
        "draftRev" => 0,
        "draft" => nil
      }
    end.compact.first(20)
  end

  def build_episodes(plan)
    requested = plan["episodes"]
    count = integer(plan["episodeCount"], 1, 100, 12)
    Array.new(count) do |index|
      source = requested.is_a?(Array) && requested[index].is_a?(Hash) ? requested[index] : {}
      {
        "id" => SecureRandom.uuid,
        "order" => index + 1,
        "title" => text(source["title"], 160, "第 #{index + 1} 集"),
        "summary" => text(source["summary"], 2_000, ""),
        "script" => text(source["script"], 20_000, ""),
        "status" => "draft",
        "rev" => 0,
        "draftRev" => 0,
        "draft" => nil,
        "shots" => []
      }
    end
  end

  def text(value, limit, fallback)
    token = value.to_s.strip[0, limit]
    token.empty? ? fallback : token
  end

  def integer(value, minimum, maximum, fallback)
    number = Integer(value)
    number.between?(minimum, maximum) ? number : fallback
  rescue ArgumentError, TypeError
    fallback
  end

  def deep_copy(value)
    JSON.parse(JSON.generate(value))
  end

  def write(payload)
    FileUtils.mkdir_p(File.dirname(path))
    temporary = nil
    temporary = "#{path}.#{Process.pid}.tmp"
    # 显式钉死编码。宿主入口会设 default_external，但直接 require 这个类的
    # 测试脚本不会；LANG 缺失时默认外部编码是 US-ASCII，中文剧名会写不出去。
    File.write(temporary, JSON.pretty_generate(payload), perm: 0o600, encoding: "UTF-8")
    File.rename(temporary, path)
    temporary = nil
  ensure
    # 早先写的是 `defined?(temporary) && File.exist?(temporary)`：赋值语句一旦
    # 被解析器看见，defined? 就返回真，于是异常路径上会拿 nil 去调 File.exist?
    # 抛 TypeError，把真正的错误顶掉。
    File.delete(temporary) if temporary && File.exist?(temporary)
  end
end
