# frozen_string_literal: true

require "fileutils"

# 媒体落点跟着宿主走。
#
# 同一个插件页面跑在两个宿主下，媒体地址的形状不同：
# - macOS 宿主把页面跑在自定义 scheme `willdeep-plugin://bundle/` 下，页面只认
#   `willdeep-plugin://bundle/__media__/<文件名>`，它映射到插件数据目录的
#   generated-images/；别的本地入口一律不放行。
# - willdeep-rs 的 Web 宿主把页面跑在 http(s) 下（`127.0.0.1:9847`），自定义
#   scheme 在那里根本解析不了，它只放行同源的
#   `/plugin-media/willdeep-video-studio/<文件名>`，文件读
#   `<WILLDEEP_HOME 或 ~/.willdeep>/plugin-media/willdeep-video-studio/`。
#
# 认宿主的依据是 MCP `initialize` 的 `clientInfo.name`：willdeep-rs 走
# willdeep-core 的 MCP 客户端，发 `willdeep`；macOS 宿主的 initialize 不带
# clientInfo。认不出来（第三方 MCP 客户端、独立运行）保持原来的 macOS 行为，
# 也可以用 VIDEO_STUDIO_HOST_MODE=web / VIDEO_STUDIO_HOST_MEDIA_ROOT 强制指定。
#
# Web 宿主下存档依旧只认文件名：读出时把文件按需硬链接到 Web 媒体根（跨卷退化
# 成拷贝），macOS 侧那份原样留着，谁都不用手工搬。
class MediaHost
  PLUGIN_ID = "willdeep-video-studio"
  DESKTOP_URL_PREFIX = "willdeep-plugin://bundle/__media__/"
  WEB_URL_PREFIX = "/plugin-media/#{PLUGIN_ID}/"
  # willdeep-rs 发的 clientInfo.name（willdeep-core/src/mcp.rs 的 initialize_params）。
  WEB_CLIENT_NAME = "willdeep"
  # 任务记录里会进页面播放器的几个地址。
  URL_FIELDS = %w[playbackURL posterURL lastFrameURL].freeze
  # 宿主对 __media__ 文件名的校验：单段、首字符字母数字、总长 128 以内。
  NAME_PATTERN = /\A[A-Za-z0-9][A-Za-z0-9._-]{0,127}\z/.freeze

  def initialize(desktop_root:, env: ENV)
    @desktop_root = File.expand_path(desktop_root.to_s)
    @env = env
    @detected = nil
    @forced = forced_mode
  end

  # macOS 侧的媒体根：所有生成物都先落在这里，也是 Web 宿主镜像的来源。
  attr_reader :desktop_root

  # 每次 initialize 都重算：宿主没发 clientInfo 就不该沿用上一次的判断。
  def record_initialize(params)
    name = params.is_a?(Hash) ? params.dig("clientInfo", "name").to_s : ""
    @detected = name == WEB_CLIENT_NAME ? "web" : nil
  end

  def mode
    @forced || @detected || "desktop"
  end

  def web?
    mode == "web"
  end

  # 当前活跃媒体根：写入与读出都以它为准。
  def root
    web? ? web_root : desktop_root
  end

  def web_root
    override = @env["VIDEO_STUDIO_HOST_MEDIA_ROOT"].to_s.strip
    return File.expand_path(override) unless override.empty?

    File.join(willdeep_home, "plugin-media", PLUGIN_ID)
  end

  def willdeep_home
    override = @env["WILLDEEP_HOME"].to_s.strip
    return File.expand_path(override) unless override.empty?

    File.expand_path("~/.willdeep")
  end

  def url_prefix
    web? ? WEB_URL_PREFIX : DESKTOP_URL_PREFIX
  end

  def url_for(file_name)
    name = File.basename(file_name.to_s)
    name.empty? ? nil : "#{url_prefix}#{name}"
  end

  # 旧存档里记的可能是另一个宿主的前缀；按当前宿主重写。认不出的地址
  # （data:、blob:、别处的 http、别处的相对路径）原样留着。
  def normalize_url(value)
    name = media_name(value)
    name ? url_for(name) : value
  end

  # 不改原对象：任务哈希常常还是存档里的那一份，落盘只该写文件名。
  def rewrite_urls(entry)
    return entry unless entry.is_a?(Hash)

    entry.each_with_object({}) do |(key, value), copy|
      copy[key] = URL_FIELDS.include?(key) && !value.to_s.empty? ? normalize_url(value) : value
    end
  end

  # 文件名，或带已知前缀的插件媒体地址 -> 媒体根一层的文件名；不是本插件的
  # 媒体返回 nil。
  def media_name(value)
    raw = value.to_s
    return nil if raw.empty?

    [WEB_URL_PREFIX, DESKTOP_URL_PREFIX].each do |prefix|
      next unless raw.start_with?(prefix)

      name = raw.delete_prefix(prefix).split("/").last.to_s
      return NAME_PATTERN.match?(name) ? name : nil
    end
    NAME_PATTERN.match?(raw) ? raw : nil
  end

  # 按当前媒体根取路径。Web 宿主下文件只在 desktop_root 时先硬链接过去；
  # 哪儿都没有就返回 nil。
  def path_for(file_name)
    name = media_name(file_name)
    return nil unless name

    active = File.join(root, name)
    return active if File.file?(active)
    return nil unless web?

    materialize(File.join(desktop_root, name))
  end

  # 绝对路径 -> 当前媒体根里的路径。文件不存在返回 nil。
  def materialize(path)
    expanded = File.expand_path(path.to_s)
    return nil unless File.file?(expanded)
    return expanded unless web?
    return expanded if File.dirname(expanded) == web_root

    link(expanded, File.join(web_root, File.basename(expanded)))
  end

  # 路径钳制的口径：活跃媒体根与 macOS 媒体根都认（换宿主后旧路径还要能读）。
  def within?(path)
    expanded = File.expand_path(path.to_s)
    [root, desktop_root].any? { |base| expanded.start_with?("#{base}#{File::SEPARATOR}") }
  end

  private

  # 同一份数据、两个目录项，不额外占磁盘；跨卷（EXDEV）或文件系统不支持硬
  # 链接时退回真拷贝。
  def link(source, destination)
    return destination if fresh?(source, destination)

    FileUtils.mkdir_p(File.dirname(destination))
    File.delete(destination) if File.exist?(destination)
    begin
      File.link(source, destination)
    rescue SystemCallError
      FileUtils.cp(source, destination)
    end
    File.file?(destination) ? destination : nil
  rescue SystemCallError
    nil
  end

  # 已经镜像过就别重来。硬链接的判据是同一个 inode；拷贝出来的只能按大小加
  # 时间比。
  def fresh?(source, destination)
    return false unless File.file?(destination)
    return true if File.identical?(source, destination)

    File.size(destination) == File.size(source) && File.mtime(destination) >= File.mtime(source)
  rescue SystemCallError
    false
  end

  def forced_mode
    root_override = @env["VIDEO_STUDIO_HOST_MEDIA_ROOT"].to_s.strip
    return "web" unless root_override.empty?

    mode = @env["VIDEO_STUDIO_HOST_MODE"].to_s.strip.downcase
    %w[web desktop].include?(mode) ? mode : nil
  end
end
