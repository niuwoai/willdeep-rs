# frozen_string_literal: true

require_relative "qa_remediation"
require_relative "reference_legend"
require_relative "identity_text"

# 候选图被拦截后重抽时加的补救句（0.40.0-rc1，docs/decisions/0008-candidate-image-qa.md）。
#
# 规矩与成片补救表（schemas/qa-remediations-v1.json）一样：只写想要的画面。出图模型会把提到的东西画出来——
# 写「不要男性」可能画出男性，写「不要文字」会招来文字。所以每类问题都换成一句「应当是什么」的正面描述，
# 内容取自这一镜的出图材料（参考图说明里的人物与造型、分镜运镜、提示词里写明的字），不取质检的 detail
# （detail 描述的是错的样子）。拼出来的句子逐分句过 QARemediation.negative?，带否定的分句丢掉；整句仍不合格时
# 退回本类的固定句（FALLBACK，测试扫过）。
module ImageRemediation
  module_function

  ID_PREFIX = "image/"
  FRAME_TARGET = %w[start end].freeze
  # 每类问题的固定句：材料里取不到具体内容、或拼出来的句子带否定时用。
  FALLBACK = {
    "identity" => "画面中每个人物的长相、年龄与男女特征和对应的长相参考图一致",
    "wardrobe" => "画面中每个人物的服装与本镜写明的造型一致",
    "text" => "招牌、包装、箱子与衣物表面是素色平面，干净整洁",
    "brand" => "皮带扣、包装、招牌与衣物上的图案是简洁的原创素面设计",
    "composition" => "主体完整位于画面之中，取景范围与机位按镜头要求",
    "aspect" => "画面方向与要求的画幅一致",
    "other" => "画面内容与提示词描述逐项一致，人物手部与肢体结构自然完整"
  }.freeze
  ORDER = %w[identity wardrobe text brand composition aspect other].freeze
  QUOTED = /[「“"]([^」”"\n]{1,24})[」”"]/.freeze
  CLAUSE_BREAK = /[，,。；;]+/.freeze
  STUDIO_FRAMING = {
    "character" => "全身正面站姿，人物从头到脚完整入画，浅灰纯色背景，光线柔和均匀",
    "appearance" => "全身正面站姿，人物从头到脚完整入画，浅灰纯色背景，光线柔和均匀",
    "prop" => "单个主体居中，完整入画，四周留白，浅灰纯色背景",
    "scene" => "场景全景，空间与陈设按描述完整呈现",
    "sceneVariant" => "场景全景，空间与陈设按描述完整呈现"
  }.freeze

  # issues：被拦截的候选上的 must 问题（{category, level, detail}）。material：ImageQA#material 的结果
  # （plan 带 entries / unreferenced / body / identity，context 带 target / shot / size / aspect）。
  # 返回 [{ "category", "id" => "image/<类别>", "text" }]，按类别固定顺序，每类一句。
  def sentences(issues, material)
    categories = Array(issues).map { |issue| issue["category"].to_s }.uniq
    categories = ["other"] if categories.empty?
    details = Array(issues).map { |issue| issue["detail"].to_s }.join("\n")
    ORDER.select { |category| categories.include?(category) }.map do |category|
      text = positive(build(category, material || {}, details))
      text = FALLBACK.fetch(category) if text.empty?
      { "category" => category, "id" => "#{ID_PREFIX}#{category}", "text" => text }
    end
  end

  def build(category, material, details)
    plan = material["plan"] || {}
    context = material["context"] || {}
    case category
    when "identity" then identity(plan, context, details)
    when "wardrobe" then wardrobe(plan)
    when "text" then text(plan)
    when "brand" then FALLBACK["brand"]
    when "composition" then composition(context)
    when "aspect" then aspect(context)
    else FALLBACK["other"]
    end
  end

  # 「画面中的「许禾」是二十七岁女性，鹅蛋脸，长相与图1一致」；质检 detail 点了名的人物优先，没点名就写全部。
  def identity(plan, context, details)
    entries = Array(plan["entries"]).each_with_index.select { |entry, _index| entry["semanticType"] == "identity" }
    named = entries.select { |entry, _index| details.include?(entry["name"].to_s) }
    named = entries if named.empty?
    parts = named.map do |entry, index|
      looks = excerpt(IdentityText.identity_only(entry["identity"].to_s))
      looks.empty? ? "画面中的「#{entry['name']}」长相与图#{index + 1}一致" : "画面中的「#{entry['name']}」是#{looks}，长相与图#{index + 1}一致"
    end
    Array(plan["unreferenced"]).each do |subject|
      looks = excerpt(IdentityText.identity_only(subject["identity"].to_s))
      parts << "画面中的「#{subject['name']}」是#{looks}" unless looks.empty?
    end
    names = (entries.map { |entry, _index| entry["name"].to_s } + Array(plan["unreferenced"]).map { |subject| subject["name"].to_s }).reject(&:empty?).uniq
    parts << "画面中出场的人物是#{names.map { |name| "「#{name}」" }.join('与')}" if names.length.positive? && FRAME_TARGET.include?(context["target"])
    if parts.empty? && !plan["identity"].to_s.strip.empty?
      looks = excerpt(plan["identity"].to_s)
      parts << "画面中的人物是#{looks}" unless looks.empty?
    end
    parts.join("；")
  end

  # 造型参考图：「「许禾」穿着图2中的造型「孝服」」；没绑造型、说明里写了日常装：「「许禾」穿着日常装：…」。
  def wardrobe(plan)
    entries = Array(plan["entries"])
    parts = entries.each_with_index.map do |entry, index|
      case entry["semanticType"]
      when "appearance"
        owner = entry["characterName"].to_s
        owner.empty? ? "人物穿着图#{index + 1}中的造型「#{entry['name']}」" : "「#{owner}」穿着图#{index + 1}中的造型「#{entry['name']}」"
      when "identity"
        dress = ReferenceLegend.wardrobe_text(entry["wardrobe"])
        dress.empty? ? nil : "「#{entry['name']}」穿着日常装：#{dress}"
      end
    end.compact
    Array(plan["unreferenced"]).each do |subject|
      dress = ReferenceLegend.wardrobe_text(subject["wardrobe"])
      parts << "「#{subject['name']}」穿着日常装：#{dress}" unless dress.empty?
    end
    parts.empty? ? FALLBACK["wardrobe"] : parts.join("；")
  end

  # 提示词正文里用引号写明的字（人物、资产名除外）：「画面中可读的文字是简体「许家鸭棚」（4 个字），字形清晰端正」。
  def text(plan)
    names = Array(plan["entries"]).flat_map { |entry| [entry["name"], entry["characterName"]] } +
            Array(plan["unreferenced"]).map { |subject| subject["name"] }
    names = names.map(&:to_s).reject(&:empty?)
    quoted = plan["body"].to_s.scan(QUOTED).flatten.map(&:strip).reject { |value| value.empty? || names.include?(value) }.uniq.first(3)
    return FALLBACK["text"] if quoted.empty?

    listed = quoted.map { |value| "「#{value}」（#{value.length} 个字）" }.join("、")
    "画面中可读的文字是简体#{listed}，逐字一致，字形清晰端正"
  end

  # 首尾帧按分镜运镜重述构图；资产图按棚拍取景。
  def composition(context)
    target = context["target"].to_s
    return STUDIO_FRAMING.fetch(target, FALLBACK["composition"]) unless FRAME_TARGET.include?(target)

    shot = context["shot"] || {}
    camera = clauses(shot["cameraIntent"])
    subject = clauses(first_sentence(target == "end" ? shot["actionEnd"] : shot["actionStart"]))
    subject = clauses(first_sentence(shot["summary"])) if subject.empty?
    parts = []
    parts << "构图：#{camera}" unless camera.empty?
    parts << "画面主体是#{subject}" unless subject.empty?
    parts << "主体完整位于画面之中"
    parts.join("；")
  end

  def aspect(context)
    size = context["size"].to_s
    width, height = size.split("x").map(&:to_i)
    return FALLBACK["aspect"] unless width.to_i.positive? && height.to_i.positive?

    shape = if height > width then "竖幅"
            elsif width > height then "横幅"
            else "方形"
            end
    label = context["aspect"].to_s.empty? ? "" : "#{context['aspect']}，"
    "画面为#{shape}构图（#{label}#{size}）"
  end

  # 逐分句去掉带否定词的，剩下的用「，」接回；整句仍不合格返回空串（调用方退回固定句）。
  def positive(text)
    kept = text.to_s.split("；").map { |sentence| clauses(sentence) }.reject(&:empty?)
    joined = kept.join("；")
    QARemediation.negative?(joined) ? "" : joined
  end

  def clauses(text)
    text.to_s.split(CLAUSE_BREAK).map(&:strip).reject { |clause| clause.empty? || QARemediation.negative?(clause) }.join("，")
  end

  def first_sentence(text)
    text.to_s.split(/[。！？!?\n]+/).map(&:strip).find { |sentence| !sentence.empty? }.to_s
  end

  def excerpt(text)
    ReferenceLegend.excerpt(text)
  end
end
