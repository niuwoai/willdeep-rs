# frozen_string_literal: true

require "base64"
require "fileutils"
require "json"
require "net/http"
require "openssl"
require "securerandom"
require "tmpdir"
require "uri"
# 不在这里 require tts_backend：它在文件末尾 require 本文件，反过来再 require 一次就是
# 循环加载。调用方一律 require "tts_backend"，本类用到的 TTSBackend::Error 等在那里定义。
require_relative "wav_tools"
require_relative "ffmpeg_tool"

# 百炼（DashScope）原生 Qwen TTS 适配器（设计稿 episode-compose 4.1）。
#
# 接口：POST {base}/api/v1/services/aigc/multimodal-generation/generation
#   { "model", "input": { "text", "voice", "language_type" [, "instructions"] } }
# 回包的 output.audio.url 是一条 24 小时有效的 WAV 下载地址，这里拿到就立即下载，
# 不把 URL 往外传。下载地址不带 API Key（那是 OSS 签名链接，Key 只发给百炼本身）。
#
# 与 OpenAI 兼容接口的差异由本类抹平：
# - 单次输入有长度上限：超过 600 字按句末标点切开，逐段合成后用 WavTools 无缝拼接；
# - 没有语速参数：speed ≠ 1 时用 ffmpeg atempo 变速，没有 ffmpeg 就原速输出并在结果里
#   标 speedIgnored；
# - instructions 只有 instruct 系列模型认，其他模型不透传。
module TTSBackend
  class DashScopeBackend
    DEFAULT_BASE = "https://dashscope.aliyuncs.com"
    DEFAULT_MODEL = "qwen3-tts-flash"
    # 声音资产没填 providerVoiceID 时用的百炼系统音色。
    DEFAULT_VOICE = "Cherry"
    DEFAULT_LANGUAGE_TYPE = "Chinese"
    ENDPOINT_PATH = "/api/v1/services/aigc/multimodal-generation/generation"
    MAX_PART_CHARACTERS = 600
    SENTENCE_BREAKS = "。！？!?；;…\n"
    SENTENCE_PATTERN = /[^#{SENTENCE_BREAKS}]+[#{SENTENCE_BREAKS}]*|[#{SENTENCE_BREAKS}]+/.freeze
    # ffmpeg 单个 atempo 只接受 0.5–2，超出的倍速拆成几级串起来。
    ATEMPO_MIN = 0.5
    ATEMPO_MAX = 2.0
    SPEED_MIN = 0.25
    SPEED_MAX = 4.0
    SPEED_TOLERANCE = 0.01
    ATEMPO_TIMEOUT_SECONDS = 120
    LOCAL_HOSTS = %w[localhost 127.0.0.1 ::1].freeze
    ERROR_EXCERPT = 300

    # base 指向百炼（dashscope*.aliyuncs.com）时返回 true。TTSBackend.select 用它决定
    # 未指定 provider 时是否沿用旧的 OpenAI 兼容直连。
    def self.dashscope_host?(base)
      host = URI.parse(base.to_s.strip).hostname.to_s.downcase
      host.end_with?(".aliyuncs.com") && host.split(".").first.to_s.start_with?("dashscope")
    rescue URI::InvalidURIError
      false
    end

    # 百炼回包里的音频直链指向阿里云 OSS，实际回的常常是 http://（同一主机也支持 HTTPS，
    # 签名与协议无关）。阿里云主机的 http 一律改走 HTTPS 再下载；其他主机仍只收 HTTPS，
    # 本机测试放行 HTTP。此前直接拒收，百炼配音一句都合成不出来。
    def self.audio_download_uri(url)
      token = url.to_s.strip
      uri = URI.parse(token)
      host = uri.hostname.to_s.downcase
      uri = URI.parse(token.sub(/\Ahttp:/i, "https:")) if uri.scheme == "http" && host.end_with?(".aliyuncs.com")
      local_http = uri.scheme == "http" && LOCAL_HOSTS.include?(host)
      unless uri.hostname && (uri.scheme == "https" || local_http)
        raise Error.new("invalid_tts_response", "DashScope audio URL must use HTTPS.")
      end

      uri
    end

    # 按句末标点切段，每段不超过 limit 字；单句超长时硬切。
    def self.split_text(text, limit: MAX_PART_CHARACTERS)
      input = text.to_s
      return [input] if input.length <= limit

      parts = []
      current = +""
      input.scan(SENTENCE_PATTERN).each do |sentence|
        if sentence.length > limit
          parts << current unless current.strip.empty?
          current = +""
          sentence.scan(/.{1,#{limit}}/m) { |slice| parts << slice }
          next
        end
        if current.length + sentence.length > limit
          parts << current unless current.strip.empty?
          current = sentence.dup
        else
          current << sentence
        end
      end
      parts << current unless current.strip.empty?
      parts.map(&:strip).reject(&:empty?)
    end

    # speed 转成 atempo 滤镜链；不需要变速时返回 nil。
    def self.atempo_filter(speed)
      value = speed.to_f
      return nil unless value.positive? && (value - 1.0).abs > SPEED_TOLERANCE

      value = [[value, SPEED_MIN].max, SPEED_MAX].min
      filters = []
      while value > ATEMPO_MAX
        filters << "atempo=#{ATEMPO_MAX}"
        value /= ATEMPO_MAX
      end
      while value < ATEMPO_MIN
        filters << "atempo=#{ATEMPO_MIN}"
        value /= ATEMPO_MIN
      end
      filters << format("atempo=%.4f", value)
      filters.join(",")
    end

    # ffmpeg_path：:auto 时按 FFmpegTool 的找法（PATH + Homebrew 目录）；测试可传 nil
    # 模拟没装 ffmpeg，或传一个具体路径。
    def initialize(api_key:, media_root:, base_url: nil, model: nil, language_type: DEFAULT_LANGUAGE_TYPE,
                   open_timeout: 20, read_timeout: 120, max_audio_bytes: TTSBackend::MAX_AUDIO_BYTES,
                   ffmpeg_path: :auto, env: ENV)
      @api_key = api_key.to_s.strip
      raise Error.new("tts_backend_misconfigured", "DashScope API key is empty.") if @api_key.empty?

      @base_url = normalize_base_url(base_url.to_s.strip.empty? ? DEFAULT_BASE : base_url)
      @media_root = File.expand_path(media_root.to_s)
      @model = model.to_s.strip.empty? ? DEFAULT_MODEL : model.to_s.strip
      @language_type = language_type.to_s.empty? ? DEFAULT_LANGUAGE_TYPE : language_type.to_s
      @open_timeout = open_timeout
      @read_timeout = read_timeout
      @max_audio_bytes = max_audio_bytes
      @ffmpeg_path = ffmpeg_path
      @env = env
    end

    attr_reader :base_url, :model

    def name
      "dashscope"
    end

    # 返回 { "filePath", "durationMs", "model" }；要求变速却没能变速时多一个 "speedIgnored" => true。
    def synthesize(text:, voice: "", speed: nil, instruction: "")
      input = text.to_s.strip
      raise Error.new("empty_text", "There is nothing to speak.") if input.empty?
      if input.length > TTSBackend::MAX_TEXT_CHARACTERS
        raise Error.new("text_too_long", "Text is longer than #{TTSBackend::MAX_TEXT_CHARACTERS} characters.")
      end

      parts = self.class.split_text(input)
      FileUtils.mkdir_p(@media_root)
      output = File.join(@media_root, "voice-#{SecureRandom.hex(12)}.wav")
      speed_ignored = false
      Dir.mktmpdir("video-studio-tts-") do |scratch|
        files = parts.each_with_index.map do |part, index|
          path = File.join(scratch, "part-#{index}.wav")
          File.binwrite(path, synthesize_part(part, voice, instruction))
          path
        end
        joined = join_parts(files, File.join(scratch, "joined.wav"))
        final = joined
        filter = self.class.atempo_filter(speed)
        if filter
          changed = change_speed(joined, File.join(scratch, "tempo.wav"), filter)
          if changed
            final = changed
          else
            speed_ignored = true
          end
        end
        FileUtils.cp(final, output)
      end
      result = { "filePath" => output, "durationMs" => duration_ms(output), "model" => @model }
      result["speedIgnored"] = true if speed_ignored
      result
    rescue SystemCallError, IOError => error
      raise Error.new("audio_write_failed", "Unable to write the generated audio: #{error.class}")
    end

    private

    def normalize_base_url(value)
      token = value.to_s.strip.sub(%r{/+\z}, "")
      uri = URI.parse(token)
      local_http = uri.scheme == "http" && LOCAL_HOSTS.include?(uri.hostname.to_s)
      unless uri.hostname && (uri.scheme == "https" || local_http)
        raise Error.new("tts_backend_misconfigured", "DashScope base URL must use HTTPS (HTTP only for localhost).")
      end
      # 用户可能照抄了控制台里的 /api/v1 或兼容模式地址；只保留到主机这一层。
      token.sub(%r{/(api/v1|compatible-mode/v1)\z}i, "")
    rescue URI::InvalidURIError
      raise Error.new("tts_backend_misconfigured", "DashScope base URL is invalid.")
    end

    def synthesize_part(text, voice, instruction)
      body = {
        "model" => @model,
        "input" => {
          "text" => text,
          "voice" => voice.to_s.strip.empty? ? DEFAULT_VOICE : voice.to_s.strip,
          "language_type" => @language_type
        }
      }
      if @model.downcase.include?("instruct") && !instruction.to_s.strip.empty?
        body["input"]["instructions"] = instruction.to_s.strip
      end
      audio = request_audio(body)
      binary = audio[:data] || download(audio[:url])
      raise Error.new("invalid_tts_response", "DashScope returned no audio.") if binary.bytesize.zero?
      unless binary[0, 4] == "RIFF".b && binary[8, 4] == "WAVE".b
        raise Error.new("invalid_tts_response", "DashScope audio is not a WAV file.")
      end

      binary
    end

    def request_audio(body)
      uri = URI.parse("#{@base_url}#{ENDPOINT_PATH}")
      request = Net::HTTP::Post.new(uri)
      request["Authorization"] = "Bearer #{@api_key}"
      request["Content-Type"] = "application/json"
      request["Accept"] = "application/json"
      request.body = JSON.generate(body)
      response = http_for(uri).request(request)
      unless (200..299).cover?(response.code.to_i)
        raise Error.new("tts_api_error", "DashScope TTS responded #{response.code}: #{error_message(response.body)}")
      end

      parse_audio(response.body)
    rescue SocketError, SystemCallError, Net::OpenTimeout, Net::ReadTimeout, OpenSSL::SSL::SSLError, IOError => error
      raise Error.new("tts_api_unreachable", "DashScope TTS request failed: #{error.class}")
    end

    def parse_audio(raw)
      parsed = JSON.parse(raw.to_s)
      raise Error.new("invalid_tts_response", "DashScope returned an unexpected body.") unless parsed.is_a?(Hash)

      audio = parsed["output"].is_a?(Hash) ? parsed["output"]["audio"] : nil
      unless audio.is_a?(Hash)
        if parsed["code"].to_s.strip.empty?
          raise Error.new("invalid_tts_response", "DashScope response has no output.audio.")
        end

        raise Error.new("tts_api_error", "DashScope TTS failed: #{error_message(raw)}")
      end
      url = audio["url"].to_s.strip
      return { url: url } unless url.empty?

      data = audio["data"].to_s
      raise Error.new("invalid_tts_response", "DashScope response has no audio URL.") if data.empty?

      { data: Base64.decode64(data).b }
    rescue JSON::ParserError
      raise Error.new("invalid_tts_response", "DashScope returned a body that is not JSON.")
    end

    # 下载 24 小时有效的 WAV。协议规则见 audio_download_uri；边读边数字节，超限即停。
    def download(url)
      uri = self.class.audio_download_uri(url)
      body = String.new(encoding: Encoding::BINARY)
      http_for(uri).request(Net::HTTP::Get.new(uri)) do |response|
        unless (200..299).cover?(response.code.to_i)
          raise Error.new("tts_api_error", "DashScope audio download responded #{response.code}.")
        end

        response.read_body do |chunk|
          body << chunk
          if body.bytesize > @max_audio_bytes
            raise Error.new("audio_too_large", "The generated audio is larger than #{@max_audio_bytes / (1024 * 1024)} MiB.")
          end
        end
      end
      body
    rescue URI::InvalidURIError
      raise Error.new("invalid_tts_response", "DashScope audio URL is invalid.")
    rescue SocketError, SystemCallError, Net::OpenTimeout, Net::ReadTimeout, OpenSSL::SSL::SSLError, IOError => error
      raise Error.new("tts_api_unreachable", "DashScope audio download failed: #{error.class}")
    end

    def http_for(uri)
      http = Net::HTTP.new(uri.hostname, uri.port)
      http.use_ssl = uri.scheme == "https"
      http.verify_mode = OpenSSL::SSL::VERIFY_PEER if http.use_ssl?
      http.open_timeout = @open_timeout
      http.read_timeout = @read_timeout
      http
    end

    def join_parts(files, output)
      return files.first if files.length == 1

      WavTools.concat(files, output, gap_ms: 0)
      output
    rescue WavTools::Error => error
      raise Error.new("invalid_tts_response", "Unable to join DashScope audio parts: #{error.message}")
    end

    # 变速成功返回新文件路径；没有 ffmpeg 或 ffmpeg 失败返回 nil（原速输出）。
    def change_speed(input, output, filter)
      ffmpeg = @ffmpeg_path == :auto ? FFmpegTool.find("ffmpeg", env: @env) : @ffmpeg_path
      return nil if ffmpeg.to_s.empty?

      FFmpegTool.run!([ffmpeg.to_s, "-nostdin", "-y", "-loglevel", "error", "-i", input,
                       "-filter:a", filter, "-map_metadata", "-1", "-fflags", "+bitexact",
                       "-c:a", "pcm_s16le", output], timeout: ATEMPO_TIMEOUT_SECONDS)
      File.file?(output) && File.size(output).positive? ? output : nil
    rescue FFmpegTool::Failed
      nil
    end

    def duration_ms(path)
      WavTools.info(path).duration_ms
    rescue WavTools::Error
      nil
    end

    def error_message(body)
      parsed = JSON.parse(body.to_s)
      text = if parsed.is_a?(Hash) && parsed["message"]
               [parsed["code"], parsed["message"]].compact.map(&:to_s).reject(&:empty?).join(": ")
             elsif parsed.is_a?(Hash) && parsed["error"].is_a?(Hash)
               parsed["error"]["message"].to_s
             else
               body.to_s
             end
      redact(text)
    rescue JSON::ParserError
      redact(body.to_s)
    end

    def redact(text)
      value = text.to_s.dup.force_encoding(Encoding::UTF_8).scrub
      value = value.gsub(@api_key, "[redacted]") unless @api_key.empty?
      value[0, ERROR_EXCERPT]
    end
  end
end
