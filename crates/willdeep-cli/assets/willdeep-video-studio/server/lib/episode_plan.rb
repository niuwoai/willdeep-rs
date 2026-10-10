# frozen_string_literal: true

require_relative "media_ref"
require_relative "reference_package"
require_relative "review_material"
require_relative "wav_tools"
require_relative "video_selection"
require_relative "clip_trim"
require_relative "speech_rate"

# 一集成片的计划（设计稿 docs/design/episode-compose.md 3.1）：每镜用哪条视频、走哪种
# 配音、每句台词的配音状态，以及哪些问题会挡住合成。页面的「成片」阶段与后台合成
# 进程共用这一份计算，两边对「能不能合、合什么」的判断不会分叉。
module EpisodePlan
  # 与 reference_package.rb 的 DIALOGUE_GAP_MS 一致：第一句前留白、句与句之间的间隔。
  LEAD_IN_MS = 300
  GAP_MS = 300
  # 提示词里要求画字幕 / 叠字 / 标题卡的说法。
  CAPTION_IN_PROMPT = /字幕|叠字|标题卡|片名卡|\b(?:caption|subtitle|title card|on-screen text)\b/i.freeze
  # 配音节奏（0.43.0-rc1，docs/decisions/0009-dialogue-paced-cut.md）：按台词说完的时刻剪掉镜头尾巴。
  # 分镜写明「留白 / 反应镜头 / 空镜」（或 holdFull 为真）的镜头保留全长。
  PACINGS = %w[picture dialogue].freeze
  HOLD_WORDS = /留白|反应镜头|反应镜|空镜|\bhold\b|\breaction shot\b/i.freeze
  # 裁完至少留这么长；比现有出点早不到这么多时不值得再裁一刀。
  PACE_MIN_KEPT_MS = 500
  PACE_EPSILON_MS = 50

  module_function

  # jobs：VideoService#jobs_with_media（已排除软删除）。
  def build(drama:, episode:, jobs:, settings:, media_root:)
    shots = Array(episode["shots"]).sort_by { |shot| shot["order"].to_i }
    blocking = []
    planned = shots.map do |shot|
      video_job, source = pick_video(shot, jobs)
      # 用台词音轨生成的片段（ref2va）：视频模型把台词重新念了一遍，音质不如配音原文件。
      # 选「配音」时直接铺生成它的那条音轨（口型就是照它对的，从第 0 毫秒起、不另留白），
      # 并把视频原声静音，免得念两遍；音轨文件不在了才退回视频原声。
      synthetic = video_job ? video_job["syntheticVoice"] == true : false
      requested = settings["shotVoiceSource"][shot["id"]] || settings["voiceSource"]
      track = synthetic && requested == "tts" ? reference_track(video_job) : nil
      voice_source = synthetic && !track ? "video" : requested
      lines = plan_lines(drama, shot, track ? "video" : voice_source, media_root)
      warnings = []
      warnings << "synthetic_voice" if synthetic && requested == "tts" && !track
      warnings << "lines_without_audio" if lines.any? { |line| %w[missing stale no_voice].include?(line["audio"]) }
      # 配音语速（0.40.0-rc1，lib/speech_rate.rb）：这一镜用到的声音整体偏慢 / 偏快时提醒，不挡合成。
      speech = voice_source == "tts" && !track ? speech_rate_warnings(drama, shot) : []
      speech.each { |entry| warnings << entry["code"] unless warnings.include?(entry["code"]) }
      speech_ms = track ? track["durationMs"] : speech_length_ms(lines)
      # 裁剪（0.39.0-rc1，lib/clip_trim.rb）：只认挂在这条成片上的；合成按入点 / 出点截取，原声与口型音轨同窗截取。
      trim = video_job ? trim_summary(ClipTrim.for_job(shot, video_job["id"]), video_job) : nil
      if trim
        # 口型音轨从片段第 0 毫秒起：入点之前那段跟着画面一起裁掉，剩下的从新的第 0 毫秒起。
        speech_ms = [speech_ms - (ClipTrim.in_seconds(trim) * 1000).round, 0].max if track
        warnings << "trim_cuts_dialogue_head" if track && ClipTrim.in_seconds(trim).positive?
        # 台词比裁剪后的画面长：照合成的老规矩定格最后一帧补足，不截台词。
        warnings << "trim_shorter_than_dialogue" if voice_source == "tts" && trim["keptDurationMs"] && speech_ms > trim["keptDurationMs"]
      elsif voice_source == "tts" && speech_ms > shot["duration"].to_i * 1000 && shot["duration"].to_i.positive?
        warnings << "audio_too_long"
      end
      # 配音节奏（0.43.0-rc1）：出点 = 台词说完 + 尾留白；没台词的镜头上限 pacingSilentMaxSeconds；留白镜头不动。
      # 算出来的裁剪不落库（设置一改就全变），只随计划走，合成按它截取。
      pacing = pacing_for(settings, shot["id"])
      pace = nil
      if pacing == "dialogue" && video_job
        if hold_full?(shot)
          warnings << "pace_hold"
        else
          pace = pace_trim(shot, video_job, trim, voice_source, track, lines, settings)
          warnings << "pace_trimmed" if pace
        end
      end
      qa_override = shot.dig("qaOverride", "jobID") == video_job&.dig("id")
      # 生效的画面质检结论：裁剪裁掉的 scene_jump 不算（原结论另在 verdictStatus / trimExcluded 里）。
      qa_review = video_job && ClipTrim.effective_for(shot, video_job)
      if video_job && qa_review && qa_review["status"] == "block"
        message = "第 #{shot['order']} 镜画面质检未通过：#{qa_review['summary']}"
        if qa_override
          warnings << "qa_override"
        else
          blocking << { "code" => "qa_blocked", "shotID" => shot["id"], "order" => shot["order"], "message" => message }
        end
      elsif video_job && qa_review && qa_review["status"] == "warn"
        warnings << "qa_warn"
      elsif video_job && settings.fetch("autoQA", true)
        warnings << "qa_pending"
      end
      # 选定的成片比批量为这一镜新生成的旧（0.38.0-rc3）：只提醒、不挡合成，合成仍用选定的那条。
      stale = source == "selected" ? VideoSelection.stale(shot, jobs) : nil
      warnings << "stale_selection" if stale
      # 台词重配过、这条成片的口型对的还是旧配音（0.40.0-rc1）：只提醒，合成照旧用它。
      # 配音节奏下（0.43.0-rc1）升为拦截项：这种模式整个建立在「口型跟台词走」上，对不上的镜头要重新生成。
      dialogue_stale = video_job ? VideoSelection.dialogue_stale(shot, video_job) : nil
      warnings << "dialogue_stale" if dialogue_stale
      if dialogue_stale && pacing == "dialogue"
        blocking << { "code" => "dialogue_stale", "shotID" => shot["id"], "order" => shot["order"],
                      "message" => "第 #{shot['order']} 镜的台词重配过，这条成片的口型对的是旧配音；配音节奏下要先重新生成这一镜（或把本镜改回画面节奏）。" }
      end
      # 首尾帧提示词里写了字幕 / 叠字（0.41.0-rc1）：出图和视频会把字画进画面、画面质检会拦。提醒改用 caption 字段。
      warnings << "caption_in_prompt" if [shot["startPrompt"], shot["endPrompt"]].any? { |text| text.to_s.match?(CAPTION_IN_PROMPT) }
      unless video_job
        blocking << { "code" => "missing_video", "shotID" => shot["id"], "order" => shot["order"],
                      "message" => "第 #{shot['order']} 镜还没有已完成的视频。" }
      end
      {
        "shotID" => shot["id"],
        "order" => shot["order"].to_i,
        "duration" => shot["duration"].to_i,
        "voiceSource" => voice_source,
        "requestedVoiceSource" => requested,
        "video" => video_summary(video_job, source),
        "syntheticVoice" => synthetic,
        "dialogueTrack" => track,
        "qaReview" => qa_review,
        "qaOverride" => qa_override,
        "staleSelection" => stale,
        "dialogueStale" => dialogue_stale,
        "trim" => trim,
        "pacing" => pacing,
        "paceTrim" => pace,
        "caption" => shot["caption"].to_s.strip.empty? ? nil : shot["caption"].to_s.strip,
        "lines" => lines,
        "speechRate" => speech.empty? ? nil : speech,
        "warnings" => warnings
      }.reject { |_, value| value.nil? }
    end
    if shots.empty?
      blocking << { "code" => "no_shots", "message" => "这一集还没有分镜。" }
    end
    planned.each { |shot| shot["effectiveDurationMs"] = effective_ms(shot) }
    {
      "shots" => planned,
      "blocking" => blocking,
      "estimatedDurationMs" => planned.sum { |shot| shot["effectiveDurationMs"] }
    }
  end

  # 这一镜在成片里大约占多长：画面（裁剪后的长度，没裁取分镜时长）与台词取长的那个——台词更长时合成会定格补足。
  def effective_ms(shot)
    trim = effective_trim(shot)
    picture = trim && trim["keptDurationMs"] ? trim["keptDurationMs"].to_i : shot["duration"] * 1000
    [picture, shot["voiceSource"] == "tts" ? shot_speech_ms(shot) : 0].max
  end

  # 合成真正用的裁剪：配音节奏算出来的（paceTrim，已并入存着的入点）优先，否则存在镜头上的那条。
  def effective_trim(shot)
    shot["paceTrim"].is_a?(Hash) ? shot["paceTrim"] : shot["trim"]
  end

  def pacing_for(settings, shot_id)
    override = settings["shotPacing"].is_a?(Hash) ? settings["shotPacing"][shot_id] : nil
    return override if PACINGS.include?(override)

    PACINGS.include?(settings["pacing"]) ? settings["pacing"] : "picture"
  end

  # 留白镜头：分镜标了 holdFull，或运镜 / 剧情写了留白、反应镜头、空镜。
  def hold_full?(shot)
    return true if shot["holdFull"] == true

    [shot["cameraIntent"], shot["summary"]].any? { |text| text.to_s.match?(HOLD_WORDS) }
  end

  def dialogue?(shot)
    Array(shot["dialogue"]).any? { |line| line.is_a?(Hash) && !line["text"].to_s.strip.empty? }
  end

  # 配音节奏下这一镜的出点。返回一条与 trim 同形的裁剪（source auto_pace）或 nil（不用裁、或算不出）：
  # - 没台词：入点 + pacingSilentMaxSeconds；
  # - 口型音轨（ref2va）：音轨从原片 0 毫秒起，出点 = 音轨长度 + 尾留白；
  # - 单独配音：入点 + 首留白 + 各句 + 句间间隔 + 尾留白（有句子还没配音时算不出，先不裁）；
  # - 用视频自带的声音：不知道台词何时说完，不裁。
  # 已有手动 / 质检裁剪时在它的窗口内再收，入点沿用。clip_ms 可显式传（单测不依赖 ffprobe）。
  def pace_trim(shot, job, trim, voice_source, track, lines, settings, clip_ms: nil)
    clip_ms ||= trim && trim["clipDurationMs"].to_i.positive? ? trim["clipDurationMs"].to_i : ClipTrim.clip_ms(job)
    return nil unless clip_ms.to_i.positive?

    in_ms = (ClipTrim.in_seconds(trim) * 1000).round
    stored_out = ClipTrim.out_seconds(trim)
    out_ms = stored_out ? [(stored_out * 1000).round, clip_ms].min : clip_ms
    tail_ms = (settings["pacingTailSeconds"].to_f * 1000).round
    reason = nil
    target = if !dialogue?(shot)
               reason = "silent_cap"
               in_ms + (settings["pacingSilentMaxSeconds"].to_f * 1000).round
             elsif track
               reason = "dialogue_end"
               track["durationMs"].to_i + tail_ms
             elsif voice_source == "tts"
               return nil if lines.any? { |line| %w[missing stale].include?(line["audio"]) }

               ready = lines.select { |line| line["audio"] == "ready" && line["durationMs"].to_i.positive? }
               return nil if ready.empty?

               reason = "dialogue_end"
               in_ms + LEAD_IN_MS + ready.sum { |line| line["durationMs"].to_i } + GAP_MS * (ready.length - 1) + tail_ms
             else
               return nil
             end
    return nil if target >= out_ms - PACE_EPSILON_MS
    return nil if target - in_ms < PACE_MIN_KEPT_MS

    paced = { "jobID" => job["id"], "outSeconds" => (target / 1000.0).round(2), "source" => "auto_pace", "reason" => reason,
              "clipDurationMs" => clip_ms, "keptDurationMs" => target - in_ms }
    paced["inSeconds"] = (in_ms / 1000.0).round(2) if in_ms.positive?
    paced["basedOn"] = trim["source"] if trim && trim["source"]
    paced
  end

  # 计划里的裁剪：原样带上，另给原片长度与裁剪后的长度（读不出原片长度时只有出点能算）。
  def trim_summary(trim, job)
    return nil unless trim

    clip_ms = ClipTrim.clip_ms(job)
    trim.merge("clipDurationMs" => clip_ms, "keptDurationMs" => ClipTrim.kept_ms(trim, clip_ms)).reject { |_key, value| value.nil? }
  end

  # ref2va 片段生成时用的对白音轨（单句是那句配音，多句是拼好的整条）；文件不在或读不了返回 nil。
  def reference_track(job)
    path = job["referenceAudioPath"].to_s
    return nil if path.empty? || !File.file?(path)

    { "filePath" => path, "durationMs" => WavTools.info(path).duration_ms }
  rescue WavTools::Error
    nil
  end

  # 口型音轨与画面同窗截取：有入点时音轨也从入点起算。
  def shot_speech_ms(shot)
    return speech_length_ms(shot["lines"]) unless shot["dialogueTrack"]

    [shot["dialogueTrack"]["durationMs"].to_i - (ClipTrim.in_seconds(effective_trim(shot)) * 1000).round, 0].max
  end

  # 镜头选定的视频优先；没选或选的那条已不可用时取该镜最新一条已完成、有成片文件的任务。
  def pick_video(shot, jobs)
    usable = jobs.select do |job|
      job["shotID"] == shot["id"] && job["state"] == "completed" && File.file?(job["outputPath"].to_s)
    end
    selected_id = shot["selectedVideoID"].to_s
    unless selected_id.empty?
      chosen = usable.find { |job| job["id"] == selected_id }
      return [chosen, "selected"] if chosen
    end
    latest = usable.max_by { |job| [job["createdAt"].to_s, job["updatedAt"].to_s] }
    latest ? [latest, "latest"] : [nil, nil]
  end

  def video_summary(job, source)
    return { "jobID" => nil, "source" => nil } unless job

    {
      "jobID" => job["id"],
      "source" => source,
      "outputPath" => job["outputPath"],
      "playbackURL" => job["playbackURL"],
      "posterURL" => job["posterURL"]
    }.reject { |_, value| value.nil? }
  end

  def plan_lines(drama, shot, voice_source, media_root)
    bindings = shot["package"].is_a?(Hash) ? Array(shot["package"]["cast"]) : []
    Array(shot["dialogue"]).select { |line| line.is_a?(Hash) && !line["text"].to_s.strip.empty? }.map do |line|
      entry = { "lineID" => line["id"], "speaker" => line["speaker"].to_s, "text" => line["text"].to_s }
      if voice_source == "video"
        entry.merge("audio" => "not_needed")
      else
        entry.merge(line_audio(drama, line, bindings, media_root))
      end
    end
  end

  def line_audio(drama, line, bindings, media_root)
    selected = line["audio"].is_a?(Hash) ? MediaRef.selected(line["audio"], media_root) : nil
    if selected
      current = ReviewMaterial.fingerprint(line["text"].to_s.strip)
      stale = !selected["textFingerprint"].to_s.empty? && selected["textFingerprint"] != current
      return { "audio" => stale ? "stale" : "ready", "durationMs" => selected["durationMs"], "filePath" => selected["filePath"] }
    end
    voice = resolve_voice(drama, line, bindings)
    { "audio" => voice ? "missing" : "no_voice" }
  end

  # 与 VoiceGeneration#voice_for 相同的取音色顺序：参考包绑定 → 该角色已授权的声音 →
  # 该角色任意未归档声音；另要求声音已确认授权，否则生成时也会被跳过。
  def resolve_voice(drama, line, bindings)
    character = Array(drama["characters"]).find { |entry| entry["name"].to_s.strip == line["speaker"].to_s.strip }
    return nil unless character

    assets = Array(drama["assets"])
    binding = bindings.find { |entry| entry.is_a?(Hash) && entry["characterID"] == character["id"] }
    voice = binding && assets.find { |entry| entry["id"] == binding["voiceID"].to_s && entry["kind"] == "voice" }
    voice ||= assets.find { |entry| entry["kind"] == "voice" && entry["characterID"] == character["id"] && !entry["archived"] && ReferencePackage.consent_granted?(entry) }
    voice && ReferencePackage.consent_granted?(voice) ? voice : nil
  end

  # 这一镜台词用到的声音（选定音频记的 voiceAssetID，没有音频时按取音色顺序）里语速异常的那些（SpeechRate.warning）。
  # 声音资产上的 speechRate 由 DramaService#decorate 算好。
  def speech_rate_warnings(drama, shot)
    bindings = shot["package"].is_a?(Hash) ? Array(shot["package"]["cast"]) : []
    assets = Array(drama["assets"])
    ids = Array(shot["dialogue"]).select { |line| line.is_a?(Hash) && !line["text"].to_s.strip.empty? }.map do |line|
      selected = line["audio"].is_a?(Hash) ? Array(line["audio"]["candidates"]).find { |entry| entry["id"] == line["audio"]["selectedCandidateID"] } : nil
      id = selected && selected["voiceAssetID"].to_s
      id = resolve_voice(drama, line, bindings)&.dig("id").to_s if id.to_s.empty?
      id
    end.reject(&:empty?).uniq
    ids.map do |id|
      voice = assets.find { |entry| entry["id"] == id && entry["kind"] == "voice" }
      voice && SpeechRate.warning(voice, voice["speechRate"])
    end.compact
  end

  # 台词按顺序排入时占用的总时长（第一句前留白 + 各句 + 句间间隔 + 末尾留白）。
  def speech_length_ms(lines)
    durations = lines.map { |line| line["durationMs"].to_i }.select(&:positive?)
    return 0 if durations.empty?

    LEAD_IN_MS + durations.sum + GAP_MS * (durations.length - 1) + GAP_MS
  end
end
