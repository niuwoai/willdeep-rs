# frozen_string_literal: true

require "time"
require_relative "clip_trim"

# 镜头选定成片的规则（0.38.0-rc3，docs/decisions/0006-batch-take-selection.md）。
#
# 《回村养鸭》第 1 集：`episode.generate_videos force: true` 为换掉造型错的旧片重新生成了 11 镜，
# 质检通过（或没有可重做的问题）的 7 镜仍选着上一轮的旧片——补救只在重拍时才选片，合成于是用了旧片。
#
# 规则：
# - 自动流程（按集批量、补救）为一镜产出了新成片，就在它就绪后（开着质检时在质检之后，按补救的排序
#   从本次产出的几条里取最好的）设为选定。
# - 例外（不换，原因记在逐项的 selectionReason / selectionKept）：
#   - newer_selection：批量开始之后有人另选了一条（选定时间不早于批量开始、选的又不是本次产出的那几条）；
#   - user_qa_override：当前选定带人工放行（qaOverride，by 不是 auto），新片没有通过画面质检；
#   - worse_qa：新片的画面质检比当前选定差（pass > warn > block > 没有结论）。
# - 每次选定都记 `selectedVideoAt`（毫秒）与 `selectedVideoBy`（user / batch / remediation）。
# - 合成计划与进度检查「选定的成片比批量为这一镜新产出的成片旧、且选定发生在新成片出现之前」，
#   给出 stale_selection 告警，不再悄悄用旧片。
module VideoSelection
  SOURCES = %w[user batch remediation].freeze
  # episode.generate_videos 每镜的 requestID 以 `:video:<镜头 ID>` 结尾（EpisodeBatch#generate_one）。
  BATCH_REQUEST_MARK = ":video:"
  # 画面质检结论的好坏（数小的好）；没有结论（没审、审失败、结论过期）最差。
  QA_RANK = { "pass" => 0, "warn" => 1, "block" => 2 }.freeze
  UNKNOWN_RANK = 3
  # 自动放行（remediateAutoOverride）记 auto；其余（user / agent）是人或 Agent 明确做的决定，批量不随便换掉。
  AUTOMATIC_OVERRIDE = "auto"

  module_function

  def now_iso
    Time.now.utc.iso8601(3)
  end

  def parse(value)
    text = value.to_s
    text.empty? ? nil : Time.iso8601(text)
  rescue ArgumentError
    nil
  end

  # 批量生成的成片（不含补救重拍：重拍挂在批量成片的补救链上，链头才是「新产出」）。
  def batch_take?(job, shot_id)
    job["requestID"].to_s.end_with?("#{BATCH_REQUEST_MARK}#{shot_id}")
  end

  def usable?(job)
    job["state"] == "completed" && job["deletedAt"].to_s.empty? && File.file?(job["outputPath"].to_s)
  end

  # 批量开始（since）之后，这一镜的选定被换成了本次产出（own_ids）以外的一条：以那次选择为准。
  def newer_choice?(shot, since, own_ids)
    since_time = since.is_a?(Time) ? since : parse(since)
    return false unless since_time

    selected = shot["selectedVideoID"].to_s
    return false if selected.empty? || Array(own_ids).include?(selected)

    at = parse(shot["selectedVideoAt"])
    !at.nil? && at >= since_time
  end

  # 选定的成片比批量为这一镜新产出的成片旧，且选定发生在新成片出现之前（或没有记选定时间的旧数据）。
  # 返回告警明细，没有问题返回 nil。
  def stale(shot, jobs)
    selected_id = shot["selectedVideoID"].to_s
    return nil if selected_id.empty?

    usable = Array(jobs).select { |job| job["shotID"] == shot["id"] && usable?(job) }
    selected = usable.find { |job| job["id"] == selected_id }
    return nil unless selected

    selected_at = parse(selected["createdAt"])
    newer = usable.select do |job|
      created = parse(job["createdAt"])
      job["id"] != selected_id && batch_take?(job, shot["id"]) && created && selected_at && created > selected_at
    end.max_by { |job| parse(job["createdAt"]) }
    return nil unless newer

    chosen_at = parse(shot["selectedVideoAt"])
    return nil if chosen_at && chosen_at >= parse(newer["createdAt"])

    detail = { "selectedJobID" => selected_id, "selectedCreatedAt" => selected["createdAt"],
               "newerJobID" => newer["id"], "newerCreatedAt" => newer["createdAt"] }
    review = newer.dig("reviews", "frames")
    detail["newerFramesStatus"] = review["status"] if review.is_a?(Hash)
    detail["selectedAt"] = shot["selectedVideoAt"] if shot["selectedVideoAt"]
    detail
  end

  # 台词重配之后，照旧配音对口型生成的成片就过期了（0.40.0-rc1）。返回 {jobID, reason, lines?} 或 nil。
  # - 任务记了 dialogueAudio（每句用的配音候选）：这一镜现在选定的配音与之不同（换了、少了、多了）即过期；
  # - 0.40 之前的任务没记：照对白音轨生成（syntheticVoice）的，若现在选定的某句配音比成片创建得晚，也算过期。
  # 不是照对白音轨生成的成片（fl2va / t2va）口型本来就不跟配音，不算。
  def dialogue_stale(shot, job)
    return nil unless shot.is_a?(Hash) && job.is_a?(Hash)

    current = {}
    created = {}
    Array(shot["dialogue"]).each do |line|
      next unless line.is_a?(Hash) && line["audio"].is_a?(Hash)

      id = line["audio"]["selectedCandidateID"].to_s
      next if id.empty?

      current[line["id"].to_s] = id
      candidate = Array(line["audio"]["candidates"]).find { |entry| entry["id"] == id }
      created[line["id"].to_s] = candidate && candidate["createdAt"]
    end
    used = job["dialogueAudio"]
    if used.is_a?(Array)
      recorded = used.each_with_object({}) { |entry, map| map[entry["lineID"].to_s] = entry["candidateID"].to_s if entry.is_a?(Hash) }
      changed = (recorded.keys | current.keys).select { |line_id| recorded[line_id] != current[line_id] }
      return changed.empty? ? nil : { "jobID" => job["id"], "reason" => "dialogue_audio_changed", "lines" => changed }
    end
    return nil unless job["syntheticVoice"] == true

    job_time = parse(job["createdAt"])
    return nil unless job_time

    newer = created.select { |_line_id, at| (time = parse(at)) && time > job_time }.keys
    newer.empty? ? nil : { "jobID" => job["id"], "reason" => "dialogue_redubbed_after_clip", "lines" => newer }
  end

  # 这一镜各条成片里口型已过期的那些（成片 ID 的列表）。
  def dialogue_stale_ids(jobs, shot)
    return [] unless shot.is_a?(Hash)

    Array(jobs).select { |job| job["shotID"] == shot["id"] && dialogue_stale(shot, job) }.map { |job| job["id"] }
  end

  # 按存档里这一镜现在的台词配音算（选片前在锁外调用）。
  def stale_ids_for(dramas, jobs, drama_id, episode_id, shot_id)
    loaded = dramas.get("id" => drama_id)
    return [] unless loaded["ok"]

    episode = Array(loaded["drama"]["episodes"]).find { |entry| entry["id"] == episode_id.to_s }
    shot = episode && Array(episode["shots"]).find { |entry| entry["id"] == shot_id.to_s }
    dialogue_stale_ids(jobs, shot)
  end

  # 批量逐项的选片字段：计划时记 previousSelectedJobID，结束时写 selectedJobID 与 selectionChanged。
  def plan_fields(shot)
    current = shot["selectedVideoID"].to_s
    value = current.empty? ? nil : current
    { "previousSelectedJobID" => value, "selectedJobID" => value, "selectionChanged" => false }
  end

  def item_fields(previous, shot)
    current = shot["selectedVideoID"].to_s
    changed = previous.to_s != current
    fields = { "selectedJobID" => current.empty? ? nil : current, "selectionChanged" => changed }
    fields["selectedBy"] = shot["selectedVideoBy"] if changed && shot["selectedVideoBy"]
    fields
  end

  # 批量开始的时间（后台任务建立的时间）：之后有人另选的成片，自动选片不覆盖。
  def batch_since(context)
    parse(context.respond_to?(:started_at) ? context.started_at : nil) || Time.now.utc
  end

  # 批量结束时给每一项（含跳过、取消、失败的）写明这一镜现在选定的是哪条、这次有没有换。
  # 比较的基准是计划时记下的 previousSelectedJobID。
  # jobs（0.40.0-rc1）：给了就检查计划时的选定是不是口型已过期（台词重配过），是则逐项写 dialogueStale。
  def annotate(context, dramas, drama_id, episode_id, jobs = nil)
    loaded = dramas.get("id" => drama_id)
    return unless loaded["ok"]

    episode = Array(loaded["drama"]["episodes"]).find { |entry| entry["id"] == episode_id.to_s }
    shots = episode ? Array(episode["shots"]) : []
    context.items.each_with_index do |item, index|
      shot = shots.find { |entry| entry["id"] == item["shotID"] }
      next unless shot

      fields = item_fields(item["previousSelectedJobID"], shot)
      previous = jobs && Array(jobs).find { |job| job["id"] == item["previousSelectedJobID"].to_s }
      stale = previous && dialogue_stale(shot, previous)
      fields["dialogueStale"] = stale if stale
      # 选片时写成功了（selected），批量结束时镜头却又回到了计划时的那条：别处（另一个进程、人、Agent）在这之后又选回了旧片。
      # 不再照抄「selected」，写明被改回了，免得读结果的人以为选上了（0.40.0-rc1，回村养鸭 duck-e1-revideo-v1）。
      if item["selectionReason"] == "selected" && !fields["selectionChanged"] && !item["previousSelectedJobID"].to_s.empty?
        fields["selectionReason"] = "selection_reverted"
        fields["selectionNote"] = "The batch selected its new take, but the shot is back on #{item['previousSelectedJobID']} " \
                                  "(selected by #{shot['selectedVideoBy'] || 'unknown'} at #{shot['selectedVideoAt'] || 'unknown time'})."
      end
      # 没走到选片的项（跳过、取消、失败、只有补救结果）也写明原因。
      fields["selectionReason"] = default_reason(item, fields) if item["selectionReason"].to_s.empty?
      context.update_item(index, fields)
    end
  end

  def default_reason(item, fields)
    nested = item["remediation"].is_a?(Hash) ? item["remediation"]["selectionReason"].to_s : ""
    return nested unless nested.empty?
    return "not_attempted" unless fields["selectionChanged"]

    fields["selectedBy"] == "user" ? "changed_by_user" : "selected"
  end

  # 自动流程选片：已经是这条就不写（不刷新选定时间）；批量开始后有人另选了就不动。
  # 返回 {"changed", "selectedJobID", "kept"（以人的选择为准时）, "error"}。
  #
  # status：这条新片的画面质检结论（pass / warn / block；没有结论传 nil）。statuses：这一镜各条成片的
  # 结论（成片 ID → 结论，statuses_for 算），和当前选定比用；在 dramas.json 的锁外先算好。
  # 返回的 reason：selected / already_selected，或不换的原因（同时放在 kept 里）。
  # trim：自动裁剪（0.39.0-rc1）与选定一起写入；带裁剪时即使已经选着这条也要写。
  # stale_ids（0.40.0-rc1）：这一镜口型已过期的成片（dialogue_stale_ids，锁外算好）。当前选定在其中时，
  # 新片不和它比质检、也不让给人工放行——它对的是旧配音，留着只会口型错位；批量开始后人另选的仍以人为准。
  def adopt(dramas, drama_id:, episode_id:, shot_id:, job_id:, by:, status: nil, statuses: {}, since: nil, own_ids: [], extra: {}, trim: nil, stale_ids: [])
    own = Array(own_ids) | [job_id]
    compared = nil
    replaced_stale = nil
    gate = lambda do |shot|
      next "newer_selection" if since && newer_choice?(shot, since, own)

      current = shot["selectedVideoID"].to_s
      next nil if current.empty? || own.include?(current)

      compared = { "currentJobID" => current, "currentStatus" => statuses[current], "candidateStatus" => status }
      if Array(stale_ids).include?(current)
        replaced_stale = current
        next nil
      end
      next "user_qa_override" if human_override?(shot, current) && status != "pass"
      next "worse_qa" if rank(status) > rank(statuses[current])

      nil
    end
    arguments = { "dramaID" => drama_id, "episodeID" => episode_id, "shotID" => shot_id, "videoID" => job_id }.merge(extra)
    selected = dramas.select_video(arguments, by: by, gate: gate, skip_if_same: extra.empty? && trim.nil?, trim: trim)
    if selected["ok"]
      result = { "changed" => selected["changed"] == true, "selectedJobID" => job_id,
                 "reason" => selected["changed"] == true ? "selected" : "already_selected" }
      result["replacedStaleDialogue"] = replaced_stale if replaced_stale && selected["changed"] == true
      return result
    end

    current = selected["selectedVideoID"]
    if selected.dig("error", "code") == "selection_kept"
      kept = { "changed" => false, "selectedJobID" => current, "kept" => selected["reason"], "reason" => selected["reason"] }
      kept["compared"] = compared if compared
      return kept
    end

    { "changed" => false, "selectedJobID" => current, "reason" => "select_failed", "error" => selected["error"] }
  end

  def rank(status)
    QA_RANK.fetch(status.to_s, UNKNOWN_RANK)
  end

  # 当前选定带人或 Agent 的放行（不是 remediateAutoOverride 自动放行的）。
  def human_override?(shot, job_id)
    override = shot["qaOverride"]
    override.is_a?(Hash) && override["jobID"].to_s == job_id && override["by"].to_s != AUTOMATIC_OVERRIDE
  end

  # 一条成片现在的画面质检结论；basis 给了就只认指纹对得上的（过期的算没有结论）。
  # trim：镜头上的裁剪（0.39.0-rc1）；挂在这条成片上时按生效结论算（裁掉的 scene_jump 不算）。
  def qa_status(job, basis = nil, trim = nil)
    review = job && job.dig("reviews", "frames")
    return nil unless review.is_a?(Hash) && QA_RANK.key?(review["status"].to_s)
    return nil if basis && review["basis"] != basis.call(job)

    trim = nil unless trim.is_a?(Hash) && trim["jobID"].to_s == job["id"].to_s
    ClipTrim.effective_review(review, trim)["status"]
  end

  def statuses_for(jobs, shot_id, basis = nil, trim = nil)
    Array(jobs).each_with_object({}) do |job, map|
      map[job["id"]] = qa_status(job, basis, trim) if job["shotID"] == shot_id
    end
  end

  # 镜头当前的裁剪（读不到为 nil），给 statuses_for 用。
  def shot_trim(dramas, drama_id, episode_id, shot_id)
    loaded = dramas.get("id" => drama_id)
    return nil unless loaded["ok"]

    episode = Array(loaded["drama"]["episodes"]).find { |entry| entry["id"] == episode_id.to_s }
    shot = episode && Array(episode["shots"]).find { |entry| entry["id"] == shot_id.to_s }
    shot && shot["trim"].is_a?(Hash) ? shot["trim"] : nil
  end
end
