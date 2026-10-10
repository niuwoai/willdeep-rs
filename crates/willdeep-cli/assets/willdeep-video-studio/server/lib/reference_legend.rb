# frozen_string_literal: true

require_relative "h3_prompt"
require_relative "identity_text"

# 参考图说明（0.35.0-rc2）：出图时把「第几张参考图是什么」写在提示词最前面。
#
# 起因（《回村养鸭》第 1 集，14 镜首帧）：参考包带了许禾的身份图、戴孝造型、
# 村民甲的身份图、鸭棚场景图，提示词却只有分镜 startPrompt。模型不知道「许禾」
# 是哪张图里的人，5 镜把 27 岁的女主画成了男人（借了男村民的脸），许大强丢了
# 标志性的衣服。
#
# 规矩：
# - 只有这一处拼说明。参考图路径、参照摘要、说明文字都由 build 从同一个有序列表
#   生成，图 N 一定就是发给后端的第 N 张（reference_paths[N-1]）。
# - 只写正面的话：出图模型会把提到的东西画出来，写「不是男性」反而可能画出男性。
#   角色描述取 visualPrompt 开头一两句（年龄、性别、脸、发型通常在这里），按分句
#   去掉带否定词的，截到 EXCERPT_MAX_WIDTH。
# - 角色绑了造型时，身份图那一项的描述只取长相（lib/identity_text.rb，0.38.0-rc1）：参考包编译与
#   造型图出图已经把 identity 换成去掉日常穿着的版本，这里拿到的就是它，与成片提示词口径一致。
# - 角色没绑造型时（0.39.0-rc1）：身份图那一项写成「长相与日常装参考」，摘录只取长相，后面跟一行
#   「日常装：…」（IdentityText.wardrobe_line：标志服饰句与不可变项里的衣物配饰）。0.38.0-rc1 拆开长相与
#   穿着后，这一项只剩开头两句长相，《回村养鸭》第 2 集首帧里许禾换了深色牛仔外套、许大强丢了酒红 POLO。
#   绑了造型的照旧：造型赢，身份里的日常穿着不写。
# - 发型单独一项（0.41.0-rc1）：长相摘录截到约 36 个汉字，visualPrompt 里发型常在第二三句，被截掉了。
#   《回村养鸭》第 3 集首帧：三奶奶的「花白小圆髻插木簪」画成了黑发、许禾的半扎小揪画成了扎后脑，
#   图片质检判 identity 不符。摘录里还没提到头发时，补一句「发型：…」。
# - 场景写出样子（0.41.0-rc1）：以前只写「场景「许家鸭棚·外」的空间与陈设」，模型只拿到一张图和一个
#   名字，第 3 集 5 镜首帧两张候选都把红砖鸭棚画成了铁皮顶木棚、茅草竹棚，质检以场景不符 block。
#   现在跟一句场景描述的摘录（建筑、材质、主色），结尾的规矩也写明建筑与材质照参考图。
# - 视频侧不用它：H3 提示词有自己的 subject_definitions（<Subject N> / <Picture 1>），
#   而且视频请求只带首帧或参考视频，身份图、造型图不随视频发出。
module ReferenceLegend
  module_function

  # 角色描述摘录的宽度上限（CJK 记 2，ASCII 记 1）：约 36 个汉字。
  EXCERPT_MAX_WIDTH = 72
  EXCERPT_MAX_SENTENCES = 2
  SENTENCE_BREAK = /[。！？!?；;\n]+|\.(?=\s|\z)/.freeze
  CLAUSE_BREAK = /[，,、]+/.freeze
  # 带这些词的分句不进说明。宁可少摘一句，也不把否定句送给出图模型。
  # 「别」只认祈使用法（别让、别穿…），不误伤「性别」「特别」。
  NEGATION = IdentityText::NEGATION
  # 日常装那一行的宽度上限：约 60 个汉字，够写全一身行头。
  WARDROBE_MAX_WIDTH = 120
  # 发型那一句的宽度上限：约 24 个汉字。
  HAIR_MAX_WIDTH = 48
  # 场景摘录的宽度上限：约 45 个汉字，够写建筑、屋顶、地面与主色。
  SCENE_MAX_WIDTH = 90
  # 说发型的分句。「洗得发白」这类说衣服的不算（先排除衣物词）。
  # 「黝黑发亮」「晒得发红」里也有「黑发」「得发」：颜色字后面跟着亮、红这类字时是在说皮肤，不算头发（0.41.0-rc4）。
  HAIR_WORDS = /头发|[短长卷直黑白灰]发(?![亮红黄紫青光烫胖福])|发髻|[盘挽圆]髻|发圈|簪|小揪|马尾|辫|寸头|平头|光头|分头|[左右侧]分|偏分|侧梳|刘海|鬓角|发际线|\bhair|\bbun\b|ponytail|braid/i.freeze
  ELLIPSIS = "…"
  # 与 ReferencePackage::WIDE_CHAR_FROM 相同：从这里往后按双宽计。
  WIDE_CHAR_FROM = 0x2E80

  # entries：按发送顺序排好的参考图，每项至少有 semanticType、filePath，
  # 以及说明要用的 name / characterName / identity / fileName / assetID / characterID。
  # unreferenced：本镜出场、但没有长相参考图的人物（{ "name", "identity" }）。
  # 返回 { "legend" => String（可能为空串）, "references" => [路径], "summary" => [摘要] }。
  def build(entries, unreferenced: [])
    entries = Array(entries)
    phrases = entries.each_with_index.map { |entry, index| "图#{index + 1} 是#{describe(entry)}" }
    summary = entries.each_with_index.map do |entry, index|
      item = { "index" => index + 1, "semanticType" => entry["semanticType"], "assetID" => entry["assetID"], "name" => entry["name"],
               "fileName" => entry["fileName"], "legend" => phrases[index] }
      # 旧字段：身份图仍报 characterID，页面与旧技能文档按它读。
      item["characterID"] = entry["characterID"] if entry["semanticType"] == "identity"
      item
    end
    text_only = Array(unreferenced).map do |subject|
      wardrobe = wardrobe_text(subject["wardrobe"])
      excerpt = excerpt(wardrobe.empty? ? subject["identity"] : IdentityText.identity_only(subject["identity"]))
      next nil if excerpt.empty? && wardrobe.empty?

      "「#{subject['name']}」按文字描述#{excerpt.empty? ? '' : "——#{excerpt}"}#{wardrobe_clause(wardrobe)}"
    end.compact
    dressed = (entries + Array(unreferenced)).any? { |item| !wardrobe_text(item["wardrobe"]).empty? }
    { "legend" => compose(phrases, text_only, entries, dressed), "references" => entries.map { |entry| entry["filePath"] }, "summary" => summary }
  end

  # 说明在前、正文在后；说明为空时原样返回正文。
  def prepend(legend, prompt)
    legend.to_s.empty? ? prompt : "#{legend}\n\n#{prompt}"
  end

  def compose(phrases, text_only, entries, dressed = false)
    return "" if phrases.empty? && text_only.empty?

    parts = []
    parts << "参考图说明（按顺序）：#{phrases.join('；')}。" unless phrases.empty?
    parts << "#{text_only.join('；')}。" unless text_only.empty?
    types = entries.map { |entry| entry["semanticType"] }
    rules = []
    rules << "画面中人物的长相、性别、年龄严格以对应的长相参考图为准" if types.include?("identity")
    rules << "服装造型以对应的造型参考图为准" if types.include?("appearance")
    rules << "写了日常装的人物按所写日常装穿着" if dressed
    rules << "场景的建筑、材质与布局以场景参考图为准" if types.include?("scene")
    rules << "道具外观以道具参考图为准" if types.include?("prop")
    parts << "#{rules.join('，')}。" unless rules.empty?
    parts.join
  end

  def describe(entry)
    case entry["semanticType"]
    when "identity"
      wardrobe = wardrobe_text(entry["wardrobe"])
      if wardrobe.empty?
        excerpt = excerpt(entry["identity"])
        "「#{entry['name']}」的长相参考#{excerpt.empty? ? '' : "——#{excerpt}"}#{hair_clause(entry['identity'], excerpt)}"
      else
        excerpt = excerpt(IdentityText.identity_only(entry["identity"]))
        "「#{entry['name']}」的长相与日常装参考#{excerpt.empty? ? '' : "——#{excerpt}"}#{hair_clause(entry['identity'], excerpt)}#{wardrobe_clause(wardrobe)}"
      end
    when "appearance"
      owner = entry["characterName"].to_s
      owner.empty? ? "服装造型「#{entry['name']}」" : "「#{owner}」本镜的服装造型「#{entry['name']}」"
    when "scene"
      look = scene_excerpt(entry["scenePrompt"])
      look.empty? ? "场景「#{entry['name']}」的空间与陈设" : "场景「#{entry['name']}」——#{look}"
    when "prop" then "道具「#{entry['name']}」的外观"
    else "参考「#{entry['name']}」"
    end
  end

  # 日常装一行：去掉带否定词的分句，截到上限。
  def wardrobe_text(text)
    clauses = H3Prompt.plain(text).split(/[，,]+/).map(&:strip).reject { |clause| clause.empty? || clause.match?(NEGATION) }
    truncate(clauses.join("，"), WARDROBE_MAX_WIDTH)
  end

  # 发型一句：摘录里已经提到头发就不重复；否则从长相部分找说头发的分句。
  def hair_clause(identity, excerpt)
    return "" if excerpt.match?(HAIR_WORDS)

    hair = hair_text(identity)
    hair.empty? ? "" : "；发型：#{hair}"
  end

  def hair_text(identity)
    clauses = IdentityText.identity_only(identity).split(SENTENCE_BREAK).flat_map { |sentence| H3Prompt.plain(sentence).split(CLAUSE_BREAK) }
                          .map(&:strip).reject(&:empty?)
    hair = clauses.reject { |clause| clause.match?(NEGATION) || clause.match?(IdentityText::IMMUTABLE_LABEL) }
                  .select { |clause| clause.match?(HAIR_WORDS) && !clause.match?(IdentityText::WARDROBE_WORDS) }
    truncate(hair.first(2).join("，"), HAIR_MAX_WIDTH)
  end

  # 场景描述开头几个分句（变体在前、场景在后，已由参考包拼好），去掉「色调：」「光线：」这类标签句与否定分句。
  def scene_excerpt(text)
    clauses = text.to_s.split(/[。！？；;\n]+/).map { |sentence| H3Prompt.plain(sentence) }
                  .reject { |sentence| sentence.empty? || sentence.match?(/\A(?:色调|光线|色彩|配色)\s*[：:]/) }
                  .flat_map { |sentence| sentence.split(CLAUSE_BREAK) }.map(&:strip)
                  .reject { |clause| clause.empty? || clause.match?(NEGATION) }
    truncate(clauses.join("，"), SCENE_MAX_WIDTH)
  end

  def wardrobe_clause(wardrobe)
    wardrobe.empty? ? "" : "；日常装：#{wardrobe}"
  end

  # visualPrompt 开头一两句里不带否定词的分句，截到上限。
  def excerpt(text)
    clauses = []
    text.to_s.split(SENTENCE_BREAK).map { |sentence| H3Prompt.plain(sentence) }.reject(&:empty?).first(EXCERPT_MAX_SENTENCES).each do |sentence|
      sentence.split(CLAUSE_BREAK).map(&:strip).reject(&:empty?).each do |clause|
        clauses << clause unless clause.match?(NEGATION)
      end
    end
    truncate(clauses.join("，"), EXCERPT_MAX_WIDTH)
  end

  def truncate(text, limit)
    width = 0
    kept = +""
    text.each_char do |char|
      width += char.ord >= WIDE_CHAR_FROM ? 2 : 1
      return kept.sub(/[，,\s]+\z/, "") + ELLIPSIS if width > limit
      kept << char
    end
    kept
  end
end
