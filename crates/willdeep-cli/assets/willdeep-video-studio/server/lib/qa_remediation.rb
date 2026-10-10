# frozen_string_literal: true

require "json"

# 成片画面质检的「怎么补救」表（0.38.0-rc1，docs/decisions/0004-qa-lessons.md）。
#
# 表本身是 schemas/qa-remediations-v1.json：每个质检类别（scene_jump / identity / text_overlay /
# injury / blood / continuity / framing）对应重做时往 H3 提示词里加的正面句子、要不要自动重做、
# 最多重做几次；另有一组「预防」句子，默认写进每条单镜提示词（如一镜到底）。
#
# 只写正面描述：视频模型把提示词里提到的东西都画出来（「no cuts」的 cut 会被画成伤口，
# 见 video-qa.md 5.4 与 lessons 文档）。scripts/qa_remediation_test.rb 扫这张表里的否定词。
module QARemediation
  PATH = File.expand_path("../../schemas/qa-remediations-v1.json", __dir__)
  TABLE = JSON.parse(File.read(PATH, encoding: "UTF-8")).freeze
  CATEGORIES = TABLE["categories"].keys.freeze
  REGENERATE_CATEGORIES = CATEGORIES.select { |name| TABLE["categories"][name]["regenerate"] == true }.freeze
  ADVISORY_CATEGORIES = (CATEGORIES - REGENERATE_CATEGORIES).freeze
  DEFAULT_AUTO_CATEGORIES = %w[scene_jump identity text_overlay].freeze
  DEFAULT_MAX_RETRIES = 2
  MAX_RETRIES_LIMIT = 5
  PREVENTION_MIN_ATTEMPTS = TABLE.dig("prevention", "minAttempts").to_i
  PREVENTION_MIN_RATE = TABLE.dig("prevention", "minResolvedRate").to_f
  # 一镜到底类的预防句只对「一个镜头」成立。分镜运镜里明写了切镜、转场、多机位，就不加它，
  # 免得和分镜打架（质检规则里分镜写了的切换本来也不算 scene_jump）。
  CONTINUITY_CATEGORIES = %w[scene_jump].freeze
  CUT_INTENT = /切到|切至|切成|切换到|切换至|切回|切特写|切近景|切远景|切全景|切中景|切反打|转场|跳切|硬切|叠化|多机位|多个机位|\bcuts?\s+(?:to|back|away|into)\b|\bcutaway\b|\bjump[\s-]?cut\b|\bsmash[\s-]?cut\b|\btransition(?:s|ing)?\b|\bmulti[\s-]?cam(?:era)?\b|\bdissolve\b/i.freeze
  # 写进提示词的句子里不许出现的词：否定，以及「提到就会被画出来」的那几样。
  NEGATIVE_WORDS = /\b(?:no|not|never|none|nothing|nobody|without|avoid|avoids|don'?t|doesn'?t|isn'?t|aren'?t|won'?t|free\s+of|lack|lacks|cut|cuts|cutting|wound|wounds|blood|bloody|bleed|bleeding|scar|scars|injury|injuries|text|subtitle|subtitles|caption|captions)\b/i.freeze
  NEGATIVE_CJK = /不|没|无|别|禁|免|勿|非|伤|血|疤|字幕/.freeze

  module_function

  def category(name)
    TABLE["categories"][name.to_s]
  end

  def regenerate?(name)
    REGENERATE_CATEGORIES.include?(name.to_s)
  end

  def max_retries(name)
    entry = category(name)
    entry ? entry["maxRetries"].to_i : 0
  end

  # 某个类别的内置补救句（按表里的顺序）。每条带 id、category、text。
  def remediations(name)
    entry = category(name)
    return [] unless entry

    Array(entry["remediations"]).map { |item| { "id" => item["id"], "category" => name.to_s, "text" => item["text"] } }
  end

  def builtin_prevention
    Array(TABLE.dig("prevention", "builtin")).map { |item| item.slice("id", "category", "text") }
  end

  def cut_intent?(camera_intent)
    camera_intent.to_s.match?(CUT_INTENT)
  end

  # 句子里有否定或「提到就画」的词：写进提示词前拒收。
  def negative?(text)
    body = text.to_s
    body.match?(NEGATIVE_WORDS) || body.match?(NEGATIVE_CJK)
  end

  # 按分镜运镜过滤预防句。返回 [保留的, 跳过的（带 reason）]。
  def applicable(directives, camera_intent)
    cut = cut_intent?(camera_intent)
    kept = []
    skipped = []
    Array(directives).each do |directive|
      entry = normalize(directive)
      next unless entry

      if cut && CONTINUITY_CATEGORIES.include?(entry["category"])
        skipped << entry.merge("reason" => "camera_intent_cut")
      else
        kept << entry
      end
    end
    [dedupe(kept), skipped]
  end

  # 字符串或 {text, category?, id?} → {id, category, text}；空句返回 nil。
  def normalize(directive)
    entry = directive.is_a?(Hash) ? directive : { "text" => directive }
    text = entry["text"].to_s.gsub(/\s*\n\s*/, " ").strip
    return nil if text.empty?

    { "id" => entry["id"].to_s.empty? ? nil : entry["id"].to_s, "category" => entry["category"].to_s.empty? ? nil : entry["category"].to_s,
      "text" => text, "source" => entry["source"] }.reject { |_key, value| value.nil? }
  end

  def dedupe(entries)
    seen = {}
    entries.select do |entry|
      key = entry["text"].downcase
      next false if seen[key]

      seen[key] = true
    end
  end

  # 质检结论里可以自动重做的问题类别（按 categories 过滤，保持出现顺序、去重）。
  def remediable_categories(review, categories = REGENERATE_CATEGORIES)
    return [] unless review.is_a?(Hash)

    Array(review["issues"]).map { |issue| issue.is_a?(Hash) ? issue["category"].to_s : "" }
                           .select { |name| regenerate?(name) && categories.include?(name) }.uniq
  end

  def advisory_categories(review)
    return [] unless review.is_a?(Hash)

    Array(review["issues"]).map { |issue| issue.is_a?(Hash) ? issue["category"].to_s : "" }
                           .select { |name| ADVISORY_CATEGORIES.include?(name) }.uniq
  end

  def clean_categories(value, fallback = DEFAULT_AUTO_CATEGORIES)
    list = case value
           when Array then value.map(&:to_s)
           when String then value.split(/[|,\s]+/)
           else return fallback.dup
           end
    list.map(&:strip).select { |name| regenerate?(name) }.uniq
  end
end
