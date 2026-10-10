# frozen_string_literal: true

require "net/http"
require "uri"
require "json"

module WilldeepConfig
  module ProviderModels
    TIMEOUT_SECONDS = 10
    MAX_RESPONSE_BYTES = 2 * 1024 * 1024

    module_function

    def fetch(base, key)
      uri = models_uri(base)
      request = Net::HTTP::Get.new(uri)
      request["Accept"] = "application/json"
      request["Authorization"] = "Bearer #{key}" unless key.empty?
      body = +""
      # 不跟随重定向，避免把凭据转发到其它地址；不返回上游正文或异常中的凭据。
      Net::HTTP.start(uri.host, uri.port, use_ssl: uri.scheme == "https",
                      open_timeout: TIMEOUT_SECONDS, read_timeout: TIMEOUT_SECONDS) do |http|
        http.request(request) do |response|
          return Json.error("models_http", "模型列表请求失败（HTTP #{response.code}）") unless response.is_a?(Net::HTTPSuccess)
          response.read_body do |chunk|
            body << chunk
            return Json.error("models_response", "模型列表响应过大") if body.bytesize > MAX_RESPONSE_BYTES
          end
        end
      end
      parse(body)
    rescue ArgumentError, URI::InvalidURIError
      Json.error("models_config", "请填写有效的 HTTP/HTTPS 接口地址，地址不得包含凭据、查询参数或片段")
    rescue StandardError
      Json.error("models_network", "无法获取模型列表，请检查接口地址、密钥与网络后重试")
    end

    def models_uri(base)
      uri = URI.parse(base.to_s.strip)
      unless %w[http https].include?(uri.scheme) && uri.host && !uri.userinfo && !uri.query && !uri.fragment
        raise ArgumentError, "invalid API base"
      end
      path = uri.path.to_s.sub(%r{/+\z}, "")
      path += "/v1" unless path.end_with?("/v1")
      uri.path = path + "/models"
      uri
    end

    def model_protocols(data, ids)
      protocols = {}
      data.each do |item|
        next unless item.is_a?(Hash) && ids.include?(item["id"])
        endpoints = item["supported_endpoints"]
        next unless endpoints.is_a?(Array)
        known = endpoints.map do |endpoint|
          case endpoint
          when "chat-completions", "chat/completions", "/chat/completions", "/v1/chat/completions" then "chat-completions"
          when "responses", "/responses", "/v1/responses" then "responses"
          when "anthropic-messages", "messages", "/messages", "/v1/messages" then "anthropic-messages"
          end
        end.compact.uniq
        protocols[item["id"]] = known unless known.empty?
      end
      protocols
    end

    def parse(body)
      payload = JSON.parse(body)
      data = payload.is_a?(Hash) ? payload["data"] : nil
      return Json.error("models_response", "模型列表响应格式错误，预期 data 数组") unless data.is_a?(Array)
      ids = data.map do |item|
        id = item.is_a?(Hash) ? item["id"] : nil
        id if id.is_a?(String) && !id.strip.empty?
      end
      result = { "ok" => true, "models" => ids.compact.uniq.sort }
      protocols = model_protocols(data, result["models"])
      result["model_protocols"] = protocols unless protocols.empty?
      result
    rescue JSON::ParserError
      Json.error("models_response", "模型列表响应不是有效的 JSON")
    end
  end
end
