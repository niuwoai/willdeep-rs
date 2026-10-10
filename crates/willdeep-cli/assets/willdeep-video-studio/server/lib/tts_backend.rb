# frozen_string_literal: true

require "fileutils"
require "json"
require "net/http"
require "openssl"
require "securerandom"
require "uri"
require_relative "host_bridge"

# TTS 后端（设计稿 7.3、3.4）。
#
# 选择顺序（TTSBackend.select）：
# 1. 宿主宣告了 willdeep/audio/synthesize → 宿主代管（HostSpeechBackend）；
# 2. 没有 VIDEO_STUDIO_TTS_API_KEY → nil（voice.generate 返回 tts_unavailable）；
# 3. VIDEO_STUDIO_TTS_PROVIDER 为 dashscope / openai 时按它选；其他值抛 tts_backend_misconfigured；
# 4. 未指定 provider：VIDEO_STUDIO_TTS_API_BASE 为空或指向 dashscope*.aliyuncs.com → dashscope；
#    base 指向别的主机 → openai。最后这条是为了兼容：0.31 之前的独立运行配置只填了
#    base + key，走的是 OpenAI 兼容接口，不能因为新增了默认后端就悄悄换掉。
#
# 后端：
# - dashscope（DashScopeBackend，见 dashscope_tts.rb）：百炼原生 Qwen TTS，模型默认 qwen3-tts-flash；
# - openai（DirectSpeechBackend）：OpenAI 兼容 `/v1/audio/speech`，POST JSON
#   { model, input, voice, response_format, speed, instructions }，回 WAV 二进制；模型默认
#   qwen3-tts，必须配 VIDEO_STUDIO_TTS_API_BASE。
# 服务侧的音色 ID 以 voice 字段透传。声音克隆条件不在插件侧生成或持有，插件只保存音频文件与版本记录。
#
# 环境变量：VIDEO_STUDIO_TTS_PROVIDER / VIDEO_STUDIO_TTS_API_BASE / VIDEO_STUDIO_TTS_API_KEY /
# VIDEO_STUDIO_TTS_MODEL / VIDEO_STUDIO_TTS_OPEN_TIMEOUT / VIDEO_STUDIO_TTS_READ_TIMEOUT。
module TTSBackend
  MAX_TEXT_CHARACTERS = 2_000
  MAX_AUDIO_BYTES = 64 * 1024 * 1024
  DEFAULT_MODEL = "qwen3-tts"

  class Error < StandardError
    attr_reader :code

    def initialize(code, message)
      super(message)
      @code = code
    end
  end

  PROVIDER_DASHSCOPE = "dashscope"
  PROVIDER_OPENAI = "openai"
  PROVIDERS = [PROVIDER_DASHSCOPE, PROVIDER_OPENAI].freeze

  def self.select(host: nil, env: ENV, media_root:)
    return HostSpeechBackend.new(host) if host && host.supports?(HostBridge::TTS_GENERATE)

    base = env["VIDEO_STUDIO_TTS_API_BASE"].to_s.strip
    key = env["VIDEO_STUDIO_TTS_API_KEY"].to_s.strip
    return nil if key.empty?

    model = env["VIDEO_STUDIO_TTS_MODEL"].to_s.strip
    open_timeout = (env["VIDEO_STUDIO_TTS_OPEN_TIMEOUT"] || 20).to_i
    read_timeout = (env["VIDEO_STUDIO_TTS_READ_TIMEOUT"] || 120).to_i
    if provider_for(env, base) == PROVIDER_DASHSCOPE
      return DashScopeBackend.new(base_url: base, api_key: key, media_root: media_root, model: model,
                                  open_timeout: open_timeout, read_timeout: read_timeout, env: env)
    end
    return nil if base.empty?

    DirectSpeechBackend.new(base_url: base, api_key: key, media_root: media_root, model: model,
                            open_timeout: open_timeout, read_timeout: read_timeout)
  end

  # 规则见文件头注释第 3、4 条。
  def self.provider_for(env, base)
    explicit = env["VIDEO_STUDIO_TTS_PROVIDER"].to_s.strip.downcase
    return explicit if PROVIDERS.include?(explicit)
    unless explicit.empty?
      raise Error.new("tts_backend_misconfigured", "VIDEO_STUDIO_TTS_PROVIDER must be one of #{PROVIDERS.join(', ')}.")
    end

    base.empty? || DashScopeBackend.dashscope_host?(base) ? PROVIDER_DASHSCOPE : PROVIDER_OPENAI
  end

  class HostSpeechBackend
    def initialize(host)
      @host = host
    end

    def name
      "host"
    end

    def synthesize(text:, voice: "", speed: nil, instruction: "")
      result = @host.request(HostBridge::TTS_GENERATE, {
        "provider" => "qwen3-tts",
        "model" => TTSBackend::DEFAULT_MODEL,
        "input" => text.to_s,
        "voice" => voice.to_s,
        "speed" => speed,
        "instructions" => instruction.to_s,
        "responseFormat" => "wav"
      })
      unless result.is_a?(Hash) && !result["filePath"].to_s.empty?
        raise Error.new("invalid_tts_response", "Host returned no audio file.")
      end
      { "filePath" => result["filePath"], "mediaURL" => result["mediaURL"],
        "durationMs" => result["durationMs"], "model" => result["model"].to_s.empty? ? DEFAULT_MODEL : result["model"] }
    rescue HostBridge::Unsupported
      raise Error.new("tts_unavailable", "The host does not provide managed TTS.")
    rescue HostBridge::RequestFailed => error
      raise Error.new("tts_host_error", error.message)
    end
  end

  # WAV 时长：读 RIFF 头里的 byteRate 与 data 块大小。读不出来返回 nil，不报错。
  def self.wav_duration_ms(binary)
    return nil unless binary.is_a?(String) && binary.bytesize > 44 && binary[0, 4] == "RIFF".b && binary[8, 4] == "WAVE".b

    offset = 12
    byte_rate = nil
    data_size = nil
    while offset + 8 <= binary.bytesize
      chunk_id = binary[offset, 4]
      chunk_size = binary[offset + 4, 4].unpack("V").first
      if chunk_id == "fmt ".b && chunk_size >= 16
        byte_rate = binary[offset + 16, 4].unpack("V").first
      elsif chunk_id == "data".b
        data_size = chunk_size
        break
      end
      offset += 8 + chunk_size + (chunk_size.odd? ? 1 : 0)
    end
    return nil unless byte_rate && byte_rate.positive? && data_size

    (data_size.to_f / byte_rate * 1000).round
  end

  class DirectSpeechBackend
    def initialize(base_url:, api_key:, media_root:, model: "", open_timeout: 20, read_timeout: 120)
      @base_url = normalize_base_url(base_url)
      @api_key = api_key.to_s.strip
      @media_root = File.expand_path(media_root.to_s)
      @model = model.to_s.empty? ? DEFAULT_MODEL : model.to_s
      @open_timeout = open_timeout
      @read_timeout = read_timeout
    end

    attr_reader :base_url, :model

    def name
      "direct"
    end

    # 返回 { "filePath", "durationMs", "model" }。
    def synthesize(text:, voice: "", speed: nil, instruction: "")
      input = text.to_s.strip
      raise Error.new("empty_text", "There is nothing to speak.") if input.empty?
      raise Error.new("text_too_long", "Text is longer than #{MAX_TEXT_CHARACTERS} characters.") if input.length > MAX_TEXT_CHARACTERS

      body = { "model" => @model, "input" => input, "response_format" => "wav" }
      body["voice"] = voice.to_s unless voice.to_s.empty?
      body["speed"] = speed.to_f if speed && speed.to_f.positive?
      body["instructions"] = instruction.to_s unless instruction.to_s.strip.empty?

      binary = perform(body)
      raise Error.new("invalid_tts_response", "The TTS API returned no audio.") if binary.bytesize.zero?
      raise Error.new("audio_too_large", "The generated audio is larger than 64 MiB.") if binary.bytesize > MAX_AUDIO_BYTES

      FileUtils.mkdir_p(@media_root)
      extension = binary[0, 4] == "RIFF".b ? ".wav" : ".mp3"
      path = File.join(@media_root, "voice-#{SecureRandom.hex(12)}#{extension}")
      File.binwrite(path, binary)
      { "filePath" => path, "durationMs" => TTSBackend.wav_duration_ms(binary), "model" => @model }
    rescue SystemCallError, IOError => error
      raise Error.new("audio_write_failed", "Unable to write the generated audio: #{error.class}")
    end

    private

    def normalize_base_url(value)
      token = value.to_s.strip.sub(%r{/+\z}, "")
      raise Error.new("tts_backend_misconfigured", "TTS API base URL is empty.") if token.empty?
      uri = URI.parse(token)
      local_http = uri.scheme == "http" && %w[localhost 127.0.0.1 ::1].include?(uri.host)
      raise Error.new("tts_backend_misconfigured", "TTS API base URL must use HTTPS (HTTP only for localhost).") unless uri.host && (uri.scheme == "https" || local_http)
      token.sub(%r{/v1\z}i, "")
    rescue URI::InvalidURIError
      raise Error.new("tts_backend_misconfigured", "TTS API base URL is invalid.")
    end

    def perform(body)
      uri = URI.parse("#{@base_url}/v1/audio/speech")
      request = Net::HTTP::Post.new(uri)
      request["Authorization"] = "Bearer #{@api_key}"
      request["Content-Type"] = "application/json"
      request["Accept"] = "audio/wav, application/octet-stream, application/json"
      request.body = JSON.generate(body)
      http = Net::HTTP.new(uri.host, uri.port)
      http.use_ssl = uri.scheme == "https"
      http.verify_mode = OpenSSL::SSL::VERIFY_PEER if http.use_ssl?
      http.open_timeout = @open_timeout
      http.read_timeout = @read_timeout
      response = http.request(request)
      unless (200..299).cover?(response.code.to_i)
        raise Error.new("tts_api_error", "TTS API responded #{response.code}: #{error_message(response.body)}")
      end
      response.body.to_s.b
    rescue SocketError, SystemCallError, Net::OpenTimeout, Net::ReadTimeout, OpenSSL::SSL::SSLError, IOError => error
      raise Error.new("tts_api_unreachable", "TTS API request failed: #{error.class}")
    end

    def error_message(body)
      parsed = JSON.parse(body.to_s)
      parsed.is_a?(Hash) && parsed["error"].is_a?(Hash) ? parsed["error"]["message"].to_s[0, 300] : body.to_s[0, 300]
    rescue JSON::ParserError
      body.to_s[0, 300]
    end
  end
end

# 放在末尾：DashScopeBackend 要用上面定义的 TTSBackend::Error 与 MAX_* 常量。
require_relative "dashscope_tts"
