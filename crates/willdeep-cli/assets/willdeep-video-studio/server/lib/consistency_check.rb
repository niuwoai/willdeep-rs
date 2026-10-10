# frozen_string_literal: true

require_relative "chinese_numeral"
require_relative "drama_canon"

# `drama.check_consistency`（0.37.0-rc1）：按全剧设定账本扫一遍策划、角色、资产、
# 分集（标题、梗概、正文）与分镜（各文字字段与台词），只读、不调模型。
#
# 四类问题：
# - bannedTerms：禁用词出现在哪、上下文是什么、该换成什么；
# - unknownSpeakers：台词里说话的人不在角色表、也不在 allowedExtras；
# - facts：剧本里提到的数和账本时间线对不上（启发式，confidence 一律 possible）；
# - visualRules：视觉口径声明的 forbiddenPhrases 出现了。
#
# 只看已采用的正式内容，与 drama.get_progress 一个口径：草稿随时会被丢。
#
# 数字的认法（宁可漏报，不要误报）：
# 1. 只认 0–99999 的汉字或阿拉伯数字（lib/chinese_numeral.rb）；
# 2. 数后面紧跟 fact 的 unit（「只」），或前 30 / 后 10 个字以内出现 fact 的
#    label 或 keywords（存栏、点数……），才算说的是这个 fact；
# 3. 「第 N」、后面跟天 / 集 / 岁 / 点钟这类别的单位的不算；
# 4. 只看数量级相近的数：账本里最小值的一半到最大值的两倍之间；
# 5. 剧情天数从正文里的「第 N 天」读，没写的沿用上一集；读不出天数时，按天生效的
#    条目都算「可能适用」，任何一个对得上就不报。
class ConsistencyCheck
  MAX_ISSUES = 300
  MAX_SPEAKER_LOCATIONS = 8
  EXCERPT_RADIUS = 14
  WINDOW_BEFORE = 30
  WINDOW_AFTER = 10
  DAY_MARKER = /第\s*([0-9０-９零〇一二两三四五六七八九十百]+)\s*天/.freeze
  # 数后面跟这些字，说的就不是账本里的那个量（除非它正是 fact 的 unit）。
  OTHER_UNITS = %w[天 集 镜 年 月 日 号 岁 次 遍 回 倍 点 分 秒 小 周 层 楼 排 页 章 位 名 句 步 成 折].freeze
  # 剧本里「XX：」开头、但不是人在说话的行。
  NON_SPEAKERS = %w[时间 地点 场景 场 人物 出场人物 字幕 备注 注 镜头 画面 动作 音效 音乐 配乐 BGM 提示 道具 内景 外景 梗概 标题 钩子 悬念 结尾 开场 转场 特写 闪回 字卡].freeze
  # 不需要角色档案的发声：旁白、画外音、群声。
  BUILTIN_SPEAKERS = %w[旁白 画外音 众人 众 全体 群众 合 OS VO V.O. O.S. 广播 电话 内心 内心独白 系统 解说 声音].freeze
  # 与页面 bridge.ts#detectUnknownCharacters 同一认法：行首 2–6 个汉字或字母、可带（OS）
  # 这类括注、冒号后真有台词。只说一句的人也报：有台词的功能性人物同样要有视觉设定。
  SPEAKER_LINE = /\A[ \t　]*([\p{Han}A-Za-z]{2,6})[ \t　]*(?:[（(][^）)\n]{0,16}[）)])?[ \t　]*[：:][ \t　]*\S/.freeze
  SHOT_FIELDS = %w[title summary actionStart actionEnd cameraIntent startPrompt endPrompt soundscape music].freeze
  DRAMA_FIELDS = %w[title genre logline coreConflict audience tone arc visualStyle].freeze

  def self.run(drama)
    new(drama).run
  end

  def initialize(drama)
    @drama = drama
    @canon = DramaCanon.read(drama)
  end

  def run
    @issues = { "bannedTerms" => [], "unknownSpeakers" => [], "facts" => [], "visualRules" => [] }
    @truncated = Hash.new(0)
    @speakers = {}
    sources.each { |source| scan_terms(source) }
    scan_speakers
    scan_facts
    summary = @issues.each_with_object({}) { |(kind, list), result| result[kind] = list.length + @truncated[kind] }
    summary["total"] = summary.values.sum
    {
      "ok" => true, "dramaID" => @drama["id"], "canonRev" => @canon["rev"].to_i, "canonEmpty" => DramaCanon.blank?(@canon),
      "summary" => summary, "issues" => @issues, "truncated" => @truncated.empty? ? nil : @truncated,
      "notes" => [
        "Committed content only; pending drafts are not scanned.",
        "facts issues are heuristic (confidence possible): confirm against the script before changing it, or update the canon with drama.save_canon if the story changed."
      ]
    }.compact
  end

  private

  # 全部文字的位置清单。每项：location（对象引用）、text、episode（分集上下文，供数字检查）。
  def sources
    @sources ||= begin
      list = []
      DRAMA_FIELDS.each { |field| list << source({ "scope" => "drama", "field" => field }, @drama[field]) }
      Array(@drama["characters"]).each do |character|
        %w[name description visualPrompt].each do |field|
          list << source({ "scope" => "character", "characterID" => character["id"], "characterName" => character["name"], "field" => field }, character[field])
        end
      end
      Array(@drama["assets"]).reject { |asset| asset["archived"] }.each do |asset|
        %w[name prompt notes].each do |field|
          list << source({ "scope" => "asset", "assetID" => asset["id"], "assetName" => asset["name"], "assetKind" => asset["kind"], "field" => field }, asset[field])
        end
      end
      episodes.each do |episode|
        base = { "episodeID" => episode["id"], "episodeOrder" => episode["order"].to_i }
        %w[title summary script].each do |field|
          list << source(base.merge("scope" => "episode", "field" => field), episode[field], episode)
        end
        sorted(episode["shots"]).each do |shot|
          shot_base = base.merge("scope" => "shot", "shotID" => shot["id"], "shotOrder" => shot["order"].to_i)
          SHOT_FIELDS.each { |field| list << source(shot_base.merge("field" => field), shot[field], episode) }
          # 一句台词很短，「四百八十六。齐了。」本身没有「存栏」二字：认数字时把本镜的
          # 剧情、开场动作和前面几句台词当作上文。
          lead = [shot["summary"], shot["actionStart"]].map(&:to_s)
          Array(shot["dialogue"]).each_with_index do |line, index|
            next unless line.is_a?(Hash)

            entry = source(shot_base.merge("field" => "dialogue", "lineIndex" => index, "speaker" => line["speaker"].to_s), line["text"], episode)
            entry["lead"] = lead.reject(&:empty?).join("\n")
            list << entry
            lead << line["text"].to_s
          end
        end
      end
      list.reject { |entry| entry["text"].strip.empty? }
    end
  end

  def source(location, text, episode = nil)
    { "location" => location, "text" => text.to_s, "episode" => episode }
  end

  def episodes
    @episodes ||= sorted(@drama["episodes"])
  end

  def sorted(collection)
    Array(collection).sort_by { |entry| entry["order"].to_i }
  end

  # ---- 禁用词与视觉口径 ----

  def scan_terms(entry)
    Array(@canon["bannedTerms"]).each do |banned|
      occurrences(entry["text"], banned["term"], banned["replacement"]).each do |index|
        add("bannedTerms", "kind" => "banned_term", "term" => banned["term"], "replacement" => banned["replacement"], "reason" => banned["reason"],
                           "location" => located(entry, index), "excerpt" => excerpt(entry["text"], index, banned["term"].length))
      end
    end
    Array(@canon["visualRules"]).each do |rule|
      Array(rule["forbiddenPhrases"]).each do |phrase|
        occurrences(entry["text"], phrase, nil).each do |index|
          add("visualRules", "kind" => "visual_rule", "rule" => rule["rule"], "phrase" => phrase,
                             "location" => located(entry, index), "excerpt" => excerpt(entry["text"], index, phrase.length))
        end
      end
    end
  end

  # 不分大小写找出每一处。替换词本身含禁用词时（禁「约」、换「还款约定」），
  # 已经是替换词的那几处不算。
  def occurrences(text, term, replacement)
    needle = term.to_s.downcase
    return [] if needle.empty?

    haystack = text.downcase
    shift = replacement.to_s.downcase.index(needle)
    found = []
    index = haystack.index(needle)
    while index
      replaced = shift && index >= shift && haystack[index - shift, replacement.length] == replacement.downcase
      found << index unless replaced
      index = haystack.index(needle, index + needle.length)
    end
    found
  end

  # ---- 说话人 ----

  def scan_speakers
    known = Array(@drama["characters"]).map { |character| character["name"].to_s.strip }.reject(&:empty?)
    known.concat(Array(@canon["allowedExtras"]))
    known.concat(BUILTIN_SPEAKERS)
    @known_speakers = known.map(&:downcase).uniq

    sources.each do |entry|
      location = entry["location"]
      if location["field"] == "dialogue"
        note_speaker(location["speaker"], location.reject { |key, _| key == "speaker" })
      elsif location["scope"] == "episode" && location["field"] == "script"
        entry["text"].each_line.with_index(1) do |line, number|
          match = SPEAKER_LINE.match(line)
          next unless match

          note_speaker(match[1], location.merge("line" => number), script: true)
        end
      end
    end
    @speakers.each_value do |entry|
      add("unknownSpeakers", "kind" => "unknown_speaker", "name" => entry["name"], "count" => entry["count"], "locations" => entry["locations"])
    end
  end

  def note_speaker(raw, location, script: false)
    names = raw.to_s.sub(/[（(][^）)]*[）)]\s*\z/, "").split(%r{[、&＆/]}).map(&:strip).reject(&:empty?)
    names.each do |name|
      next if script && (NON_SPEAKERS.include?(name) || name.start_with?("第") || name.match?(/\A[0-9０-９]/))
      next if @known_speakers.include?(name.downcase)

      entry = (@speakers[name] ||= { "name" => name, "count" => 0, "locations" => [] })
      entry["count"] += 1
      entry["locations"] << location if entry["locations"].length < MAX_SPEAKER_LOCATIONS
    end
  end

  # ---- 数字 ----

  def scan_facts
    facts = Array(@canon["facts"]).map { |fact| numeric_fact(fact) }.compact
    return if facts.empty?

    carried = nil
    episodes.each do |episode|
      span = span_of(episode, carried)
      sources.select { |entry| entry["episode"].equal?(episode) }.each do |entry|
        location = entry["location"]
        script = location["scope"] == "episode" && location["field"] == "script"
        markers = script ? day_markers(entry["text"]) : []
        ChineseNumeral.scan(entry["text"]).each do |mention|
          days = if script
                   last = markers.select { |marker| marker[0] < mention["start"] }.last
                   last ? [last[1]] : [span[:start]].compact
                 else
                   span[:days]
                 end
          check_mention(facts, entry, mention, episode["order"].to_i, days)
        end
      end
      carried = span[:end]
    end
  end

  # 一集涉及的剧情天数：标题、梗概、正文里写到的「第 N 天」，没写就沿用上一集最后一天。
  def span_of(episode, carried)
    head = day_markers("#{episode['title']}\n#{episode['summary']}").map(&:last)
    script = episode["script"].to_s
    markers = day_markers(script)
    body = markers.map(&:last)
    # 正文第一行就写了「第 N 天」（场景标题），这一集就从那天开始，不再沿用上一集。
    text_start = script.index(/\S/) || 0
    first_line = script.index("\n", text_start) || script.length
    opening = markers.first && markers.first[0] <= first_line ? markers.first[1] : nil
    start = head.first || opening || carried
    days = ([start] + head + body).compact.uniq
    { start: start, days: days, end: body.last || head.last || carried }
  end

  def day_markers(text)
    markers = []
    text.to_s.to_enum(:scan, DAY_MARKER).each do
      match = Regexp.last_match
      day = ChineseNumeral.parse(match[1])
      markers << [match.begin(0), day] if day&.positive?
    end
    markers
  end

  def numeric_fact(fact)
    entries = Array(fact["values"]).map do |value|
      number = value["value"].is_a?(Numeric) ? value["value"] : ChineseNumeral.parse(value["value"].to_s)
      number && value.merge("number" => number)
    end
    numbers = entries.compact.map { |entry| entry["number"] }
    return nil if numbers.empty? || entries.compact.length != entries.length

    words = ([fact["label"], fact["key"]] + Array(fact["keywords"])).map(&:to_s).reject(&:empty?).uniq
    { "fact" => fact, "entries" => entries, "words" => words, "low" => numbers.min * 0.5, "high" => numbers.max * 2 }
  end

  def check_mention(facts, entry, mention, order, days)
    text = entry["text"]
    matching = facts.select { |fact| refers_to?(fact, text, mention, entry["lead"].to_s) }
    return if matching.empty?

    value = mention["value"]
    expectations = matching.map { |fact| [fact, acceptable(fact, order, days)] }
    # 同一处数字可能说的是别的 fact（「订单」「存栏」都按「只」算），对得上任何一个就不报。
    return if expectations.any? { |_, allowed| allowed.include?(value) }

    expectations.each do |fact, allowed|
      next if allowed.empty?

      add("facts", "kind" => "fact_mismatch", "confidence" => "possible", "factKey" => fact["fact"]["key"], "label" => fact["fact"]["label"],
                   "found" => value, "foundText" => mention["text"], "expected" => allowed, "day" => days.length == 1 ? days.first : nil,
                   "location" => located(entry, mention["start"]), "excerpt" => excerpt(text, mention["start"], mention["text"].length))
    end
  end

  def refers_to?(fact, text, mention, lead = "")
    value = mention["value"]
    return false unless value >= fact["low"] && value <= fact["high"]

    before = text[[mention["start"] - 3, 0].max...mention["start"]].to_s.rstrip
    return false if before.end_with?("第")

    after = text[mention["end"], 4].to_s.lstrip
    unit = fact["fact"]["unit"].to_s
    return true if !unit.empty? && after.start_with?(unit)
    return false if OTHER_UNITS.include?(after[0, 1]) || after.start_with?(*unit_words_other_than(unit))

    before_text = lead.empty? ? text[0...mention["start"]].to_s : "#{lead}\n#{text[0...mention['start']]}"
    window = before_text[-WINDOW_BEFORE..-1].to_s
    window = before_text if window.empty?
    window += text[mention["end"], WINDOW_AFTER].to_s
    fact["words"].any? { |word| window.include?(word) }
  end

  # 常见量词：数后面跟着别的量词，说的就是别的东西。
  def unit_words_other_than(unit)
    (%w[只 个 块 元 斤 头 条 张 箱 笼 亩 万 千] - [unit]).reject(&:empty?)
  end

  # 某处可能成立的账本值。days 为空表示读不出天数：按天生效的条目都算可能适用。
  def acceptable(fact, order, days)
    candidates = days.empty? ? [nil] : days
    candidates.flat_map { |day| values_on(fact["entries"], order, day) }.uniq
  end

  def values_on(entries, order, day)
    set = []
    entries.each do |entry|
      status = if entry["fromEpisode"]
                 entry["fromEpisode"] <= order
               elsif entry["fromDay"]
                 day.nil? ? :unknown : entry["fromDay"] <= day
               else
                 true
               end
      if status == true
        set = [entry["number"]]
      elsif status == :unknown
        set << entry["number"]
      end
    end
    set
  end

  # ---- 工具 ----

  def located(entry, index)
    location = entry["location"].reject { |key, _| key == "speaker" }
    text = entry["text"]
    location = location.merge("line" => text[0, index].count("\n") + 1) if text.include?("\n")
    location
  end

  def excerpt(text, index, length)
    from = [index - EXCERPT_RADIUS, 0].max
    snippet = text[from, length + (index - from) + EXCERPT_RADIUS].to_s.gsub(/\s+/, " ").strip
    "#{from.positive? ? '…' : ''}#{snippet}#{index + length + EXCERPT_RADIUS < text.length ? '…' : ''}"
  end

  def add(kind, issue)
    if @issues[kind].length >= MAX_ISSUES
      @truncated[kind] += 1
    else
      @issues[kind] << issue.reject { |_, value| value.nil? }
    end
  end
end
