# frozen_string_literal: true

# 角色身份描述里「长相」与「穿着」分开（0.38.0-rc1）。
#
# 起因（《回村养鸭》第 1 集第 3 镜）：许禾绑了「戴孝」造型，H3 的 subject_definitions 却是
# 「wearing 服装：白色粗布孝衣罩在浅蓝牛仔衬衫外面… 27 岁女性，鹅蛋脸… 标志服饰：浅蓝色牛仔衬衫…
# 腰间系一条旧帆布围裙… 色彩主调：浅牛仔蓝… 不可变项：…浅蓝牛仔衬衫挽袖、墨绿高筒胶靴、胸前口袋的
# 笔和小本子…」——造型和身份里的日常穿着同时出现，模型多半选了牛仔衬衫加围裙，孝衣没了。
# 出造型图时也是同一个问题（复盘 2.1「造型图与身份设定打架」）。
#
# 规矩：镜头绑了造型（且造型有描述）时，身份只留脸、身材、发型、眼镜这类长相锚点，穿着全听造型：
# - 以「标志服饰 / 服饰 / 服装 / 穿着 / 着装 / 衣着 / 色彩主调 / 色彩 / 配色」（及英文 Outfit /
#   Wardrobe / Clothing / Palette 等）开头的句子整句去掉；
# - 「不可变项」逐项过滤，衣物、鞋帽、包、表、手串、笔、本子这类随造型变的去掉，发型、眼镜、
#   脸部特征、表情基线留下；
# - 其他句子按逗号分句，去掉提到衣物配饰的分句。
# 没绑造型时原样返回。出图（参考图说明、造型图）与成片提示词都经这里，两边口径一致。
module IdentityText
  module_function

  WARDROBE_LABEL = /\A(?:标志服饰|标志性服饰|日常服饰|服饰|服装|穿着|穿搭|着装|衣着|装束|色彩主调|主色调|色彩|配色|(?:signature\s+)?(?:outfit|wardrobe|clothing|clothes|attire)|colou?r\s+palette|palette|wearing)\s*[：:]?/i.freeze
  IMMUTABLE_LABEL = /\A(不可变项|不可变特征|不可变|固定特征|(?:immutable|fixed)\s+(?:traits|features))\s*([：:])\s*/i.freeze
  # 标签出现在句中（前面没有句号）时也当新的一节。
  LABEL_BREAK = /(?<!\p{Han})(?=(?:标志服饰|标志性服饰|日常服饰|服饰|服装|穿着|着装|衣着|色彩主调|主色调|色彩|配色|不可变项|不可变特征|固定特征)\s*[：:])/.freeze
  SENTENCE_BREAK = /(?<=[。！？；;!?\n])|(?<=\.)\s+/.freeze
  CLAUSE_BREAK = /(?<=[，,])/.freeze
  ITEM_BREAK = /[、，,;；]+/.freeze
  # 随造型变的东西。眼镜、发型、发饰算长相，不在这里。
  WARDROBE_WORDS = /衣|衫|裙|裤|靴|鞋|袜|帽|围裙|外套|夹克|西装|西服|马甲|背心|卫衣|毛衣|制服|领带|领结|围巾|披肩|腰带|皮带|手套|手表|腕表|手串|手链|手镯|戒指|项链|耳环|胸针|(?<!面)包(?!子)|口袋|钢笔|圆珠笔|(?<![直挺])笔(?![直挺])|本子|笔记本|工牌|徽章|\bPOLO\b|T恤|\b(?:shirt|t-shirt|jacket|coat|dress|skirt|trousers|pants|jeans|boots?|shoes?|socks?|hat|cap|apron|uniform|tie|scarf|belt|gloves?|watch|bracelet|necklace|earrings?|ring|bag|pocket|pen|notebook|badge|hoodie|sweater|vest)\b/i.freeze

  # 只换发型、妆容、年龄段的造型不管衣服：这几类保留身份里的穿着，免得画面上衣服无人描述。
  KEEPS_WARDROBE = %w[hair makeup age].freeze
  # 色彩主调这类句子不算穿着本身（只是配色），日常装那一行不收。
  PALETTE_LABEL = /\A(?:色彩主调|主色调|色彩|配色|colou?r\s+palette|palette)\s*[：:]?/i.freeze
  # 带否定的分句不进日常装那一行：出图 / 视频模型会把提到的东西画出来。
  NEGATION = /不要|不能|不会|不是|不戴|不穿|不带|不留|不用|没有|无须|别(?:让|把|有|带|穿|戴|露|出现)|勿|避免|禁止|\b(?:no|not|without|never|avoid|none)\b|n't/i.freeze

  # 角色的日常装（0.39.0-rc1）：identity_only 去掉的那部分里，真正描述穿着的内容，压成一行。
  #
  # 起因（《回村养鸭》第 2 集首帧）：0.38.0-rc1 拆开长相与穿着后，没绑造型的人物在参考图说明里
  # 只剩开头两句长相摘录，出图模型给许禾换了深色牛仔外套、许大强丢了酒红 POLO、陆青山丢了藏青立领
  # 夹克。没有造型时，这一行就是这个人物的穿着来源。
  #
  # 取法：以「标志服饰 / 服装 / 穿着…」开头的句子去掉标签后整句收下；其他句子只收提到衣物配饰的分句；
  # 「不可变项」里的衣物配饰项只收前面还没提到过的（按衣物词判断）。色彩主调句不收。带否定的分句不收。
  # 什么都没有时返回空串。
  def wardrobe_line(visual_prompt)
    pieces = visual_prompt.to_s.gsub(LABEL_BREAK, "\n").split(SENTENCE_BREAK).map(&:strip).reject(&:empty?)
    labeled = []
    loose = []
    items = []
    pieces.each do |sentence|
      if sentence.match?(PALETTE_LABEL)
        next
      elsif sentence.match?(WARDROBE_LABEL)
        labeled.concat(clauses(sentence.sub(WARDROBE_LABEL, "")))
      elsif (immutable = sentence.match(IMMUTABLE_LABEL))
        list = sentence[immutable[0].length..-1].to_s.sub(/[。！？；;!?.]\z/, "")
        items.concat(list.split(ITEM_BREAK).map(&:strip).select { |item| item.match?(WARDROBE_WORDS) })
      else
        loose.concat(clauses(sentence).select { |clause| clause.match?(WARDROBE_WORDS) })
      end
    end
    kept = (labeled + loose).reject { |clause| clause.match?(NEGATION) }
    mentioned = kept.join
    extra = items.reject { |item| item.match?(NEGATION) || covered?(item, mentioned) }
    parts = kept + extra
    return "" if parts.empty?

    cjk = parts.join.match?(/\p{Han}/)
    parts.uniq.join(cjk ? "，" : ", ")
  end

  # 句子按逗号切成分句，去掉句末标点与空白。
  def clauses(sentence)
    sentence.to_s.sub(/[。！？；;!?.]\s*\z/, "").split(/[，,]/).map(&:strip).reject(&:empty?)
  end

  # 不可变项里的一项，衣物词都已经在前面出现过：算重复，不再列。
  def covered?(item, mentioned)
    words = item.scan(WARDROBE_WORDS)
    !words.empty? && words.all? { |word| mentioned.include?(word) }
  end

  # 有造型（appearance 非空，类别是服装 / 状态 / 未分类）时只留长相；否则原样。
  def for_cast(visual_prompt, appearance, category = nil)
    return visual_prompt.to_s if appearance.to_s.strip.empty? || KEEPS_WARDROBE.include?(category.to_s)

    identity_only(visual_prompt)
  end

  # 什么都不用去掉时原样返回（逐字不变）；去掉了东西才按句重拼，句与句之间补句号。
  def identity_only(visual_prompt)
    original = visual_prompt.to_s
    pieces = original.gsub(LABEL_BREAK, "\n").split(SENTENCE_BREAK).map(&:strip).reject(&:empty?)
    filtered = pieces.map { |sentence| filter_sentence(sentence) }
    return original if filtered == pieces

    kept = filtered.compact
    kept.each_with_index.map { |piece, index| index == kept.length - 1 ? piece : terminate(piece) }
        .join.gsub(/[ \t]*\n[ \t]*/, "").gsub(/\s{2,}/, " ").strip
  end

  # 不用动的句子原样返回（同一个字符串），要删整句返回 nil，删了一部分返回新的句子。
  def filter_sentence(sentence)
    body = sentence.strip
    return nil if body.empty? || body.match?(WARDROBE_LABEL)
    return body unless body.match?(WARDROBE_WORDS) || body.match?(IMMUTABLE_LABEL)

    immutable = body.match(IMMUTABLE_LABEL)
    return filter_immutable(body, immutable) if immutable

    clauses = body.split(CLAUSE_BREAK).reject { |clause| clause.match?(WARDROBE_WORDS) }
    return nil if clauses.empty?

    joined = clauses.join.sub(/[，,]\s*\z/, "")
    ending = body[/[。！？；;!?.]\z/]
    # 原句没有句末标点（一整段只有一句）就不补；拼接时 identity_only 会给中间的句子补。
    ending ? terminate(joined, ending) : joined.strip
  end

  # 每句收尾：原来的句末标点；被截断或原本没有时，中文补「。」，英文补「.」。英文句后留一个空格。
  def terminate(text, ending = nil)
    text = text.strip
    text = "#{text}#{ending || (text.match?(/[⺀-鿿]/) ? '。' : '.')}" unless text.match?(/[。！？；;!?.]\z/)
    text.match?(/[.;!?]\z/) ? "#{text} " : text
  end

  # 「不可变项：a、b、c。」逐项过滤；全被去掉时整句不要。
  def filter_immutable(body, match)
    rest = body[match[0].length..-1].to_s
    ending = rest[/[。！？；;!?.]\z/]
    rest = rest.sub(/[。！？；;!?.]\z/, "")
    items = immutable_items(rest)
    return nil if items.empty?

    cjk = rest.match?(/\p{Han}/)
    terminate("#{match[1]}#{match[2]}#{cjk ? '' : ' '}#{items.join(cjk ? '、' : ', ')}", ending)
  end

  def immutable_items(list)
    list.split(ITEM_BREAK).map(&:strip).reject(&:empty?).reject { |item| item.match?(WARDROBE_WORDS) }
  end
end
