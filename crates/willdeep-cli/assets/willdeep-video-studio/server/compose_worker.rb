# frozen_string_literal: true

# 分集成片的后台进程（设计稿 docs/design/episode-compose.md 第 1 节）。
#
# 由 EpisodeComposer#start 以独立进程组拉起：ruby compose_worker.rb <spec.json>。
# spec 里只有路径与任务 ID；TTS / 音乐的密钥经环境变量从 MCP 主进程继承，不落盘。
# 宿主模式（macOS / Web）由主进程经 VIDEO_STUDIO_HOST_MODE 显式传入。

require "json"
require "time"

require_relative "lib/compose_pipeline"
require_relative "lib/compose_store"
require_relative "lib/drama_service"
require_relative "lib/drama_store"
require_relative "lib/history_store"
require_relative "lib/media_host"
require_relative "lib/media_mirror"
require_relative "lib/media_ref"
require_relative "lib/tts_backend"
require_relative "lib/video_store"
require_relative "lib/voice_generation"
require_relative "lib/music_backend"

spec_path = ARGV.first.to_s
spec = JSON.parse(File.read(spec_path, encoding: "UTF-8"))
store = ComposeStore.new(spec.fetch("composeStore"))
job_id = spec.fetch("jobID")

# 取消时主进程向整个进程组发 TERM：ffmpeg 子进程一起结束，任务状态已由主进程写好。
Signal.trap("TERM") { exit!(143) }

begin
  job = store.find_job(job_id)
  raise "compose job #{job_id} is missing" unless job
  exit(0) unless ComposeStore::ACTIVE_STATES.include?(job["state"])

  store.update_job(job_id, "state" => "running", "startedAt" => Time.now.utc.iso8601, "pid" => Process.pid)
  media_host = MediaHost.new(desktop_root: spec.fetch("desktopMediaRoot"))
  MediaRef.configure(media_host)
  dramas = DramaService.new(store: DramaStore.new(spec.fetch("dramaStore")), media_root: media_host,
                            history: HistoryStore.new(spec.fetch("historyDirectory")))
  video_store = VideoStore.new(spec.fetch("videoStore"))
  # 设置填错（非法 provider / 地址）时 select 会抛 *_misconfigured：当作没有配置，
  # 流水线记 tts_unavailable / music_unavailable 警告，其余步骤照常。
  tts_ready = begin
    !TTSBackend.select(media_root: dramas.media_root).nil?
  rescue TTSBackend::Error => error
    warn "compose worker: TTS unavailable (#{error.code})"
    false
  end
  voice = tts_ready ? VoiceGeneration.new(drama_service: dramas) : nil
  music = begin
    MusicBackend.select(env: ENV, media_root: dramas.media_root)
  rescue MusicBackend::Error => error
    warn "compose worker: music backend unavailable (#{error.code})"
    nil
  end
  pipeline = ComposePipeline.new(
    store: store,
    dramas: dramas,
    video_jobs: -> { video_store.jobs.select { |entry| entry["deletedAt"].to_s.empty? } },
    media_mirror: MediaMirror.new(host: media_host),
    output_directory: spec.fetch("outputDirectory"),
    voice: voice,
    music: music
  )
  pipeline.run(job)
rescue ComposePipeline::Failed, FFmpegTool::Failed => error
  store.update_job(job_id, "state" => "failed", "finishedAt" => Time.now.utc.iso8601,
                           "error" => { "code" => error.code, "message" => error.message })
  exit(1)
rescue StandardError, ScriptError => error
  warn "compose worker failed: #{error.class}: #{error.message}\n#{Array(error.backtrace).first(8).join("\n")}"
  store.update_job(job_id, "state" => "failed", "finishedAt" => Time.now.utc.iso8601,
                           "error" => { "code" => "compose_failed", "message" => "#{error.class}: #{error.message}"[0, 500] })
  exit(1)
ensure
  File.delete(spec_path) if File.file?(spec_path)
end
