# frozen_string_literal: true

require "base64"
require_relative "media_ref"

# `media.read`：把媒体目录里的一张图或一段音频以 MCP 内容块交给调用方。
#
# 挑定妆图要看候选。WillDeep 主 Agent 有自己的识图能力，Claude Code 能按路径读图，
# 别的 harness 未必有这两样；MCP 的工具结果本身支持 image / audio 内容块，返回它
# 就不依赖 harness 提供任何文件读取工具（设计稿第 0 节）。
#
# 只认媒体根一层里的文件名，不认路径：与宿主的钳制同一口径。
module MediaReader
  module_function

  MAX_BYTES = 6 * 1024 * 1024
  MIME_TYPES = {
    ".png" => "image/png", ".jpg" => "image/jpeg", ".jpeg" => "image/jpeg", ".webp" => "image/webp",
    ".wav" => "audio/wav", ".mp3" => "audio/mpeg", ".m4a" => "audio/mp4"
  }.freeze

  # arguments: fileName，或 dramaID + candidateID（在整部剧的候选里找）。
  def read(drama_service, arguments)
    host = drama_service.respond_to?(:media_host) ? drama_service.media_host : nil
    root = File.expand_path(drama_service.media_root)
    name = arguments["fileName"].to_s.strip
    if name.empty? && !arguments["candidateID"].to_s.empty?
      loaded = drama_service.get("id" => arguments["dramaID"])
      return loaded unless loaded["ok"]
      entry = locate(loaded["drama"], arguments["candidateID"].to_s)
      return failure("candidate_not_found", "No candidate with that id in this drama.") unless entry
      name = entry["fileName"].to_s
    end
    return failure("invalid_media_name", "Pass a file name inside the plugin media directory, or dramaID plus candidateID.") unless MediaRef::NAME_PATTERN.match?(name)

    mime = MIME_TYPES[File.extname(name).downcase]
    return failure("unsupported_media_type", "Only PNG, JPEG, WebP, WAV, MP3 and M4A files can be returned inline.") unless mime

    # Web 宿主下文件可能只在 macOS 媒体根里：先按活跃根取，取不到再镜像过去。
    path = (host && host.path_for(name)) || File.join(root, name)
    return failure("media_not_found", "File does not exist in the plugin media directory.") unless File.file?(path) && !File.symlink?(path)
    size = File.size(path)
    return failure("media_too_large", "File is larger than #{MAX_BYTES / 1024 / 1024} MiB; open it by path instead: #{path}") if size > MAX_BYTES

    data = Base64.strict_encode64(File.binread(path))
    type = mime.start_with?("image/") ? "image" : "audio"
    { "ok" => true, "fileName" => name, "filePath" => path, "mimeType" => mime, "bytes" => size, "mediaURL" => "#{MediaRef.url_prefix}#{name}",
      "content" => [{ "type" => type, "data" => data, "mimeType" => mime }] }
  end

  def locate(drama, candidate_id)
    Array(drama["characters"]).each do |character|
      found = Array(character["candidates"]).find { |entry| entry["id"] == candidate_id }
      return found if found
    end
    Array(drama["assets"]).each do |asset|
      found = Array(asset["candidates"]).find { |entry| entry["id"] == candidate_id }
      return found if found
    end
    Array(drama["episodes"]).each do |episode|
      Array(episode["shots"]).each do |shot|
        found = (Array(shot["startCandidates"]) + Array(shot["endCandidates"])).find { |entry| entry["id"] == candidate_id }
        return found if found
        Array(shot["dialogue"]).each do |line|
          next unless line.is_a?(Hash) && line["audio"].is_a?(Hash)
          found = Array(line["audio"]["candidates"]).find { |entry| entry["id"] == candidate_id }
          return found if found
        end
      end
    end
    nil
  end

  def failure(code, message)
    { "ok" => false, "error" => { "code" => code, "message" => message } }
  end
end
