# frozen_string_literal: true

require "json"
require_relative "creative_schema"
require_relative "review_record"
require_relative "drama_canon"

# 审核材料与内容指纹，逐字对应 ui/src/review.ts。
#
# 为什么服务端要再实现一遍：Agent 在聊天里跑 `review.run` 时没有页面。而结论里存的
# `basis` 是「审的是哪一份内容」的指纹，页面用它判断结论是否过期——两边拼出的
# 材料差一个字、指纹算法差一位，Agent 刚审过的对象在页面上就会显示「待复审」。
#
# 所以每一处都照抄 TS 的口径，包括几处看起来多余的细节：
# - 指纹按 UTF-16 码元算（JS 的 charCodeAt / length），不是按字节或码点；
# - trim 用 JS 的空白定义，比 Ruby 的 strip 宽（含全角空格、NBSP）；
# - 标签文字来自 schemas/review-labels.json，页面侧有测试保证与 i18n.ts 一致。
module ReviewMaterial
  module_function

  FENCE = "short_drama_review"
  # 专家席审稿（0.42.0-rc1，lib/review_panel.rb）：drama / episode 的第三个面。材料正文进指纹；
  # 相邻集梗概、角色小传、账本正文只作上下文（context），不进指纹——它们不是被审对象，
  # 账本改了由「设定账本版本」这一行带动结论过期。
  PANEL_ASPECT = "panel"
  MAX_IMAGES = 12
  LABELS = JSON.parse(File.read(File.expand_path("../../schemas/review-labels.json", __dir__), encoding: "UTF-8")).freeze
  JS_SPACE = "[\\s\\u00A0\\u1680\\u2000-\\u200A\\u2028\\u2029\\u202F\\u205F\\u3000\\uFEFF]"
  JS_TRIM = /\A#{JS_SPACE}+|#{JS_SPACE}+\z/.freeze
  VARIATION_SUFFIX = /\n*\(variation \d+\)\s*\z/.freeze

  def translate(locale, key, values = {})
    dictionary = LABELS.fetch(locale == "en" ? "en" : "zh-Hans")
    template = dictionary[key] || LABELS["en"][key] || key
    values.reduce(template) { |text, (name, value)| text.gsub("{#{name}}", value.to_s) }
  end

  def js_trim(value)
    value.to_s.gsub(JS_TRIM, "")
  end

  # 32 位 FNV-1a，按 UTF-16 码元。与 review.ts 的 fingerprint 同值。
  def fingerprint(text)
    units = text.to_s.encode("UTF-16LE").unpack("v*")
    hash = 0x811c9dc5
    units.each do |unit|
      hash ^= unit
      hash = (hash * 0x01000193) & 0xffffffff
    end
    "#{units.length.to_s(36)}-#{hash.to_s(36)}"
  end

  # target：{ "scope" => drama|character|episode|shot, "aspect" => ..., ids }。
  # media：{ "images" => bool, "videos" => bool }，对应页面的 hostMedia()。
  # 返回 nil 表示对象不存在或没有可审的东西。
  def build(drama, target, locale:, media:)
    lines = []
    label = ""
    image_paths = []
    context = ""
    panel = target["aspect"] == PANEL_ASPECT
    # 定妆图、资产参考图选定了一张时只审那一张（0.35.0-rc1）。以前所有候选一起送审，
    # 选定图干干净净，落选候选上的品牌皮带扣、真实店招照样让结论变成 warn。
    selected_id = nil
    push = lambda do |name, value|
      trimmed = js_trim(value)
      lines << "【#{name}】\n#{trimmed}" unless trimmed.empty?
    end
    t = ->(key, values = {}) { translate(locale, key, values) }

    case target["scope"]
    when "drama"
      label = t.call(panel ? "reviewTargetDramaPanel" : "reviewTargetDrama", "title" => drama["title"])
      push.call(t.call("reviewFieldTitle"), "#{drama['title']} / #{drama['genre']} / #{drama['format']}")
      # 专家席多看集数与每集时长（节奏、成本要按它估）。与页面 review.ts 同一顺序。
      push.call(t.call("reviewFieldFormatCount"), "#{drama['episodes'].length} / #{whole_number(drama['episodeDurationSeconds'])}") if panel
      push.call(t.call("logline"), drama["logline"])
      push.call(t.call("coreConflict"), drama["coreConflict"])
      push.call(t.call("reviewFieldAudienceTone"), [drama["audience"], drama["tone"]].reject { |value| value.to_s.empty? }.join(" / "))
      # 画面风格原样进成片提示词，要一起审。空着就不出现，旧剧的指纹不变。
      push.call(t.call("visualStyle"), drama["visualStyle"])
      push.call(t.call("seriesArc"), drama["arc"])
      push.call(t.call("reviewFieldCast"), drama["characters"].map { |item| "#{item['name']}：#{item['description']}" }.join("\n"))
      push.call(t.call("reviewFieldEpisodes"), drama["episodes"].map { |item| "#{t.call('episodeNumber', 'number' => item['order'])} #{item['title']}：#{item['summary']}" }.join("\n"))
      if panel
        push.call(t.call("reviewFieldCanonRev"), "rev #{DramaCanon.read(drama)['rev'].to_i}")
        context = DramaCanon.brief(DramaCanon.read(drama)).to_s
      end
    when "character"
      character = drama["characters"].find { |item| item["id"] == target["characterID"] }
      return nil unless character

      if target["aspect"] == "content"
        label = t.call("reviewTargetCharacter", "name" => character["name"])
        push.call(t.call("characterName"), character["name"])
        push.call(t.call("characterDescription"), character["description"])
        push.call(t.call("identityPrompt"), character["visualPrompt"])
      else
        candidates = Array(character["candidates"])
        selected = selected_candidate(candidates, character["selectedCandidateID"])
        push.call(t.call("identityPrompt"), character["visualPrompt"])
        if selected
          selected_id = selected["id"]
          label = t.call("reviewTargetCharacterSelectedImage", "name" => character["name"])
          push.call(t.call("reviewFieldSelectedImagePrompt"), candidate_prompts([selected]))
          push.call(t.call("reviewFieldReviewScope"), t.call("reviewScopeSelectedImage"))
          image_paths = selected_paths(selected) if media["images"]
        else
          label = t.call("reviewTargetCharacterImages", "name" => character["name"])
          push.call(t.call("reviewFieldImagePrompts"), candidate_prompts(candidates))
          image_paths = ordered_paths(candidates, []) if media["images"]
        end
      end
    when "episode"
      episode = drama["episodes"].find { |item| item["id"] == target["episodeID"] }
      return nil unless episode

      if target["aspect"] == "content" || panel
        label = t.call(panel ? "reviewTargetEpisodePanel" : "reviewTargetEpisode", "number" => episode["order"])
        push.call(t.call("episodeTitle"), episode["title"])
        push.call(t.call("episodeSummary"), episode["summary"])
        push.call(t.call("episodeScript"), episode["script"])
        if panel
          push.call(t.call("reviewFieldCanonRev"), "rev #{DramaCanon.read(drama)['rev'].to_i}")
          context = panel_episode_context(drama, episode, t)
        end
      else
        label = t.call("reviewTargetStoryboard", "number" => episode["order"])
        push.call(t.call("reviewFieldShots"), storyboard_text(episode, t))
      end
    when "shot"
      episode = drama["episodes"].find { |item| item["id"] == target["episodeID"] }
      shot = episode && Array(episode["shots"]).find { |item| item["id"] == target["shotID"] }
      return nil unless shot

      label = t.call("reviewTargetShotImages", "number" => shot["order"])
      push.call(t.call("startFrame"), shot["startPrompt"])
      push.call(t.call("endFrame"), shot["endPrompt"])
      candidates = Array(shot["startCandidates"]) + Array(shot["endCandidates"])
      push.call(t.call("reviewFieldImagePrompts"), candidate_prompts(candidates))
      image_paths = ordered_paths(candidates, [shot["selectedStartID"], shot["selectedEndID"]]) if media["images"]
    when "asset"
      asset = Array(drama["assets"]).find { |item| item["id"] == target["assetID"] }
      return nil unless asset

      if target["aspect"] == "content"
        label = t.call("reviewTargetAsset", "name" => asset["name"])
        push.call(t.call("assetName"), asset["name"])
        push.call(t.call("assetPrompt"), asset["prompt"])
        push.call(t.call("assetNotes"), asset["notes"])
        if asset["kind"] == "voice"
          push.call(t.call("voiceTranscript"), asset["referenceTranscript"])
          consent = asset["consent"].is_a?(Hash) ? asset["consent"] : {}
          push.call(t.call("voiceConsent"), [consent["status"], consent["grantedBy"]].map(&:to_s).reject(&:empty?).join(" / "))
        end
      else
        # 声音资产的候选是音频，没有可审的画面。
        return nil if asset["kind"] == "voice"

        push.call(t.call("assetPrompt"), asset["prompt"])
        images = Array(asset["candidates"]).reject { |item| item["mediaType"] == "audio" }
        selected = selected_candidate(images, asset["selectedCandidateID"])
        if selected
          selected_id = selected["id"]
          label = t.call("reviewTargetAssetSelectedImage", "name" => asset["name"])
          push.call(t.call("reviewFieldSelectedImagePrompt"), candidate_prompts([selected]))
          push.call(t.call("reviewFieldReviewScope"), t.call("reviewScopeSelectedImage"))
          image_paths = selected_paths(selected) if media["images"]
        else
          label = t.call("reviewTargetAssetImages", "name" => asset["name"])
          push.call(t.call("reviewFieldImagePrompts"), candidate_prompts(images))
          image_paths = ordered_paths(images, []) if media["images"]
        end
      end
    else
      return nil
    end

    return nil if lines.empty? && image_paths.empty?

    text = lines.join("\n\n")
    # 选定图的 ID 也进指纹：宿主不收图时 imagePaths 为空，两张候选的提示词又常常只差
    # 「(variation N)」，不记 ID 的话换选一张，指纹不变，页面看不出要复审。
    parts = [text] + image_paths
    parts << "selected:#{selected_id}" if selected_id
    { "label" => label, "text" => text, "basis" => fingerprint(parts.join("\n")),
      "imagePaths" => image_paths, "videoPaths" => [], "selectedCandidateID" => selected_id, "context" => context }
  end

  # 专家席审一集时的上下文（不进指纹）：剧的定位、整季弧光、每集时长、角色小传、相邻集梗概、设定账本。
  def panel_episode_context(drama, episode, t)
    episodes = Array(drama["episodes"]).sort_by { |item| item["order"].to_i }
    neighbours = episodes.select { |item| (item["order"].to_i - episode["order"].to_i).abs == 1 }
    section = lambda do |name, value|
      trimmed = js_trim(value)
      trimmed.empty? ? nil : "【#{name}】\n#{trimmed}"
    end
    [
      section.call(t.call("reviewFieldTitle"), "#{drama['title']} / #{drama['genre']} / #{drama['format']}"),
      section.call(t.call("reviewFieldFormatCount"), "#{episodes.length} / #{whole_number(drama['episodeDurationSeconds'])}"),
      section.call(t.call("logline"), drama["logline"]),
      section.call(t.call("reviewFieldAudienceTone"), [drama["audience"], drama["tone"]].reject { |value| value.to_s.empty? }.join(" / ")),
      section.call(t.call("seriesArc"), drama["arc"]),
      section.call(t.call("reviewFieldCast"), Array(drama["characters"]).map { |item| "#{item['name']}：#{item['description']}" }.join("\n")),
      section.call(t.call("panelFieldNeighbours"), neighbours.map { |item| "#{t.call('episodeNumber', 'number' => item['order'])} #{item['title']}：#{item['summary']}" }.join("\n")),
      DramaCanon.brief(DramaCanon.read(drama))
    ].compact.map(&:to_s).reject(&:empty?).join("\n\n")
  end

  # 与页面 `Number(value ?? 0) || 0` 同值：空、非数都是 0。
  def whole_number(value)
    Integer(value)
  rescue ArgumentError, TypeError
    value.is_a?(Numeric) ? value.to_i : 0
  end

  # selectedCandidateID 指向的那张候选；没选、或选的那张已不在候选里时返回 nil。
  def selected_candidate(candidates, selected_id)
    id = selected_id.to_s
    return nil if id.empty?

    candidates.find { |item| item["id"] == id }
  end

  def selected_paths(candidate)
    path = candidate["filePath"].to_s
    path.empty? ? [] : [path]
  end

  # 成片：宿主不收视频、任务没完成或本地没有镜像文件时返回 nil。
  def build_job(job, locale:, media:)
    return nil unless media["videos"] && job["state"] == "completed" && !job["mediaPath"].to_s.empty?

    title = js_trim(job["title"])
    title = translate(locale, "queueUntitled") if title.empty?
    prompt = js_trim(job["prompt"])
    text = ["【#{translate(locale, 'reviewFieldVideoTitle')}】\n#{title}",
            prompt.empty? ? "" : "【#{translate(locale, 'reviewFieldVideoPrompt')}】\n#{prompt}"].reject(&:empty?).join("\n\n")
    {
      "label" => translate(locale, "reviewTargetVideo", "title" => title), "text" => text,
      "basis" => fingerprint([job["id"], job["mediaPath"], job["mediaFile"] || ""].join("\n")),
      "imagePaths" => [], "videoPaths" => [job["mediaPath"]]
    }
  end

  def ordered_paths(candidates, selected)
    chosen = selected.compact.map(&:to_s).reject(&:empty?)
    sorted = chosen.map { |id| candidates.find { |item| item["id"] == id } }.compact +
             candidates.reject { |item| chosen.include?(item["id"]) }
    seen = {}
    sorted.map { |item| item["filePath"] }.select do |path|
      next false if path.to_s.empty? || seen[path]

      seen[path] = true
    end.first(MAX_IMAGES)
  end

  def candidate_prompts(candidates)
    seen = {}
    candidates.map { |item| js_trim(item["prompt"].to_s.sub(VARIATION_SUFFIX, "")) }.select do |prompt|
      next false if prompt.empty? || seen[prompt]

      seen[prompt] = true
    end.join("\n---\n")
  end

  def storyboard_text(episode, t)
    Array(episode["shots"]).each_with_index.sort_by { |shot, index| [shot["order"].to_i, index] }.map(&:first).map do |shot|
      dialogue = Array(shot["dialogue"]).reject { |line| js_trim(line["text"]).empty? }.map { |line| "#{line['speaker']}：#{line['text']}" }.join("\n")
      actions = [shot["actionStart"], shot["actionEnd"], shot["cameraIntent"]].reject { |value| js_trim(value).empty? }.join(" / ")
      ["#{t.call('shotNumber', 'number' => shot['order'])} #{shot['title']}", shot["summary"].to_s, dialogue, actions]
        .reject { |value| js_trim(value).empty? }.join("\n")
    end.join("\n\n")
  end

  # 与 review.ts 的 parseReview 同一套：先认约定围栏，再在全文里找第一个配平的
  # 对象；校验不过就算失败，不猜。列了 block 级问题就按 block 算。
  def parse(raw)
    named = raw.to_s.match(/```#{FENCE}(?![A-Za-z0-9_])\s*([\s\S]*?)```/i)
    [named && named[1], raw.to_s].compact.each do |candidate|
      json, truncated = first_balanced_object(candidate)
      next if json.nil? || truncated

      value = begin
        JSON.parse(json)
      rescue JSON::ParserError
        next
      end
      next unless CreativeSchema.valid?("review", value)

      # level 缺省或写错时按 severity 推（ReviewRecord.level_for），与页面 parseReview 一致。
      issues = value["issues"].map { |issue| issue.merge("level" => ReviewRecord.level_for(issue)) }
      status = if issues.any? { |issue| issue["severity"] == "block" } then "block"
               elsif !issues.empty? && value["status"] == "pass" then "warn"
               else value["status"]
               end
      return value.merge("status" => status, "issues" => issues)
    end
    nil
  end

  def first_balanced_object(text)
    start = text.index("{")
    return [nil, false] unless start

    depth = 0
    in_string = false
    escaped = false
    text.chars.each_with_index do |character, index|
      next if index < start

      if escaped
        escaped = false
        next
      end
      if character == "\\"
        escaped = true
        next
      end
      if character == "\""
        in_string = !in_string
        next
      end
      next if in_string

      if character == "{"
        depth += 1
      elsif character == "}"
        depth -= 1
        return [text[start..index], false] if depth.zero?
      end
    end
    [text[start..], true]
  end
end
