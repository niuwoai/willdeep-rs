# frozen_string_literal: true

require_relative "tts_backend"
require_relative "reference_package"
require_relative "review_material"
require_relative "speech_rate"

# `voice.generate`：用声音资产给台词配音（设计稿第 8 节）。
#
# 两种用法：
# - 试听：assetID + text，结果登记为该声音资产的候选。
# - 对白：episodeID + shotID（可选 lineIDs），逐句生成并挂在台词上。每句用哪个声音：
#   参考包里该角色绑定的 voiceID，否则该角色第一个未归档、已授权的声音资产。
#
# 授权是硬门槛：consent.status 不是 granted 的声音资产一律拒绝，不管是谁在调。
# 台词指纹记在音频上：台词改了，进度里报「音频过期」。
# dryRun 只列出将要配音的句子与声音；requestID 相同的重复调用返回上一次的结果。
class VoiceGeneration
  MAX_LINES_PER_CALL = 20

  def initialize(drama_service:, host: nil, backend_selector: nil)
    @dramas = drama_service
    @backend_selector = backend_selector || -> { TTSBackend.select(host: host, media_root: drama_service.media_root) }
  end

  def generate(arguments)
    loaded = @dramas.get("id" => arguments["dramaID"])
    return loaded unless loaded["ok"]
    drama = loaded["drama"]
    request_id = arguments["requestID"].to_s.strip[0, 200]

    plan = if !arguments["assetID"].to_s.empty?
             preview_plan(drama, arguments)
           elsif !arguments["shotID"].to_s.empty?
             dialogue_plan(drama, arguments)
           else
             failure("invalid_target", "Pass assetID with text for a preview, or episodeID and shotID for dialogue.")
           end
    return plan unless plan["ok"]

    unless request_id.empty?
      done = plan["items"].select { |item| candidates_for(drama, item).any? { |entry| entry["requestID"] == request_id } }
      unless done.empty?
        return { "ok" => true, "deduplicated" => true, "generated" => done.map { |item| { "lineID" => item["lineID"], "assetID" => item["assetID"] } }, "skipped" => plan["skipped"], "failed" => [] }
      end
    end

    if arguments["dryRun"] == true
      return { "ok" => true, "dryRun" => true, "items" => plan["items"].map { |item| item.reject { |key, _| key == "voice" } }, "skipped" => plan["skipped"] }
    end

    backend = @backend_selector.call
    return failure("tts_unavailable", "No TTS backend is configured. Set VIDEO_STUDIO_TTS_API_KEY to a DashScope (Bailian) API key, or also set VIDEO_STUDIO_TTS_PROVIDER=openai with VIDEO_STUDIO_TTS_API_BASE for an OpenAI-compatible service.") unless backend
    return failure("nothing_to_generate", "No dialogue line has a usable voice.", "skipped" => plan["skipped"]) if plan["items"].empty?

    generated = []
    failed = []
    plan["items"].each do |item|
      voice = item["voice"]
      preset = Array(voice["presets"]).find { |entry| entry["id"] == arguments["presetID"].to_s } if arguments["presetID"]
      begin
        audio = backend.synthesize(text: item["text"], voice: voice["providerVoiceID"].to_s, speed: preset && preset["speed"], instruction: preset ? preset["instruction"] : "")
      rescue TTSBackend::Error => error
        failed << { "lineID" => item["lineID"], "code" => error.code, "message" => error.message }
        next
      end
      speech = SpeechRate.rate(item["text"], audio["durationMs"])
      recorded = record(arguments, item, voice, preset, audio, request_id, speech)
      if recorded["ok"]
        entry = { "lineID" => item["lineID"], "assetID" => item["assetID"], "candidateID" => recorded["candidate"]["id"],
                  "fileName" => recorded["candidate"]["fileName"], "durationMs" => recorded["candidate"]["durationMs"], "voiceAssetID" => voice["id"] }
        entry.merge!("charsPerSecond" => speech["rate"], "speechUnit" => speech["unit"]) if speech
        generated << entry
      else
        failed << { "lineID" => item["lineID"], "code" => recorded.dig("error", "code"), "message" => recorded.dig("error", "message"), "filePath" => audio["filePath"] }
      end
    end

    summary = { "backend" => backend.name, "generated" => generated, "failed" => failed, "skipped" => plan["skipped"] }
    summary["warnings"] = speech_warnings(arguments["dramaID"], generated.map { |entry| entry["voiceAssetID"] }.uniq) unless generated.empty?
    return { "ok" => true }.merge(summary) unless generated.empty?
    { "ok" => false, "error" => { "code" => "voice_generation_failed", "message" => failed.first ? failed.first["message"] : "No audio was generated." } }.merge(summary)
  end

  # 后台任务提交前的检查（0.36.0-rc1）：现在有没有可用的 TTS 后端。配置写错时
  # 当作没有，真正调用时再把原因报出来。
  def available?
    !@backend_selector.call.nil?
  rescue TTSBackend::Error
    false
  end

  private

  def preview_plan(drama, arguments)
    voice = Array(drama["assets"]).find { |entry| entry["id"] == arguments["assetID"].to_s }
    return failure("asset_not_found", "Voice asset was not found.") unless voice && voice["kind"] == "voice"
    return failure("voice_consent_missing", "Voice \"#{voice['name']}\" has no confirmed consent; generation is disabled.") unless ReferencePackage.consent_granted?(voice)
    text = arguments["text"].to_s.strip
    text = voice["referenceTranscript"].to_s.strip if text.empty?
    return failure("empty_text", "Pass text to preview, or fill the voice's referenceTranscript.") if text.empty?
    { "ok" => true, "items" => [{ "kind" => "preview", "assetID" => voice["id"], "voiceName" => voice["name"], "text" => text, "voice" => voice }], "skipped" => [] }
  end

  def dialogue_plan(drama, arguments)
    episode = drama["episodes"].find { |entry| entry["id"] == arguments["episodeID"].to_s }
    return failure("episode_not_found", "Episode was not found.") unless episode
    shot = episode["shots"].find { |entry| entry["id"] == arguments["shotID"].to_s }
    return failure("shot_not_found", "Shot was not found.") unless shot
    if shot["draft"].is_a?(Hash) && !shot["draft"].empty?
      return failure("shot_has_draft", "This shot has an unadopted draft. Adopt or discard it before generating audio.")
    end

    wanted = arguments["lineIDs"].is_a?(Array) ? arguments["lineIDs"].map(&:to_s) : nil
    bindings = shot["package"].is_a?(Hash) ? Array(shot["package"]["cast"]) : []
    items = []
    skipped = []
    Array(shot["dialogue"]).each do |line|
      next unless line.is_a?(Hash)
      next if wanted && !wanted.include?(line["id"].to_s)
      text = line["text"].to_s.strip
      next skipped << { "lineID" => line["id"], "reason" => "empty_text" } if text.empty?

      character = drama["characters"].find { |entry| entry["name"].to_s.strip == line["speaker"].to_s.strip }
      next skipped << { "lineID" => line["id"], "reason" => "speaker_not_in_cast", "speaker" => line["speaker"] } unless character
      voice = voice_for(drama, character, bindings)
      next skipped << { "lineID" => line["id"], "reason" => "no_voice_asset", "speaker" => line["speaker"] } unless voice
      next skipped << { "lineID" => line["id"], "reason" => "voice_consent_missing", "assetID" => voice["id"] } unless ReferencePackage.consent_granted?(voice)
      items << { "kind" => "dialogue", "lineID" => line["id"], "speaker" => line["speaker"], "text" => text, "assetID" => voice["id"], "voiceName" => voice["name"], "voice" => voice }
      break if items.length >= MAX_LINES_PER_CALL
    end
    { "ok" => true, "items" => items, "skipped" => skipped, "episodeID" => episode["id"], "shotID" => shot["id"] }
  end

  def voice_for(drama, character, bindings)
    binding = bindings.find { |entry| entry.is_a?(Hash) && entry["characterID"] == character["id"] }
    bound = binding && Array(drama["assets"]).find { |entry| entry["id"] == binding["voiceID"].to_s && entry["kind"] == "voice" }
    return bound if bound
    Array(drama["assets"]).find { |entry| entry["kind"] == "voice" && entry["characterID"] == character["id"] && !entry["archived"] && ReferencePackage.consent_granted?(entry) } ||
      Array(drama["assets"]).find { |entry| entry["kind"] == "voice" && entry["characterID"] == character["id"] && !entry["archived"] }
  end

  # 这次用到的声音里，整体语速偏慢 / 偏快的（0.40.0-rc1，lib/speech_rate.rb）：按存档里这个声音的全部音频算平均。
  def speech_warnings(drama_id, voice_ids)
    loaded = @dramas.get("id" => drama_id)
    return [] unless loaded["ok"]

    voice_ids.map do |id|
      voice = Array(loaded["drama"]["assets"]).find { |entry| entry["id"] == id }
      voice && SpeechRate.warning(voice, voice["speechRate"])
    end.compact
  end

  def record(arguments, item, voice, preset, audio, request_id, speech = nil)
    common = { "dramaID" => arguments["dramaID"], "filePath" => audio["filePath"], "model" => audio["model"], "durationMs" => audio["durationMs"],
               "requestID" => request_id, "presetID" => preset && preset["id"] }
    common.merge!("charsPerSecond" => speech["rate"], "speechUnit" => speech["unit"]) if speech
    if item["kind"] == "preview"
      @dramas.record_asset_media(common.merge("assetID" => voice["id"], "prompt" => item["text"]))
    else
      @dramas.record_dialogue_audio(common.merge("episodeID" => arguments["episodeID"], "shotID" => arguments["shotID"], "lineID" => item["lineID"],
                                                 "voiceAssetID" => voice["id"], "textFingerprint" => ReviewMaterial.fingerprint(item["text"])))
    end
  end

  def candidates_for(drama, item)
    if item["kind"] == "preview"
      asset = Array(drama["assets"]).find { |entry| entry["id"] == item["assetID"] }
      Array(asset && asset["candidates"])
    else
      drama["episodes"].each do |episode|
        episode["shots"].each do |shot|
          line = Array(shot["dialogue"]).find { |entry| entry.is_a?(Hash) && entry["id"] == item["lineID"] }
          return Array(line["audio"].is_a?(Hash) ? line["audio"]["candidates"] : []) if line
        end
      end
      []
    end
  end

  def failure(code, message, extra = {})
    { "ok" => false, "error" => { "code" => code, "message" => message } }.merge(extra)
  end
end
