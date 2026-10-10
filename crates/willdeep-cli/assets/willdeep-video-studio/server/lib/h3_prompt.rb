# frozen_string_literal: true

# MiniMax-H3（Tsingfly Hub）的结构化提示词。
#
# H3 的 prompt 不是一句自然语言：t2va / fl2va 是三段式，ref2va 是六段式，分段靠行首
# 的段名识别，台词有 (S1) <d>[Chinese]…</d> 的硬性语法。这是设计稿 5.3 说的「适配器把
# PromptIR 翻译成 Provider 语法」那一层：参考包编译给出主体、场景、道具、台词与槽位，
# 这里按任务与组合渲染成 Hub 文档规定的样子（2026-09-17 核实，见
# https://hub.tsingfly.com/zh/tutor/videos）。
#
# 页面与 Agent 不必自己拼：video.generate 带 shotID 时默认发这份；dryRun / preview 里
# 能看到全文，想改就把改好的整段作为 prompt 传回来（已含段名的 prompt 原样透传）。
module H3Prompt
  module_function

  SECTION_NAMES = %w[integrated_multimodal_description overall_soundscape non_diegetic_music
                     subject_definitions summary retention_analysis detailed_description].freeze
  DEFAULT_SOUNDSCAPE = "Ambient sound consistent with the setting and the characters' actions."
  DEFAULT_MUSIC = "N/A"
  # 每条渲染出的提示词都带的画面约束。2026-09-27《老宅有锁》：H3 把台词烧成了字幕
  # （E5 第 6 镜），把提示词里的中文句子当字幕叠上（E6 第 9 镜），还在墙上写了
  # 「潮湿的」。剧情里本来就该看得见的字（手机屏幕、信、招牌）不在禁止之列。
  NO_TEXT = "No subtitles, captions or text overlays. Words in this prompt are directions, not text to draw; " \
            "only writing the action itself shows (a phone screen, a letter, a sign) may appear in the picture."
  DIALOGUE_NOT_TEXT = "The dialogue is spoken aloud, never written on screen."
  LANGUAGE_TAGS = {
    "zh" => "Chinese", "en" => "English", "ja" => "Japanese", "ko" => "Korean", "fr" => "French",
    "de" => "German", "es" => "Spanish", "yue" => "Cantonese", "ru" => "Russian", "pt" => "Portuguese"
  }.freeze

  SHOT_MARKER = "[Shot 1]"
  THREE_SECTION_SHOT = /^integrated_multimodal_description:[ \t]*#{Regexp.escape(SHOT_MARKER)}/
  SIX_SECTION_START = /^detailed_description:/
  LINE_LEADING_SHOT = /^#{Regexp.escape(SHOT_MARKER)}/

  # 已经是结构化写法的 prompt 不再包装。
  def structured?(text)
    body = text.to_s
    SECTION_NAMES.any? { |name| body.match?(/^\s*#{name}\s*:/m) }
  end

  # 调用方给的一句自然语言，接在镜头描述起头的 [Shot 1] 后面。
  #
  # 不能替换第一个 [Shot 1]：fl2va 开头的对齐句是 "(from [Shot 1])"，ref2va 的
  # retention_analysis 里是 "(appears in [Shot 1])"，两处都排在镜头描述前面。
  # 三段式的锚点是 "integrated_multimodal_description: [Shot 1]"，六段式是
  # detailed_description 段里行首的 [Shot 1]。
  def insert_into_shot(prompt, sentence)
    text = plain(sentence)
    return prompt if text.empty?

    anchor = shot_anchor(prompt)
    raise ArgumentError, "No #{SHOT_MARKER} shot description in prompt: #{prompt.to_s[0, 80].inspect}" unless anchor

    "#{prompt[0...anchor]} #{text}#{prompt[anchor..]}"
  end

  # 镜头描述起头那个 [Shot 1] 的结束位置；不是 render 出来的文本时为 nil。
  def shot_anchor(prompt)
    three = prompt.match(THREE_SECTION_SHOT)
    return three.end(0) if three

    section = prompt.index(SIX_SECTION_START)
    start = section && prompt.index(LINE_LEADING_SHOT, section)
    start && start + SHOT_MARKER.length
  end

  # ir：ReferencePackage 编译结果里的 "ir" 部分。
  # task：t2va / fl2va / ref2va；combo：ref2va 的 videos / image_audio。
  def render(ir, task:, combo: nil)
    case task
    when "ref2va" then six_sections(ir, combo)
    when "fl2va" then "#{ALIGNMENT}\n\n#{three_sections(ir, picture: true)}"
    else three_sections(ir, picture: false)
    end
  end

  ALIGNMENT = "For the target video, at 0.00 seconds into the target video, <Picture 1> (from [Shot 1]) is fully referenced."

  def three_sections(ir, picture:)
    subjects = ir["subjects"].map do |subject|
      base = picture ? "#{subject['name']} shown in <Picture 1>" : "#{subject['name']}, #{plain(subject['identity'])}"
      look = subject["appearance"].to_s.empty? ? "" : ", wearing #{plain(subject['appearance'])}"
      # fl2va 只写「<Picture 1> 里的某某」，不带身份文字：没绑造型时补上日常装（0.39.0-rc1），
      # 与出图的参考图说明同一行（IdentityText.wardrobe_line）。t2va 带全文身份，本来就有。
      look = ", wearing #{plain(subject['wardrobe'])}" if picture && look.empty? && !subject["wardrobe"].to_s.strip.empty?
      position = subject["screenPosition"].to_s.empty? ? "" : " at the #{subject['screenPosition']}"
      "#{base}#{look}#{position}"
    end
    scene = ir["scene"] ? "Setting: #{plain([ir['scene']['name'], ir['scene']['prompt'], *ir['scene']['rules']].compact.join('. '))}." : nil
    props = ir["props"].empty? ? nil : "Props: #{ir['props'].map { |prop| plain("#{prop['name']}, #{prop['prompt']}") }.join('; ')}."
    body = [
      "[Shot 1]", ir["style"],
      subjects.empty? ? nil : "#{subjects.join('; ')}.",
      scene, props,
      plain(ir["actionStart"]), dialogue_lines(ir), plain(ir["actionEnd"]), camera(ir), directive_lines(ir), text_rule(ir)
    ].compact.reject(&:empty?).join(" ")
    [
      "integrated_multimodal_description: #{body}",
      "overall_soundscape: #{ir['soundscape']}",
      "non_diegetic_music: #{ir['music']}"
    ].join("\n")
  end

  def six_sections(ir, combo)
    definitions = []
    retention = []
    ir["subjects"].each do |subject|
      source = combo == "videos" ? "appearing in <Video 1>" : "in <Picture 1>"
      look = subject["appearance"].to_s.empty? ? "" : ", wearing #{plain(subject['appearance'])}#{appearance_tag(subject, ' (%s)')}"
      definitions << "<Subject #{subject['index']}> is #{subject['name']}, the person #{source}#{look}. #{plain(subject['identity'])}".strip
      retention << "<Subject #{subject['index']}> (appears in [Shot 1]): fully_preserved - #{retained(subject)}"
    end
    if combo == "videos"
      ir["videos"].each_with_index do |video, index|
        definitions << "<Video #{index + 1}> is #{video['label']}."
      end
      audio_count = ir["videos"].count { |video| video["hasAudio"] }
      (1..audio_count).each { |number| definitions << "<Audio #{number}> is the voice timbre carried by <Video #{number}>." }
      retention << "<Video 1>: attribute_transfer - style, lighting and setting carry over." unless ir["videos"].empty?
      (1..audio_count).each { |number| retention << "<Audio #{number}>: reference - timbre only, the signal is not copied." }
      summary = "[subject reference] The target video continues the story from <Video 1> with the same characters."
    else
      definitions << "<Picture 1> shows the opening frame: the subjects, wardrobe and composition of the shot."
      definitions << "<Audio 1> is the dialogue track for this shot#{speakers(ir)}."
      retention << "<Picture 1>: fully_preserved - composition and wardrobe are retained."
      retention << "<Audio 1>: #{ir['audioRetention']} - #{ir['audioRetention'] == 'fully_copy' ? 'the dialogue audio is used as is and the mouths follow it' : 'timbre only, the signal is not copied'}."
      summary = "[subject reference] The target video is generated from the referenced frame and speaks the referenced dialogue."
    end
    scene = ir["scene"] ? "Setting: #{plain([ir['scene']['name'], ir['scene']['prompt'], *ir['scene']['rules']].compact.join('. '))}." : nil
    props = ir["props"].empty? ? nil : "Props: #{ir['props'].map { |prop| plain("#{prop['name']}, #{prop['prompt']}") }.join('; ')}."
    subjects_in_shot = ir["subjects"].map { |subject| "<Subject #{subject['index']}>#{subject['screenPosition'].to_s.empty? ? '' : " at the #{subject['screenPosition']}"}" }
    shot_line = [
      "[Shot 1]",
      subjects_in_shot.empty? ? nil : "#{subjects_in_shot.join(' and ')} in frame.",
      scene, props, plain(ir["actionStart"]), dialogue_lines(ir), plain(ir["actionEnd"]), camera(ir), directive_lines(ir)
    ].compact.reject(&:empty?).join(" ")
    [
      "subject_definitions:\n#{definitions.join("\n")}",
      "summary:\n#{summary} #{plain(ir['summary'])}".strip,
      "retention_analysis:\n#{retention.join("\n")}",
      "detailed_description:\n#{ir['style']}\n#{shot_line}\n#{text_rule(ir)}",
      "overall_soundscape:\n#{ir['soundscape']}",
      "non_diegetic_music:\n#{ir['music']}"
    ].join("\n\n")
  end

  # 绑了造型的人物：身份只留长相（参考包编译已用 IdentityText 去掉日常穿着），穿着跟造型走
  # （0.38.0-rc1，《回村养鸭》第 1 集第 3 镜孝衣被牛仔衬衫顶掉）。
  def retained(subject)
    return "identity and wardrobe are retained." if !subject["appearance"].to_s.empty? && subject["wardrobeFromAppearance"] == false
    # 没绑造型：与 0.37 逐字相同（已有成片的提示词不因这次改动而变）。
    return "identity are retained." if subject["appearance"].to_s.empty?

    "identity (face, hair, build) is retained; wardrobe follows #{appearance_tag(subject, '%s')}."
  end

  def appearance_tag(subject, format_string)
    name = plain(subject["appearanceName"])
    return "" if name.empty? && format_string.include?("(")

    format(format_string, name.empty? ? "the bound outfit" : "<Appearance: #{name}>")
  end

  def speakers(ir)
    ids = ir["dialogue"].map { |line| line["speakerID"] }.compact.uniq
    ids.empty? ? "" : " for #{ids.map { |id| "(#{id})" }.join(', ')}"
  end

  def dialogue_lines(ir)
    lines = ir["dialogue"].map do |line|
      text = plain(line["text"])
      next nil if text.empty?
      who = line["speakerID"] ? "(#{line['speakerID']}) #{line['speaker']} says" : "#{line['speaker']} says"
      "#{who}: <d>[#{line['language']}]#{text}</d>"
    end.compact
    lines.empty? ? nil : lines.join(" ")
  end

  def camera(ir)
    intent = plain(ir["cameraIntent"])
    intent.empty? ? nil : "Camera: #{intent}."
  end

  # 画面质检沉淀下来的正面约束（0.38.0-rc1，lib/qa_remediation.rb）：默认的一镜到底句、
  # 经验库里验证过的预防句，以及自动重做时按问题类别加的补救句。参考包编译已按运镜过滤、去重。
  def directive_lines(ir)
    texts = Array(ir["directives"]).map { |entry| plain(entry.is_a?(Hash) ? entry["text"] : entry) }.reject(&:empty?)
    return nil if texts.empty?

    texts.map { |text| text.end_with?(".", "!", "?") ? text : "#{text}." }.join(" ")
  end

  def text_rule(ir)
    Array(ir["dialogue"]).empty? ? NO_TEXT : "#{DIALOGUE_NOT_TEXT} #{NO_TEXT}"
  end

  # 正文里不能出现以段名打头的行，也不该带换行把分段搞乱：全部折成一行。
  def plain(value)
    value.to_s.gsub(/\s*\n\s*/, " ").strip
  end

  def language_tag(code)
    token = code.to_s.downcase
    return "Chinese" if token.empty?
    return "Cantonese" if token.start_with?("yue")
    LANGUAGE_TAGS.fetch(token.split(/[-_]/).first, code.to_s)
  end
end
