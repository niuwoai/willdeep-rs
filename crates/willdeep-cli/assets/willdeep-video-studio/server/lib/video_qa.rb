# frozen_string_literal: true

require "digest"
# Digest::SHA256 默认是自动加载的；几条流水线线程第一次同时用到它时，Ruby 2.6 会报
# 「Digest::Base cannot be directly inherited in Ruby」（加载竞争）。启动时就加载好。
require "digest/sha2"
require "fileutils"
require "open3"
require_relative "ffmpeg_tool"

# Local preparation for the host's multimodal visual reviewer.
class VideoQA
  RUBRIC_VERSION = "frames-v1"
  SCENE_THRESHOLD = 0.1
  SHEET_FPS = 2
  SHEET_COLUMNS = 6
  SHEET_ROWS = 4
  SHEET_PREFIX = "qa-sheet"

  def initialize(media_host:)
    @media_host = media_host
  end

  def material(job, expected_duration: nil)
    source = job["outputPath"].to_s
    return failure("missing_output", "This completed clip has no local video file.") unless File.file?(source)

    info = FFmpegTool.probe(source)
    return failure("video_probe_failed", "Unable to read the clip duration or video stream.") unless info && info["hasVideo"]

    images, stamped = contact_sheets(job, source)
    return failure("video_sheets_empty", "Frame extraction did not produce review sheets.") if images.empty?

    expected_ms = expected_duration.to_i * 1000
    {
      "ok" => true,
      "imagePaths" => images,
      "autoChecks" => {
        # 拼图怎么读：每格的时间点。ffmpeg 没有 drawtext 时格子上没有时间戳，
        # 审核模型只能按版式算：第 N 张（从 1 数）第 k 格（从 0 数，按行从左到右）
        # 的时间是 ((N-1)*cellsPerSheet + k) * secondsPerCell。
        "sheetLayout" => { "columns" => SHEET_COLUMNS, "rows" => SHEET_ROWS, "secondsPerCell" => 1.0 / SHEET_FPS,
                           "timestampsBurnedIn" => stamped },
        "durationMs" => info["durationMs"],
        "expectedDurationMs" => expected_ms.positive? ? expected_ms : nil,
        "durationSuspect" => expected_ms.positive? && info["durationMs"].to_i < expected_ms - 500,
        "cuts" => scene_cuts(source, info["durationMs"]),
        "sync" => nil
      }.reject { |_, value| value.nil? }
    }
  rescue FFmpegTool::Failed => error
    failure(error.code, error.message)
  rescue SystemCallError => error
    failure("video_qa_io_failed", "Unable to prepare review images: #{error.class}.")
  end

  private

  # 返回 [拼图路径, 格子上是否烧了时间戳]。
  def contact_sheets(job, source)
    ffmpeg = FFmpegTool.find("ffmpeg")
    return [[], false] unless ffmpeg

    # Homebrew 的 ffmpeg 没有 drawtext（不带 libfreetype）。以前照样用它，每次质检都以
    # 「No such filter: 'drawtext'」失败；没有就出不带时间戳的拼图，版式写进 autoChecks。
    stamped = FFmpegTool.filter?(ffmpeg, "drawtext")
    stat = File.stat(source)
    digest = Digest::SHA256.hexdigest([job["id"], stat.size, stat.mtime.to_f, RUBRIC_VERSION, stamped ? "stamped" : "plain"].join("\n"))[0, 18]
    # 拼图直接放在媒体根这一层，不建子目录（0.38.0-rc1）。WillDeep 宿主只收「插件生成目录的直接
    # 子文件」当附件（Xedit AgentPluginAIMedia.clampedFileURL：resolved.deletingLastPathComponent() == root），
    # 0.33～0.37 写在 generated-images/qa/ 下，宿主内每次画面质检都报「Attached media is outside this
    # plugin's generated-media folder」。文件名带任务键，换了成片（指纹变了）就删掉这一条任务的旧拼图。
    output = @media_host.desktop_root
    FileUtils.mkdir_p(output)
    job_key = Digest::SHA256.hexdigest(job["id"].to_s)[0, 10]
    glob = File.join(output, "#{SHEET_PREFIX}-#{job_key}-#{digest}-*.jpg")
    cached = Dir.glob(glob).sort
    # 不用 filter_map：mcp.json 钉的 /usr/bin/ruby 是 2.6，没有这个方法，质检会当场 NoMethodError。
    return [cached.map { |path| @media_host.materialize(path) }.compact, stamped] unless cached.empty?

    remove_stale_sheets(output, job_key, digest)
    pattern = File.join(output, "#{SHEET_PREFIX}-#{job_key}-#{digest}-%02d.jpg")
    stamp = stamped ? ",drawtext=text='%{pts\\:hms}':x=4:y=4:fontsize=14:fontcolor=white:box=1:boxcolor=black@0.5" : ""
    filter = "fps=#{SHEET_FPS},scale=256:-2#{stamp},tile=#{SHEET_COLUMNS}x#{SHEET_ROWS}"
    FFmpegTool.run!([ffmpeg, "-hide_banner", "-loglevel", "error", "-i", source, "-vf", filter, "-y", pattern])
    [Dir.glob(glob).sort.map { |path| @media_host.materialize(path) }.compact, stamped]
  end

  # 同一条任务换过成片后留下的旧拼图（Web 宿主下的硬链接副本一并删）。
  def remove_stale_sheets(output, job_key, digest)
    roots = [output, @media_host.root].uniq
    roots.each do |root|
      Dir.glob(File.join(root, "#{SHEET_PREFIX}-#{job_key}-*.jpg")).each do |path|
        next if File.basename(path).start_with?("#{SHEET_PREFIX}-#{job_key}-#{digest}-")

        File.delete(path)
      end
    end
  rescue SystemCallError => error
    warn "video-studio: stale QA sheets not removed (#{error.class}: #{error.message})"
  end

  def scene_cuts(source, duration_ms)
    ffmpeg = FFmpegTool.find("ffmpeg")
    return [] unless ffmpeg

    command = [ffmpeg, "-hide_banner", "-i", source, "-an", "-vf",
               "select='gt(scene,#{SCENE_THRESHOLD})',metadata=print", "-f", "null", "-"]
    _stdout, stderr, status = Open3.capture3(*command)
    return [] unless status.success?

    duration = duration_ms.to_i / 1000.0
    stderr.scan(/pts_time:([0-9.]+).*?lavfi\.scene_score=([0-9.]+)/m).map do |time, score|
      seconds = time.to_f
      next if seconds < 0.2 || seconds > duration - 0.2

      { "t" => seconds.round(3), "score" => score.to_f.round(4) }
    end.compact
  rescue SystemCallError
    []
  end

  def failure(code, message)
    { "ok" => false, "error" => { "code" => code, "message" => message } }
  end
end
