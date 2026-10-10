# frozen_string_literal: true

require "json"
require "open3"

# 找 ffmpeg / ffprobe、带超时地跑、读媒体信息。
#
# 宿主启动 MCP 进程时的 PATH 常常只有系统默认那几段，Homebrew 装的 ffmpeg 不在
# 里面，所以除了 PATH 再看 /opt/homebrew/bin 与 /usr/local/bin（与 MediaMirror 同一
# 套找法）。合成一集要跑几十条命令，每条都要能被超时和取消打断，也要留下 stderr
# 末尾几行给用户看得懂的错误。
module FFmpegTool
  EXTRA_BIN_DIRECTORIES = %w[/opt/homebrew/bin /usr/local/bin].freeze
  # 单条命令默认最多跑 15 分钟：一集 3 分钟的成片在慢机器上转码也够了。
  DEFAULT_TIMEOUT_SECONDS = 900
  STDERR_TAIL_LINES = 6

  class Failed < StandardError
    attr_reader :code

    def initialize(code, message)
      super(message)
      @code = code
    end
  end

  module_function

  def find(name, env: ENV)
    override = env["VIDEO_STUDIO_#{name.upcase}"].to_s.strip
    return override if !override.empty? && File.executable?(override)

    directories = env["PATH"].to_s.split(File::PATH_SEPARATOR) + EXTRA_BIN_DIRECTORIES
    directories.each do |directory|
      next if directory.to_s.empty?

      candidate = File.join(directory, name)
      return candidate if File.file?(candidate) && File.executable?(candidate)
    end
    nil
  end

  def available?(env: ENV)
    !find("ffmpeg", env: env).nil? && !find("ffprobe", env: env).nil?
  end

  # 这份 ffmpeg 编进了某个滤镜没有。Homebrew 的 ffmpeg 不带 libfreetype，没有 drawtext；
  # 不先问一句，用到它的命令会以「No such filter」整条失败。按可执行文件路径缓存。
  FILTER_CACHE = {}
  FILTER_LOCK = Mutex.new

  def filter?(ffmpeg, name)
    FILTER_LOCK.synchronize do
      FILTER_CACHE[ffmpeg] ||= begin
        output, status = Open3.capture2e(ffmpeg, "-hide_banner", "-filters")
        status.success? ? output.lines.map { |line| line.split[1] }.compact : []
      rescue SystemCallError
        []
      end
      FILTER_CACHE[ffmpeg].include?(name)
    end
  end

  # 跑一条命令，失败抛 Failed（带 stderr 末尾几行）。超时先 TERM 再 KILL。
  def run!(command, timeout: DEFAULT_TIMEOUT_SECONDS, code: "ffmpeg_failed")
    stderr_text = +""
    status = nil
    Open3.popen3(*command) do |stdin, stdout, stderr, thread|
      stdin.close
      readers = [
        Thread.new { stdout.read },
        Thread.new { stderr_text << stderr.read.to_s }
      ]
      unless thread.join(timeout)
        terminate(thread.pid)
        readers.each { |reader| reader.join(2) }
        raise Failed.new("ffmpeg_timeout", "#{File.basename(command.first)} did not finish within #{timeout} seconds.")
      end
      readers.each(&:join)
      status = thread.value
    end
    return true if status && status.success?

    tail = stderr_text.to_s.encode("UTF-8", invalid: :replace, undef: :replace).lines.last(STDERR_TAIL_LINES).join.strip
    raise Failed.new(code, "#{File.basename(command.first)} exited #{status && status.exitstatus}: #{tail}")
  rescue Errno::ENOENT => error
    raise Failed.new("ffmpeg_missing", "#{File.basename(command.first)} is not installed (#{error.message}).")
  end

  # 时长（毫秒）、宽高与是否有音轨。读不出来返回 nil。
  def probe(path, env: ENV)
    ffprobe = find("ffprobe", env: env)
    return nil unless ffprobe && File.file?(path.to_s)

    output, status = Open3.capture2(ffprobe, "-v", "error", "-show_entries",
                                    "format=duration:stream=codec_type,width,height", "-of", "json", path.to_s)
    return nil unless status.success?

    parsed = JSON.parse(output)
    streams = Array(parsed["streams"])
    video = streams.find { |stream| stream["codec_type"] == "video" }
    duration = parsed.dig("format", "duration").to_f
    {
      "durationMs" => duration.positive? ? (duration * 1000).round : nil,
      "width" => video && video["width"].to_i,
      "height" => video && video["height"].to_i,
      "hasVideo" => !video.nil?,
      "hasAudio" => streams.any? { |stream| stream["codec_type"] == "audio" }
    }
  rescue JSON::ParserError, SystemCallError
    nil
  end

  def terminate(pid)
    Process.kill("TERM", pid)
    deadline = Time.now + 3
    while Time.now < deadline
      return if Process.waitpid(pid, Process::WNOHANG)
      sleep 0.1
    end
    Process.kill("KILL", pid)
  rescue Errno::ESRCH, Errno::ECHILD
    nil
  end
end
