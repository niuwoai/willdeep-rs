# frozen_string_literal: true

require "base64"
require "fileutils"
require "json"
require "net/http"
require "openssl"
require "securerandom"
require "uri"
require_relative "host_bridge"

# 出图后端（设计稿 7.2）。
#
# 「工具全在插件 MCP 里」要求 image.generate 在任何 harness 里都能工作，而出图
# 需要凭据。两条路：
# - HostImageBackend：WillDeep 宿主宣告了 `willdeep/images/generate`，借宿主的
#   some.im 凭据经反向请求出图，凭据不出宿主。
# - DirectImageBackend：环境变量给了地址与 Key，MCP 服务自己按 OpenAI 兼容的
#   images 接口请求（无参照 /v1/images/generations，带参照 /v1/images/edits），
#   结果写进媒体目录。Claude Code、Codex 等没有反向请求的客户端走这条。
#
# 两者的 generate 返回同一个形状：{ "filePath", "mediaURL"?, "model" }。
module ImageBackend
  MAX_REFERENCES = 9
  MAX_IMAGE_BYTES = 32 * 1024 * 1024

  # retryable：过一会儿重试有没有指望。true 是网关超时、5xx、断网这类；false 是
  # 额度用完、请求本身不对、配置错，原样重试只会再失败一次（还可能再扣一次钱）。
  # upstream_quota：只对 quota_exhausted 有意义——true 是平台上游的额度耗尽
  # （some.im 网关 upstream_quota_exhausted，要平台方补），false 是本账号自己的
  # 额度 / 余额不足（要账号充值）。
  class Error < StandardError
    attr_reader :code, :retryable, :upstream_quota, :http_status

    def initialize(code, message, retryable: nil, upstream_quota: nil, http_status: nil)
      super(message)
      @code = code
      @retryable = retryable.nil? ? ImageBackend.default_retryable(code) : retryable
      @upstream_quota = upstream_quota
      @http_status = http_status
    end
  end

  QUOTA_EXHAUSTED = "quota_exhausted"
  # 上游额度耗尽的错误码。upstream_quota_exhausted 是 some.im 网关（0.246.0-rc3 起）
  # 在平台自己的上游凭据全部没额度时回的 503；其余是 OpenAI 兼容接口对本账号的说法。
  UPSTREAM_QUOTA_CODES = %w[upstream_quota_exhausted].freeze
  ACCOUNT_QUOTA_CODES = %w[insufficient_quota quota_exceeded billing_not_active].freeze
  # 只有原话没有错误码时按措辞认。中转站常把 code 留空、只在 message 里写中文。
  QUOTA_PHRASES = [
    /配额不足/, /额度不足/, /余额不足/, /额度已用完/, /配额已用完/,
    /insufficient[\s_-]*(?:quota|balance|credit)/i, /quota[\s_-]*(?:exceeded|exhausted)/i
  ].freeze
  # 过一会儿重试可能成功的 HTTP 状态：请求超时、限流、各种 5xx（含 Cloudflare 504）。
  RETRYABLE_STATUSES = [408, 425, 429].freeze
  # 不看状态码时各错误码默认能不能重试。
  NON_RETRYABLE_CODES = %w[host_image_unsupported image_backend_misconfigured invalid_reference image_too_large image_write_failed].freeze

  def self.default_retryable(code)
    return false if code == QUOTA_EXHAUSTED

    !NON_RETRYABLE_CODES.include?(code)
  end

  # 把一次上游失败归类。status 是 HTTP 状态（宿主那条从原话里的「HTTP 403:」抠出来，
  # 抠不到为 nil），text 是响应体或宿主转来的原话。返回 Error 的关键字参数：
  # { code:, retryable:, upstream_quota:, http_status: }；code 为 nil 表示不是额度问题，
  # 调用方沿用自己的错误码（host_error / image_api_error）。
  def self.classify_failure(status, text)
    upstream_code = error_code_in(text)
    quota = if UPSTREAM_QUOTA_CODES.include?(upstream_code) then :upstream
            elsif ACCOUNT_QUOTA_CODES.include?(upstream_code) then :account
            elsif QUOTA_PHRASES.any? { |pattern| pattern.match?(text.to_s) } then :account
            end
    if quota
      return { code: QUOTA_EXHAUSTED, retryable: false, upstream_quota: quota == :upstream, http_status: status }
    end

    retryable = status.nil? || status >= 500 || RETRYABLE_STATUSES.include?(status)
    { code: nil, retryable: retryable, upstream_quota: nil, http_status: status }
  end

  # 宿主转来的原话形如「HTTP 403: {"error":{…}}」。
  def self.http_status_in(text)
    match = text.to_s.match(/\AHTTP\s+(\d{3})\b/)
    match && match[1].to_i
  end

  # 原话或响应体里 JSON 的 error.code（或顶层 code）。宿主可能把原话截短，
  # JSON 解析不了时退回按字面找第一个 "code":"…"。都没有返回 nil。
  def self.error_code_in(text)
    source = text.to_s
    start = source.index("{")
    return nil unless start

    begin
      parsed = JSON.parse(source[start..-1])
      if parsed.is_a?(Hash)
        error = parsed["error"]
        code = error.is_a?(Hash) ? error["code"] : parsed["code"]
        return code.is_a?(String) ? code.strip.downcase : nil
      end
    rescue JSON::ParserError
      nil
    end
    match = source.match(/"code"\s*:\s*"([^"]+)"/)
    match && match[1].strip.downcase
  end

  # nano-banana 系（Gemini 出图）认的比例。与宿主 WillDeep 的
  # SomeIMImageGenerationRequest.supportedAspectRatios 同一张表、同一个顺序：
  # size 约分后不在表里时贴到最近的一档，而不是送一个上游不认、会被悄悄当成 1:1 的值。
  SUPPORTED_ASPECT_RATIOS = [
    ["1:1", 1, 1],
    ["2:3", 2, 3], ["3:2", 3, 2],
    ["3:4", 3, 4], ["4:3", 4, 3],
    ["4:5", 4, 5], ["5:4", 5, 4],
    ["9:16", 9, 16], ["16:9", 16, 9],
    ["21:9", 21, 9]
  ].freeze

  # 直连 OpenAI 兼容 images 接口的请求字段。nano-banana 系不认 `size`，只按
  # `aspect_ratio` 出图——不带它，竖屏首帧永远是 1024x1024 的方图。gpt-image /
  # dall-e 家族不带 `aspect_ratio`：它们认 `size`，有的中继对多余字段直接回
  # unknown_parameter。宿主桥接那条只传 size，比例由宿主换算。
  def self.request_fields(model:, prompt:, size:)
    fields = { "model" => model, "prompt" => prompt, "size" => size, "n" => 1, "response_format" => "b64_json" }
    ratio = openai_image_family?(model) ? nil : aspect_ratio_for(size)
    fields["aspect_ratio"] = ratio if ratio
    fields
  end

  # "1024x1536" → "2:3"。解析不了的（"auto"、空串、非正数）返回 nil，调用方就不带
  # 这个字段。按对数比值找最近的一档：2:1 与 1:2 离 1:1 一样远，线性比值做不到。
  def self.aspect_ratio_for(size)
    parts = size.to_s.downcase.tr("×", "x").split("x", -1).map(&:strip)
    return nil unless parts.length == 2

    width = Integer(parts[0], 10, exception: false)
    height = Integer(parts[1], 10, exception: false)
    return nil unless width&.positive? && height&.positive?

    target = Math.log(width.to_f / height)
    SUPPORTED_ASPECT_RATIOS.min_by { |(_, w, h)| (Math.log(w.to_f / h) - target).abs }&.first
  end

  # OpenAI Images API 家族（gpt-image-*、dall-e-*），判据与宿主 isOpenAIImageFamily 一致。
  def self.openai_image_family?(model)
    normalized = model.to_s.strip.downcase.tr(" ", "-")
    return false if normalized.empty?
    return true if normalized.include?("gpt-image") || normalized.include?("dall-e")

    normalized.match?(/gpt-?\d+[-.]?image/)
  end

  # 宿主宣告了就用宿主，否则有直连配置用直连，都没有返回 nil。
  def self.select(host:, env: ENV, media_root:)
    return HostImageBackend.new(host) if host && host.supports?(HostBridge::IMAGE_GENERATE)

    base = env["VIDEO_STUDIO_IMAGE_API_BASE"].to_s.strip
    key = env["VIDEO_STUDIO_IMAGE_API_KEY"].to_s.strip
    return nil if base.empty? || key.empty?

    DirectImageBackend.new(base_url: base, api_key: key, media_root: media_root,
                           open_timeout: (env["VIDEO_STUDIO_IMAGE_OPEN_TIMEOUT"] || 20).to_i,
                           read_timeout: (env["VIDEO_STUDIO_IMAGE_READ_TIMEOUT"] || 180).to_i)
  end

  class HostImageBackend
    def initialize(host)
      @host = host
    end

    def name
      "host"
    end

    def generate(prompt:, model:, size:, reference_paths:)
      @host.request(HostBridge::IMAGE_GENERATE, {
        "prompt" => prompt, "provider" => "some-im", "model" => model, "size" => size,
        "referenceImagePaths" => Array(reference_paths)
      })
    rescue HostBridge::RequestFailed => error
      # 宿主把上游的 HTTP 状态和响应体拼成一句原话交回来，只能从原话里认。
      status = ImageBackend.http_status_in(error.message)
      kind = ImageBackend.classify_failure(status, error.message)
      raise Error.new(kind[:code] || "host_error", error.message, retryable: kind[:retryable],
                                                                  upstream_quota: kind[:upstream_quota], http_status: status)
    rescue HostBridge::Unsupported
      raise Error.new("host_image_unsupported", "Host stopped supporting image generation.")
    end
  end

  class DirectImageBackend
    def initialize(base_url:, api_key:, media_root:, open_timeout: 20, read_timeout: 180)
      @base_url = normalize_base_url(base_url)
      @api_key = api_key.to_s.strip
      @media_root = File.expand_path(media_root.to_s)
      @open_timeout = open_timeout
      @read_timeout = read_timeout
    end

    attr_reader :base_url

    def name
      "direct"
    end

    def generate(prompt:, model:, size:, reference_paths:)
      references = Array(reference_paths).first(MAX_REFERENCES).map { |path| validate_reference(path) }
      fields = ImageBackend.request_fields(model: model, prompt: prompt, size: size)
      payload = references.empty? ? post_json("/v1/images/generations", fields) : post_multipart("/v1/images/edits", fields, references)
      entry = Array(payload["data"]).first
      raise Error.new("invalid_image_response", "The image API returned no image.") unless entry.is_a?(Hash)

      binary = if entry["b64_json"].to_s.empty?
                 raise Error.new("invalid_image_response", "The image API returned neither b64_json nor url.") if entry["url"].to_s.empty?
                 download(entry["url"].to_s)
               else
                 Base64.decode64(entry["b64_json"].to_s)
               end
      raise Error.new("invalid_image_response", "The image API returned an empty image.") if binary.bytesize.zero?
      raise Error.new("image_too_large", "The generated image is larger than 32 MiB.") if binary.bytesize > MAX_IMAGE_BYTES

      FileUtils.mkdir_p(@media_root)
      name = "img-#{SecureRandom.hex(12)}#{extension_of(binary)}"
      path = File.join(@media_root, name)
      File.binwrite(path, binary)
      { "filePath" => path, "model" => model, "providerID" => "direct" }
    rescue Error
      raise
    rescue SystemCallError, IOError => error
      raise Error.new("image_write_failed", "Unable to write the generated image: #{error.class}")
    end

    private

    def normalize_base_url(value)
      token = value.to_s.strip.sub(%r{/+\z}, "")
      raise Error.new("image_backend_misconfigured", "Image API base URL is empty.") if token.empty?
      uri = URI.parse(token)
      local_http = uri.scheme == "http" && %w[localhost 127.0.0.1 ::1].include?(uri.host)
      raise Error.new("image_backend_misconfigured", "Image API base URL must use HTTPS (HTTP only for localhost).") unless uri.host && (uri.scheme == "https" || local_http)
      token.sub(%r{/v1\z}i, "")
    rescue URI::InvalidURIError
      raise Error.new("image_backend_misconfigured", "Image API base URL is invalid.")
    end

    def validate_reference(path)
      expanded = File.expand_path(path.to_s)
      raise Error.new("invalid_reference", "Reference image does not exist: #{File.basename(expanded)}") unless File.file?(expanded)
      raise Error.new("invalid_reference", "Reference image must live in the plugin media directory.") unless File.dirname(expanded) == @media_root
      raise Error.new("invalid_reference", "Reference image is too large.") if File.size(expanded) > MAX_IMAGE_BYTES
      expanded
    end

    def post_json(path, body)
      request = Net::HTTP::Post.new(URI.parse("#{@base_url}#{path}"))
      request["Content-Type"] = "application/json"
      request.body = JSON.generate(body)
      perform(request)
    end

    def post_multipart(path, fields, files)
      boundary = "WillDeepImage#{SecureRandom.hex(16)}"
      request = Net::HTTP::Post.new(URI.parse("#{@base_url}#{path}"))
      request["Content-Type"] = "multipart/form-data; boundary=#{boundary}"
      body = String.new(encoding: Encoding::BINARY)
      fields.each do |name, value|
        body << "--#{boundary}\r\nContent-Disposition: form-data; name=\"#{name}\"\r\n\r\n" << value.to_s.encode(Encoding::UTF_8).b << "\r\n"
      end
      # OpenAI 的 edits 接口多张参照用 image[]；只有一张的网关也认这个名字。
      files.each do |file|
        body << "--#{boundary}\r\nContent-Disposition: form-data; name=\"image[]\"; filename=\"#{File.basename(file)}\"\r\n"
        body << "Content-Type: #{mime_type(file)}\r\n\r\n" << File.binread(file) << "\r\n"
      end
      body << "--#{boundary}--\r\n"
      request.body = body
      perform(request)
    end

    def perform(request)
      request["Authorization"] = "Bearer #{@api_key}"
      request["Accept"] = "application/json"
      uri = request.uri
      http = Net::HTTP.new(uri.host, uri.port)
      http.use_ssl = uri.scheme == "https"
      http.verify_mode = OpenSSL::SSL::VERIFY_PEER if http.use_ssl?
      http.open_timeout = @open_timeout
      http.read_timeout = @read_timeout
      response = http.request(request)
      status = response.code.to_i
      unless (200..299).cover?(status)
        kind = ImageBackend.classify_failure(status, response.body.to_s)
        raise Error.new(kind[:code] || "image_api_error", "Image API responded #{response.code}: #{error_message(response.body)}",
                        retryable: kind[:retryable], upstream_quota: kind[:upstream_quota], http_status: status)
      end
      parsed = JSON.parse(response.body.to_s)
      raise Error.new("invalid_image_response", "The image API returned invalid JSON.") unless parsed.is_a?(Hash)
      parsed
    rescue JSON::ParserError
      raise Error.new("invalid_image_response", "The image API returned invalid JSON.")
    rescue SocketError, SystemCallError, Net::OpenTimeout, Net::ReadTimeout, OpenSSL::SSL::SSLError, IOError => error
      raise Error.new("image_api_unreachable", "Image API request failed: #{error.class}")
    end

    def download(url)
      uri = URI.parse(url)
      raise Error.new("invalid_image_response", "Image URL must be HTTPS.") unless uri.scheme == "https" || (uri.scheme == "http" && %w[localhost 127.0.0.1].include?(uri.host))
      http = Net::HTTP.new(uri.host, uri.port)
      http.use_ssl = uri.scheme == "https"
      http.open_timeout = @open_timeout
      http.read_timeout = @read_timeout
      response = http.get(uri.request_uri)
      raise Error.new("image_api_error", "Image download responded #{response.code}.") unless (200..299).cover?(response.code.to_i)
      response.body.to_s.b
    rescue URI::InvalidURIError
      raise Error.new("invalid_image_response", "Image URL is invalid.")
    rescue SocketError, SystemCallError, Net::OpenTimeout, Net::ReadTimeout, OpenSSL::SSL::SSLError, IOError => error
      raise Error.new("image_api_unreachable", "Image download failed: #{error.class}")
    end

    def error_message(body)
      parsed = JSON.parse(body.to_s)
      parsed.is_a?(Hash) && parsed["error"].is_a?(Hash) ? parsed["error"]["message"].to_s[0, 300] : body.to_s[0, 300]
    rescue JSON::ParserError
      body.to_s[0, 300]
    end

    def extension_of(binary)
      return ".png" if binary.start_with?("\x89PNG".b)
      return ".jpg" if binary.start_with?("\xFF\xD8".b)
      return ".webp" if binary[0, 4] == "RIFF".b && binary[8, 4] == "WEBP".b
      ".png"
    end

    def mime_type(path)
      case File.extname(path).downcase
      when ".png" then "image/png"
      when ".webp" then "image/webp"
      else "image/jpeg"
      end
    end
  end
end
