# frozen_string_literal: true

require "securerandom"
require "time"

require_relative "media_host"

# 媒体引用：候选图、候选音频、成片镜像在存档里的样子，以及读出来时怎么补齐。
#
# 落盘只记 `fileName`：宿主只接受父目录恰好是媒体根的文件，页面的加载入口则跟
# 宿主走（macOS 是 `willdeep-plugin://bundle/__media__/<文件名>`，Web 宿主是同源
# `/plugin-media/<插件 ID>/<文件名>`，见 media_host.rb）。绝对路径与 URL 都是运行
# 时按媒体根派生出来的东西，写进存档只会在数据目录搬家、换宿主时失效。
#
# 旧候选记的是绝对路径 `filePath` 和宿主给的 `mediaURL`，原样保留、照常认；读出时
# 补上 `fileName`（父目录是媒体根时）。服务端所有读者都经 DramaService 拿剧，
# DramaService 在边界上统一调 `decorate_drama!`，模块内部不再各自拼路径。
module MediaRef
  module_function

  URL_PREFIX = "willdeep-plugin://bundle/__media__/"
  # 宿主对 __media__ 文件名的校验：单段、首字符字母数字、总长 128 以内。
  NAME_PATTERN = /\A[A-Za-z0-9][A-Za-z0-9._-]{0,127}\z/.freeze
  AUDIO_EXTENSIONS = %w[.wav .mp3 .m4a .aac .ogg .flac].freeze
  MEDIA_TYPES = %w[image audio video].freeze
  SOURCES = %w[generated imported extracted].freeze

  # 当前宿主由 server/video_studio.rb 在装配时注入（见 media_host.rb）：macOS
  # 宿主下就是老行为，Web 宿主下前缀、媒体根与文件镜像都跟着换。没注入时一律
  # 按 macOS 宿主算，单测与独立调用方不受影响。
  def configure(host)
    @host = host
  end

  def host
    @host
  end

  def url_prefix
    host ? host.url_prefix : URL_PREFIX
  end

  # 新建一条候选。file_path 必须已经过媒体根钳制。
  def build(file_path, root, prompt: "", model: "", media_type: nil, source: "generated", extra: {})
    name = File.basename(file_path.to_s)
    entry = {
      "id" => SecureRandom.uuid,
      "fileName" => name,
      "mediaType" => MEDIA_TYPES.include?(media_type.to_s) ? media_type.to_s : type_of(name),
      "prompt" => prompt.to_s[0, 8_000],
      "model" => model.to_s[0, 160],
      "source" => SOURCES.include?(source.to_s) ? source.to_s : "generated",
      "createdAt" => Time.now.utc.iso8601
    }
    extra.each { |key, value| entry[key.to_s] = value unless value.nil? }
    # 文件名不在媒体根一层时（旧调用方传了别处的路径），退回记绝对路径。
    entry["filePath"] = File.expand_path(file_path.to_s) unless File.dirname(File.expand_path(file_path.to_s)) == File.expand_path(root.to_s)
    entry
  end

  def type_of(name)
    AUDIO_EXTENSIONS.include?(File.extname(name.to_s).downcase) ? "audio" : (File.extname(name.to_s).downcase == ".mp4" ? "video" : "image")
  end

  # 文件名：有就用；没有就看旧 filePath 是否正好落在媒体根下。
  def file_name_of(entry, root)
    return nil unless entry.is_a?(Hash)
    name = entry["fileName"].to_s
    return name unless name.empty?

    path = entry["filePath"].to_s
    return nil if path.empty?
    expanded = File.expand_path(path)
    File.dirname(expanded) == File.expand_path(root.to_s) ? File.basename(expanded) : nil
  end

  def path(entry, root)
    return nil unless entry.is_a?(Hash)
    name = file_name_of(entry, root)
    if name && host&.web?
      resolved = host.path_for(name)
      return resolved if resolved
    end
    stored = entry["filePath"].to_s
    return File.expand_path(stored) unless stored.empty?
    name && !name.empty? ? File.join(File.expand_path(root.to_s), name) : nil
  end

  def url(entry, root = nil)
    return nil unless entry.is_a?(Hash)
    name = file_name_of(entry, root)
    return host.url_for(name) if host && name && !name.empty?

    stored = entry["mediaURL"].to_s
    return stored unless stored.empty?
    name && !name.empty? ? "#{URL_PREFIX}#{name}" : nil
  end

  # 读出时补齐：fileName / filePath / mediaURL / mediaType 一个都不缺。就地改，
  # 调用方拿到的都是刚从存档解析出来的副本。
  def decorate!(entry, root)
    return entry unless entry.is_a?(Hash)
    name = file_name_of(entry, root)
    entry["fileName"] = name if name && entry["fileName"].to_s.empty?
    resolved = path(entry, root)
    entry["filePath"] = resolved if resolved
    resolved_url = url(entry, root)
    entry["mediaURL"] = resolved_url if resolved_url
    entry["mediaType"] = type_of(entry["fileName"] || entry["filePath"]) if entry["mediaType"].to_s.empty?
    entry
  end

  def decorate_all!(entries, root)
    Array(entries).each { |entry| decorate!(entry, root) }
    entries
  end

  # 整部剧：角色定妆候选、分镜首尾帧候选、台词音频候选、资产候选。
  def decorate_drama!(drama, root)
    return drama unless drama.is_a?(Hash)
    Array(drama["characters"]).each { |character| decorate_all!(character["candidates"], root) }
    Array(drama["episodes"]).each do |episode|
      Array(episode["shots"]).each do |shot|
        decorate_all!(shot["startCandidates"], root)
        decorate_all!(shot["endCandidates"], root)
        Array(shot["dialogue"]).each do |line|
          next unless line.is_a?(Hash) && line["audio"].is_a?(Hash)
          decorate_all!(line["audio"]["candidates"], root)
        end
      end
    end
    Array(drama["assets"]).each { |asset| decorate_all!(asset["candidates"], root) }
    drama
  end

  # 选定的那一项；没选或文件已不在时返回 nil。
  def selected(node, root)
    return nil unless node.is_a?(Hash)
    entry = Array(node["candidates"]).find { |item| item["id"] == node["selectedCandidateID"] }
    return nil unless entry
    resolved = path(entry, root)
    resolved && File.file?(resolved) ? decorate!(entry, root) : nil
  end
end
