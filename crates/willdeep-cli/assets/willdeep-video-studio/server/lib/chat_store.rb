# frozen_string_literal: true

require "fileutils"
require "json"
require "securerandom"
require "time"

# 共创聊天的落盘。一部短剧一个文件，刻意不塞进 dramas.json：那个文件每次
# 保存都是全量覆写全部短剧，把逐条消息写进去等于每发一句话重写整库。
#
# 范围键是阶段（planning / characters / script / storyboard / frames）。消息
# 自己带 objectID 和发出时的 draftRev，这样「这句话是对着哪个角色、基于
# 哪一版草稿说的」在事后仍然查得出来。
class ChatStore
  VERSION = 1
  MAX_MESSAGES_PER_STAGE = 400
  MAX_CONTENT = 20_000
  STAGES = %w[planning characters script storyboard frames].freeze
  ROLES = %w[user assistant].freeze

  Unavailable = Class.new(StandardError)

  def initialize(directory)
    @directory = File.expand_path(directory)
  end

  attr_reader :directory

  def list(drama_id, stage)
    key = stage_key(stage)
    return [] unless key
    data(drama_id).fetch("stages", {}).fetch(key, [])
  end

  # 追加一条消息，返回落库后的那条（带 id 和 createdAt）。
  def append(drama_id, stage, message)
    key = stage_key(stage)
    raise ArgumentError, "unknown stage" unless key
    with_lock(drama_id) do
      current = data(drama_id)
      current["stages"][key] ||= []
      entry = normalize(message)
      current["stages"][key] << entry
      # 超长的历史从头截。共创靠的是最近几轮，早期的闲聊留着只会让文件
      # 越写越慢，而它已经不进任何一次请求的上下文了。
      overflow = current["stages"][key].length - MAX_MESSAGES_PER_STAGE
      current["stages"][key] = current["stages"][key].last(MAX_MESSAGES_PER_STAGE) if overflow.positive?
      write(drama_id, current)
      entry
    end
  end

  def clear(drama_id, stage)
    key = stage_key(stage)
    raise ArgumentError, "unknown stage" unless key
    with_lock(drama_id) do
      current = data(drama_id)
      current["stages"][key] = []
      write(drama_id, current)
      true
    end
  end

  def path_for(drama_id)
    File.join(@directory, "#{sanitized(drama_id)}.json")
  end

  private

  def stage_key(stage)
    token = stage.to_s.strip
    STAGES.include?(token) ? token : nil
  end

  # 短剧 ID 来自存档，理论上是 UUID，但它要进文件名，不该能写出 `../` 来。
  def sanitized(drama_id)
    token = drama_id.to_s.gsub(/[^A-Za-z0-9._-]/, "_")[0, 120]
    token.empty? ? "unknown" : token
  end

  def normalize(message)
    source = message.is_a?(Hash) ? message : {}
    role = ROLES.include?(source["role"].to_s) ? source["role"].to_s : "user"
    {
      "id" => SecureRandom.uuid,
      "role" => role,
      "content" => source["content"].to_s[0, MAX_CONTENT],
      "objectID" => source["objectID"].to_s[0, 200],
      "draftRev" => source["draftRev"].to_i,
      "createdAt" => Time.now.utc.iso8601
    }
  end

  def with_lock(drama_id)
    FileUtils.mkdir_p(@directory)
    File.open("#{path_for(drama_id)}.lock", File::RDWR | File::CREAT, 0o600) do |handle|
      handle.flock(File::LOCK_EX)
      yield
    end
  end

  # 读不出来就停手，理由和 DramaStore 一样：写是全量覆写，把读失败当成
  # 空档会在下一次追加时把整段聊天抹掉。
  def data(drama_id)
    parsed = JSON.parse(File.read(path_for(drama_id), encoding: "UTF-8"))
    raise Unavailable, "chat store is not a valid archive" unless parsed.is_a?(Hash) && parsed["stages"].is_a?(Hash)
    { "version" => VERSION, "stages" => parsed["stages"] }
  rescue Errno::ENOENT
    { "version" => VERSION, "stages" => {} }
  rescue JSON::ParserError, EncodingError, SystemCallError, IOError => error
    raise Unavailable, "chat store is unreadable (#{error.class})"
  end

  def write(drama_id, payload)
    path = path_for(drama_id)
    FileUtils.mkdir_p(File.dirname(path))
    temporary = "#{path}.#{Process.pid}.tmp"
    File.write(temporary, JSON.pretty_generate(payload), perm: 0o600, encoding: "UTF-8")
    File.rename(temporary, path)
    temporary = nil
  ensure
    File.delete(temporary) if temporary && File.exist?(temporary)
  end
end
