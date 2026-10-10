# frozen_string_literal: true

require "digest"
require "fileutils"
require "json"
require "open3"
require "time"
require "tmpdir"

require_relative "episode_plan"
require_relative "clip_trim"
require_relative "ffmpeg_tool"
require_relative "caption_overlay"

# 分集成片流水线（设计稿 docs/design/episode-compose.md 第 1 节），只在后台合成进程里跑：
# 补配音 → 背景音乐 → 逐镜合成 → 拼接 → 混背景音乐 → 发布。
#
# 每一步都把进度写回 ComposeStore，页面轮询 episode.compose_status 看到的就是这里写的。
# 失败抛 FFmpegTool::Failed 或 ComposePipeline::Failed，由调用方记进任务。
class ComposePipeline
  FPS = 30
  AUDIO_RATE = 48_000
  DEFAULT_WIDTH = 720
  DEFAULT_HEIGHT = 1280
  BGM_FADE_IN_SECONDS = 1.0
  BGM_FADE_OUT_SECONDS = 2.0
  # 成片响度：手机短视频常用的 -16 LUFS 积分响度、真峰值 -1.5 dBTP。TTS 原文件多在 -20～-24 LUFS，
  # 不拉齐的话整集偏小声，用户得把音量开得很大才听得清台词。
  TARGET_LOUDNESS_LUFS = -16.0
  TARGET_TRUE_PEAK_DBTP = -1.5
  TARGET_LOUDNESS_RANGE_LU = 11.0
  # 背景音乐时长：本集预计时长向上取整到 10 秒，夹到 30–600 秒（ACE-Step 支持 10–600）。
  MIN_MUSIC_SECONDS = 30
  MAX_MUSIC_SECONDS = 600
  VOICE_BATCH = 20

  class Failed < StandardError
    attr_reader :code

    def initialize(code, message)
      super(message)
      @code = code
    end
  end

  # dramas：DramaService；video_jobs：返回 VideoService#jobs_with_media 的 lambda；
  # voice：VoiceGeneration（可为 nil）；music：MusicBackend 适配器（可为 nil）。
  def initialize(store:, dramas:, video_jobs:, media_mirror:, output_directory:, voice: nil, music: nil, env: ENV)
    @store = store
    @dramas = dramas
    @video_jobs = video_jobs
    @media_mirror = media_mirror
    @output_directory = output_directory
    @voice = voice
    @music = music
    @env = env
  end

  def run(job)
    raise Failed.new("ffmpeg_missing", "没有找到 ffmpeg / ffprobe，请先安装（brew install ffmpeg）。") unless FFmpegTool.available?(env: @env)

    @job = job
    @warnings = []
    Dir.mktmpdir("episode-compose-") do |work|
      if job["kind"] == "music"
        generate_music(force: true)
        return finish({})
      end

      dub
      bgm = prepare_music
      drama, episode, plan = load_plan
      raise Failed.new("compose_blocked", plan["blocking"].map { |entry| entry["message"] }.join(" ")) unless plan["blocking"].empty?

      size = target_size(drama, plan)
      clips = render_shots(plan, size, work)
      joined = File.join(work, "joined.mp4")
      progress("concat", 85, "拼接 #{clips.length} 个镜头")
      concat(clips, joined, work)
      final = joined
      if bgm
        progress("mix", 90, "混入背景音乐")
        final = File.join(work, "final.mp4")
        mix_music(joined, bgm, final)
      end
      progress("mix", 93, "统一响度")
      normalized = File.join(work, "normalized.mp4")
      final = normalized if normalize_loudness(final, normalized)
      progress("publish", 95, "写出成片")
      finish(publish(drama, episode, final))
    end
  end

  private

  def progress(step, value, message)
    @store.update_job(@job["id"], "step" => step, "progress" => value, "message" => message, "warnings" => @warnings)
  end

  def warn_about(code, extra = {})
    @warnings << { "code" => code }.merge(extra)
  end

  def finish(fields)
    @store.update_job(@job["id"], fields.merge("state" => "completed", "progress" => 100, "message" => "",
                                               "warnings" => @warnings, "finishedAt" => Time.now.utc.iso8601))
  end

  def load_plan
    loaded = @dramas.get("id" => @job["dramaID"])
    raise Failed.new("drama_not_found", "这部剧已经不存在了。") unless loaded["ok"]

    drama = loaded["drama"]
    episode = Array(drama["episodes"]).find { |entry| entry["id"] == @job["episodeID"] }
    raise Failed.new("episode_not_found", "这一集已经不存在了。") unless episode

    settings = @store.settings(@job["dramaID"], @job["episodeID"])
    plan = EpisodePlan.build(drama: drama, episode: episode, jobs: @video_jobs.call, settings: settings, media_root: @dramas.media_root)
    [drama, episode, plan.merge("settings" => settings)]
  end

  # 第 1 步：给走「单独配音」的镜头补缺失或过期的台词配音。
  def dub
    _, _, plan = load_plan
    targets = plan["shots"].map do |shot|
      ids = shot["lines"].select { |line| %w[missing stale].include?(line["audio"]) }.map { |line| line["lineID"] }
      [shot, ids]
    end.reject { |_, ids| ids.empty? }
    plan["shots"].each do |shot|
      shot["lines"].select { |line| line["audio"] == "no_voice" }.each do |line|
        warn_about("line_without_voice", "shotID" => shot["shotID"], "lineID" => line["lineID"], "speaker" => line["speaker"])
      end
    end
    return if targets.empty?

    unless @voice
      targets.each { |shot, ids| ids.each { |id| warn_about("line_without_audio", "shotID" => shot["shotID"], "lineID" => id) } }
      warn_about("tts_unavailable")
      return
    end

    total = targets.sum { |_, ids| ids.length }
    done = 0
    targets.each do |shot, ids|
      ids.each_slice(VOICE_BATCH) do |batch|
        progress("dub", 5 + (done * 25 / [total, 1].max), "配音 #{done + 1}/#{total} 句")
        result = @voice.generate("dramaID" => @job["dramaID"], "episodeID" => @job["episodeID"], "shotID" => shot["shotID"],
                                 "lineIDs" => batch, "requestID" => "compose-#{@job['id']}-#{shot['shotID']}-#{done}")
        Array(result["failed"]).each { |entry| warn_about("line_dub_failed", "shotID" => shot["shotID"], "lineID" => entry["lineID"], "message" => entry["message"]) }
        Array(result["skipped"]).each { |entry| warn_about("line_dub_skipped", "shotID" => shot["shotID"], "lineID" => entry["lineID"], "reason" => entry["reason"]) }
        if !result["ok"] && result.dig("error", "code") == "tts_unavailable"
          warn_about("tts_unavailable")
          return
        end
        done += batch.length
      end
    end
  end

  # 第 2 步：拿到这一集要用的背景音乐文件路径（没有返回 nil）。
  def prepare_music
    settings = @store.settings(@job["dramaID"], @job["episodeID"])
    bgm = settings["bgm"]
    case bgm["source"]
    when "file"
      path = music_path(bgm)
      warn_about("music_missing") unless path
      path
    when "acestep"
      path = music_path(bgm)
      return path if path && bgm["generatedFor"] == music_fingerprint(bgm)

      generate_music(force: false)
    end
  end

  def generate_music(force:)
    settings = @store.settings(@job["dramaID"], @job["episodeID"])
    bgm = settings["bgm"]
    unless @music
      raise Failed.new("music_unavailable", "没有可用的背景音乐服务（ACE-Step 未配置）。") if force

      warn_about("music_unavailable")
      return nil
    end
    prompt = bgm["prompt"].to_s.strip
    prompt = "纯音乐，短剧背景配乐，情绪克制，无人声" if prompt.empty?
    seconds = music_seconds
    progress("music", 32, "生成背景音乐（约 #{seconds} 秒）")
    begin
      result = @music.generate(prompt: prompt, duration_seconds: seconds, request_id: @job["id"])
    rescue StandardError => error
      raise Failed.new(error.respond_to?(:code) ? error.code : "music_failed", "背景音乐生成失败：#{error.message}") if force

      warn_about("music_failed", "message" => error.message)
      return nil
    end
    fingerprint = music_fingerprint(bgm)
    @store.record_music(@job["dramaID"], @job["episodeID"],
                        "fileName" => result["fileName"], "durationMs" => result["durationMs"], "generatedFor" => fingerprint)
    result["filePath"]
  end

  def music_seconds
    _, _, plan = load_plan
    seconds = (plan["estimatedDurationMs"].to_f / 1000 / 10).ceil * 10
    [[seconds, MIN_MUSIC_SECONDS].max, MAX_MUSIC_SECONDS].min
  end

  def music_fingerprint(bgm)
    Digest::SHA256.hexdigest("#{bgm['prompt'].to_s.strip}\n#{music_seconds}")[0, 16]
  end

  def music_path(bgm)
    name = bgm["fileName"].to_s
    return nil if name.empty? || name.include?("/")

    path = File.join(@dramas.media_root, name)
    File.file?(path) ? path : nil
  end

  # 成片宽高以剧的画幅表为准（drama.frame.video，0.30.0-rc3 起；旧剧 9:16 实为 768x1152），
  # 读不到时退回本集第一镜片段的实际宽高。其余镜头等比缩放加黑边。
  def target_size(drama, plan)
    video = drama["frame"].is_a?(Hash) ? drama["frame"]["video"] : nil
    if video.is_a?(Hash) && video["width"].to_i.positive? && video["height"].to_i.positive?
      width = video["width"].to_i
      height = video["height"].to_i
      return [width - width % 2, height - height % 2]
    end

    first = plan["shots"].first
    info = first && FFmpegTool.probe(first["video"]["outputPath"], env: @env)
    width = info && info["width"].to_i.positive? ? info["width"] : DEFAULT_WIDTH
    height = info && info["height"].to_i.positive? ? info["height"] : DEFAULT_HEIGHT
    [width - width % 2, height - height % 2]
  end

  # 第 3 步：逐镜统一规格并混入配音。
  def render_shots(plan, size, work)
    total = plan["shots"].length
    plan["shots"].each_with_index.map do |shot, index|
      progress("shots", 35 + (index * 50 / [total, 1].max), "合成第 #{index + 1}/#{total} 镜")
      output = File.join(work, format("shot-%03d.mp4", index + 1))
      render_shot(shot, plan["settings"], size, output)
      output
    end
  end

  def render_shot(shot, settings, size, output)
    clip = shot["video"]["outputPath"]
    info = FFmpegTool.probe(clip, env: @env) || {}
    clip_ms = info["durationMs"].to_i
    clip_ms = shot["duration"].to_i * 1000 if clip_ms <= 0
    # 裁剪（0.39.0-rc1，lib/clip_trim.rb）：输入端 -ss / -t 截取，画面与原声同一个窗口，不会错位；
    # 之后的一切（台词排入、定格补足）都按截取后的长度算。配音节奏算出的裁剪（paceTrim，0.43.0-rc1）优先。
    trim = EpisodePlan.effective_trim(shot)
    from = ClipTrim.in_seconds(trim)
    window = []
    if trim
      clip_ms = ClipTrim.kept_ms(trim, clip_ms) || clip_ms
      window += ["-ss", format("%.3f", from)] if from.positive?
      window += ["-t", format("%.3f", clip_ms / 1000.0)]
    end
    width, height = size
    # ref2va 片段选了配音：铺生成它的那条对白音轨，从 0 毫秒起（口型照它对），视频原声静音。
    track = shot["voiceSource"] == "tts" ? shot["dialogueTrack"] : nil
    # 口型音轨与画面同窗：有入点时音轨也从入点起读，口型不错位。
    track_skip_ms = track ? [(from * 1000).round, track["durationMs"].to_i].min : 0
    lines = if track
              [{ "filePath" => track["filePath"], "durationMs" => track["durationMs"].to_i - track_skip_ms }]
            elsif shot["voiceSource"] == "tts"
              shot["lines"].select { |line| line["audio"] == "ready" && line["filePath"] }
            else
              []
            end
    shot["lines"].each do |line|
      next unless shot["voiceSource"] == "tts" && !track && line["audio"] != "ready"

      warn_about("line_without_audio", "shotID" => shot["shotID"], "lineID" => line["lineID"])
    end
    offsets = []
    cursor = track ? 0 : EpisodePlan::LEAD_IN_MS
    lines.each do |line|
      offsets << cursor
      cursor += line["durationMs"].to_i + (track ? 0 : EpisodePlan::GAP_MS)
    end
    speech_end = lines.empty? ? 0 : cursor
    total_ms = [clip_ms, speech_end].max
    total = format("%.3f", total_ms / 1000.0)
    extend_seconds = format("%.3f", [(total_ms - clip_ms) / 1000.0, 0].max)

    command = [FFmpegTool.find("ffmpeg", env: @env), "-nostdin", "-y", "-loglevel", "error", *window, "-i", clip]
    command += ["-f", "lavfi", "-t", total, "-i", "anullsrc=r=#{AUDIO_RATE}:cl=stereo"]
    lines.each do |line|
      command += ["-ss", format("%.3f", track_skip_ms / 1000.0)] if track && track_skip_ms.positive?
      command += ["-i", line["filePath"]]
    end

    # 画面字幕（0.41.0-rc1，lib/caption_overlay.rb）：渲染成 PNG 作为最后一路输入叠上去；渲染不出来只记警告。
    caption_png = nil
    unless shot["caption"].to_s.strip.empty?
      caption_png = CaptionOverlay.render(shot["caption"], height, output.sub(/\.mp4\z/, "-caption.png"), env: @env)
      warn_about("caption_not_rendered", "shotID" => shot["shotID"], "caption" => shot["caption"]) unless caption_png
    end
    command += CaptionOverlay.input_args(caption_png, total_ms / 1000.0) if caption_png

    filters = []
    scaled = caption_png ? "[base]" : "[v]"
    filters << "[0:v]scale=#{width}:#{height}:force_original_aspect_ratio=decrease,pad=#{width}:#{height}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=#{FPS},format=yuv420p,tpad=stop_mode=clone:stop_duration=#{extend_seconds}#{scaled}"
    filters.concat(CaptionOverlay.filters("[base]", lines.length + 2, "[v]", width, height, total_ms / 1000.0)) if caption_png
    original_volume = if track
                        0.0
                      elsif shot["voiceSource"] == "tts"
                        settings["originalVolume"].to_f
                      else
                        1.0
                      end
    base = info["hasAudio"] ? "[0:a]" : "[1:a]"
    filters << "#{base}aresample=#{AUDIO_RATE},aformat=channel_layouts=stereo,volume=#{original_volume},apad[orig]"
    mix_inputs = ["[orig]"]
    lines.each_with_index do |_, index|
      delay = offsets[index]
      filters << "[#{index + 2}:a]aresample=#{AUDIO_RATE},aformat=channel_layouts=stereo,adelay=#{delay}|#{delay}[l#{index}]"
      mix_inputs << "[l#{index}]"
    end
    filters << "#{mix_inputs.join}amix=inputs=#{mix_inputs.length}:normalize=0:duration=first,atrim=0:#{total},asetpts=N/SR/TB[a]"

    command += ["-filter_complex", filters.join(";"), "-map", "[v]", "-map", "[a]", "-t", total,
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-pix_fmt", "yuv420p",
                "-c:a", "aac", "-b:a", "192k", "-ar", AUDIO_RATE.to_s, "-ac", "2", "-movflags", "+faststart", output]
    FFmpegTool.run!(command)
  end

  # 第 4 步：各镜已统一编码参数，concat demuxer 直接无损拼接。
  def concat(clips, output, work)
    list = File.join(work, "concat.txt")
    File.write(list, clips.map { |path| "file '#{path.gsub("'", "'\\\\''")}'" }.join("\n") + "\n")
    FFmpegTool.run!([FFmpegTool.find("ffmpeg", env: @env), "-nostdin", "-y", "-loglevel", "error",
                     "-f", "concat", "-safe", "0", "-i", list, "-c", "copy", "-movflags", "+faststart", output])
  end

  # 第 5 步：背景音乐循环或截断到整集长度，有台词 / 原声时自动压低，首尾淡入淡出。
  def mix_music(input, music, output)
    info = FFmpegTool.probe(input, env: @env) || {}
    total = info["durationMs"].to_f / 1000
    raise Failed.new("concat_failed", "拼接后的成片读不出时长。") unless total.positive?

    settings = @store.settings(@job["dramaID"], @job["episodeID"])
    volume = settings["bgm"]["volume"].to_f
    fade_out_start = format("%.3f", [total - BGM_FADE_OUT_SECONDS, 0].max)
    duration = format("%.3f", total)
    filters = [
      "[1:a]aresample=#{AUDIO_RATE},aformat=channel_layouts=stereo,volume=#{volume},atrim=0:#{duration},asetpts=N/SR/TB," \
        "afade=t=in:st=0:d=#{BGM_FADE_IN_SECONDS},afade=t=out:st=#{fade_out_start}:d=#{BGM_FADE_OUT_SECONDS}[bg]",
      "[0:a]asplit=2[main][side]",
      "[bg][side]sidechaincompress=threshold=0.03:ratio=6:attack=20:release=500[ducked]",
      "[main][ducked]amix=inputs=2:normalize=0:duration=first[a]"
    ]
    FFmpegTool.run!([FFmpegTool.find("ffmpeg", env: @env), "-nostdin", "-y", "-loglevel", "error", "-i", input,
                     "-stream_loop", "-1", "-i", music, "-filter_complex", filters.join(";"),
                     "-map", "0:v", "-map", "[a]", "-c:v", "copy", "-c:a", "aac", "-b:a", "192k", "-ar", AUDIO_RATE.to_s,
                     "-t", duration, "-movflags", "+faststart", output])
  end

  # 第 5.5 步：两遍 loudnorm 把整集拉到目标响度（线性增益，不压动态，ducking 形成的台词/音乐比例不变）。
  # 整集无声（测出 -inf）或测量失败时不处理，返回 false，沿用原文件。
  def normalize_loudness(input, output)
    target = "I=#{TARGET_LOUDNESS_LUFS}:TP=#{TARGET_TRUE_PEAK_DBTP}:LRA=#{TARGET_LOUDNESS_RANGE_LU}"
    ffmpeg = FFmpegTool.find("ffmpeg", env: @env)
    _, stderr, status = Open3.capture3(ffmpeg, "-nostdin", "-hide_banner", "-i", input, "-vn", "-af", "loudnorm=#{target}:print_format=json", "-f", "null", "-")
    measured = status.success? ? JSON.parse(stderr.to_s[/\{[^{}]*"input_i"[^{}]*\}/m].to_s) : {}
    keys = %w[input_i input_tp input_lra input_thresh target_offset]
    unless keys.all? { |key| measured[key].to_s.match?(/\A-?\d+(\.\d+)?\z/) }
      warn_about("loudness_skipped")
      return false
    end

    filter = "loudnorm=#{target}:measured_I=#{measured['input_i']}:measured_TP=#{measured['input_tp']}:measured_LRA=#{measured['input_lra']}" \
             ":measured_thresh=#{measured['input_thresh']}:offset=#{measured['target_offset']}:linear=true"
    FFmpegTool.run!([ffmpeg, "-nostdin", "-y", "-loglevel", "error", "-i", input, "-map", "0:v", "-map", "0:a", "-c:v", "copy",
                     "-af", filter, "-c:a", "aac", "-b:a", "192k", "-ar", AUDIO_RATE.to_s, "-movflags", "+faststart", output])
    true
  rescue JSON::ParserError
    warn_about("loudness_skipped")
    false
  end

  # 第 6 步：写到输出目录，再镜像进媒体根供页面播放。
  def publish(drama, episode, final)
    folder = File.join(@output_directory, safe_segment(drama["title"], "短剧"))
    FileUtils.mkdir_p(folder)
    title = safe_segment(episode["title"], "")
    name = format("第%02d集", episode["order"].to_i) + (title.empty? ? "" : "-#{title}") + ".mp4"
    destination = File.join(folder, name)
    temporary = "#{destination}.#{@job['id'][0, 8]}.tmp"
    FileUtils.cp(final, temporary)
    File.rename(temporary, destination)
    info = FFmpegTool.probe(destination, env: @env) || {}
    fields = { "outputPath" => destination, "durationMs" => info["durationMs"] }
    if @media_mirror
      mirrored = @media_mirror.publish(id: "episode-#{@job['id']}", path: destination)
      fields.merge!(mirrored.select { |key, _| %w[mediaFile mediaPath playbackURL posterFile posterURL].include?(key) })
    end
    fields
  ensure
    File.delete(temporary) if temporary && File.exist?(temporary)
  end

  # 文件夹 / 文件名里去掉路径分隔符与 macOS 访达不认的冒号，长度截到 60 字。
  def safe_segment(value, fallback)
    text = value.to_s.gsub(%r{[/\\:*?"<>|\u0000-\u001f]}, " ").strip.gsub(/\s+/, " ")
    text = text[0, 60].strip
    text.empty? ? fallback : text
  end
end
