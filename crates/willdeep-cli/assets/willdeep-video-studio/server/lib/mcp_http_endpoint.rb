# frozen_string_literal: true

require_relative "media_host"

# 本机 Streamable HTTP 入口的连接文件写在哪里：按拉起本进程的宿主分开。
#
# 2026-09-27 实际发生：WillDeep macOS 与 willdeep-rs 的 Web 宿主都不传
# VIDEO_STUDIO_DATA_DIR，两边拉起的插件进程共用 macOS 数据目录，各自把随机端口
# 和 token 写进同一份 mcp-http.json，后写的赢。macOS 宿主的网关照这份文件转发，
# 请求落进了 willdeep-rs 拉起的进程：插件设置里的视频 API Key 是 macOS 宿主经
# mcp.json 注入的，那个进程里没有；反向请求（出图、审核）也发给了另一个宿主。
#
# 两个网关读哪里（docs/decisions/0001-plugin-mcp-gateway.md）：
# - macOS：只读 ~/Library/Application Support/WillDeep/plugin-data/<插件 ID>/mcp-http.json；
# - willdeep-rs：先读 <WILLDEEP_HOME 或 ~/.willdeep>/plugin-data/<插件 ID>/mcp-http.json，
#   没有才退到 macOS 那份。
# 于是各写各宿主先读的那一处，两个网关不用改就不再串线：
# - macOS 宿主 → <数据目录>/mcp-http.json，与原来一致；
# - willdeep-rs → <WILLDEEP_HOME>/plugin-data/<插件 ID>/mcp-http.json（WILLDEEP_HOME 由
#   willdeep-rs 注入，数据目录仍与 macOS 共用，只有连接文件分开）；
# - 其他 stdio 客户端 → <数据目录>/mcp-http.<客户端名>.json，不占任何网关会读的位置。
#
# 宿主身份只有 stdio 那头的 initialize 才说得清，所以连接文件在那时才写；HTTP
# 客户端的 initialize 不算，它们不是拉起本进程的宿主。不看 VIDEO_STUDIO_HOST_MODE：
# 那是媒体根的强制开关，macOS 宿主下强制 Web 媒体，也不该把 macOS 网关要读的文件
# 挪走。
module MCPHTTPEndpoint
  FILE_NAME = "mcp-http.json"
  PLUGIN_ID = MediaHost::PLUGIN_ID
  # 宿主 initialize 的 clientInfo.name：willdeep-rs 发 `willdeep`（willdeep-core
  # src/mcp.rs），macOS 发 `WillDeep Desktop (some.im)`（AgentNetworkSupport.swift）；
  # 早期 macOS 宿主不带 clientInfo，同样按 macOS 算。
  RS_CLIENT_NAME = MediaHost::WEB_CLIENT_NAME
  MACOS_CLIENT_PREFIX = "WillDeep Desktop"
  # 与发现文件 mcp-gateway.json 的 host 字段同名。
  MACOS = "willdeep-macos"
  RS = "willdeep-rs"
  OTHER = "other"
  CLIENT_SLUG_LIMIT = 40

  Location = Struct.new(:host, :path, keyword_init: true)

  module_function

  def locate(params, data_directory:, willdeep_home:)
    name = client_name(params)
    case host_for(name)
    when RS
      Location.new(host: RS, path: File.join(willdeep_home, "plugin-data", PLUGIN_ID, FILE_NAME))
    when MACOS
      Location.new(host: MACOS, path: File.join(data_directory, FILE_NAME))
    else
      Location.new(host: OTHER, path: File.join(data_directory, "mcp-http.#{slug(name)}.json"))
    end
  end

  def host_for(name)
    return MACOS if name.empty? || name.start_with?(MACOS_CLIENT_PREFIX)
    return RS if name == RS_CLIENT_NAME

    OTHER
  end

  def client_name(params)
    name = params.is_a?(Hash) ? params.dig("clientInfo", "name") : nil
    name.is_a?(String) ? name.strip : ""
  end

  def slug(name)
    value = name.downcase.gsub(/[^a-z0-9]+/, "-").gsub(/\A-+|-+\z/, "")[0, CLIENT_SLUG_LIMIT].to_s.chomp("-")
    value.empty? ? "client" : value
  end
end
