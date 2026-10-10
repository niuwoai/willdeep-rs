# frozen_string_literal: true

require "fileutils"
require "json"
require "securerandom"
require "time"

# 定稿版本的历史存档。
#
# AI 出稿以后不再等用户点一次「采用草稿」，正式内容会被直接覆盖——那就必须
# 有一条退路，否则「自动采用」等于「上一版没了」。这里存的是**被覆盖之前**
# 的那一份：每次定稿、每次手动保存、每次回滚，都先把当前正式内容拍一张快照
# 塞进来，用户随时能挑一版回去。
#
# 一部剧一个文件，不跟 dramas.json 混在一起：分集正文一条两万字，几十个版本
# 堆进主存档会让每一次「读全量、改、全量覆写」都拖着几兆走。
class HistoryStore
  VERSION = 1

  # 单个对象保留多少版。二十版够翻回一整轮反复打磨，再多的话列表本身就没法看。
  MAX_PER_OBJECT = 20

  # 一部剧总共保留多少条。二十四集加二十个角色各自留满也就四百出头。
  MAX_ENTRIES = 600

  # 列表里给前端看的摘要长度。正文整段在 read 时才取。
  PREVIEW_LIMIT = 160

  Unavailable = Class.new(StandardError)

  def initialize(root)
    @root = File.expand_path(root)
  end

  attr_reader :root

  # 记一版。fields 全空就不记：给一个还没写过任何内容的对象留一条空白版本，
  # 只会把列表撑长，回滚过去还会把用户手上的内容清成空的。
  def record(drama_id, scope:, object_id:, rev:, fields:)
    return nil unless writable_id?(drama_id) && writable_id?(object_id)
    return nil unless fields.is_a?(Hash)
    return nil if fields.values.all? { |value| value.to_s.strip.empty? }

    entry = {
      "id" => SecureRandom.uuid,
      "scope" => scope.to_s,
      "objectID" => object_id.to_s,
      "rev" => rev.to_i,
      "fields" => fields,
      "createdAt" => Time.now.utc.iso8601
    }
    with_lock(drama_id) do
      current = data(drama_id)
      current["entries"].unshift(entry)
      current["entries"] = trimmed(current["entries"])
      write(drama_id, current)
    end
    entry
  end

  # 一个对象的版本列表，新的在前。刻意只给摘要和字数：分集正文一条两万字，
  # 二十版整份回给页面就是四百万字符，而列表上真正要读的是「哪一版、什么时候、
  # 多长」。整段正文走 read。
  def list(drama_id, scope:, object_id:)
    entries(drama_id).select { |entry| entry["scope"] == scope.to_s && entry["objectID"] == object_id.to_s }
                     .map { |entry| summarize(entry) }
  end

  def read(drama_id, version_id)
    entries(drama_id).find { |entry| entry["id"] == version_id.to_s }
  end

  private

  def summarize(entry)
    fields = entry["fields"].is_a?(Hash) ? entry["fields"] : {}
    preview = {}
    lengths = {}
    fields.each do |key, value|
      text = value.is_a?(String) ? value : JSON.generate(value)
      preview[key] = text[0, PREVIEW_LIMIT].to_s
      lengths[key] = text.length
    end
    {
      "id" => entry["id"], "scope" => entry["scope"], "objectID" => entry["objectID"],
      "rev" => entry["rev"], "createdAt" => entry["createdAt"],
      "preview" => preview, "lengths" => lengths
    }
  end

  # 先按对象裁，再按总量裁。只按总量裁的话，一集剧本反复重写二十次就能把
  # 其他二十三集的历史全挤掉。
  def trimmed(entries)
    seen = Hash.new(0)
    kept = []
    entries.each do |entry|
      key = "#{entry['scope']}/#{entry['objectID']}"
      next if seen[key] >= MAX_PER_OBJECT
      seen[key] += 1
      kept << entry
    end
    kept.first(MAX_ENTRIES)
  end

  def entries(drama_id)
    return [] unless writable_id?(drama_id)
    data(drama_id)["entries"]
  end

  # 文件名直接来自调用方给的 dramaID。放行 UUID 那几类字符就够，别让一个
  # 带 ../ 的 ID 把写入指到插件目录外面去。
  def writable_id?(value)
    token = value.to_s
    !token.empty? && token.length <= 100 && token.match?(/\A[A-Za-z0-9_-]+\z/)
  end

  def path_for(drama_id)
    File.join(@root, "#{drama_id}.json")
  end

  def with_lock(drama_id)
    FileUtils.mkdir_p(@root)
    File.open("#{path_for(drama_id)}.lock", File::RDWR | File::CREAT, 0o600) do |handle|
      handle.flock(File::LOCK_EX)
      yield
    end
  end

  def data(drama_id)
    parsed = JSON.parse(File.read(path_for(drama_id), encoding: "UTF-8"))
    raise Unavailable, "history archive is not valid" unless parsed.is_a?(Hash) && parsed["entries"].is_a?(Array)
    { "version" => VERSION, "entries" => parsed["entries"] }
  rescue Errno::ENOENT
    { "version" => VERSION, "entries" => [] }
  rescue JSON::ParserError, EncodingError, SystemCallError, IOError => error
    raise Unavailable, "history archive is unreadable (#{error.class})"
  end

  def write(drama_id, payload)
    FileUtils.mkdir_p(@root)
    target = path_for(drama_id)
    temporary = "#{target}.#{Process.pid}.tmp"
    begin
      # 显式钉死编码，理由同 drama_store：LANG 缺失时默认外部编码是 US-ASCII，
      # 中文剧本会写不出去。
      File.write(temporary, JSON.pretty_generate(payload), perm: 0o600, encoding: "UTF-8")
      File.rename(temporary, target)
      temporary = nil
    ensure
      File.delete(temporary) if temporary && File.exist?(temporary)
    end
  end
end
