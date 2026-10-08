# frozen_string_literal: true

require "json"
require "uri"

# 持久化受限文档树，绝不接受可执行 HTML 或任意 DOM 属性。
module RichContent
  MAX_BYTES = 256 * 1024
  MAX_TEXT_BYTES = 100 * 1024
  MAX_NODES = 2_000
  MAX_DEPTH = 16
  MAX_CELLS = 1_000
  TYPES = %w[p h1 h2 h3 h4 h5 h6 strong em s u ul ol li blockquote pre code a table thead tbody tfoot tr th td br].freeze
  BLOCKS = %w[p h1 h2 h3 h4 h5 h6 li blockquote pre tr].freeze
  class Invalid < StandardError; end

  def self.safe_url(raw)
    value = raw.to_s.strip
    return nil if value.bytesize > 2_048 || value.match?(/[\x00-\x20\x7f]/)
    uri = URI.parse(value)
    return nil unless %w[http https].include?(uri.scheme) && uri.host && !uri.host.empty?
    return nil if uri.userinfo
    value
  rescue URI::InvalidURIError
    nil
  end

  def self.validate(raw, image_ids)
    return nil if raw.nil?
    raise Invalid, "invalid rich content" unless raw.is_a?(Hash) && raw["version"] == 1
    raise Invalid, "content limit exceeded" if JSON.generate(raw).bytesize > MAX_BYTES
    budget = { nodes: 0, cells: 0, text: 0 }
    { "version" => 1, "nodes" => validate_nodes(raw["nodes"], image_ids, budget, 0) }
  end

  def self.validate_nodes(nodes, image_ids, budget, depth)
    raise Invalid, "invalid rich content" unless nodes.is_a?(Array)
    raise Invalid, "content limit exceeded" if depth > MAX_DEPTH
    nodes.map do |node|
      budget[:nodes] += 1
      raise Invalid, "content limit exceeded" if budget[:nodes] > MAX_NODES
      validate_node(node, image_ids, budget, depth)
    end
  end

  def self.validate_node(node, image_ids, budget, depth)
    raise Invalid, "invalid rich content" unless node.is_a?(Hash)
    type = node["type"]
    if type == "text"
      raise Invalid, "invalid rich content" unless node["text"].is_a?(String)
      budget[:text] += node["text"].bytesize
      raise Invalid, "content limit exceeded" if budget[:text] > MAX_TEXT_BYTES
      return { "type" => type, "text" => node["text"] }
    end
    if type == "image"
      raise Invalid, "invalid rich content" unless node["imageId"].is_a?(String) && image_ids.include?(node["imageId"])
      return { "type" => type, "imageId" => node["imageId"] }
    end
    raise Invalid, "invalid rich content" unless TYPES.include?(type)
    budget[:cells] += 1 if %w[td th].include?(type)
    raise Invalid, "content limit exceeded" if budget[:cells] > MAX_CELLS
    clean = { "type" => type }
    if type == "a"
      url = safe_url(node["href"])
      clean["href"] = url if url
    end
    clean["children"] = validate_nodes(node["children"] || [], image_ids, budget, depth + 1) unless type == "br"
    clean
  end

  def self.text(nodes)
    nodes.map do |node|
      case node["type"]
      when "text" then node["text"]
      when "br" then "\n"
      when "image" then ""
      else
        body = text(node["children"] || [])
        body + (BLOCKS.include?(node["type"]) ? "\n" : (%w[td th].include?(node["type"]) ? "\t" : ""))
      end
    end.join
  end

  def self.preview(content)
    budget = { chars: 1_000, nodes: 100 }
    { "version" => 1, "nodes" => preview_nodes(content["nodes"], budget) }
  end

  def self.preview_nodes(nodes, budget)
    result = []
    nodes.each do |node|
      break if budget[:chars] <= 0 || budget[:nodes] <= 0
      budget[:nodes] -= 1
      clean = node.dup
      if node["type"] == "text"
        clean["text"] = node["text"][0, budget[:chars]]
        budget[:chars] -= clean["text"].length
      elsif node["children"]
        clean["children"] = preview_nodes(node["children"], budget)
      end
      result << clean
    end
    result
  end
end
