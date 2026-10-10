# frozen_string_literal: true

require "time"
require_relative "ffmpeg_tool"

# 镜头成片的裁剪（入点 / 出点，0.39.0-rc1，docs/decisions/0007-clip-trim.md）。
#
# 起因：大量 ref2va / fl2va 片段「前半段对、后半段跑偏」——H3 在片段中途自己剪一刀，跳到分镜没写的机位或场景。
# 画面质检判 scene_jump，补救只会重拍，一次重拍要排队 15 分钟到几小时、按条计费。可跑偏点之前的那一段
# 往往完全能用：裁到切点之前就行。
#
# 存法：裁剪挂在镜头上、只对一条成片有效——
#   shot["trim"] = { "jobID", "inSeconds"?, "outSeconds"?, "source" => user | auto, "reason"?, "at",
#                    "issueTime"?（自动裁剪依据的切点）, "excludes"?（裁掉的问题类别，目前只有 scene_jump） }
# 换选另一条成片时作废（DramaService#select_video 删掉它）。合成计划只在 trim.jobID 就是这一镜要用的那条时生效。
#
# 质检口径：画面质检结论本身不改（job.reviews.frames 原样），闸门与补救看的是「生效结论」——
# effective_review 把落在裁掉区间里的 scene_jump 问题拿掉、按剩下的问题重算状态。只认 scene_jump：
# 它是一个时间点上的事件；人物错位、叠字这类问题给的时间点只是看到的那一格，不能说明裁掉之后就没有了。
module ClipTrim
  module_function

  SOURCES = %w[user auto].freeze
  # 自动裁剪的设置（video.settings）：裁完至少还剩多长、切点前后让出多少。
  DEFAULT_MIN_SECONDS = 1.5
  DEFAULT_MARGIN = 0.15
  MIN_SECONDS_RANGE = (0.5..10.0).freeze
  MARGIN_RANGE = (0.0..1.0).freeze
  # 手动裁剪最短留多长：再短的片段合成出来只是一闪。
  MANUAL_MIN_SECONDS = 0.5
  # 裁剪只解决 scene_jump（见上）。
  CUT_CATEGORIES = %w[scene_jump].freeze
  # 经验库里裁剪这一招的 ID（qa-lessons.json 的 remediationID / lessonID）。
  REMEDIATION_ID = "trim_before_cut"
  LESSON_TEXT = "Trim the clip just before the unplanned cut instead of regenerating it."
  REASON_LIMIT = 300
  # 浮点比较的余量：质检时间读到 0.1 秒。
  EPSILON = 0.001

  NUMBER = '(\d+(?:\.\d+)?)'
  # 单位：s / sec / secs / second(s) / 秒。英文单位后面不能紧跟字母（"2 shots" 不算）。
  UNIT = '\s*(?:秒|s(?:ec(?:ond)?s?)?(?![a-z]))'
  RANGE = /#{NUMBER}\s*(?:#{UNIT})?\s*(?:-|–|—|~|～|至|到|to)\s*#{NUMBER}#{UNIT}/i.freeze
  SINGLE = /#{NUMBER}#{UNIT}/i.freeze
  CLOCK = /(?<![\d.:])(\d{1,2}):(\d{2}(?:\.\d+)?)(?![\d:])/.freeze

  # 质检 location 里最早的时间点（秒），读不出返回 nil。认得「约2.0s处」「about 2.3s」「1.0s起」「约2.3秒处」
  # 「0.5s」「1.2–3.4s」「00:02.5」这类写法；区间取起点。
  def parse_time(location)
    text = location.to_s
    return nil if text.strip.empty?

    times = []
    text.scan(RANGE) { |from, _to| times << from.to_f }
    text.scan(SINGLE) { |value| times << value[0].to_f }
    text.scan(CLOCK) { |minutes, seconds| times << minutes.to_i * 60 + seconds.to_f }
    times.min
  end

  def issue_time(issue)
    return nil unless issue.is_a?(Hash)

    parse_time(issue["location"]) || parse_time(issue["reason"])
  end

  def cut_issue?(issue)
    issue.is_a?(Hash) && CUT_CATEGORIES.include?(issue["category"].to_s)
  end

  # 设置里的两个数，夹到合法范围。
  def min_seconds(settings)
    clamp_float(settings && settings["trimMinSeconds"], DEFAULT_MIN_SECONDS, MIN_SECONDS_RANGE)
  end

  def margin(settings)
    clamp_float(settings && settings["trimMargin"], DEFAULT_MARGIN, MARGIN_RANGE)
  end

  def clamp_float(value, fallback, range)
    number = Float(value)
    number.finite? ? number.clamp(range.begin, range.end) : fallback
  rescue ArgumentError, TypeError
    fallback
  end

  # 这一镜对这条成片有效的裁剪；没有或属于别的成片返回 nil。
  def for_job(shot, job_id)
    trim = shot.is_a?(Hash) ? shot["trim"] : nil
    return nil unless trim.is_a?(Hash) && !job_id.to_s.empty? && trim["jobID"].to_s == job_id.to_s

    trim
  end

  def in_seconds(trim)
    value = trim && trim["inSeconds"]
    value.is_a?(Numeric) && value.positive? ? value.to_f : 0.0
  end

  def out_seconds(trim)
    value = trim && trim["outSeconds"]
    value.is_a?(Numeric) && value.positive? ? value.to_f : nil
  end

  # 裁剪后的长度（毫秒）。clip_ms 是原片长度（读不出传 nil）；没有出点又不知道原片多长时返回 nil。
  def kept_ms(trim, clip_ms)
    from = in_seconds(trim)
    to = out_seconds(trim)
    to = [to, clip_ms / 1000.0].min if to && clip_ms.to_i.positive?
    to ||= clip_ms.to_i.positive? ? clip_ms / 1000.0 : nil
    return nil unless to

    [((to - from) * 1000).round, 0].max
  end

  # 落在裁掉区间里的 scene_jump：出点之后（含出点），或入点之前（含入点）。
  def excluded?(issue, trim)
    return false unless trim && cut_issue?(issue)

    time = issue_time(issue)
    return false unless time

    to = out_seconds(trim)
    from = in_seconds(trim)
    (to && time >= to - EPSILON) || (from.positive? && time <= from + EPSILON)
  end

  # 生效结论：裁剪拿掉的问题不算，状态按剩下的问题重算（有 block 级问题为 block，没问题为 pass，否则 warn）。
  # 没有裁剪或什么也没拿掉时原样返回同一个对象。拿掉的问题放在 trimExcluded，供页面与 Agent 看。
  def effective_review(review, trim)
    return review unless review.is_a?(Hash) && trim.is_a?(Hash)

    issues = Array(review["issues"])
    excluded = issues.select { |issue| excluded?(issue, trim) }
    return review if excluded.empty?

    kept = issues - excluded
    status = if kept.any? { |issue| issue.is_a?(Hash) && issue["severity"] == "block" } then "block"
             elsif kept.empty? then "pass"
             else "warn"
             end
    review.merge("status" => status, "issues" => kept, "trimExcluded" => excluded, "verdictStatus" => review["status"])
  end

  # 这一镜这条成片的生效结论（job.reviews.frames + 镜头上的裁剪）。
  def effective_for(shot, job)
    review = job && job.dig("reviews", "frames")
    effective_review(review, for_job(shot, job && job["id"]))
  end

  # ---- 自动裁剪 ----

  # 按画面质检结论给一条成片找一个裁剪。dialogue_ms：这一镜台词要占的长度（毫秒；nil 表示有台词但算不出）；
  # lip_synced：台词音轨从片段第 0 毫秒起、口型照它对（ref2va 对白音轨），这时裁掉片头会丢台词。
  # 返回 {"ok" => true, "trim" => {...}} 或 {"ok" => false, "reason" => 原因码}。
  #
  # - 切点 t = 结论里 scene_jump 问题最早的时间点；任何一条 scene_jump 读不出时间就不裁（no_cut_time）。
  # - 切点要在片段里面（t + 余量 < 原片长度），否则说明时间点读错了或切在最后一帧，不裁（cut_outside_clip）。
  # - t − 余量 ≥ 最短长度：出点 = t − 余量；台词要在出点之前说完（dialogue_past_cut / dialogue_unknown）。
  # - 否则（切在片头）：入点 = t + 余量；入点之后若还有切点，出点 = 下一个切点 − 余量；剩下的要 ≥ 最短长度（too_short），
  #   口型对齐的台词音轨不裁片头（dialogue_in_head），其余台词要放得下（dialogue_past_cut）。
  def plan_auto(review, clip_ms:, dialogue_ms:, lip_synced: false, min_seconds: DEFAULT_MIN_SECONDS, margin: DEFAULT_MARGIN, reason: nil)
    cuts = Array(review && review["issues"]).select { |issue| cut_issue?(issue) }
    return refuse("no_cut_issue") if cuts.empty?

    times = cuts.map { |issue| issue_time(issue) }
    return refuse("no_cut_time") if times.any?(&:nil?)
    return refuse("clip_length_unknown") unless clip_ms.to_i.positive?

    clip = clip_ms / 1000.0
    first = times.min
    return refuse("cut_outside_clip") unless first + margin < clip - EPSILON

    out = round2(first - margin)
    if out >= min_seconds - EPSILON
      return refuse("dialogue_unknown") if dialogue_ms.nil?
      return refuse("dialogue_past_cut") if dialogue_ms > out * 1000 + 1

      return accept(nil, out, first, reason)
    end

    start = round2(first + margin)
    later = times.select { |time| time > start + EPSILON }.min
    stop = later ? round2(later - margin) : nil
    length = (stop || clip) - start
    return refuse("too_short") if length < min_seconds - EPSILON
    return refuse("dialogue_unknown") if dialogue_ms.nil?
    return refuse("dialogue_in_head") if lip_synced && dialogue_ms.positive?
    return refuse("dialogue_past_cut") if dialogue_ms > length * 1000 + 1

    accept(start, stop, first, reason)
  end

  def accept(from, to, issue_time, reason)
    trim = { "inSeconds" => from, "outSeconds" => to, "source" => "auto", "issueTime" => issue_time, "excludes" => CUT_CATEGORIES.dup,
             "reason" => (reason || default_reason(from, to, issue_time)).to_s[0, REASON_LIMIT] }.reject { |_key, value| value.nil? }
    { "ok" => true, "trim" => trim }
  end

  def default_reason(from, to, time)
    where = format("%.1fs", time)
    return "scene_jump at #{where}: kept #{format('%.2f', from)}s to #{format('%.2f', to)}s" if from && to
    return "scene_jump at #{where}: kept from #{format('%.2f', from)}s" if from

    "scene_jump at #{where}: kept up to #{format('%.2f', to)}s"
  end

  def refuse(reason)
    { "ok" => false, "reason" => reason }
  end

  def round2(value)
    (value * 100).round / 100.0
  end

  # ---- 手动裁剪（drama.set_clip_trim） ----

  # 校验 inSeconds / outSeconds（都可省，省了就是从头 / 到尾），返回 [trim, nil] 或 [nil, 错误]。
  # clip_ms：原片长度（读不出传 nil，不查超长）。
  def normalize_manual(arguments, clip_ms)
    from = seconds_argument(arguments, "inSeconds")
    to = seconds_argument(arguments, "outSeconds")
    return [nil, error("invalid_trim", "inSeconds and outSeconds must be numbers of seconds (0 or more).")] if from == :invalid || to == :invalid

    from = nil if from && from <= 0
    clip = clip_ms.to_i.positive? ? clip_ms / 1000.0 : nil
    return [nil, error("invalid_trim", "Pass inSeconds, outSeconds or both (or clear: true).")] if from.nil? && to.nil?
    if clip && ((from && from >= clip - EPSILON) || (to && to > clip + EPSILON))
      return [nil, error("trim_out_of_range", format("The clip is %.2f s long; in and out points must fall inside it.", clip))]
    end

    to = nil if to && clip && (to - clip).abs <= EPSILON
    length = (to || clip || Float::INFINITY) - (from || 0)
    if length < MANUAL_MIN_SECONDS - EPSILON
      return [nil, error("trim_too_short", format("Keep at least %.1f s of the clip (outSeconds must be after inSeconds).", MANUAL_MIN_SECONDS))]
    end

    reason = arguments["reason"].to_s.strip[0, REASON_LIMIT]
    trim = { "inSeconds" => from && round2(from), "outSeconds" => to && round2(to), "source" => "user", "reason" => reason.empty? ? nil : reason }
    [trim.reject { |_key, value| value.nil? }, nil]
  end

  def seconds_argument(arguments, key)
    return nil unless arguments.key?(key) && !arguments[key].nil?

    number = Float(arguments[key])
    number.finite? && number >= 0 ? number : :invalid
  rescue ArgumentError, TypeError
    :invalid
  end

  def error(code, message)
    { "code" => code, "message" => message }
  end

  # drama.set_clip_trim：给这一镜现在要用的那条成片（选定的，没选就是最新完成的，与合成计划同一口径）
  # 设入点 / 出点，或 clear: true 清掉。jobID 可省；给了必须就是那一条（trim_not_selected：先 drama.select_video）。
  # dramas：DramaService；jobs：VideoService#jobs_with_media 的结果。
  def apply(dramas, jobs, arguments, env: ENV)
    loaded = dramas.get("id" => arguments["dramaID"])
    return loaded unless loaded["ok"]

    episode = Array(loaded["drama"]["episodes"]).find { |entry| entry["id"] == arguments["episodeID"].to_s }
    return failure("episode_not_found", "Episode was not found.") unless episode

    shot = Array(episode["shots"]).find { |entry| entry["id"] == arguments["shotID"].to_s }
    return failure("shot_not_found", "Storyboard shot was not found.") unless shot

    current = current_clip(shot, jobs)
    return failure("no_completed_video", "This shot has no completed clip to trim yet.") unless current

    wanted = arguments["jobID"].to_s
    unless wanted.empty? || wanted == current["id"]
      return failure("trim_not_selected", "A trim applies to the clip the shot uses (#{current['id']}). Select clip #{wanted} with drama.select_video first, then trim it.")
    end

    length = clip_ms(current, env: env)
    if arguments["clear"] == true
      saved = dramas.set_clip_trim(arguments, job_id: current["id"], trim: nil)
      return saved unless saved["ok"]

      return { "ok" => true, "shotID" => shot["id"], "jobID" => current["id"], "trim" => nil, "clipDurationMs" => length, "keptDurationMs" => length }
    end

    trim, problem = normalize_manual(arguments, length)
    return { "ok" => false, "error" => problem } if problem

    saved = dramas.set_clip_trim(arguments, job_id: current["id"], trim: trim)
    return saved unless saved["ok"]

    stored = for_job(find_shot(saved["drama"], arguments), current["id"])
    { "ok" => true, "shotID" => shot["id"], "jobID" => current["id"], "trim" => stored, "clipDurationMs" => length, "keptDurationMs" => kept_ms(stored, length),
      "framesQA" => framesqa_summary(current, stored) }.reject { |_key, value| value.nil? }
  end

  # 选定的成片，没有就取最新完成、有文件的（与 EpisodePlan.pick_video 同一口径）。
  def current_clip(shot, jobs)
    usable = Array(jobs).select { |job| job["shotID"] == shot["id"] && job["state"] == "completed" && File.file?(job["outputPath"].to_s) }
    usable.find { |job| job["id"] == shot["selectedVideoID"].to_s } || usable.max_by { |job| [job["createdAt"].to_s, job["updatedAt"].to_s] }
  end

  def find_shot(drama, arguments)
    episode = Array(drama && drama["episodes"]).find { |entry| entry["id"] == arguments["episodeID"].to_s }
    episode && Array(episode["shots"]).find { |entry| entry["id"] == arguments["shotID"].to_s }
  end

  # 裁剪之后这条成片的画面质检口径：原结论与生效结论（裁掉的 scene_jump 不算）。
  def framesqa_summary(job, trim)
    review = job.dig("reviews", "frames")
    return nil unless review.is_a?(Hash)

    effective = effective_review(review, trim)
    { "verdictStatus" => review["status"], "effectiveStatus" => effective["status"], "excluded" => Array(effective["trimExcluded"]).length }
  end

  def failure(code, message)
    { "ok" => false, "error" => error(code, message) }
  end

  # 原片长度（毫秒）；读不出返回 nil。
  def clip_ms(job, env: ENV)
    path = job && job["outputPath"].to_s
    return nil if path.nil? || path.empty? || !File.file?(path)

    info = FFmpegTool.probe(path, env: env)
    value = info && info["durationMs"].to_i
    value && value.positive? ? value : nil
  rescue StandardError
    nil
  end
end
