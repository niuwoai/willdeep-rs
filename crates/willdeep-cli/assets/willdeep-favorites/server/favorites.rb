#!/usr/bin/env ruby
# frozen_string_literal: true

# Favorites — WillDeep 收藏夹插件的 MCP stdio 服务端。
#
# 提供的工具：
#   - favorites.add     收藏一段文字（聊天里选中文字 → 气泡「收藏」会打到这里）
#   - favorites.list    返回收藏（支持关键词/标签过滤，附带标签直方图，供页面渲染）
#   - favorites.update  改备注、改标签、置顶/取消置顶
#   - favorites.remove  按 id 删除一条，并把删掉的整条原样回传（页面据此做撤销）
#   - favorites.restore 把整条塞回去（撤销删除，保留原 id 与创建时间）
#   - favorites.clear   清空（必须显式传 confirm=true，免得被 Agent 顺手调掉）
#
# 存储：单个 JSON 文件，默认 ~/Library/Application Support/WillDeep/plugin-data/favorites.json，
# 可用 WD_FAVORITES_FILE 覆盖（测试与沙箱用）。写入走「临时文件 + rename」，
# 中途被杀不会留下半截文件。
#
# 注意：宿主用系统 /usr/bin/ruby（2.6）启动本文件，所以这里只用 2.6 就有的语法与方法，
# 不要用 filter_map / tally / Hash#except 这些 2.7+ 的东西。

require "json"
require "base64"
require "fileutils"
require "securerandom"
require "time"
require "uri"
require_relative "rich_content"

$stdout.sync = true

# 宿主用 /usr/bin/ruby 起服务端时不带 UTF-8 locale，收藏内容多是中文，先钉死编码。
Encoding.default_external = Encoding::UTF_8
Encoding.default_internal = Encoding::UTF_8
$stdout.set_encoding(Encoding::UTF_8)
$stderr.set_encoding(Encoding::UTF_8)

STORE_VERSION = 5
MAX_ITEMS = 500
MAX_TEXT_BYTES = RichContent::MAX_TEXT_BYTES
MAX_NOTE_BYTES = 2_000
MAX_IMAGES = 8
MAX_IMAGE_BYTES = 8 * 1024 * 1024
MAX_IMAGE_TOTAL_BYTES = 24 * 1024 * 1024
MAX_TAGS = 8
MAX_TAG_CHARS = 24
DEFAULT_LIST_LIMIT = 500

def store_path
  ENV["WD_FAVORITES_FILE"] ||
    File.expand_path("~/Library/Application Support/WillDeep/plugin-data/favorites.json")
end

def media_root
  "#{store_path}.media"
end

def image_mime_type(raw)
  mime = raw.to_s.downcase.split(";", 2).first.strip
  %w[image/png image/jpeg image/webp image/gif].include?(mime) ? mime : nil
end

def image_extension(mime)
  { "image/png" => "png", "image/jpeg" => "jpg", "image/webp" => "webp", "image/gif" => "gif" }[mime]
end

def image_file_path(item_id, image_id, mime)
  File.join(media_root, item_id, "#{image_id}.#{image_extension(mime)}")
end

def normalize_images(raw, item_id)
  return [] unless raw.is_a?(Array)

  images = []
  raw.first(MAX_IMAGES).each do |image|
    next unless image.is_a?(Hash)
    mime = image_mime_type(image["mimeType"] || image["mime_type"])
    image_id = valid_id(image["id"]) || SecureRandom.uuid
    next if mime.nil?
    images << {
      "id" => image_id,
      "mimeType" => mime,
      "filename" => clip(image["filename"], 120).strip,
      "bytes" => image["bytes"].to_i
    }
  end
  images
end

# 单条收藏的规范形态。v1 只有 id/text/note/source/createdAt，缺的字段在这里补齐，
# 所以旧存储文件不需要单独的迁移步骤：读进来就是 v2。
def normalize_item(raw)
  return nil unless raw.is_a?(Hash)

  id = valid_id(raw["id"]) || SecureRandom.uuid
  images = normalize_images(raw["images"], id)
  content = RichContent.validate(raw["content"], images.map { |image| image["id"] })
  text = content ? RichContent.text(content["nodes"]).strip : raw["text"].to_s.strip
  raise RichContent::Invalid, "content limit exceeded" if text.bytesize > MAX_TEXT_BYTES
  return nil if text.empty? && images.empty?

  created = timestamp(raw["createdAt"])
  {
    "id" => id,
    "text" => text,
    "content" => content,
    "note" => clip(raw["note"], MAX_NOTE_BYTES).strip,
    "tags" => normalize_tags(raw["tags"]),
    "source" => clip(raw["source"], 200).strip,
    "sourceSessionID" => valid_id(raw["sourceSessionID"]),
    "sourceMessageID" => valid_id(raw["sourceMessageID"]),
    "sourceTurnID" => valid_id(raw["sourceTurnID"]),
    "pinned" => raw["pinned"] == true,
    "images" => images,
    "createdAt" => created,
    "updatedAt" => timestamp(raw["updatedAt"]) || created
  }
end

def write_images(item_id, images)
  total = 0
  prepared = []
  return nil unless (images || []).is_a?(Array)
  return nil if (images || []).length > MAX_IMAGES
  (images || []).each do |image|
    return nil unless image.is_a?(Hash)
    mime = image_mime_type(image["mime_type"] || image["mimeType"])
    return nil if mime.nil?
    encoded = image["data"].to_s.sub(%r{\Adata:[^;]+;base64,}, "")
    return nil if encoded.bytesize > (MAX_IMAGE_BYTES * 4 / 3) + 128
    begin
      bytes = Base64.strict_decode64(encoded)
    rescue ArgumentError
      return nil
    end
    return nil if bytes.empty? || bytes.bytesize > MAX_IMAGE_BYTES
    total += bytes.bytesize
    return nil if total > MAX_IMAGE_TOTAL_BYTES

    image_id = valid_id(image["id"]) || SecureRandom.uuid
    return nil unless safe_media_id?(image_id) && safe_media_id?(item_id)
    return nil if prepared.any? { |entry| entry[0]["id"] == image_id }
    return nil unless valid_image_bytes?(bytes, mime)
    prepared << [{
      "id" => image_id, "mimeType" => mime,
      "filename" => clip(image["filename"], 120).strip, "bytes" => bytes.bytesize
    }, bytes]
  end
  prepared.each do |metadata, bytes|
    image_id = metadata["id"]
    mime = metadata["mimeType"]
    path = image_file_path(item_id, image_id, mime)
    FileUtils.mkdir_p(File.dirname(path))
    File.binwrite(path, bytes)
  end
  prepared.map(&:first)
end

def safe_media_id?(id)
  id.is_a?(String) && id.match?(/\A[a-zA-Z0-9_-]{1,64}\z/)
end

def valid_image_bytes?(bytes, mime)
  case mime
  when "image/png" then bytes.start_with?("\x89PNG\r\n\x1a\n".b)
  when "image/jpeg" then bytes.start_with?("\xff\xd8\xff".b)
  when "image/gif" then bytes.start_with?("GIF87a", "GIF89a")
  when "image/webp" then bytes.start_with?("RIFF") && bytes.byteslice(8, 4) == "WEBP"
  else false
  end
end

def valid_id(raw)
  id = raw.to_s.strip
  id.empty? || id.length > 64 ? nil : id
end

def timestamp(raw)
  return nil if raw.nil?
  string = raw.to_s.strip
  return nil if string.empty?
  begin
    Time.parse(string).utc.iso8601
  rescue ArgumentError, TypeError
    nil
  end
end

def now_iso
  Time.now.utc.iso8601
end

def normalize_tags(raw)
  list = case raw
         when Array then raw
         when String then raw.split(/[,，\s]+/)
         else []
         end
  seen = []
  list.each do |value|
    tag = value.to_s.strip.gsub(/\s+/, " ")
    next if tag.empty?
    tag = tag[0, MAX_TAG_CHARS]
    next if seen.any? { |existing| existing.casecmp(tag).zero? }
    seen << tag
    break if seen.length >= MAX_TAGS
  end
  seen
end

def clip(value, limit)
  string = value.to_s
  return string if string.bytesize <= limit
  string.byteslice(0, limit).scrub("")
end

def load_items
  raw = File.read(store_path)
  data = JSON.parse(raw)
  items = data.is_a?(Hash) ? data["items"] : data
  return [] unless items.is_a?(Array)
  normalized = []
  items.each do |item|
    begin
      normalized << normalize_item(item)
    rescue RichContent::Invalid
      warn "favorites: invalid stored content; preserving plain text fallback"
      fallback = item.merge("content" => nil, "text" => clip(item["text"], MAX_TEXT_BYTES))
      normalized << normalize_item(fallback)
    end
  end
  normalized.compact
rescue Errno::ENOENT
  []
rescue JSON::ParserError, SystemCallError => error
  # 存储被写坏时按空列表继续跑：页面还能用，下一次写入会重建文件。
  warn "favorites: unreadable store (#{error.message}); starting empty"
  []
end

def save_items(items)
  kept = ordered(items).first(MAX_ITEMS)
  FileUtils.mkdir_p(File.dirname(store_path))
  payload = JSON.pretty_generate({ "version" => STORE_VERSION, "items" => kept })
  temporary = "#{store_path}.tmp"
  File.write(temporary, payload)
  File.rename(temporary, store_path)
  kept
end

# 置顶的永远在前，其余按更新时间倒序；同一时间戳时按创建时间兜底，
# 保证列表顺序是一个确定的全序，不会因为排序不稳定而抖动。
def ordered(items)
  items.each_with_index.sort_by do |item, index|
    [
      item["pinned"] ? 0 : 1,
      -sort_key(item["updatedAt"] || item["createdAt"]),
      -sort_key(item["createdAt"]),
      index
    ]
  end.map { |pair| pair[0] }
end

def sort_key(iso)
  return 0 if iso.nil?
  begin
    Time.parse(iso).to_f
  rescue ArgumentError, TypeError
    0
  end
end

def tag_histogram(items)
  counts = {}
  items.each do |item|
    item["tags"].each do |tag|
      key = counts.keys.find { |existing| existing.casecmp(tag).zero? } || tag
      counts[key] = (counts[key] || 0) + 1
    end
  end
  counts.sort_by { |tag, count| [-count, tag] }.map { |tag, count| { "name" => tag, "count" => count } }
end

def matches?(item, query, tag)
  unless tag.nil? || tag.empty?
    return false unless item["tags"].any? { |value| value.casecmp(tag).zero? }
  end
  return true if query.nil? || query.empty?
  needle = query.downcase
  haystack = [item["text"], item["note"], item["source"], item["tags"].join(" ")].join("\n").downcase
  haystack.include?(needle)
end

def add_item(arguments)
  item_id = SecureRandom.uuid
  image_inputs = arguments["images"] || []
  return { "ok" => false, "error" => "invalid images" } unless image_inputs.is_a?(Array) && image_inputs.length <= MAX_IMAGES
  return { "ok" => false, "error" => "invalid images" } unless image_inputs.all? { |image| image.is_a?(Hash) && (!image.key?("id") || safe_media_id?(image["id"])) }
  image_ids = image_inputs.map { |image| image.is_a?(Hash) && image["id"] }
  content = RichContent.validate(arguments["content"], image_ids)
  text = content ? RichContent.text(content["nodes"]).strip : arguments["text"].to_s.strip
  raise RichContent::Invalid, "content limit exceeded" if text.bytesize > MAX_TEXT_BYTES
  images = write_images(item_id, arguments["images"])
  return { "ok" => false, "error" => "invalid images" } if images.nil?
  candidate = normalize_item(
    "id" => item_id,
    "text" => arguments["text"],
    "content" => content,
    "note" => arguments["note"],
    "tags" => arguments["tags"],
    "source" => arguments["source"],
    "sourceSessionID" => arguments["source_session_id"],
    "sourceMessageID" => arguments["source_message_id"],
    "sourceTurnID" => arguments["source_turn_id"],
    "pinned" => arguments["pinned"],
    "images" => images,
    "createdAt" => now_iso
  )
  return { "ok" => false, "error" => "empty memo" } if candidate.nil?

  items = load_items
  # 同一段文字重复收藏不再堆一条新的：把老的顶上来，并告诉调用方这是重复。
  existing_index = candidate["content"].nil? && candidate["images"].empty? && !candidate["text"].empty? ? items.index { |item| item["content"].nil? && item["images"].empty? && item["text"] == candidate["text"] } : nil
  if existing_index
    existing = items[existing_index]
    existing["updatedAt"] = now_iso
    existing["note"] = candidate["note"] unless candidate["note"].empty?
    existing["tags"] = normalize_tags(existing["tags"] + candidate["tags"]) unless candidate["tags"].empty?
    existing["sourceSessionID"] = candidate["sourceSessionID"] unless candidate["sourceSessionID"].nil?
    existing["sourceMessageID"] = candidate["sourceMessageID"] unless candidate["sourceMessageID"].nil?
    existing["sourceTurnID"] = candidate["sourceTurnID"] unless candidate["sourceTurnID"].nil?
    existing["images"] = candidate["images"] unless candidate["images"].empty?
    saved = save_items(items)
    return { "ok" => true, "duplicate" => true, "item" => existing, "count" => saved.length }
  end

  saved = save_items([candidate] + items)
  { "ok" => true, "duplicate" => false, "item" => candidate, "count" => saved.length }
end

def restore_item(arguments)
  payload = arguments["item"]
  payload = arguments if payload.nil?
  candidate = normalize_item(payload)
  return { "ok" => false, "error" => "empty memo" } if candidate.nil?

  items = load_items.reject { |item| item["id"] == candidate["id"] }
  saved = save_items(items + [candidate])
  { "ok" => true, "item" => candidate, "count" => saved.length }
end

def read_image(arguments)
  item_id = arguments["item_id"].to_s
  image_id = arguments["image_id"].to_s
  return { "ok" => false, "error" => "invalid image id" } unless safe_media_id?(item_id) && safe_media_id?(image_id)
  item = load_items.find { |entry| entry["id"] == item_id }
  image = item && item["images"].find { |entry| entry["id"] == image_id }
  return { "ok" => false, "error" => "not found" } if image.nil?
  bytes = File.binread(image_file_path(item_id, image_id, image["mimeType"]))
  {
    "ok" => true,
    "itemId" => item_id,
    "imageId" => image_id,
    "mimeType" => image["mimeType"],
    "data" => "data:#{image["mimeType"]};base64,#{Base64.strict_encode64(bytes)}"
  }
rescue Errno::ENOENT, SystemCallError => error
  { "ok" => false, "error" => "image unavailable: #{error.message}" }
end

def update_item(arguments)
  id = arguments["id"].to_s
  return { "ok" => false, "error" => "missing id" } if id.empty?

  items = load_items
  index = items.index { |item| item["id"] == id }
  return { "ok" => false, "error" => "not found" } if index.nil?

  item = items[index]
  item["note"] = clip(arguments["note"], MAX_NOTE_BYTES).strip if arguments.key?("note")
  item["tags"] = normalize_tags(arguments["tags"]) if arguments.key?("tags")
  item["pinned"] = arguments["pinned"] == true || arguments["pinned"].to_s == "true" if arguments.key?("pinned")
  item["updatedAt"] = now_iso
  save_items(items)
  { "ok" => true, "item" => item }
end

def remove_item(arguments)
  id = arguments["id"].to_s
  return { "ok" => false, "error" => "missing id" } if id.empty?

  items = load_items
  removed = items.find { |item| item["id"] == id }
  remaining = items.reject { |item| item["id"] == id }
  saved = save_items(remaining)
  # 把整条回传，页面才能用它做撤销（保留原 id 与创建时间）。
  { "ok" => true, "removed" => removed.nil? ? 0 : 1, "item" => removed, "count" => saved.length }
end

def clear_items(arguments)
  confirmed = arguments["confirm"] == true || arguments["confirm"].to_s == "true"
  return { "ok" => false, "error" => "clear requires confirm=true" } unless confirmed

  count = load_items.length
  save_items([])
  { "ok" => true, "removed" => count, "count" => 0 }
end

def list_items(arguments)
  items = ordered(load_items)
  query = clip(arguments["query"], 200).strip
  tag = clip(arguments["tag"], MAX_TAG_CHARS).strip
  limit = arguments["limit"].to_i
  limit = DEFAULT_LIST_LIMIT if limit <= 0
  filtered = items.select { |item| matches?(item, query, tag) }
  {
    "ok" => true,
    "total" => items.length,
    "count" => filtered.length,
    "pinned" => items.count { |item| item["pinned"] },
    "tags" => tag_histogram(items),
    "items" => filtered.first([limit, MAX_ITEMS].min).map { |item| list_summary(item) },
    "storePath" => store_path
  }
end

def list_summary(item)
  summary = item.reject { |key, _| key == "content" }
  summary["text"] = clip(item["text"], 4_000)
  summary["hasMoreText"] = summary["text"] != item["text"]
  summary["richContent"] = !item["content"].nil?
  summary["contentPreview"] = RichContent.preview(item["content"]) if item["content"]
  summary
end

def read_content(arguments)
  item = load_items.find { |entry| entry["id"] == arguments["id"] }
  item ? { "ok" => true, "item" => item } : { "ok" => false, "error" => "not found" }
end

def open_link(arguments)
  url = RichContent.safe_url(arguments["url"])
  return { "ok" => false, "error" => "invalid link" } unless url
  return { "ok" => false, "error" => "link opening unavailable" } unless File.executable?("/usr/bin/open")
  # 独立参数，不经过 shell；只由正文中的显式点击调用。
  opened = system("/usr/bin/open", url, out: File::NULL, err: File::NULL)
  opened ? { "ok" => true } : { "ok" => false, "error" => "link opening failed" }
end

def call_tool(name, arguments)
  return { "ok" => false, "error" => "invalid arguments" } unless arguments.is_a?(Hash)
  case name
  when "favorites.add" then add_item(arguments)
  when "favorites.list" then list_items(arguments)
  when "favorites.update" then update_item(arguments)
  when "favorites.remove" then remove_item(arguments)
  when "favorites.restore" then restore_item(arguments)
  when "favorites.read_image" then read_image(arguments)
  when "favorites.read_content" then read_content(arguments)
  when "favorites.open_link" then open_link(arguments)
  when "favorites.clear" then clear_items(arguments)
  else { "ok" => false, "error" => "unknown tool #{name}" }
  end
rescue RichContent::Invalid => error
  { "ok" => false, "error" => error.message }
end

TOOLS = [
  {
    name: "favorites.add",
    description: "Save a text favorite or a personal memo with optional pasted images.",
    inputSchema: {
      type: "object",
      properties: {
        text: { type: "string", description: "The text or memo body to save." },
        content: { type: "object", description: "Optional v1 safe document tree: nodes of text, paragraphs, headings, emphasis, lists, quotes, code, HTTP(S) links, tables and imageId references. No HTML or CSS." },
        images: { type: "array", description: "Optional pasted images as base64 data URLs." },
        note: { type: "string", description: "Optional note." },
        tags: { type: "array", items: { type: "string" }, description: "Optional tags." },
        source: { type: "string", description: "Where the snippet came from." },
        source_session_id: { type: "string", description: "Stable source conversation id." },
        source_message_id: { type: "string", description: "Stable source message id." },
        source_turn_id: { type: "string", description: "Stable source user-turn id." }
      },
      required: []
    }
  },
  {
    name: "favorites.read_content",
    description: "Read the full text and safe rich document of a favorite on demand.",
    inputSchema: { type: "object", properties: { id: { type: "string" } }, required: ["id"] }
  },
  {
    name: "favorites.open_link",
    description: "Open an explicitly clicked HTTP(S) link in the default browser. Rejects credentials and other schemes.",
    inputSchema: { type: "object", properties: { url: { type: "string" } }, required: ["url"] }
  },
  {
    name: "favorites.read_image",
    description: "Read one persisted memo image as a data URL for display.",
    inputSchema: {
      type: "object",
      properties: {
        item_id: { type: "string", description: "Memo id." },
        image_id: { type: "string", description: "Image id." }
      },
      required: ["item_id", "image_id"]
    }
  },
  {
    name: "favorites.list",
    description: "Return saved favorites as JSON, pinned first, newest first.",
    inputSchema: {
      type: "object",
      properties: {
        query: { type: "string", description: "Filter by text, note, source or tag." },
        tag: { type: "string", description: "Keep only favorites carrying this tag." },
        limit: { type: "integer", description: "Maximum number of favorites to return." }
      }
    }
  },
  {
    name: "favorites.update",
    description: "Update the note, tags or pinned flag of one favorite.",
    inputSchema: {
      type: "object",
      properties: {
        id: { type: "string", description: "Favorite id." },
        note: { type: "string", description: "Replacement note." },
        tags: { type: "array", items: { type: "string" }, description: "Replacement tags." },
        pinned: { type: "boolean", description: "Pin or unpin the favorite." }
      },
      required: ["id"]
    }
  },
  {
    name: "favorites.remove",
    description: "Remove one favorite by id and return the removed record.",
    inputSchema: {
      type: "object",
      properties: { id: { type: "string", description: "Favorite id." } },
      required: ["id"]
    }
  },
  {
    name: "favorites.restore",
    description: "Put a previously removed favorite back, keeping its id and timestamps.",
    inputSchema: {
      type: "object",
      properties: { item: { type: "object", description: "The record returned by favorites.remove." } },
      required: ["item"]
    }
  },
  {
    name: "favorites.clear",
    description: "Remove every favorite. Requires confirm=true.",
    inputSchema: {
      type: "object",
      properties: { confirm: { type: "boolean", description: "Must be true." } },
      required: ["confirm"]
    }
  }
].freeze

ARGF.each_line do |line|
  request = nil
  max_request_bytes = MAX_IMAGE_TOTAL_BYTES * 4 / 3 + RichContent::MAX_BYTES + 1024 * 1024
  raise JSON::ParserError, "request too large" if line.bytesize > max_request_bytes
  request = JSON.parse(line)
  next unless request.is_a?(Hash) && request["id"]
  if request.key?("params") && !request["params"].is_a?(Hash)
    puts JSON.generate(jsonrpc: "2.0", id: request["id"], error: { code: -32602, message: "invalid params" })
    next
  end

  result = case request["method"]
           when "initialize"
             { protocolVersion: "2025-06-18",
               capabilities: { tools: {} },
               serverInfo: { name: "favorites", version: "2.3.0" } }
           when "tools/list"
             { tools: TOOLS }
           when "tools/call"
             arguments = request.dig("params", "arguments") || {}
             payload = call_tool(request.dig("params", "name"), arguments)
             { content: [{ type: "text", text: JSON.generate(payload) }] }
           else
             {}
           end

  puts JSON.generate(jsonrpc: "2.0", id: request["id"], result: result)
rescue JSON::ParserError => error
  warn "favorites: invalid JSON-RPC request"
  puts JSON.generate(jsonrpc: "2.0", id: nil, error: { code: -32700, message: "invalid JSON request" })
end
