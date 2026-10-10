# frozen_string_literal: true

# `drama.get_stage_context`：把页面在各个创作阶段喂给模型的那套材料交给 Agent。
#
# 页面每次请模型写东西，都按同一个顺序装配：阶段技能 + 合规红线（system），
# 剧名题材基调、梗概、整季弧光、角色表、当前对象、相邻集或本集分镜（context）。
# Agent 自己就是那个写东西的模型，缺的正是这份材料——自己拼的话，要么漏了合规
# 红线，要么把整部剧塞进上下文。
#
# 与页面的区别只在「怎么交稿」：页面让模型输出 ```short_drama_*``` 代码块再由
# 前端解析；Agent 直接调写入工具，所以这里返回 `write` 说明该调哪个工具、能写
# 哪些字段，而不附代码块格式要求。
require_relative "drama_canon"

class StageContext
  STAGES = %w[planning characters episodes script storyboard shot frames appearance scene prop voice package].freeze
  ASSET_STAGES = { "appearance" => %w[appearance], "scene" => %w[scene sceneVariant], "prop" => %w[prop], "voice" => %w[voice] }.freeze
  KIND_NAMES = { "appearance" => "造型", "scene" => "场景", "sceneVariant" => "场景变体", "prop" => "道具", "voice" => "声音" }.freeze
  # 各阶段带账本的哪几块（0.37.0-rc1）。首尾帧是静态画面：数字和说话人用不上。
  CANON_SECTIONS = { "frames" => %w[props visualRules bannedTerms] }.freeze

  WRITE_GUIDES = {
    "planning" => {
      "tool" => "drama.confirm_plan (new drama, only after the user confirms the plan) or drama.save_draft scope=drama commit=true (revise)",
      "fields" => %w[title genre format episodeCount episodeDurationSeconds logline coreConflict audience tone visualStyle arc characters episodes],
      "note" => "tone is writing guidance for the writers and never reaches image or video models. visualStyle is one visual-only sentence (light, colour, texture, lens, era) that goes verbatim into every video prompt: no story, dialogue, rules or instructions. Once the plan is saved, record the series canon (numbers that evolve, props, visual rules, banned terms, functional speakers) with drama.save_canon."
    },
    "characters" => { "tool" => "drama.save_draft scope=character commit=true", "fields" => %w[name description visualPrompt] },
    "episodes" => {
      "tool" => "drama.save_episode_drafts commit=true",
      "fields" => %w[order summary title script],
      "note" => "episodes is an array; order is required. Episodes whose script would stay empty are kept as drafts. Numbers, props and wording follow the canon section of the context; a story day goes in as 第N天 so drama.check_consistency can place it."
    },
    "script" => {
      "tool" => "drama.save_draft scope=episode commit=true", "fields" => %w[title summary script],
      "note" => "Write dialogue as one line per speaker: 角色名：台词. Speakers must be in the cast or the canon's functional speakers; numbers, props and wording follow the canon section. Need a number or prop the canon lacks? Say so instead of inventing it (drama.save_canon after the user agrees)."
    },
    "storyboard" => {
      "tool" => "drama.save_shot_drafts commit=true (create=true when the episode has no shots yet)",
      "fields" => %w[order title summary dialogue actionStart actionEnd cameraIntent soundscape music caption startPrompt endPrompt duration],
      "note" => "shots is an array; order is required. dialogue is [{speaker, text}]. duration is 4 to 15 seconds (video provider contract; 4 is the floor, lines get rushed below 6). soundscape is ambient and action sound only (English, 1-4 sentences, no dialogue); music is off-screen score the characters cannot hear, or N/A. caption is on-screen text such as a day count (第 3 天), overlaid at compose time; keep it out of startPrompt/endPrompt."
    },
    "shot" => {
      "tool" => "drama.save_shot_drafts commit=true (one entry)",
      "fields" => %w[order title summary dialogue actionStart actionEnd cameraIntent soundscape music caption startPrompt endPrompt duration]
    },
    "frames" => {
      "tool" => "drama.save_draft scope=shot commit=true", "fields" => %w[startPrompt endPrompt],
      "note" => "Frame prompts must be drawable static descriptions. Reuse the identity wording of the characters in the shot."
    },
    "appearance" => {
      "tool" => "drama.save_asset kind=appearance characterID (new) or drama.save_draft scope=asset assetID commit=true (revise)",
      "fields" => %w[name prompt notes category],
      "note" => "prompt describes wardrobe, hair, makeup or state only; the face and body stay in the character's visualPrompt. category is wardrobe, hair, makeup, age or state."
    },
    "scene" => {
      "tool" => "drama.save_asset kind=scene (parentSceneID optional) or kind=sceneVariant sceneID (lighting optional); revise with drama.save_draft scope=asset assetID commit=true",
      "fields" => %w[name prompt notes],
      "note" => "prompt describes the space; notes are continuity rules (what must not change). A variant only describes what differs from its scene."
    },
    "prop" => {
      "tool" => "drama.save_asset kind=prop or drama.save_draft scope=asset assetID commit=true",
      "fields" => %w[name prompt notes]
    },
    "voice" => {
      "tool" => "drama.save_asset kind=voice characterID (language, dialect, referenceTranscript, providerVoiceID, presets, consent) or drama.save_draft scope=asset assetID commit=true",
      "fields" => %w[name prompt notes referenceTranscript],
      "note" => "consent must be confirmed by the user (status granted, grantedBy) before voice.generate works; never claim consent yourself."
    },
    "package" => {
      "tool" => "drama.set_reference_package (then drama.preview_reference_package purpose=image|video to check)",
      "fields" => %w[sceneID sceneVariantID cast propIDs generationMode excluded],
      "note" => "cast is [{characterID, appearanceID, voiceID, role, screenPosition, appearanceChange}]. Bind only what appears in this shot; explain appearance changes between adjacent shots."
    }
  }.freeze

  def initialize(drama_service:, skills:)
    @dramas = drama_service
    @skills = skills
  end

  def get(arguments)
    stage = arguments["stage"].to_s
    return failure("invalid_stage", "stage must be one of #{STAGES.join(', ')}.") unless STAGES.include?(stage)

    system = @skills.compose(stage: stage, common: Array(arguments["common"]))["system"]
    if stage == "planning" && arguments["dramaID"].to_s.empty?
      return result(stage, system, "", nil)
    end

    loaded = @dramas.get("id" => arguments["dramaID"])
    return loaded unless loaded["ok"]

    drama = loaded["drama"]
    built = case stage
            when "planning" then [planning_context(drama), view(drama, DramaService::DRAFT_TEXT_FIELDS["drama"].keys)]
            when "characters" then character_context(drama, arguments)
            when "episodes" then batch_context(drama, arguments)
            when "script" then script_context(drama, arguments)
            when "storyboard", "shot" then storyboard_context(drama, arguments, stage)
            when "frames" then frames_context(drama, arguments)
            when "appearance", "scene", "prop", "voice" then asset_context(drama, arguments, stage)
            when "package" then package_context(drama, arguments)
            end
    return built if built.is_a?(Hash)

    # canonRev：要改账本时 drama.save_canon 的 expectedRev。
    result(stage, system, built[0], built[1]).merge("canonRev" => DramaCanon.read(drama)["rev"].to_i)
  end

  private

  # 资产阶段：现有同类资产的清单 + 当前资产（如果 assetID 给了）+ 相关角色或场景。
  def asset_context(drama, arguments, stage)
    assets = Array(drama["assets"]).reject { |asset| asset["archived"] }
    kinds = ASSET_STAGES.fetch(stage)
    # 当前资产在全部资产里找：归档的也能改文字，只是不出现在清单里。
    current = arguments["assetID"].to_s.empty? ? nil : find(drama["assets"], arguments["assetID"])
    return failure("asset_not_found", "Asset was not found.") if !arguments["assetID"].to_s.empty? && current.nil?

    parts = [header(drama), labelled("梗概", drama["logline"])]
    case stage
    when "appearance", "voice"
      character = find(drama["characters"], current ? current["characterID"] : arguments["characterID"])
      return failure("character_not_found", "Character was not found; pass characterID.") unless character
      merged_character = merged(character)
      parts << "角色：#{merged_character['name']}\n人物小传：#{or_placeholder(merged_character['description'], '（还没写小传）')}\n身份视觉提示词：#{or_placeholder(merged_character['visualPrompt'], '（还没写）')}"
      mine = assets.select { |asset| kinds.include?(asset["kind"]) && asset["characterID"] == character["id"] }
      parts << "该角色现有#{KIND_NAMES.fetch(stage)}：\n#{mine.empty? ? '（还没有）' : mine.map { |asset| asset_line(asset) }.join("\n")}"
    when "scene"
      scenes = assets.select { |asset| asset["kind"] == "scene" }
      variants = assets.select { |asset| asset["kind"] == "sceneVariant" }
      tree = scenes.map do |scene|
        parent = scene["parentSceneID"].to_s.empty? ? "" : "（属于：#{find(scenes, scene['parentSceneID'])&.fetch('name') || '?'}）"
        own = variants.select { |variant| variant["sceneID"] == scene["id"] }.map { |variant| "  变体 #{asset_line(variant)}" }
        ["#{asset_line(scene)}#{parent}", *own].join("\n")
      end
      parts << "现有场景：\n#{tree.empty? ? '（还没有）' : tree.join("\n")}"
    when "prop"
      props = assets.select { |asset| asset["kind"] == "prop" }
      parts << "现有道具：\n#{props.empty? ? '（还没有）' : props.map { |asset| asset_line(asset) }.join("\n")}"
    end
    parts << "当前#{KIND_NAMES.fetch(current['kind'])}：#{JSON.generate(view(current, DramaService::DRAFT_TEXT_FIELDS['asset'].keys)['current'])}" if current
    target = current ? view(current, DramaService::DRAFT_TEXT_FIELDS["asset"].keys).merge("kind" => current["kind"]) : nil
    [parts.compact.join("\n\n"), target]
  end

  # 参考包阶段：本镜摘要 + 全部可绑定的资产目录 + 当前参考包。
  def package_context(drama, arguments)
    episode = find(drama["episodes"], arguments["episodeID"])
    return failure("episode_not_found", "Episode was not found.") unless episode
    shot = find(Array(episode["shots"]), arguments["shotID"])
    return failure("shot_not_found", "Shot was not found.") unless shot

    assets = Array(drama["assets"]).reject { |asset| asset["archived"] }
    cast = drama["characters"].map do |character|
      locked = Array(character["candidates"]).any? { |entry| entry["id"] == character["selectedCandidateID"] }
      looks = assets.select { |asset| asset["kind"] == "appearance" && asset["characterID"] == character["id"] }.map { |asset| "#{asset['name']}（#{asset['id']}）" }
      voices = assets.select { |asset| asset["kind"] == "voice" && asset["characterID"] == character["id"] }.map { |asset| "#{asset['name']}（#{asset['id']}）" }
      "#{character['name']}（#{character['id']}，#{locked ? '已锁定形象' : '未锁定形象'}）造型：#{looks.empty? ? '无' : looks.join('、')}；声音：#{voices.empty? ? '无' : voices.join('、')}"
    end
    scenes = assets.select { |asset| asset["kind"] == "scene" }.map do |scene|
      own = assets.select { |asset| asset["kind"] == "sceneVariant" && asset["sceneID"] == scene["id"] }.map { |asset| "#{asset['name']}（#{asset['id']}）" }
      "#{scene['name']}（#{scene['id']}）变体：#{own.empty? ? '无' : own.join('、')}"
    end
    props = assets.select { |asset| asset["kind"] == "prop" }.map { |asset| "#{asset['name']}（#{asset['id']}）" }
    previous = sorted(Array(episode["shots"])).select { |entry| entry["order"].to_i < shot["order"].to_i }.last
    context = [
      header(drama),
      "第 #{episode['order']} 集第 #{shot['order']} 镜：\n#{digest(shot)}",
      previous && previous["package"].is_a?(Hash) ? "上一镜参考包：#{JSON.generate(previous['package'])}" : nil,
      "角色：\n#{cast.empty? ? '（没有角色）' : cast.join("\n")}",
      "场景：\n#{scenes.empty? ? '（没有场景资产）' : scenes.join("\n")}",
      "道具：#{props.empty? ? '（没有道具资产）' : props.join('、')}",
      "只绑定本镜真正出现的对象；造型与上一镜不同时写 appearanceChange。"
    ].compact.join("\n\n")
    target = { "episodeID" => episode["id"], "shotID" => shot["id"], "package" => shot["package"] }
    [context, target]
  end

  def asset_line(asset)
    current = merged(asset)
    extra = asset["kind"] == "appearance" ? "，#{asset['category']}" : ""
    "#{current['name']}（#{asset['id']}#{extra}）：#{or_placeholder(current['prompt'], '（还没写描述）')}"
  end

  def result(stage, system, context, target)
    { "ok" => true, "stage" => stage, "system" => system, "context" => context, "target" => target, "write" => WRITE_GUIDES.fetch(stage) }
  end

  def planning_context(drama)
    [
      header(drama),
      labelled("画面风格", drama["visualStyle"]),
      labelled("梗概", drama["logline"]),
      labelled("核心冲突", drama["coreConflict"]),
      labelled("整季弧光", drama["arc"]),
      "角色表：\n#{cast(drama, :description)}",
      canon_brief(drama, "planning")
    ].compact.join("\n\n")
  end

  def character_context(drama, arguments)
    character = find(drama["characters"], arguments["characterID"])
    return failure("character_not_found", "Character was not found.") unless character

    target = view(character, %w[name description visualPrompt])
    context = [
      header(drama), labelled("梗概", drama["logline"]),
      "当前角色：#{JSON.generate(target['current'])}",
      "其他角色：\n#{cast(drama, :description, except: character['id'])}",
      canon_brief(drama, "characters")
    ].compact.join("\n\n")
    [context, target]
  end

  # 与页面「AI 串起全部 N 集」一致：本批现有梗概 + 前后各一集。默认整部剧一批，
  # from / to 可以收窄。
  def batch_context(drama, arguments)
    episodes = sorted(drama["episodes"])
    return failure("episode_not_found", "This drama has no episodes.") if episodes.empty?

    from = integer(arguments["from"], 1)
    to = integer(arguments["to"], episodes.last["order"].to_i)
    in_batch = episodes.select { |episode| episode["order"].to_i.between?(from, to) }
    neighbours = episodes.select { |episode| [from - 1, to + 1].include?(episode["order"].to_i) }
    existing = in_batch.map do |episode|
      current = merged(episode)
      mark = current["script"].to_s.strip.empty? ? " ［正文还没写］" : ""
      "第 #{episode['order']} 集#{mark}：#{or_placeholder(current['summary'], '（还没写梗概）')}"
    end
    context = [
      header(drama), labelled("梗概", drama["logline"]), labelled("整季弧光", drama["arc"]),
      "角色表：\n#{cast(drama, :description)}",
      canon_brief(drama, "episodes"),
      "本批（第 #{from} 到 #{to} 集）现有梗概：\n#{existing.join("\n")}",
      neighbours.empty? ? nil : "相邻集梗概：\n#{neighbours.map { |episode| "第 #{episode['order']} 集：#{or_placeholder(merged(episode)['summary'], '（还没写梗概）')}" }.join("\n")}"
    ].compact.join("\n\n")
    [context, { "from" => from, "to" => to, "episodeIDs" => in_batch.map { |episode| episode["id"] } }]
  end

  def script_context(drama, arguments)
    episode = find(drama["episodes"], arguments["episodeID"])
    return failure("episode_not_found", "Episode was not found.") unless episode

    target = view(episode, %w[title summary script])
    neighbours = sorted(drama["episodes"]).select { |entry| (entry["order"].to_i - episode["order"].to_i).abs == 1 }
    context = [
      header(drama), labelled("梗概", drama["logline"]), labelled("整季弧光", drama["arc"]),
      "角色表：\n#{cast(drama, :description)}",
      canon_brief(drama, "script"),
      "第 #{episode['order']} 集：#{target['current']['title']} / #{target['current']['summary']}",
      neighbours.empty? ? nil : "相邻集梗概：\n#{neighbours.map { |entry| "第 #{entry['order']} 集：#{merged(entry)['summary']}" }.join("\n")}",
      target["current"]["script"].to_s.strip.empty? ? "这一集还没有正文。这次必须写出 script，写出完整的台词正文，不要只回梗概。" : nil
    ].compact.join("\n\n")
    [context, target]
  end

  def storyboard_context(drama, arguments, stage)
    episode = find(drama["episodes"], arguments["episodeID"])
    return failure("episode_not_found", "Episode was not found.") unless episode

    shots = sorted(Array(episode["shots"]))
    shot = nil
    if stage == "shot"
      shot = find(shots, arguments["shotID"])
      return failure("shot_not_found", "Shot was not found.") unless shot
    end
    current = merged(episode)
    context = [
      header(drama),
      "角色表：\n#{cast(drama, :visual)}",
      canon_brief(drama, stage),
      "第 #{episode['order']} 集：#{current['title']} / #{current['summary']}",
      current["script"].to_s.strip.empty? ? nil : current["script"],
      shots.empty? ? "根据本集完整剧本生成第一版分镜：每镜包含 order、title、summary、dialogue、actionStart、actionEnd、cameraIntent 和 duration。" : nil,
      "本集现有分镜：\n#{shots.empty? ? '（还没有分镜）' : shots.map { |entry| digest(entry) }.join("\n")}",
      shot ? "只改第 #{shot['order']} 镜。" : nil
    ].compact.join("\n\n")
    target = shot ? view(shot, DramaService::DRAFT_TEXT_FIELDS["shot"].keys + %w[order dialogue duration]) : { "episodeID" => episode["id"], "shots" => shots.length }
    [context, target]
  end

  def frames_context(drama, arguments)
    episode = find(drama["episodes"], arguments["episodeID"])
    return failure("episode_not_found", "Episode was not found.") unless episode

    shot = find(Array(episode["shots"]), arguments["shotID"])
    return failure("shot_not_found", "Shot was not found.") unless shot

    target = view(shot, %w[title summary actionStart actionEnd cameraIntent startPrompt endPrompt dialogue duration])
    context = [
      header(drama),
      labelled("画面风格（光线、色调、质感跟它走）", drama["visualStyle"]),
      "角色表（身份视觉提示词）：\n#{cast(drama, :visual)}",
      canon_brief(drama, "frames"),
      "第 #{episode['order']} 集第 #{shot['order']} 镜：\n#{digest(shot)}",
      "首帧对应开场状态，尾帧对应收尾状态；提示词写成画得出来的静态画面，出现的角色沿用其身份视觉提示词里的不可改变项。"
    ].compact.join("\n\n")
    [context, target]
  end

  # 全剧设定账本那一段（lib/drama_canon.rb#brief）。账本是空的就不出现。
  def canon_brief(drama, stage)
    DramaCanon.brief(DramaCanon.read(drama), sections: CANON_SECTIONS.fetch(stage, DramaCanon::LISTS))
  end

  def header(drama)
    "短剧：#{drama['title']} / #{drama['genre']} / #{drama['tone']}"
  end

  def labelled(label, value)
    value.to_s.strip.empty? ? nil : "#{label}：#{value}"
  end

  # :description 给写剧本用（人物小传），:visual 给分镜和首尾帧用（身份视觉提示词）。
  def cast(drama, kind, except: nil)
    lines = drama["characters"].reject { |character| character["id"] == except }.map do |character|
      current = merged(character)
      detail = kind == :visual ? (current["visualPrompt"].to_s.strip.empty? ? current["description"] : current["visualPrompt"]) : current["description"]
      "#{current['name']}：#{or_placeholder(detail, '（还没写小传）')}"
    end
    lines.empty? ? "（策划里还没带出角色）" : lines.join("\n")
  end

  def digest(shot)
    current = merged(shot)
    lines = Array(current["dialogue"]).map { |line| "#{line['speaker']}：#{line['text']}" }.join(" / ")
    [
      "第 #{current['order']} 镜 #{current['title']}（#{current['duration']} 秒，#{or_placeholder(current['cameraIntent'], '未定运镜')}）",
      current["summary"].to_s.empty? ? nil : "  剧情：#{current['summary']}",
      lines.empty? ? nil : "  台词：#{lines}",
      current["actionStart"].to_s.empty? ? nil : "  开场：#{current['actionStart']}",
      current["actionEnd"].to_s.empty? ? nil : "  收尾：#{current['actionEnd']}"
    ].compact.join("\n")
  end

  # 当前对象：正式内容与未采用草稿分开给。Agent 要改的是正式内容，但草稿里可能
  # 有用户手改到一半的东西，覆盖之前应当看见。
  def view(node, fields)
    current = fields.each_with_object({}) { |field, result| result[field] = node[field] }
    draft = node["draft"].is_a?(Hash) && !node["draft"].empty? ? node["draft"] : nil
    { "id" => node["id"], "rev" => node["rev"].to_i, "draftRev" => node["draftRev"].to_i, "current" => current, "pendingDraft" => draft }
  end

  # 写材料时按页面的口径读「草稿优先」：用户看到的就是这一版。
  def merged(node)
    node.merge(node["draft"].is_a?(Hash) ? node["draft"] : {})
  end

  def find(collection, id)
    Array(collection).find { |entry| entry["id"] == id.to_s }
  end

  def sorted(collection)
    Array(collection).sort_by { |entry| entry["order"].to_i }
  end

  def or_placeholder(value, placeholder)
    value.to_s.strip.empty? ? placeholder : value.to_s
  end

  def integer(value, fallback)
    Integer(value)
  rescue ArgumentError, TypeError
    fallback
  end

  def failure(code, message)
    { "ok" => false, "error" => { "code" => code, "message" => message } }
  end
end
