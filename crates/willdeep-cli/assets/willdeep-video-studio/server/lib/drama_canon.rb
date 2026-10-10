# frozen_string_literal: true

require "time"

# 全剧设定账本（canon，0.37.0-rc1）：一部剧里不许前后打架的那些东西。
#
# 《回村养鸭》并行写的 24 集里，鸭子数 486 / 479 / 474 混用、定金 50 / 90 两说，
# 策划写「白鸭成群」、剧本写「全是麻鸭」，审核要改的词散在四处——写作代理只看相邻
# 集梗概，看不到一本全剧的账（docs/lessons/2026-10-02-回村养鸭-生产复盘.md 2.3、4.3）。
#
# 账本存在剧上的 `canon` 字段，自带 rev 与历史（scope=canon），不走草稿：它是结构化
# 列表，改一处就是定一处。五块：
# - facts：随剧情变化的数（存栏、订单、欠款、价格），每个 fact 一条时间线
#   values [{fromDay | fromEpisode, value, note}]，按时间先后排；unit 与 keywords
#   供一致性检查在剧本里认出「说的是这个数」。
# - props：剧情道具与用途，可挂道具资产（assetID）。
# - visualRules：视觉口径，原样给写作者；可带 forbiddenPhrases 供检查。
# - bannedTerms：禁用词与替换词（replacement 为空表示整个不出现）。
# - allowedExtras：没有角色档案、但允许说台词的功能性人物（村民、伙计）。
#   说话人本身来自角色表，不在这里重复。
#
# 这里只管规范化与渲染；读写在 DramaService，检查在 lib/consistency_check.rb。
module DramaCanon
  LISTS = %w[facts props visualRules bannedTerms allowedExtras].freeze
  LIMITS = { "facts" => 60, "props" => 120, "visualRules" => 60, "bannedTerms" => 120, "allowedExtras" => 120 }.freeze
  MAX_VALUES = 60
  MAX_KEYWORDS = 12
  MAX_PHRASES = 20
  TEXT_LIMIT = 300
  NOTE_LIMIT = 500
  NAME_LIMIT = 60
  NORMALIZERS = {
    "facts" => :normalize_facts, "props" => :normalize_props, "visualRules" => :normalize_visual_rules,
    "bannedTerms" => :normalize_banned_terms, "allowedExtras" => :normalize_allowed_extras
  }.freeze

  # 校验不过时抛出，message 是给调用方看的原因，code 统一是 invalid_canon。
  class Invalid < StandardError; end

  module_function

  def empty
    { "facts" => [], "props" => [], "visualRules" => [], "bannedTerms" => [], "allowedExtras" => [], "rev" => 0, "updatedAt" => nil }
  end

  # 读出时补齐形状：旧剧没有 canon，页面和 Agent 拿到的仍是同一个结构。
  def read(drama)
    stored = drama.is_a?(Hash) && drama["canon"].is_a?(Hash) ? drama["canon"] : {}
    base = empty
    LISTS.each { |key| base[key] = stored[key].is_a?(Array) ? stored[key] : [] }
    base["rev"] = stored["rev"].to_i
    base["updatedAt"] = stored["updatedAt"]
    base
  end

  def blank?(canon)
    LISTS.all? { |key| Array(canon[key]).empty? }
  end

  def count(canon)
    LISTS.sum { |key| Array(canon[key]).length }
  end

  # 部分更新：只换调用方给了的那几块，每块整份替换。
  def merge(current, patch)
    raise Invalid, "canon must be an object with any of #{LISTS.join(', ')}." unless patch.is_a?(Hash)
    unknown = patch.keys - LISTS
    raise Invalid, "Unknown canon fields: #{unknown.join(', ')}." unless unknown.empty?

    merged = read("canon" => current)
    LISTS.each do |key|
      next unless patch.key?(key)

      merged[key] = normalize_list(key, patch[key])
    end
    merged
  end

  # 历史快照只拍五块内容。
  def snapshot(canon)
    LISTS.each_with_object({}) { |key, fields| fields[key] = Array(canon[key]) }
  end

  def normalize_list(key, value)
    raise Invalid, "#{key} must be an array." unless value.is_a?(Array)
    raise Invalid, "#{key} has more than #{LIMITS.fetch(key)} entries." if value.length > LIMITS.fetch(key)

    items = value.each_with_index.map { |entry, index| send(NORMALIZERS.fetch(key), entry, index) }.compact
    if key == "facts"
      keys = items.map { |fact| fact["key"] }
      duplicate = keys.find { |entry| keys.count(entry) > 1 }
      raise Invalid, "facts: duplicate key #{duplicate}." if duplicate
    end
    items
  end

  def normalize_facts(entry, index)
    raise Invalid, "facts[#{index}] must be an object." unless entry.is_a?(Hash)
    label = text(entry["label"], NAME_LIMIT)
    key = text(entry["key"], NAME_LIMIT)
    key = label if key.empty?
    raise Invalid, "facts[#{index}] needs a label." if label.empty? && key.empty?
    label = key if label.empty?
    values = entry["values"].nil? ? [] : entry["values"]
    raise Invalid, "facts[#{index}].values must be an array." unless values.is_a?(Array)
    raise Invalid, "facts[#{index}] has more than #{MAX_VALUES} values." if values.length > MAX_VALUES

    {
      "key" => key, "label" => label, "unit" => text(entry["unit"], 10),
      "keywords" => strings(entry["keywords"], MAX_KEYWORDS, 20),
      "values" => values.each_with_index.map { |value, position| normalize_value(value, index, position) }
    }
  end

  def normalize_value(entry, fact_index, position)
    where = "facts[#{fact_index}].values[#{position}]"
    raise Invalid, "#{where} must be an object." unless entry.is_a?(Hash)
    day = positive(entry["fromDay"], "#{where}.fromDay")
    episode = positive(entry["fromEpisode"], "#{where}.fromEpisode")
    raise Invalid, "#{where} takes fromDay or fromEpisode, not both." if day && episode
    raw = entry["value"]
    value = raw.is_a?(Numeric) ? raw : text(raw, 100)
    raise Invalid, "#{where} needs a value." if value.is_a?(String) && value.empty?

    result = { "value" => value, "note" => text(entry["note"], NOTE_LIMIT) }
    result["fromDay"] = day if day
    result["fromEpisode"] = episode if episode
    result
  end

  def normalize_props(entry, index)
    raise Invalid, "props[#{index}] must be an object." unless entry.is_a?(Hash)
    name = text(entry["name"], NAME_LIMIT)
    raise Invalid, "props[#{index}] needs a name." if name.empty?

    result = { "name" => name, "description" => text(entry["description"], TEXT_LIMIT), "usage" => text(entry["usage"], TEXT_LIMIT) }
    asset = text(entry["assetID"], 100)
    result["assetID"] = asset unless asset.empty?
    result
  end

  # 允许直接给一句字符串；带检查短语时写成 {rule, forbiddenPhrases}。
  def normalize_visual_rules(entry, index)
    entry = { "rule" => entry } if entry.is_a?(String)
    raise Invalid, "visualRules[#{index}] must be a string or an object." unless entry.is_a?(Hash)
    rule = text(entry["rule"] || entry["text"], TEXT_LIMIT)
    raise Invalid, "visualRules[#{index}] needs rule text." if rule.empty?

    { "rule" => rule, "forbiddenPhrases" => strings(entry["forbiddenPhrases"], MAX_PHRASES, 60) }
  end

  def normalize_banned_terms(entry, index)
    entry = { "term" => entry } if entry.is_a?(String)
    raise Invalid, "bannedTerms[#{index}] must be an object." unless entry.is_a?(Hash)
    term = text(entry["term"], NAME_LIMIT)
    raise Invalid, "bannedTerms[#{index}] needs a term." if term.empty?

    { "term" => term, "replacement" => text(entry["replacement"], NAME_LIMIT), "reason" => text(entry["reason"], TEXT_LIMIT) }
  end

  def normalize_allowed_extras(entry, index)
    raise Invalid, "allowedExtras[#{index}] must be a name." unless entry.is_a?(String)
    name = text(entry, NAME_LIMIT)
    name.empty? ? nil : name
  end

  # 写给模型的那一段（get_stage_context 的 context）。sections 选哪几块：首尾帧只要
  # 道具、视觉口径和禁用词，数字与说话人对画面没用。
  def brief(canon, sections: LISTS)
    return nil if blank?(canon)

    parts = []
    facts = Array(canon["facts"])
    if sections.include?("facts") && !facts.empty?
      lines = facts.map do |fact|
        timeline = Array(fact["values"]).map { |value| "#{since(value)}#{value['value']}#{fact['unit']}#{value['note'].to_s.empty? ? '' : "（#{value['note']}）"}" }
        "- #{fact['label']}：#{timeline.empty? ? '（还没定数）' : timeline.join('；')}"
      end
      parts << "关键数字（按时间线，不在表里的数不要自己编）：\n#{lines.join("\n")}"
    end
    props = Array(canon["props"])
    if sections.include?("props") && !props.empty?
      lines = props.map { |prop| "- #{prop['name']}#{prop['description'].to_s.empty? ? '' : "：#{prop['description']}"}#{prop['usage'].to_s.empty? ? '' : "（用途：#{prop['usage']}）"}" }
      parts << "道具：\n#{lines.join("\n")}"
    end
    rules = Array(canon["visualRules"])
    parts << "视觉口径：\n#{rules.map { |rule| "- #{rule['rule']}" }.join("\n")}" if sections.include?("visualRules") && !rules.empty?
    banned = Array(canon["bannedTerms"])
    if sections.include?("bannedTerms") && !banned.empty?
      lines = banned.map do |entry|
        replacement = entry["replacement"].to_s.empty? ? "（不出现）" : entry["replacement"]
        "- #{entry['term']} → #{replacement}#{entry['reason'].to_s.empty? ? '' : "（#{entry['reason']}）"}"
      end
      parts << "禁用词（不要写左边的词，改用右边的写法）：\n#{lines.join("\n")}"
    end
    extras = Array(canon["allowedExtras"])
    parts << "没有角色档案、但可以说台词的功能性人物：#{extras.join('、')}" if sections.include?("allowedExtras") && !extras.empty?
    return nil if parts.empty?

    "全剧设定账本（canon rev #{canon['rev'].to_i}，必须遵守；需要新的数字、道具或改口径时，不要自己编，在回复里写明要改什么，" \
      "由用户确认后用 drama.save_canon 更新）：\n#{parts.join("\n")}"
  end

  def since(value)
    return "第 #{value['fromDay']} 天起 " if value["fromDay"]
    return "第 #{value['fromEpisode']} 集起 " if value["fromEpisode"]

    "开篇 "
  end

  def text(value, limit)
    return "" if value.nil?

    value.to_s.strip[0, limit].to_s
  end

  def strings(value, max, limit)
    list = value.is_a?(String) ? value.split(/[,，、;；\n]/) : Array(value)
    list.map { |entry| text(entry, limit) }.reject(&:empty?).uniq.first(max)
  end

  def positive(value, where)
    return nil if value.nil? || value.to_s.strip.empty?

    number = Integer(value)
    raise Invalid, "#{where} must be a positive integer." unless number.positive?

    number
  rescue ArgumentError, TypeError
    raise Invalid, "#{where} must be a positive integer."
  end
end
