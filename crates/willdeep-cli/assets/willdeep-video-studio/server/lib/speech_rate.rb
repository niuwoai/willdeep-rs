# frozen_string_literal: true

# 配音语速自检（0.40.0-rc1）。
#
# 《回村养鸭》：预置音色「Arthur」是慢吞吞的老年讲故事腔，给 38 岁的生意人配音只有每秒 1.3～1.9 个字，
# 用户说「像聊斋」。正常对白每秒 3.5～5 个字。现在每条试听 / 台词音频记下语速（charsPerSecond），
# 一个声音资产所有音频的平均语速低于 2.5 或高于 6.5 字 / 秒时给告警：voice.generate 结果、声音资产摘要、
# 成片计划里用到这个声音的镜头，页面在声音资产上显示「语速偏慢 1.9字/秒」。
#
# 计数：中日韩文字按字数；一个都没有时按拉丁词数（单位 words，阈值另给）。标点、空白不算。
module SpeechRate
  module_function

  CJK = /[\p{Han}\p{Hiragana}\p{Katakana}\p{Hangul}]/.freeze
  WORD = /[A-Za-z0-9]+(?:['’][A-Za-z]+)?/.freeze
  # 字 / 秒（中日韩）与词 / 秒（拉丁）的正常区间；超出即告警。
  LIMITS = { "chars" => { slow: 2.5, fast: 6.5 }, "words" => { slow: 1.2, fast: 4.0 } }.freeze
  # 太短的音频（小于这么多毫秒）不参与：一两个字的语气词算不出语速。
  MIN_DURATION_MS = 800
  MIN_UNITS = 3

  # [数量, 单位]：单位 chars（中日韩字数）或 words（拉丁词数）。
  def units(text)
    body = text.to_s
    chars = body.scan(CJK).length
    return [chars, "chars"] if chars.positive?

    [body.scan(WORD).length, "words"]
  end

  # 每秒多少字（或词），保留一位小数；算不出（没有时长、太短、太少字）返回 nil。
  def rate(text, duration_ms)
    count, unit = units(text)
    ms = duration_ms.to_i
    return nil if ms < MIN_DURATION_MS || count < MIN_UNITS

    { "rate" => (count * 1000.0 / ms).round(1), "unit" => unit }
  end

  # 一个声音资产的语速：试听候选 + 台词音频里 voiceAssetID 是它的那些。候选上存了 charsPerSecond 就用，
  # 没存（0.40 之前的音频）按候选 prompt（台词 / 试听文字）与时长现算。只取占多数的那种单位。
  # 返回 {charsPerSecond, unit, samples, status: normal|slow|fast} 或 nil（没有样本）。
  def for_voice(drama, voice)
    samples = []
    Array(voice["candidates"]).each { |candidate| samples << sample(candidate) if candidate.is_a?(Hash) }
    Array(drama["episodes"]).each do |episode|
      Array(episode["shots"]).each do |shot|
        Array(shot["dialogue"]).each do |line|
          next unless line.is_a?(Hash) && line["audio"].is_a?(Hash)

          Array(line["audio"]["candidates"]).each do |candidate|
            samples << sample(candidate, line["text"]) if candidate["voiceAssetID"].to_s == voice["id"].to_s
          end
        end
      end
    end
    samples.compact!
    return nil if samples.empty?

    unit = samples.group_by { |entry| entry["unit"] }.max_by { |_unit, group| group.length }.first
    picked = samples.select { |entry| entry["unit"] == unit }
    average = (picked.sum { |entry| entry["rate"] } / picked.length).round(1)
    limits = LIMITS.fetch(unit)
    status = if average < limits[:slow] then "slow"
             elsif average > limits[:fast] then "fast"
             else "normal"
             end
    { "charsPerSecond" => average, "unit" => unit, "samples" => picked.length, "status" => status }
  end

  def sample(candidate, fallback_text = nil)
    stored = candidate["charsPerSecond"]
    if stored.is_a?(Numeric) && stored.positive?
      return { "rate" => stored.to_f, "unit" => candidate["speechUnit"].to_s.empty? ? "chars" : candidate["speechUnit"].to_s }
    end

    rate(candidate["prompt"].to_s.empty? ? fallback_text : candidate["prompt"], candidate["durationMs"])
  end

  # 告警：{code speech_rate_slow|speech_rate_fast, assetID, voiceName, charsPerSecond, unit, samples, message}；正常返回 nil。
  def warning(voice, stats)
    return nil unless stats && stats["status"] != "normal"

    slow = stats["status"] == "slow"
    unit = stats["unit"] == "words" ? "词" : "字"
    limits = LIMITS.fetch(stats["unit"])
    range = "#{limits[:slow]}～#{limits[:fast]}"
    advice = slow ? "换一个语速正常的音色（providerVoiceID），或调高预设的 speed" : "换一个音色（providerVoiceID），或调低预设的 speed"
    { "code" => slow ? "speech_rate_slow" : "speech_rate_fast", "assetID" => voice["id"], "voiceName" => voice["name"],
      "charsPerSecond" => stats["charsPerSecond"], "unit" => stats["unit"], "samples" => stats["samples"],
      "message" => "声音「#{voice['name']}」的音频平均每秒 #{stats['charsPerSecond']} #{unit}（#{stats['samples']} 条），" \
                   "正常对白约每秒 #{range} #{unit}，听起来#{slow ? '偏慢、拖沓' : '偏快、赶'}。建议#{advice}后重新配音。" }
  end

  # 读出时给每个声音资产挂只读的 speechRate（DramaService#decorate 调用）。
  def annotate!(drama)
    Array(drama["assets"]).each do |asset|
      next unless asset.is_a?(Hash) && asset["kind"] == "voice"

      stats = for_voice(drama, asset)
      if stats
        asset["speechRate"] = stats
      else
        asset.delete("speechRate")
      end
    end
    drama
  end
end
