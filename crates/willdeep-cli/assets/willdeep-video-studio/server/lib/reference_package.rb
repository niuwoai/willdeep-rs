# frozen_string_literal: true

require_relative "media_ref"
require_relative "h3_prompt"
require_relative "wav_tools"
require_relative "qa_remediation"
require_relative "identity_text"

# 镜头参考包的编译：把「这一镜绑了哪个场景、哪些角色穿什么、带哪些道具、参考哪几段
# 视频」展开成有顺序的参考槽位，按用途（出图 / 出视频）与 Provider 能力裁到上限，
# 再生成提示词。
#
# 只有这一份实现：页面的「本镜将发送给模型的参考包」面板、Agent 的 dryRun、
# image.generate 与 video.generate 真正发请求前用的都是它。
#
# 出视频（Tsingfly Hub 的 MiniMax-H3，2026-09-17 核实）：
# - t2va 不带任何素材；fl2va 只带一张首帧；
# - ref2va 只收两种互斥组合：1~3 段参考视频（input_references），或恰好一张图加
#   恰好一段音频（input_reference + audio_reference）。设计稿里「最多 9 图 3 视频 3 音频
#   混放」的矩阵当前部署不支持。所以视频侧的参考包是：首帧（<Picture 1>）+ 对白音轨
#   （<Audio 1>），或前几镜的成片（<Video N>）；身份图、造型图、场景图只进提示词。
# - 出图（purpose=image）仍按宿主的 9 张参照图上限带身份、造型、场景、道具图。
module ReferencePackage
  module_function

  # 宿主 ai.generateImage / willdeep/images/generate 一次最多收这么多张参照图。
  IMAGE_REFERENCE_LIMIT = 9
  MODES = %w[auto t2va fl2va ref2va].freeze
  COMBOS = %w[videos image_audio].freeze
  ROLES = %w[lead supporting extra].freeze
  POSITIONS = ["", "foreground-left", "foreground-right", "center", "background"].freeze
  ROLE_ORDER = { "lead" => 0, "supporting" => 1, "extra" => 2 }.freeze
  PURPOSES = %w[image video].freeze
  AUDIO_RETENTIONS = %w[fully_copy reference].freeze
  MAX_VIDEO_REFS = 3
  DIALOGUE_GAP_MS = 300

  # 旧剧没写 visualStyle 时，tone 只有长得像「冷峻、克制」这种一串短形容词才借来当
  # 画面风格。宽度按 CJK 算 2、ASCII 算 1：一段超过 12（中文 6 字、英文 12 个字母）
  # 就是句子，是写给编剧的话。
  TONE_LOOK_MAX_SEGMENTS = 5
  TONE_LOOK_MAX_SEGMENT_WIDTH = 12
  TONE_LOOK_SEPARATORS = %r{[、，,／/|]+}.freeze
  TONE_LOOK_SENTENCE_MARKS = /[。！？；;!?：:"“”「」『』（）()\[\]]/.freeze
  # 结构上像形容词、字面上却是写作要求的词：出现就不借。
  TONE_LOOK_WRITER_WORDS = %w[台词 对白 说话 每集 结尾 剧情 不写 不要 避免 必须].freeze
  TRAILING_STOPS = /[。.．!！]+\z/.freeze
  # CJK 部首补充区的起点；从这里往后（汉字、全角标点、假名、谚文）按双宽计。
  WIDE_CHAR_FROM = 0x2E80

  # capabilities：第 7 节的能力声明；purpose=image 时只用图片上限。
  # cast_override：image.generate 的 castIDs，给了就只带这些角色的身份图。
  # jobs：视频任务列表（带 mediaPath），参考视频从里面找成片。
  # directives：成片提示词的预防句（{id, category, text}）；nil 用内置默认（一镜到底），
  # 调用方一般传经验库算好的那份（QALessons#prevention_directives）。分镜运镜明写切镜时
  # 一镜到底类会被跳过。extra_directives：自动重做时按质检问题加的补救句，不按运镜过滤。
  # 两者只进视频提示词，出图不用。
  def compile(drama, episode, shot, purpose:, capabilities:, mode: nil, cast_override: nil, media_root:, jobs: [],
              directives: nil, extra_directives: [])
    purpose = purpose.to_s
    return failure("invalid_purpose", "purpose must be image or video.") unless PURPOSES.include?(purpose)

    package = shot["package"].is_a?(Hash) ? shot["package"] : {}
    assets = Array(drama["assets"])
    warnings = []
    subjects = []
    slots = []
    excluded = Array(package["excluded"]).map(&:to_s)

    bindings = cast_bindings(drama, shot, package, cast_override, episode)
    bindings.each_with_index do |binding, index|
      character = find(drama["characters"], binding["characterID"])
      next unless character

      subject = { "index" => index + 1, "characterID" => character["id"], "name" => character["name"].to_s,
                  "identity" => character["visualPrompt"].to_s,
                  "role" => binding["role"], "screenPosition" => binding["screenPosition"].to_s,
                  "appearanceID" => binding["appearanceID"], "voiceID" => binding["voiceID"], "appearanceChange" => binding["appearanceChange"].to_s,
                  "appearance" => "", "appearanceName" => "" }
      identity = MediaRef.selected(character, media_root)
      key = "identity:#{character['id']}"
      if identity.nil?
        warnings << warning("identity_missing", "「#{character['name']}」还没有选定形象图，本镜不会带上这个角色的脸。", "characterID" => character["id"])
      elsif !excluded.include?(key)
        slots << slot(key, "identity", "character", character["id"], character["name"], identity, true, "#{character['name']} 的身份")
      end

      appearance = binding["appearanceID"].to_s.empty? ? nil : find(assets, binding["appearanceID"])
      if binding["inferred"]
        subject["inferred"] = true
        carried = appearance ? "，造型沿用第 #{binding['appearanceFromOrder']} 镜的「#{appearance['name']}」" : ""
        warnings << warning("cast_inferred", "本镜参考包没有绑定出场角色，按镜头文字带上了「#{character['name']}」#{carried}；在参考包里绑定角色与造型会更稳。",
                            "characterID" => character["id"])
      end
      if appearance
        subject["appearanceName"] = appearance["name"].to_s
        subject["appearance"] = appearance["prompt"].to_s
        # 绑了造型：身份只留长相锚点，穿着全听造型（lib/identity_text.rb，0.38.0-rc1）。
        subject["identity"] = IdentityText.for_cast(character["visualPrompt"], subject["appearance"], appearance["category"])
        subject["wardrobeFromAppearance"] = !subject["appearance"].strip.empty? && !IdentityText::KEEPS_WARDROBE.include?(appearance["category"].to_s)
        warnings << warning("asset_archived", "造型「#{appearance['name']}」已归档，仍会按原样带上。", "assetID" => appearance["id"]) if appearance["archived"]
        image = MediaRef.selected(appearance, media_root)
        akey = "appearance:#{appearance['id']}"
        if image.nil?
          warnings << warning("appearance_no_image", "造型「#{appearance['name']}」还没有选定参考图。", "assetID" => appearance["id"])
        elsif !excluded.include?(akey)
          slots << slot(akey, "appearance", "appearance", appearance["id"], appearance["name"], image, false, "#{character['name']} 的造型「#{appearance['name']}」")
        end
      elsif !binding["appearanceID"].to_s.empty?
        warnings << warning("asset_missing", "绑定的造型不存在。", "assetID" => binding["appearanceID"])
      end
      # 穿着不由造型决定时（没绑造型，或造型只管发型 / 妆容 / 年龄）：身份里的日常装压成一行，
      # 出图的参考图说明与 fl2va 的主体描述都写上它（0.39.0-rc1，lib/identity_text.rb#wardrobe_line）。
      subject["wardrobe"] = subject["wardrobeFromAppearance"] ? "" : IdentityText.wardrobe_line(character["visualPrompt"])

      voice = binding["voiceID"].to_s.empty? ? nil : find(assets, binding["voiceID"])
      if voice
        subject["voiceName"] = voice["name"].to_s
        subject["language"] = voice["language"].to_s
        warnings << warning("voice_consent_missing", "声音「#{voice['name']}」还没有确认授权。", "assetID" => voice["id"]) unless consent_granted?(voice)
      elsif !binding["voiceID"].to_s.empty?
        warnings << warning("asset_missing", "绑定的声音不存在。", "assetID" => binding["voiceID"])
      end
      subjects << subject
    end

    scene_block = scene_slot(drama, package, assets, excluded, warnings, media_root)
    slots << scene_block[:slot] if scene_block[:slot]

    props = []
    Array(package["propIDs"]).each do |prop_id|
      prop = find(assets, prop_id)
      next warnings << warning("asset_missing", "绑定的道具不存在。", "assetID" => prop_id) unless prop
      props << { "id" => prop["id"], "name" => prop["name"].to_s, "prompt" => prop["prompt"].to_s }
      warnings << warning("asset_archived", "道具「#{prop['name']}」已归档，仍会按原样带上。", "assetID" => prop["id"]) if prop["archived"]
      image = MediaRef.selected(prop, media_root)
      key = "prop:#{prop['id']}"
      next warnings << warning("prop_no_image", "道具「#{prop['name']}」还没有选定参考图。", "assetID" => prop["id"]) if image.nil?
      next if excluded.include?(key)
      slots << slot(key, "prop", "prop", prop["id"], prop["name"], image, false, "道具「#{prop['name']}」")
    end

    ir = build_ir(drama, shot, package, subjects, scene_block[:description], props, assets)

    if purpose == "image"
      limit = IMAGE_REFERENCE_LIMIT
      slots, dropped = trim(slots, limit, warnings)
      slots.each_with_index { |entry, index| entry["order"] = index + 1 }
      return {
        "ok" => true, "purpose" => purpose, "mode" => "image", "modeReason" => "image", "limit" => limit,
        "slots" => slots, "dropped" => dropped, "warnings" => warnings, "subjects" => subjects,
        "scene" => scene_block[:description], "promptIR" => image_prompt_ir(ir, slots), "ir" => ir,
        "package" => shot["package"].is_a?(Hash) ? shot["package"] : nil,
        "overLimit" => slots.count { |entry| entry["required"] } > limit
      }
    end

    kept, skipped = QARemediation.applicable(directives.nil? ? QARemediation.builtin_prevention : directives, shot["cameraIntent"])
    extra = Array(extra_directives).map { |entry| QARemediation.normalize(entry) }.compact.map { |entry| { "source" => "remediation" }.merge(entry) }
    ir["directives"] = QARemediation.dedupe(kept + extra)
    ir["directivesSkipped"] = skipped
    video_compile(drama, shot, package, capabilities, mode, slots, warnings, subjects, scene_block[:description], ir, media_root, jobs)
  end

  # ---- 视频侧 ----

  def video_compile(drama, shot, package, capabilities, mode, image_slots, warnings, subjects, scene, ir, media_root, jobs)
    start = MediaRef.selected({ "candidates" => shot["startCandidates"], "selectedCandidateID" => shot["selectedStartID"] }, media_root)
    track = dialogue_track(shot, media_root, capabilities, warnings)
    videos = video_refs(package, jobs, warnings)
    ir["videos"] = videos.map { |video| { "label" => video["label"], "hasAudio" => true } }

    resolved = resolve_video_mode(mode || package["generationMode"], capabilities, start, track, videos)
    return resolved unless resolved["ok"]

    visual_style_warning(ir, warnings)
    lip_sync_warning(shot, resolved["mode"], resolved["combo"], warnings)

    task = resolved["mode"]
    combo = resolved["combo"]
    slots = []
    dropped = []
    case task
    when "t2va"
      dropped = image_slots.map { |entry| entry.merge("reason" => "t2va_has_no_references") }
      limit = 0
    when "fl2va"
      dropped = image_slots.map { |entry| entry.merge("reason" => "fl2va_first_frame_only") }
      slots = [slot("frame:start", "frame", "frame", shot["id"], shot["title"], start, true, "首帧 <Picture 1>")]
      limit = 1
    when "ref2va"
      dropped = image_slots.map { |entry| entry.merge("reason" => "ref2va_#{combo}_only") }
      if combo == "videos"
        slots = videos.map { |video| video_slot(video) }
        limit = capabilities.dig("ref2va", "maxVideos").to_i
      else
        slots = [slot("frame:start", "frame", "frame", shot["id"], shot["title"], start, true, "首帧 <Picture 1>"), audio_slot(shot, track)]
        limit = 2
      end
    end
    slots.each_with_index { |entry, index| entry["order"] = index + 1 }
    prompt = H3Prompt.render(ir, task: task, combo: combo)

    {
      "ok" => true, "purpose" => "video", "mode" => task, "combo" => combo, "modeReason" => resolved["reason"], "limit" => limit,
      "slots" => slots, "dropped" => dropped, "warnings" => warnings, "subjects" => subjects, "scene" => scene,
      "promptIR" => prompt, "ir" => ir, "audioTrack" => track, "videoRefs" => videos,
      "directives" => ir["directives"], "directivesSkipped" => ir["directivesSkipped"],
      "package" => shot["package"].is_a?(Hash) ? shot["package"] : nil, "overLimit" => false
    }
  end

  # 对白音轨：本镜每句台词选定的音频。一句直接用；多句要拼成一条（WAV 同格式），
  # 拼接在提交时做，这里只算时长与体积。
  def dialogue_track(shot, media_root, capabilities, warnings)
    files = []
    Array(shot["dialogue"]).each do |line|
      next unless line.is_a?(Hash) && line["audio"].is_a?(Hash)
      selected = MediaRef.selected(line["audio"], media_root)
      next unless selected
      files << { "lineID" => line["id"], "speaker" => line["speaker"], "candidateID" => selected["id"], "fileName" => selected["fileName"], "filePath" => selected["filePath"], "durationMs" => selected["durationMs"] }
    end
    return nil if files.empty?

    max_bytes = capabilities.dig("ref2va", "maxAudioBytes").to_i
    max_bytes = 750_000 unless max_bytes.positive?
    begin
      if files.length == 1
        info = WavTools.info(files[0]["filePath"])
        planned = { "files" => 1, "durationMs" => info.duration_ms, "bytes" => File.size(files[0]["filePath"]) }
      else
        planned = WavTools.plan(files.map { |file| file["filePath"] }, gap_ms: DIALOGUE_GAP_MS)
      end
    rescue WavTools::Error => error
      warnings << warning("audio_unusable", "对白音频不能作为参考：#{error.message}")
      return nil
    end
    track = { "lines" => files, "needsConcat" => files.length > 1, "durationMs" => planned["durationMs"], "bytes" => planned["bytes"],
              "fileName" => files.length == 1 ? files[0]["fileName"] : nil, "filePath" => files.length == 1 ? files[0]["filePath"] : nil }
    if planned["bytes"] > max_bytes
      warnings << warning("audio_too_large", "对白音轨约 #{planned['bytes'] / 1024} KB，超过 Provider 的 #{max_bytes / 1024} KB 上限；换更低采样率或拆镜。")
      track["tooLarge"] = true
    end
    duration_limit = shot["duration"].to_i * 1000
    if duration_limit.positive? && planned["durationMs"] > duration_limit
      warnings << warning("audio_too_long", "对白音轨 #{planned['durationMs']} 毫秒，超过镜头时长 #{shot['duration']} 秒。")
    end
    track
  end

  # 口型两档（0.43.0-rc1，docs/decisions/0009-dialogue-paced-cut.md）：有台词、运镜是说话人的近景 / 特写 / 正面，
  # 却没有对白音轨可对口型（不是 ref2va 首帧加音轨）时提醒——后期铺上去的配音在这种机位下口型对不上。
  CLOSE_UP_WORDS = /近景|特写|正面|close-?up|facing the camera/i.freeze

  def lip_sync_warning(shot, task, combo, warnings)
    return unless Array(shot["dialogue"]).any? { |line| line.is_a?(Hash) && !line["text"].to_s.strip.empty? }
    return if task == "ref2va" && combo == "image_audio"
    return unless shot["cameraIntent"].to_s.match?(CLOSE_UP_WORDS)

    warnings << warning("lip_sync_risk", "这一镜有台词、运镜是说话人的近景或特写，但没有对白音轨可对口型，后期配音会对不上嘴。" \
                                         "先给台词配音让它走 ref2va（首帧加对白音轨），或把运镜改成反应镜头、过肩、背影、手部这类不看嘴的机位。")
  end

  def video_refs(package, jobs, warnings)
    refs = Array(package["videoRefs"]).select { |entry| entry.is_a?(Hash) }.first(MAX_VIDEO_REFS)
    refs.map do |entry|
      job = Array(jobs).find { |item| item["id"] == entry["jobID"].to_s }
      if job.nil?
        warnings << warning("video_ref_missing", "参考视频任务不存在。", "jobID" => entry["jobID"].to_s)
        next nil
      end
      path = [job["mediaPath"], job["outputPath"]].map(&:to_s).find { |candidate| !candidate.empty? && File.file?(candidate) }
      if path.nil?
        warnings << warning("video_ref_unavailable", "参考视频「#{job['title']}」还没有可用的成片文件。", "jobID" => job["id"])
        next nil
      end
      { "jobID" => job["id"], "label" => job["title"].to_s.empty? ? job["id"] : job["title"].to_s, "filePath" => path, "fileName" => File.basename(path),
        "shotID" => job["shotID"] }
    end.compact
  end

  def resolve_video_mode(requested, capabilities, start, track, videos)
    tasks = Array(capabilities["tasks"]).map(&:to_s)
    wanted = requested.to_s.empty? ? "auto" : requested.to_s
    return failure("invalid_mode", "mode must be one of #{MODES.join(', ')}.") unless MODES.include?(wanted)

    combos = Array(capabilities.dig("ref2va", "combos")).map(&:to_s)
    audio_ok = track && !track["tooLarge"]
    videos_ok = tasks.include?("ref2va") && combos.include?("videos") && !videos.empty?
    image_audio_ok = tasks.include?("ref2va") && combos.include?("image_audio") && start && audio_ok

    if wanted != "auto"
      return failure("mode_unsupported", "The current video provider does not support #{wanted} (supports #{tasks.join(', ')}).") unless tasks.include?(wanted)
      return failure("mode_unsupported", "fl2va needs a selected start frame.") if wanted == "fl2va" && start.nil?
      if wanted == "ref2va"
        return { "ok" => true, "mode" => "ref2va", "combo" => "videos", "reason" => "requested; #{videos.length} reference video(s) bound" } if videos_ok
        return { "ok" => true, "mode" => "ref2va", "combo" => "image_audio", "reason" => "requested; start frame plus dialogue track" } if image_audio_ok
        return failure("mode_unsupported", "ref2va needs either 1-3 reference videos (package.videoRefs with completed jobs) or a selected start frame plus dialogue audio under the provider's size limit.")
      end
      return { "ok" => true, "mode" => wanted, "combo" => nil, "reason" => "requested" }
    end
    return { "ok" => true, "mode" => "ref2va", "combo" => "videos", "reason" => "reference videos bound" } if videos_ok
    return { "ok" => true, "mode" => "ref2va", "combo" => "image_audio", "reason" => "start frame and dialogue track available" } if image_audio_ok
    return { "ok" => true, "mode" => "fl2va", "combo" => nil, "reason" => "start frame selected" } if tasks.include?("fl2va") && start
    return { "ok" => true, "mode" => "t2va", "combo" => nil, "reason" => start ? "provider has no first-frame mode" : "no start frame selected" } if tasks.include?("t2va")

    failure("mode_unsupported", "The current video provider supports none of the known modes.")
  end

  # 快照：写进视频任务的那份，只留复现需要的字段。
  def snapshot(slots)
    Array(slots).map do |entry|
      { "order" => entry["order"], "semanticType" => entry["semanticType"], "assetKind" => entry["assetKind"], "assetID" => entry["assetID"],
        "mediaID" => entry["mediaID"], "fileName" => entry["fileName"], "promptLabel" => entry["promptLabel"] }
    end
  end

  # 参考包里被镜头引用的资产 ID，给归档保护与引用计数用。
  def referenced_asset_ids(shot)
    package = shot.is_a?(Hash) && shot["package"].is_a?(Hash) ? shot["package"] : {}
    ids = [package["sceneID"], package["sceneVariantID"]]
    Array(package["cast"]).each { |binding| ids << binding["appearanceID"] << binding["voiceID"] if binding.is_a?(Hash) }
    ids.concat(Array(package["propIDs"]))
    ids.map(&:to_s).reject(&:empty?).uniq
  end

  # ---- 提示词材料 ----

  # 提示词要用的结构化材料。H3Prompt 按任务渲染；出图侧用 image_prompt_ir。
  def build_ir(drama, shot, package, subjects, scene, props, assets)
    speaker_ids = {}
    Array(drama["characters"]).each_with_index { |character, index| speaker_ids[character["name"].to_s.strip] = "S#{index + 1}" }
    dialogue = Array(shot["dialogue"]).map do |line|
      next nil unless line.is_a?(Hash) && !line["text"].to_s.strip.empty?
      speaker = line["speaker"].to_s.strip
      subject = subjects.find { |entry| entry["name"] == speaker }
      language = subject && !subject["language"].to_s.empty? ? subject["language"] : voice_language(drama, assets, speaker)
      { "speaker" => speaker, "speakerID" => speaker_ids[speaker], "text" => line["text"].to_s, "language" => H3Prompt.language_tag(language) }
    end.compact
    retention = AUDIO_RETENTIONS.include?(package["audioRetention"].to_s) ? package["audioRetention"].to_s : "fully_copy"
    look, source = visual_look(drama)
    {
      "style" => style_line(look), "visualStyle" => look, "styleSource" => source, "subjects" => subjects, "scene" => scene, "props" => props,
      "summary" => shot["summary"].to_s, "actionStart" => shot["actionStart"].to_s, "actionEnd" => shot["actionEnd"].to_s,
      "cameraIntent" => shot["cameraIntent"].to_s, "dialogue" => dialogue,
      "soundscape" => shot["soundscape"].to_s.strip.empty? ? H3Prompt::DEFAULT_SOUNDSCAPE : H3Prompt.plain(shot["soundscape"]),
      "music" => shot["music"].to_s.strip.empty? ? H3Prompt::DEFAULT_MUSIC : H3Prompt.plain(shot["music"]),
      "audioRetention" => retention, "videos" => [], "duration" => shot["duration"].to_i
    }
  end

  # 成片提示词的风格行只写画面：真人实拍、电影感，加上剧的「画面风格」。
  #
  # 不再带 tone 和 genre。tone 是写给编剧的基调，常是「人物说话不端着……每集结尾落在
  # 一个动作或一句反话上」这种写作要求；2026-09-27《老宅有锁》把它原样塞进 H3 后，
  # 这几句话被当成字幕烧进了成片。genre 同理，是策划口径而不是画面。
  def style_line(look)
    look.empty? ? "Live-action, cinematic." : "Live-action, cinematic. Visual style: #{look}."
  end

  # [画面风格, 来源]。来源是 visualStyle、tone（旧剧借用），都用不上时是 nil。
  def visual_look(drama)
    look = H3Prompt.plain(drama["visualStyle"]).sub(TRAILING_STOPS, "")
    return [look, "visualStyle"] unless look.empty?

    borrowed = tone_as_look(drama["tone"])
    borrowed.empty? ? ["", nil] : [borrowed, "tone"]
  end

  # tone 像一串画面形容词才借用，否则返回空串。宁可漏借也不能把写作要求送进成片。
  def tone_as_look(tone)
    text = H3Prompt.plain(tone).sub(TRAILING_STOPS, "")
    return "" if text.empty? || text.match?(TONE_LOOK_SENTENCE_MARKS)
    return "" if TONE_LOOK_WRITER_WORDS.any? { |word| text.include?(word) }

    segments = text.split(TONE_LOOK_SEPARATORS).map(&:strip).reject(&:empty?)
    return "" if segments.empty? || segments.length > TONE_LOOK_MAX_SEGMENTS
    return "" if segments.any? { |segment| display_width(segment) > TONE_LOOK_MAX_SEGMENT_WIDTH }

    text
  end

  def display_width(text)
    text.each_char.sum { |char| char.ord >= WIDE_CHAR_FROM ? 2 : 1 }
  end

  # 成片侧才用风格行，所以只在出视频时提醒；出图不受影响。
  def visual_style_warning(ir, warnings)
    case ir["styleSource"]
    when "visualStyle" then nil
    when "tone"
      warnings << warning("visual_style_missing", "这部剧还没写画面风格，成片暂借用基调「#{ir['visualStyle']}」。在剧集元数据里写一句画面风格（光线、色调、质感）会更稳。")
    else
      warnings << warning("visual_style_missing", "这部剧还没写画面风格，成片只按真人实拍、电影感来拍；基调是写给编剧的，不会放进成片。可以在剧集元数据里写一句画面风格（光线、色调、质感）。")
    end
  end

  def voice_language(drama, assets, speaker)
    character = Array(drama["characters"]).find { |entry| entry["name"].to_s.strip == speaker }
    return "" unless character
    voice = assets.find { |asset| asset["kind"] == "voice" && asset["characterID"] == character["id"] && !asset["archived"] }
    voice ? voice["language"].to_s : ""
  end

  # 出图侧的描述性中间表示：<Subject N> / <Picture N> 指代，给人看也给 Agent 参考。
  def image_prompt_ir(ir, slots)
    picture = {}
    slots.each { |entry| picture[entry["key"]] = "<Picture #{entry['order']}>" }
    lines = []
    ir["subjects"].each do |subject|
      parts = ["<Subject #{subject['index']}> is #{subject['name']}"]
      identity = picture["identity:#{subject['characterID']}"]
      parts << (identity ? "the person in #{identity}" : H3Prompt.plain(subject["identity"]))
      if subject["appearanceID"]
        appearance = picture["appearance:#{subject['appearanceID']}"]
        parts << (appearance ? "wearing the look from #{appearance}" : H3Prompt.plain(subject["appearance"]))
        parts << "(#{subject['appearanceName']})" unless subject["appearanceName"].to_s.empty?
      end
      parts << "at the #{subject['screenPosition']}" unless subject["screenPosition"].to_s.empty?
      lines << parts.reject(&:empty?).join(", ") + "."
    end
    if ir["scene"]
      key = slots.find { |entry| entry["semanticType"] == "scene" }
      reference = key ? " from #{picture[key['key']]}" : ""
      text = ["<Scene 1> is #{ir['scene']['name']}#{reference}", ir["scene"]["prompt"], *ir["scene"]["rules"], ir["scene"]["lighting"].to_s.empty? ? nil : "lighting: #{ir['scene']['lighting']}"]
      lines << text.compact.map(&:to_s).reject(&:empty?).join(". ") + "."
    end
    ir["props"].each do |prop|
      reference = picture["prop:#{prop['id']}"]
      lines << "Prop #{prop['name']}#{reference ? " is the object in #{reference}" : ''}: #{prop['prompt']}".sub(/: \z/, ".")
    end
    shot_lines = [ir["summary"], ir["actionStart"], ir["actionEnd"], ir["cameraIntent"]].map(&:to_s).reject(&:empty?)
    ["subject_definitions:", *lines, "", "shot:", *shot_lines].join("\n").strip
  end

  # ---- 绑定与场景 ----

  # 出场角色。参考包的 cast 为空时按名字在本镜文本里找角色（inferred）。
  #
  # cast 为空不当作「本镜没有人」：drama.set_reference_package 每次都把 cast 写成数组，
  # 只绑了场景的包和「明确没有人物」的包在存储上分不出来；设计稿 9.2 也只把它当兼容
  # 旧镜头的退路。真要纯空镜，用 castIDs: [] 或在文字里不写角色名。
  #
  # 推断出的角色沿用本集其它镜头里给同一角色绑的造型（0.35.0-rc2）：先找前面最近的
  # 一镜，没有再找后面最近的。此前只带身份图，《回村养鸭》第 1 集里许大强在没绑
  # 参考包的几镜丢了标志性的衣服。
  def cast_bindings(drama, shot, package, cast_override, episode = nil)
    if cast_override.is_a?(Array)
      return cast_override.map { |id| { "characterID" => id.to_s, "role" => "supporting", "appearanceID" => nil, "voiceID" => nil } }
    end
    bindings = Array(package["cast"]).select { |entry| entry.is_a?(Hash) }
    if bindings.empty?
      haystack = [shot["summary"], shot["actionStart"], shot["actionEnd"], shot["startPrompt"], shot["endPrompt"],
                  *Array(shot["dialogue"]).map { |line| line.is_a?(Hash) ? line["speaker"] : nil }].map(&:to_s).join("\n").unicode_normalize(:nfc)
      bindings = Array(drama["characters"]).select do |character|
        name = character["name"].to_s.strip.unicode_normalize(:nfc)
        !name.empty? && haystack.include?(name)
      end.map { |character| inferred_binding(drama, episode, shot, character["id"]) }
    end
    bindings.each_with_index.sort_by { |binding, index| [ROLE_ORDER.fetch(binding["role"].to_s, 1), index] }.map(&:first)
  end

  def inferred_binding(drama, episode, shot, character_id)
    binding = { "characterID" => character_id, "role" => "supporting", "appearanceID" => nil, "voiceID" => nil, "inferred" => true }
    source = nearest_binding(drama, episode, shot, character_id)
    return binding unless source

    binding.merge("appearanceID" => source[:binding]["appearanceID"], "appearanceFromShotID" => source[:shot]["id"],
                  "appearanceFromOrder" => source[:shot]["order"])
  end

  # 本集里离这一镜最近、给该角色绑了造型的镜头：前面的优先。
  def nearest_binding(drama, episode, shot, character_id)
    episode ||= Array(drama["episodes"]).find { |entry| Array(entry["shots"]).any? { |item| item["id"] == shot["id"] } }
    return nil unless episode

    order = shot["order"].to_i
    shots = Array(episode["shots"]).select { |item| item.is_a?(Hash) && item["id"] != shot["id"] }
    before, after = shots.partition { |item| item["order"].to_i < order }
    candidates = before.sort_by { |item| -item["order"].to_i } + after.sort_by { |item| item["order"].to_i }
    candidates.each do |item|
      package = item["package"].is_a?(Hash) ? item["package"] : {}
      found = Array(package["cast"]).find { |entry| entry.is_a?(Hash) && entry["characterID"] == character_id && !entry["appearanceID"].to_s.empty? }
      return { binding: found, shot: item } if found
    end
    nil
  end

  def scene_slot(drama, package, assets, excluded, warnings, media_root)
    scene_id = package["sceneID"].to_s
    variant_id = package["sceneVariantID"].to_s
    return { slot: nil, description: nil } if scene_id.empty? && variant_id.empty?

    variant = variant_id.empty? ? nil : find(assets, variant_id)
    warnings << warning("asset_missing", "绑定的场景变体不存在。", "assetID" => variant_id) if !variant_id.empty? && variant.nil?
    scene = scene_id.empty? ? (variant && find(assets, variant["sceneID"])) : find(assets, scene_id)
    warnings << warning("asset_missing", "绑定的场景不存在。", "assetID" => scene_id) if !scene_id.empty? && scene.nil?
    return { slot: nil, description: nil } unless scene || variant

    chain = ancestors(assets, scene)
    [variant, scene, *chain].compact.each do |node|
      warnings << warning("asset_archived", "场景「#{node['name']}」已归档，仍会按原样带上。", "assetID" => node["id"]) if node["archived"]
    end
    # 变体优先，其次场景本身，再沿父场景往上找有选定图的。
    source = [variant, scene, *chain].compact.find { |node| MediaRef.selected(node, media_root) }
    description = {
      "sceneID" => scene && scene["id"], "sceneVariantID" => variant && variant["id"],
      "name" => [scene && scene["name"], variant && variant["name"]].compact.join(" · "),
      "prompt" => [variant && variant["prompt"], scene && scene["prompt"]].compact.map(&:to_s).reject(&:empty?).join("；"),
      "rules" => ([*chain.reverse, scene].compact.map { |node| node["notes"].to_s } + [variant && variant["notes"].to_s]).compact.reject(&:empty?),
      "lighting" => variant && variant["lighting"].to_s
    }
    unless source
      warnings << warning("scene_no_image", "场景「#{description['name']}」及其父场景都没有选定参考图。", "assetID" => (variant || scene)["id"])
      return { slot: nil, description: description }
    end
    key = "scene:#{source['id']}"
    return { slot: nil, description: description } if excluded.include?(key)

    { slot: slot(key, "scene", source["kind"], source["id"], source["name"], MediaRef.selected(source, media_root), false, "场景「#{description['name']}」"),
      description: description }
  end

  def ancestors(assets, scene)
    chain = []
    cursor = scene
    seen = {}
    while cursor && !cursor["parentSceneID"].to_s.empty? && !seen[cursor["id"]]
      seen[cursor["id"]] = true
      cursor = find(assets, cursor["parentSceneID"])
      chain << cursor if cursor
    end
    chain
  end

  # 先裁 required: false 的（从后往前），仍超限时再裁多余的身份图，并告警。
  def trim(slots, limit, warnings)
    return [slots, []] if limit <= 0 || slots.length <= limit

    dropped = []
    kept = slots.dup
    kept.reverse_each do |entry|
      break if kept.length <= limit
      next if entry["required"]
      kept.delete(entry)
      dropped << entry.merge("reason" => "over_limit")
    end
    if kept.length > limit
      warnings << warning("over_limit_required", "必带的身份图就有 #{kept.length} 张，超过上限 #{limit} 张；请在参考包里精简出场角色。")
      kept.reverse_each do |entry|
        break if kept.length <= limit
        kept.delete(entry)
        dropped << entry.merge("reason" => "over_limit_required")
      end
    elsif !dropped.empty?
      warnings << warning("over_limit", "参考超过上限 #{limit} 张，已去掉 #{dropped.length} 项非必带参考；可在参考包里手动取消其它项。")
    end
    [kept, dropped.reverse]
  end

  def slot(key, semantic, kind, asset_id, name, media, required, label)
    { "key" => key, "order" => 0, "semanticType" => semantic, "assetKind" => kind, "assetID" => asset_id.to_s, "assetName" => name.to_s,
      "mediaID" => media["id"], "fileName" => media["fileName"], "filePath" => media["filePath"], "required" => required, "promptLabel" => label }
  end

  def video_slot(video)
    { "key" => "video:#{video['jobID']}", "order" => 0, "semanticType" => "video", "assetKind" => "video", "assetID" => video["jobID"], "assetName" => video["label"],
      "mediaID" => video["jobID"], "fileName" => video["fileName"], "filePath" => video["filePath"], "required" => true, "promptLabel" => "参考视频「#{video['label']}」" }
  end

  def audio_slot(shot, track)
    { "key" => "audio:dialogue", "order" => 0, "semanticType" => "audio", "assetKind" => "dialogue", "assetID" => shot["id"], "assetName" => "对白音轨",
      "mediaID" => track["needsConcat"] ? "track" : track["lines"][0]["candidateID"], "fileName" => track["fileName"], "filePath" => track["filePath"],
      "required" => true, "promptLabel" => "对白音轨 <Audio 1>（#{track['lines'].length} 句，#{track['durationMs']} 毫秒）" }
  end

  def warning(code, message, extra = {})
    { "code" => code, "message" => message }.merge(extra)
  end

  def consent_granted?(voice)
    consent = voice["consent"]
    consent.is_a?(Hash) && consent["status"] == "granted" && !consent["grantedBy"].to_s.strip.empty?
  end

  def find(collection, id)
    token = id.to_s
    return nil if token.empty?
    Array(collection).find { |entry| entry.is_a?(Hash) && entry["id"] == token }
  end

  def failure(code, message)
    { "ok" => false, "error" => { "code" => code, "message" => message } }
  end
end
