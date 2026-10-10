# frozen_string_literal: true

require "fileutils"
require "tmpdir"

require_relative "media_host"
# 让插件页面能播放已下载的成片。
#
# 页面跑在 willdeep-plugin:// 这个自定义 scheme 下，CSP 是
# `default-src 'self' data: blob:; connect-src 'none'`：既不能 fetch 本地文件，
# 也不能加载 file:// 或 http://127.0.0.1（宿主明写「Loopback and private hosts
# are never reachable from a plugin page」）。宿主唯一放行的本地媒体入口是
# `willdeep-plugin://bundle/__media__/<文件名>`，它指向本插件数据目录下的
# generated-images/，文件名只允许一段，且必须匹配宿主那条正则。
#
# 成片落在用户的输出目录（默认 ~/Movies/WillDeep Video Studio），在那个入口
# 之外。所以这里在 generated-images/ 里给它做一个**硬链接**：同一份数据、
# 两个目录项，不额外占磁盘；跨卷时才退化成真拷贝。顺手再抽一张首帧 PNG，
# 用作播放器的封面——图片这条路是宿主本来就在用的，mp4 万一被宿主拦下，
# 首帧至少还在。
class MediaMirror
  # macOS 宿主 AgentPluginGeneratedMediaResolver 认的前缀（见 media_host.rb：
  # Web 宿主下换成同源前缀，这里只作常量兜底）。
  URL_PREFIX = MediaHost::DESKTOP_URL_PREFIX
  # 宿主对 __media__ 文件名的校验：单段、首字符字母数字、总长 128 以内。
  HOST_NAME_PATTERN = /\A[A-Za-z0-9][A-Za-z0-9._-]{0,127}\z/
  # 抽首帧最多等这么久。取帧卡住不该把一次页面请求拖死。
  POSTER_TIMEOUT_SECONDS = 20
  POSTER_WIDTH = 960
  # 找外部工具时除了 PATH 再看这两处：宿主启动 MCP 进程时的 PATH 常常只有
  # 系统默认那几段，Homebrew 装的 ffmpeg 不在里面。
  EXTRA_BIN_DIRECTORIES = %w[/opt/homebrew/bin /usr/local/bin].freeze

  Unavailable = Class.new(StandardError)

  def initialize(host:)
    @host = host
  end

  attr_reader :host

  # 活跃媒体根：Web 宿主下是 plugin-media/，macOS 下是 generated-images/。
  def root
    host.root
  end

  # 返回可直接并进任务记录的字段。找不到源文件就抛 Unavailable，不静默成功。
  def publish(id:, path:)
    source = File.expand_path(path.to_s)
    raise Unavailable, "the downloaded video is missing" unless File.file?(source)

    base = safe_base(id)
    FileUtils.mkdir_p(root)
    video_name = "#{base}.mp4"
    mirror_video(source, File.join(root, video_name))
    poster_name = mirror_poster(source, base)
    last_frame_name = mirror_last_frame(source, base)
    {
      "mediaFile" => video_name,
      # 绝对路径给宿主的 ai.complete 用：审核成片时 videoPaths 只收插件生成目录里的
      # 绝对路径，页面自己拼不出这个目录。
      "mediaPath" => File.join(root, video_name),
      "playbackURL" => host.url_for(video_name),
      "posterFile" => poster_name,
      "posterURL" => poster_name ? host.url_for(poster_name) : nil,
      "lastFrameFile" => last_frame_name,
      "lastFramePath" => last_frame_name ? File.join(root, last_frame_name) : nil,
      "lastFrameURL" => last_frame_name ? host.url_for(last_frame_name) : nil
    }
  end

  private

  # 任务 ID 是 UUID，本来就合规；remoteID 之类来自上游的字符串不一定，统一
  # 过一遍。前缀 job- 保证首字符是字母，也让这些文件在 generated-images 里
  # 一眼能和生成图片区分开。
  def safe_base(id)
    token = id.to_s.gsub(/[^A-Za-z0-9._-]/, "-")[0, 90].to_s
    token = "unnamed" if token.empty?
    name = "job-#{token}"
    raise Unavailable, "job id cannot be turned into a media file name" unless HOST_NAME_PATTERN.match?("#{name}.mp4")

    name
  end

  def mirror_video(source, destination)
    return destination if fresh?(source, destination)

    File.delete(destination) if File.exist?(destination)
    begin
      File.link(source, destination)
    rescue SystemCallError
      # 跨卷（EXDEV）或文件系统不支持硬链接时退回拷贝。多占一份磁盘，但能播。
      FileUtils.cp(source, destination)
    end
    destination
  end

  # 已经镜像过就别重来。硬链接的判据是「同一个 inode」；拷贝出来的那份只能
  # 按大小加时间比。
  def fresh?(source, destination)
    return false unless File.file?(destination)
    return true if File.identical?(source, destination)

    File.size(destination) == File.size(source) && File.mtime(destination) >= File.mtime(source)
  rescue SystemCallError
    false
  end

  def mirror_poster(source, base)
    name = "#{base}-poster.png"
    destination = File.join(root, name)
    return name if File.file?(destination) && File.mtime(destination) >= File.mtime(source)

    File.delete(destination) if File.exist?(destination)
    return name if extract_poster(source, destination)

    # 抽不出首帧不是错：播放器自己加载到元数据后也会显示第一帧。
    nil
  end

  def mirror_last_frame(source, base)
    name = "#{base}-last-frame.png"
    destination = File.join(root, name)
    return name if File.file?(destination) && File.mtime(destination) >= File.mtime(source)

    File.delete(destination) if File.exist?(destination)
    ffmpeg = executable("ffmpeg")
    return nil unless ffmpeg
    return nil unless run([ffmpeg, "-nostdin", "-y", "-loglevel", "error", "-sseof", "-0.15", "-i", source,
                           "-frames:v", "1", "-f", "image2", destination])

    File.file?(destination) && File.size(destination).positive? ? name : nil
  end

  # ffmpeg 给的是**真正的第一帧**，优先用它；没装 ffmpeg 时退回系统自带的
  # qlmanage（它给的是 Quick Look 的预览帧，通常也是开头那一帧）。
  def extract_poster(source, destination)
    ffmpeg = executable("ffmpeg")
    if ffmpeg && run([ffmpeg, "-nostdin", "-y", "-loglevel", "error", "-i", source,
                      "-frames:v", "1", "-f", "image2", destination])
      return true if File.file?(destination) && File.size(destination).positive?
    end
    quicklook_poster(source, destination)
  end

  def quicklook_poster(source, destination)
    return false unless File.executable?("/usr/bin/qlmanage")

    Dir.mktmpdir("video-studio-poster-") do |temporary|
      return false unless run(["/usr/bin/qlmanage", "-t", "-s", POSTER_WIDTH.to_s, "-o", temporary, source])

      produced = Dir.glob(File.join(temporary, "*.png")).first
      return false unless produced && File.size(produced).positive?

      FileUtils.mv(produced, destination)
    end
    File.file?(destination)
  rescue SystemCallError
    false
  end

  def executable(name)
    directories = ENV["PATH"].to_s.split(File::PATH_SEPARATOR) + EXTRA_BIN_DIRECTORIES
    directories.each do |directory|
      next if directory.to_s.empty?

      candidate = File.join(directory, name)
      return candidate if File.executable?(candidate) && File.file?(candidate)
    end
    nil
  end

  # 超时就杀掉。外部工具卡死时不能让页面的这次请求一起挂住。
  def run(command)
    pid = Process.spawn(*command, out: File::NULL, err: File::NULL)
    deadline = Time.now + POSTER_TIMEOUT_SECONDS
    loop do
      _, status = Process.waitpid2(pid, Process::WNOHANG)
      return status.success? if status
      if Time.now > deadline
        Process.kill("KILL", pid)
        Process.waitpid(pid)
        return false
      end
      sleep 0.1
    end
  rescue SystemCallError
    false
  end
end
