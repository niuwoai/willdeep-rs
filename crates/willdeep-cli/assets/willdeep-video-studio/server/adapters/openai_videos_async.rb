# frozen_string_literal: true

require "base64"
require "fileutils"
require "json"
require "net/http"
require "openssl"
require "securerandom"
require "uri"

class VideoProviderError < StandardError
  attr_reader :http_status, :code, :retry_after, :ambiguous_submission

  def initialize(message, http_status: nil, code: nil, retry_after: nil, ambiguous_submission: false)
    super(message)
    @http_status = http_status
    @code = code
    @retry_after = retry_after
    @ambiguous_submission = ambiguous_submission
  end

  def as_json
    {
      "message" => message,
      "httpStatus" => http_status,
      "code" => code,
      "retryAfter" => retry_after,
      "ambiguousSubmission" => ambiguous_submission
    }.reject { |_key, value| value.nil? }
  end
end

class OpenAIVideosAsyncAdapter
  MAX_REFERENCE_BYTES = 32 * 1024 * 1024
  MAX_DOWNLOAD_BYTES = 1024 * 1024 * 1024

  # Provider 能力声明（设计稿 7.1）。按 Tsingfly Hub 的 MiniMax-H3 教程如实写
  # （https://hub.tsingfly.com/zh/tutor/videos，2026-09-17 核实）：
  # - t2va 不带素材；fl2va 带一张首帧（input_reference）；
  # - ref2va 只收两种互斥组合：1~3 段参考视频（input_references，音轨取自视频），
  #   或恰好一张图（input_reference）加恰好一段音频（audio_reference，JSON 文本
  #   {"audio_url":"data:audio/wav;base64,…"}，原始音频约 750 KB 以内）。
  #   「≤9 图 / ≤3 视频 / ≤3 音频混放」当前部署不支持。
  # - fps 固定 24；duration 合同区间 4~15 秒；flow_shift=12 与 audio_flow_shift=3 成对。
  # 接口不返回能力信息，所以写死在这里并注明核实日期。
  CAPABILITIES = {
    "provider" => "openai-videos-async",
    "tasks" => %w[t2va fl2va ref2va].freeze,
    "maxImageReferences" => 1,
    "maxVideoReferences" => 3,
    "maxAudioReferences" => 1,
    "supportsNativeAudio" => true,
    "supportsMultipleSubjects" => true,
    "supportsCallbacks" => false,
    "durationRange" => [4, 15].freeze,
    "ref2va" => {
      "combos" => %w[videos image_audio].freeze,
      "maxVideos" => 3,
      "maxAudioBytes" => 750_000,
      "audioFormats" => %w[wav mp3].freeze
    }.freeze,
    "documentedAt" => "2026-09-17 https://hub.tsingfly.com/zh/tutor/videos"
  }.freeze
  TASKS = %w[t2va fl2va ref2va].freeze
  MIN_DURATION = 4
  MAX_DURATION = 15
  MAX_AUDIO_REFERENCE_BYTES = 750_000
  MAX_VIDEO_REFERENCES = 3

  def self.capabilities
    CAPABILITIES
  end

  def capabilities
    CAPABILITIES
  end

  def initialize(base_url:, api_key:, output_directory:, open_timeout: 20, read_timeout: 90)
    @base_url = normalize_base_url(base_url)
    @api_key = api_key.to_s.strip
    @output_directory = File.expand_path(output_directory)
    @open_timeout = open_timeout
    @read_timeout = read_timeout
  end

  attr_reader :base_url, :output_directory

  def configured?
    !@api_key.empty?
  end

  def create(request)
    raise VideoProviderError, "Video API key is not configured." unless configured?

    fields, files = multipart_fields(request)
    response = post_multipart(endpoint("/v1/videos"), fields, files)
    begin
      task = parse_task(response)
      raise VideoProviderError, "Video provider accepted the request without returning a task id." unless task["remoteID"]
      task
    rescue VideoProviderError => error
      raise VideoProviderError.new(error.message, ambiguous_submission: true)
    end
  rescue VideoProviderError
    raise
  rescue StandardError => error
    raise VideoProviderError.new(
      "The create request ended without a confirmed response: #{error.class}",
      ambiguous_submission: true
    )
  end

  def retrieve(remote_id)
    id = validate_remote_id(remote_id)
    request = Net::HTTP::Get.new(endpoint("/v1/videos/#{id}"))
    parse_task(request_json(request))
  end

  def download(url, remote_id)
    temporary = nil
    uri = validate_download_url(url)
    FileUtils.mkdir_p(output_directory)
    filename = "#{safe_filename(remote_id)}.mp4"
    destination = unique_path(File.join(output_directory, filename))
    temporary = "#{destination}.part"
    stream_download(uri, temporary, 0)
    File.rename(temporary, destination)
    temporary = nil
    destination
  rescue VideoProviderError
    raise
  rescue StandardError => error
    raise VideoProviderError, "Output download failed: #{error.class}"
  ensure
    # 早先写的是 `defined?(temporary) && ...`：validate_download_url 抛错时
    # temporary 还是 nil，File.exist?(nil) 的 TypeError 会把真正的
    # VideoProviderError 顶掉，调用方的 rescue 就接不住了。
    File.delete(temporary) if temporary && File.exist?(temporary)
  end

  private

  def normalize_base_url(value)
    token = value.to_s.strip.sub(%r{/+\z}, "")
    raise VideoProviderError, "Video API base URL is empty." if token.empty?
    uri = URI.parse(token)
    raise VideoProviderError, "Video API base URL must not contain credentials, a query, or a fragment." if uri.userinfo || uri.query || uri.fragment
    local_http = uri.scheme == "http" && %w[localhost 127.0.0.1 ::1].include?(uri.host)
    unless uri.host && (uri.scheme == "https" || local_http)
      raise VideoProviderError, "Video API base URL must use HTTPS (HTTP is allowed only for localhost)."
    end
    token.sub(%r{/v1\z}i, "")
  rescue URI::InvalidURIError
    raise VideoProviderError, "Video API base URL is invalid."
  end

  def endpoint(path)
    URI.parse("#{base_url}#{path}")
  end

  def multipart_fields(request)
    prompt = request["prompt"].to_s.strip
    model = request["model"].to_s.strip
    raise VideoProviderError, "prompt is required" if prompt.empty?
    raise VideoProviderError, "model is required" if model.empty?

    reference = request["referenceImagePath"].to_s.strip
    task = request["task"].to_s.strip
    task = (reference.empty? ? "t2va" : "fl2va") if task.empty?
    raise VideoProviderError, "unknown task #{task}" unless TASKS.include?(task)
    fields = {
      "model" => model,
      "prompt" => prompt,
      "width" => request["width"].to_i.to_s,
      "height" => request["height"].to_i.to_s,
      "fps" => "24",
      "num_inference_steps" => request["inferenceSteps"].to_i.to_s,
      # 与 scheduler 的 shift=12 对齐，必须和 extra_params.audio_flow_shift=3 成对，
      # 只改单边会破坏音画同步（Hub 教程第 2 节）。
      "flow_shift" => "12",
      "extra_params" => JSON.generate(
        "task" => task,
        "duration" => request["duration"].to_i,
        "audio_flow_shift" => 3
      )
    }
    optional_field(fields, "negative_prompt", request["negativePrompt"])
    optional_field(fields, "seed", request["seed"])
    # files 是 [字段名, 路径] 的列表：ref2va 的 input_references 同名字段要重复出现。
    files = []
    case task
    when "t2va"
      raise VideoProviderError, "t2va does not accept a reference image." unless reference.empty?
    when "fl2va"
      raise VideoProviderError, "fl2va needs a start frame (referenceImagePath)." if reference.empty?
      files << ["input_reference", validate_reference(reference)]
    when "ref2va"
      videos = Array(request["referenceVideoPaths"]).map(&:to_s).reject(&:empty?)
      audio = request["referenceAudioPath"].to_s.strip
      if request["combo"].to_s == "videos" || (!videos.empty? && audio.empty?)
        raise VideoProviderError, "ref2va with reference videos takes 1 to #{MAX_VIDEO_REFERENCES} videos." if videos.empty? || videos.length > MAX_VIDEO_REFERENCES
        raise VideoProviderError, "ref2va with reference videos must not carry an image or audio." unless reference.empty? && audio.empty?
        videos.each { |path| files << ["input_references", validate_video_reference(path)] }
      else
        raise VideoProviderError, "ref2va image+audio needs a start frame (referenceImagePath) and a dialogue audio file (referenceAudioPath)." if reference.empty? || audio.empty?
        raise VideoProviderError, "ref2va image+audio must not carry reference videos." unless videos.empty?
        files << ["input_reference", validate_reference(reference)]
        # 后端（vLLM-Omni）把 audio_reference 当 JSON 解析，要的是 {"audio_url": …}。
        # 裸 data URL 会被拒：「backend HTTP 400: Invalid JSON in form field.」（2026-09-27 实测
        # 229 条全拒；Hub 文档 09-11 起把示例写成了裸 data URL，09-10 的成功实测用的是 JSON）。
        fields["audio_reference"] = JSON.generate("audio_url" => audio_data_url(audio))
      end
    end
    [fields, files]
  end

  def validate_video_reference(path)
    expanded = File.expand_path(path)
    raise VideoProviderError, "Reference video does not exist." unless File.file?(expanded)
    raise VideoProviderError, "Reference video is too large." if File.size(expanded) > MAX_REFERENCE_BYTES
    raise VideoProviderError, "Reference video must be MP4, MOV or WebM." unless %w[.mp4 .mov .webm].include?(File.extname(expanded).downcase)
    expanded
  end

  # audio_reference 是文本字段，受 1 MiB 上限约束；base64 放大三分之一，原始音频要在
  # 750 KB 以内。超了在提交前就拒绝，别让网关回 400 以后才发现。
  def audio_data_url(path)
    expanded = File.expand_path(path)
    raise VideoProviderError, "Reference audio does not exist." unless File.file?(expanded)
    raise VideoProviderError, "Reference audio is larger than #{MAX_AUDIO_REFERENCE_BYTES / 1024} KB; use a lower sample rate or split the shot." if File.size(expanded) > MAX_AUDIO_REFERENCE_BYTES
    mime = case File.extname(expanded).downcase
           when ".wav" then "audio/wav"
           when ".mp3" then "audio/mpeg"
           else raise VideoProviderError, "Reference audio must be WAV or MP3."
           end
    "data:#{mime};base64,#{Base64.strict_encode64(File.binread(expanded))}"
  end

  def optional_field(fields, key, value)
    token = value.to_s.strip
    fields[key] = token unless token.empty?
  end

  def validate_reference(path)
    expanded = File.expand_path(path)
    raise VideoProviderError, "Reference image does not exist." unless File.file?(expanded)
    raise VideoProviderError, "Reference image is too large." if File.size(expanded) > MAX_REFERENCE_BYTES
    extension = File.extname(expanded).downcase
    raise VideoProviderError, "Reference image must be PNG, JPEG, or WebP." unless %w[.png .jpg .jpeg .webp].include?(extension)
    expanded
  end

  def post_multipart(uri, fields, files)
    boundary = "WillDeepVideoStudio#{SecureRandom.hex(16)}"
    request = Net::HTTP::Post.new(uri)
    request["Authorization"] = "Bearer #{@api_key}"
    request["Accept"] = "application/json"
    request["Content-Type"] = "multipart/form-data; boundary=#{boundary}"
    request.body = multipart_body(boundary, fields, files)
    request_json(request, accepted_statuses: [200, 202], ambiguous_submission: true)
  end

  def multipart_body(boundary, fields, files)
    body = String.new(encoding: Encoding::BINARY)
    fields.each do |name, value|
      body << "--#{boundary}\r\n"
      body << "Content-Disposition: form-data; name=\"#{name}\"\r\n\r\n"
      body << value.to_s.encode(Encoding::UTF_8).b << "\r\n"
    end
    files.each do |name, path|
      body << "--#{boundary}\r\n"
      body << "Content-Disposition: form-data; name=\"#{name}\"; filename=\"#{File.basename(path)}\"\r\n"
      body << "Content-Type: #{mime_type(path)}\r\n\r\n"
      body << File.binread(path) << "\r\n"
    end
    body << "--#{boundary}--\r\n"
    body
  end

  def mime_type(path)
    case File.extname(path).downcase
    when ".png" then "image/png"
    when ".webp" then "image/webp"
    when ".mp4" then "video/mp4"
    when ".mov" then "video/quicktime"
    when ".webm" then "video/webm"
    else "image/jpeg"
    end
  end

  def request_json(request, accepted_statuses: nil, ambiguous_submission: false)
    request["Authorization"] ||= "Bearer #{@api_key}"
    request["Accept"] ||= "application/json"
    response = perform(request)
    allowed = accepted_statuses || (200..299)
    handle_http_error(response) unless allowed.include?(response.code.to_i)
    JSON.parse(response.body)
  rescue JSON::ParserError
    raise VideoProviderError.new("Video provider returned invalid JSON.", ambiguous_submission: ambiguous_submission)
  rescue Timeout::Error, EOFError, IOError, SocketError, SystemCallError,
         OpenSSL::OpenSSLError => error
    # OpenSSL::SSL::SSLError 不是 SystemCallError 的后代。漏掉它，握手失败就会
    # 越过 retrieve 的调用方一路冲到顶层，退避时间也不会写回任务。
    raise VideoProviderError.new(
      "Video provider request failed: #{error.class}",
      ambiguous_submission: ambiguous_submission
    )
  end

  def perform(request)
    uri = request.uri
    Net::HTTP.start(
      uri.host,
      uri.port,
      use_ssl: uri.scheme == "https",
      open_timeout: @open_timeout,
      read_timeout: @read_timeout
    ) { |http| http.request(request) }
  end

  def handle_http_error(response)
    parsed = begin
      JSON.parse(response.body)
    rescue JSON::ParserError, TypeError
      {}
    end
    raw_error = parsed.is_a?(Hash) ? parsed["error"] : nil
    message = raw_error.is_a?(Hash) ? (raw_error["message"] || raw_error["code"]) : raw_error
    message = "Video provider HTTP #{response.code}" if message.to_s.strip.empty?
    code = raw_error.is_a?(Hash) ? raw_error["code"] : nil
    retry_after = response["Retry-After"].to_s.strip
    raise VideoProviderError.new(
      redact(message.to_s)[0, 1000],
      http_status: response.code.to_i,
      code: code,
      retry_after: retry_after.empty? ? nil : retry_after
    )
  end

  def parse_task(raw)
    raise VideoProviderError, "Video provider returned a non-object task." unless raw.is_a?(Hash)
    {
      "remoteID" => raw["id"].to_s.empty? ? nil : raw["id"].to_s,
      "state" => normalize_status(raw["status"]),
      "progress" => normalize_progress(raw["progress"]),
      "outputURL" => raw["url"].to_s.empty? ? nil : raw["url"].to_s,
      "error" => raw["error"].nil? ? nil : JSON.parse(redact(JSON.generate(raw["error"]))),
      "providerResponse" => safe_provider_summary(raw)
    }
  end

  def normalize_status(value)
    case value.to_s.downcase
    when "queued", "created", "pending", "submitted" then "queued"
    when "in_progress", "running", "processing", "generating" then "in_progress"
    when "completed", "complete", "succeeded", "success", "done" then "completed"
    when "failed", "failure", "error" then "failed"
    else "queued"
    end
  end

  def redact(value)
    @api_key.empty? ? value : value.gsub(@api_key, "[redacted]")
  end

  def normalize_progress(value)
    [[value.to_i, 0].max, 100].min
  end

  def safe_provider_summary(raw)
    keys = %w[id object status model progress created_at started_at completed_at expires_at seconds size_bytes inference_time_s]
    keys.each_with_object({}) { |key, result| result[key] = raw[key] if raw.key?(key) }
  end

  def validate_remote_id(value)
    token = value.to_s.strip
    raise VideoProviderError, "Remote task id is missing." unless token.match?(/\A[A-Za-z0-9_.:-]{1,200}\z/)
    token
  end

  def validate_download_url(value)
    uri = URI.parse(value.to_s)
    local_http = uri.scheme == "http" && %w[localhost 127.0.0.1 ::1].include?(uri.host)
    unless uri.host && (uri.scheme == "https" || local_http)
      raise VideoProviderError, "Output URL must use HTTPS."
    end
    uri
  rescue URI::InvalidURIError
    raise VideoProviderError, "Output URL is invalid."
  end

  def stream_download(uri, destination, redirects)
    raise VideoProviderError, "Too many output download redirects." if redirects > 4
    redirect = nil
    request = Net::HTTP::Get.new(uri)
    Net::HTTP.start(
      uri.host,
      uri.port,
      use_ssl: uri.scheme == "https",
      open_timeout: @open_timeout,
      read_timeout: @read_timeout
    ) do |http|
      http.request(request) do |response|
        if response.is_a?(Net::HTTPRedirection)
          location = response["Location"]
          raise VideoProviderError, "Output download redirect has no location." if location.to_s.empty?
          redirect = URI.join(uri.to_s, location)
          next
        end
        handle_http_error(response) unless response.is_a?(Net::HTTPSuccess)
        expected = response["Content-Length"].to_i
        raise VideoProviderError, "Output video is larger than 1 GiB." if expected > MAX_DOWNLOAD_BYTES
        written = 0
        File.open(destination, "wb") do |file|
          response.read_body do |chunk|
            written += chunk.bytesize
            raise VideoProviderError, "Output video exceeded the 1 GiB limit." if written > MAX_DOWNLOAD_BYTES
            file.write(chunk)
          end
        end
      end
    end
    stream_download(validate_download_url(redirect.to_s), destination, redirects + 1) if redirect
  end

  def safe_filename(value)
    token = value.to_s.gsub(/[^A-Za-z0-9_.-]+/, "-").sub(/\A-+/, "")[0, 120]
    token.empty? ? "video-#{Time.now.to_i}" : token
  end

  def unique_path(candidate)
    return candidate unless File.exist?(candidate)
    stem = File.basename(candidate, File.extname(candidate))
    extension = File.extname(candidate)
    directory = File.dirname(candidate)
    index = 2
    loop do
      path = File.join(directory, "#{stem}-#{index}#{extension}")
      return path unless File.exist?(path)
      index += 1
    end
  end
end
