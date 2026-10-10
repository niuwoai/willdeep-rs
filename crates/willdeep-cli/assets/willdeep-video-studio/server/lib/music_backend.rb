# frozen_string_literal: true

require "fileutils"
require "json"
require "net/http"
require "openssl"
require "securerandom"
require "uri"
require_relative "wav_tools"
require_relative "ffmpeg_tool"

# 背景音乐后端（设计稿 episode-compose 4.2）。目前只有本地 ACE-Step 1.5 一种。
#
# ACE-Step 1.5 REST API（docs/en/API.md）是异步的：
# 1. POST /release_task         → data.task_id
# 2. POST /query_result         { "task_id_list": [id] } → data[].status 0 进行中 / 1 成功 / 2 失败，
#                                 data[].result 是 JSON 字符串，解析后每项的 file 是 "/v1/audio?path=…"
# 3. GET  /v1/audio?path=…      下载音频
# 所有回包都包在 { data, code, error, timestamp, extra } 里；HTTP 错误体是 { "detail": "…" }。
# 可选鉴权：Authorization: Bearer <ACESTEP_API_KEY>（下载接口同样校验）。
#
# 纯音乐：歌词传 "[Instrumental]"、vocal_language 传 "unknown"（API 没有单独的纯音乐开关）。
# 时长走 audio_duration（文档范围 10–600 秒）。batch_size 固定 1，只要一条。
#
# 环境变量：VIDEO_STUDIO_MUSIC_PROVIDER（acestep 才启用）/ VIDEO_STUDIO_MUSIC_API_BASE
# （默认 http://127.0.0.1:8001，只允许 HTTPS 或本机 HTTP）/ VIDEO_STUDIO_MUSIC_API_KEY（可选）/
# VIDEO_STUDIO_MUSIC_TIMEOUT（秒，默认 1200）/ VIDEO_STUDIO_MUSIC_POLL_SECONDS（默认 3，测试用）。
module MusicBackend
  PROVIDER_ACESTEP = "acestep"

  class Error < StandardError
    attr_reader :code

    def initialize(code, message)
      super(message)
      @code = code
    end
  end

  def self.select(env: ENV, media_root:)
    return nil unless env["VIDEO_STUDIO_MUSIC_PROVIDER"].to_s.strip.downcase == PROVIDER_ACESTEP

    AceStepBackend.new(
      base_url: env["VIDEO_STUDIO_MUSIC_API_BASE"],
      api_key: env["VIDEO_STUDIO_MUSIC_API_KEY"],
      media_root: media_root,
      timeout_seconds: positive_number(env["VIDEO_STUDIO_MUSIC_TIMEOUT"], AceStepBackend::DEFAULT_TIMEOUT_SECONDS),
      poll_interval: positive_number(env["VIDEO_STUDIO_MUSIC_POLL_SECONDS"], AceStepBackend::POLL_INTERVAL_SECONDS),
      env: env
    )
  end

  def self.positive_number(value, fallback)
    number = Float(value.to_s.strip)
    number.positive? ? number : fallback
  rescue ArgumentError
    fallback
  end

  class AceStepBackend
    DEFAULT_BASE = "http://127.0.0.1:8001"
    DEFAULT_TIMEOUT_SECONDS = 20 * 60
    POLL_INTERVAL_SECONDS = 3
    # 本机服务偶尔一次轮询没接上（GPU 满载、事件循环卡了一下）不算失败；连续这么多次才算。
    MAX_POLL_FAILURES = 3
    MIN_DURATION_SECONDS = 10
    MAX_DURATION_SECONDS = 600
    MAX_AUDIO_BYTES = 256 * 1024 * 1024
    DEFAULT_AUDIO_FORMAT = "wav"
    INSTRUMENTAL_LYRICS = "[Instrumental]"
    INSTRUMENTAL_LANGUAGE = "unknown"
    STATUS_RUNNING = 0
    STATUS_SUCCEEDED = 1
    STATUS_FAILED = 2
    LOCAL_HOSTS = %w[localhost 127.0.0.1 ::1].freeze
    EXTENSIONS = %w[wav mp3 flac opus aac ogg m4a].freeze
    ERROR_EXCERPT = 300

    def initialize(base_url: nil, media_root:, api_key: nil, timeout_seconds: DEFAULT_TIMEOUT_SECONDS,
                   poll_interval: POLL_INTERVAL_SECONDS, open_timeout: 10, read_timeout: 60,
                   audio_format: DEFAULT_AUDIO_FORMAT, max_audio_bytes: MAX_AUDIO_BYTES, env: ENV)
      @base_url = normalize_base_url(base_url.to_s.strip.empty? ? DEFAULT_BASE : base_url)
      @base_uri = URI.parse(@base_url)
      @api_key = api_key.to_s.strip
      @media_root = File.expand_path(media_root.to_s)
      @timeout_seconds = timeout_seconds.to_f
      @poll_interval = poll_interval.to_f
      @open_timeout = open_timeout
      @read_timeout = read_timeout
      @audio_format = audio_format.to_s
      @max_audio_bytes = max_audio_bytes
      @env = env
    end

    attr_reader :base_url

    def name
      PROVIDER_ACESTEP
    end

    # 给 capabilities 用：不含 Key。
    def describe
      { "provider" => PROVIDER_ACESTEP, "base" => @base_url }
    end

    # 页面打开「成片」时探一下本机服务在不在：只看能不能建连并拿到任意 HTTP 回应，
    # 不管路径是否存在。超时很短，服务没起时页面能立刻提示，而不是等生成失败。
    def reachable?(timeout: 1.5)
      uri = URI.parse("#{@base_url}/health")
      http = http_for(uri)
      http.open_timeout = timeout
      http.read_timeout = timeout
      request = Net::HTTP::Get.new(uri)
      authorize(request)
      http.request(request)
      true
    rescue StandardError
      false
    end

    # 阻塞到完成：提交 → 轮询 → 下载。返回 { "filePath", "fileName", "durationMs" }。
    # request_id 只用于调用方自己的去重：ACE-Step 没有幂等键，这里不往上游传。
    def generate(prompt:, duration_seconds:, request_id: nil)
      _ = request_id
      caption = prompt.to_s.strip
      raise Error.new("empty_prompt", "Music prompt is empty.") if caption.empty?

      deadline = monotonic + @timeout_seconds
      task_id = submit(caption, clamp_duration(duration_seconds))
      file = wait_for_file(task_id, deadline)
      binary = download(file)
      write(binary, file)
    end

    private

    def normalize_base_url(value)
      token = value.to_s.strip.sub(%r{/+\z}, "")
      uri = URI.parse(token)
      local_http = uri.scheme == "http" && LOCAL_HOSTS.include?(uri.hostname.to_s)
      unless uri.hostname && (uri.scheme == "https" || local_http)
        raise Error.new("music_backend_misconfigured", "Music API base URL must use HTTPS (HTTP only for localhost).")
      end

      token
    rescue URI::InvalidURIError
      raise Error.new("music_backend_misconfigured", "Music API base URL is invalid.")
    end

    def clamp_duration(value)
      seconds = value.to_f
      seconds = MIN_DURATION_SECONDS if seconds.nan? || seconds < MIN_DURATION_SECONDS
      [seconds, MAX_DURATION_SECONDS].min.round(1)
    end

    def submit(caption, duration)
      body = {
        "prompt" => caption,
        "lyrics" => INSTRUMENTAL_LYRICS,
        "vocal_language" => INSTRUMENTAL_LANGUAGE,
        "audio_duration" => duration,
        "batch_size" => 1,
        "audio_format" => @audio_format
      }
      data = post_json("/release_task", body)
      task_id = data.is_a?(Hash) ? data["task_id"].to_s.strip : ""
      raise Error.new("invalid_music_response", "ACE-Step returned no task_id.") if task_id.empty?

      task_id
    end

    def wait_for_file(task_id, deadline)
      failures = 0
      loop do
        raise Error.new("music_timeout", "ACE-Step did not finish within #{format('%g', @timeout_seconds)} seconds.") if monotonic > deadline

        begin
          data = post_json("/query_result", { "task_id_list" => [task_id] })
          failures = 0
        rescue Error => error
          raise unless error.code == "music_unreachable"

          failures += 1
          raise if failures >= MAX_POLL_FAILURES

          data = nil
        end
        entry = Array(data).find { |item| item.is_a?(Hash) && item["task_id"].to_s == task_id }
        status = entry ? entry["status"].to_i : STATUS_RUNNING
        results = entry ? parse_results(entry["result"]) : []
        if status == STATUS_FAILED
          reason = results.map { |item| item["error"].to_s }.find { |text| !text.empty? } || "generation failed"
          raise Error.new("music_api_error", "ACE-Step task failed: #{excerpt(reason)}")
        end
        if status == STATUS_SUCCEEDED
          file = results.map { |item| item["file"].to_s.strip }.find { |text| !text.empty? }
          raise Error.new("invalid_music_response", "ACE-Step finished without an audio file.") unless file

          return file
        end
        sleep(@poll_interval)
      end
    end

    # result 是 JSON 字符串（文档 5.3）；兼容直接给数组的实现。
    def parse_results(raw)
      parsed = raw.is_a?(String) ? JSON.parse(raw) : raw
      Array(parsed).select { |item| item.is_a?(Hash) }
    rescue JSON::ParserError
      []
    end

    def post_json(path, body)
      uri = URI.parse("#{@base_url}#{path}")
      request = Net::HTTP::Post.new(uri)
      request["Content-Type"] = "application/json"
      request["Accept"] = "application/json"
      authorize(request)
      request.body = JSON.generate(body)
      response = http_for(uri).request(request)
      unless (200..299).cover?(response.code.to_i)
        raise Error.new("music_api_error", "ACE-Step #{path} responded #{response.code}: #{error_message(response.body)}")
      end

      unwrap(path, response.body)
    rescue SocketError, SystemCallError, Net::OpenTimeout, Net::ReadTimeout, OpenSSL::SSL::SSLError, IOError => error
      raise Error.new("music_unreachable", "ACE-Step #{path} request failed: #{error.class}")
    end

    def unwrap(path, raw)
      parsed = JSON.parse(raw.to_s)
      raise Error.new("invalid_music_response", "ACE-Step #{path} returned an unexpected body.") unless parsed.is_a?(Hash)

      code = parsed["code"]
      if (code && code.to_i != 200) || !parsed["error"].to_s.strip.empty?
        raise Error.new("music_api_error", "ACE-Step #{path} failed: #{excerpt(parsed['error'] || code)}")
      end

      parsed["data"]
    rescue JSON::ParserError
      raise Error.new("invalid_music_response", "ACE-Step #{path} returned a body that is not JSON.")
    end

    # file 通常是相对地址 "/v1/audio?path=…"，拼到 base 后面（base 可能带反代前缀）。
    # 给了绝对地址时必须与 base 同源，Key 不发给别的主机。
    def resolve_file(file)
      if file.start_with?("/")
        URI.parse("#{@base_url}#{file}")
      else
        uri = URI.parse(file)
        same_origin = uri.scheme == @base_uri.scheme && uri.hostname == @base_uri.hostname && uri.port == @base_uri.port
        raise Error.new("invalid_music_response", "ACE-Step returned an audio URL on another host.") unless same_origin

        uri
      end
    rescue URI::InvalidURIError
      raise Error.new("invalid_music_response", "ACE-Step returned an invalid audio URL.")
    end

    def download(file)
      uri = resolve_file(file)
      request = Net::HTTP::Get.new(uri)
      authorize(request)
      body = String.new(encoding: Encoding::BINARY)
      http_for(uri).request(request) do |response|
        unless (200..299).cover?(response.code.to_i)
          raise Error.new("music_api_error", "ACE-Step audio download responded #{response.code}.")
        end

        response.read_body do |chunk|
          body << chunk
          if body.bytesize > @max_audio_bytes
            raise Error.new("invalid_music_response", "ACE-Step audio is larger than #{@max_audio_bytes / (1024 * 1024)} MiB.")
          end
        end
      end
      raise Error.new("invalid_music_response", "ACE-Step returned an empty audio file.") if body.bytesize.zero?

      body
    rescue SocketError, SystemCallError, Net::OpenTimeout, Net::ReadTimeout, OpenSSL::SSL::SSLError, IOError => error
      raise Error.new("music_unreachable", "ACE-Step audio download failed: #{error.class}")
    end

    def write(binary, file)
      extension = extension_for(binary, file)
      name = "music-#{SecureRandom.hex(12)}.#{extension}"
      FileUtils.mkdir_p(@media_root)
      path = File.join(@media_root, name)
      File.binwrite(path, binary)
      { "filePath" => path, "fileName" => name, "durationMs" => duration_ms(binary, path, extension) }
    rescue SystemCallError, IOError => error
      raise Error.new("music_write_failed", "Unable to write the generated music: #{error.class}")
    end

    # 先认文件头，认不出再看下载地址里 path 的扩展名，最后退回请求的格式。
    def extension_for(binary, file)
      return "wav" if binary[0, 4] == "RIFF".b && binary[8, 4] == "WAVE".b
      return "flac" if binary[0, 4] == "fLaC".b
      return "ogg" if binary[0, 4] == "OggS".b
      return "mp3" if binary[0, 3] == "ID3".b || (binary.getbyte(0) == 0xFF && (binary.getbyte(1).to_i & 0xE0) == 0xE0)

      from_path = File.extname(query_path(file)).delete(".").downcase
      return from_path if EXTENSIONS.include?(from_path)

      EXTENSIONS.include?(@audio_format) ? @audio_format : "mp3"
    end

    def query_path(file)
      pairs = URI.decode_www_form(URI.parse(file).query.to_s)
      pair = pairs.find { |key, _| key == "path" }
      pair ? pair[1].to_s : ""
    rescue URI::InvalidURIError, ArgumentError
      ""
    end

    def duration_ms(binary, path, extension)
      if extension == "wav"
        begin
          return WavTools.parse(binary).duration_ms
        rescue WavTools::Error
          nil
        end
      end
      probed = FFmpegTool.probe(path, env: @env)
      probed && probed["durationMs"]
    end

    def authorize(request)
      request["Authorization"] = "Bearer #{@api_key}" unless @api_key.empty?
    end

    def http_for(uri)
      http = Net::HTTP.new(uri.hostname, uri.port)
      http.use_ssl = uri.scheme == "https"
      http.verify_mode = OpenSSL::SSL::VERIFY_PEER if http.use_ssl?
      http.open_timeout = @open_timeout
      http.read_timeout = @read_timeout
      http
    end

    def error_message(body)
      parsed = JSON.parse(body.to_s)
      text = if parsed.is_a?(Hash) && parsed["detail"]
               parsed["detail"].is_a?(String) ? parsed["detail"] : JSON.generate(parsed["detail"])
             elsif parsed.is_a?(Hash) && parsed["error"]
               parsed["error"].to_s
             else
               body.to_s
             end
      excerpt(text)
    rescue JSON::ParserError
      excerpt(body.to_s)
    end

    def excerpt(text)
      value = text.to_s.dup.force_encoding(Encoding::UTF_8).scrub
      value = value.gsub(@api_key, "[redacted]") unless @api_key.to_s.empty?
      value[0, ERROR_EXCERPT]
    end

    def monotonic
      Process.clock_gettime(Process::CLOCK_MONOTONIC)
    end
  end
end
