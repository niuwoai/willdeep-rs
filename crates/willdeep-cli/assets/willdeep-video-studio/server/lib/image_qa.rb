# frozen_string_literal: true

require "json"
require "time"
require_relative "host_bridge"
require_relative "review_material"
require_relative "image_remediation"
require_relative "image_generation"
require_relative "drama_service"
require_relative "video_store"
require_relative "video_selection"

# 候选图质检与推荐（0.40.0-rc1，复盘 docs/lessons/2026-10-02-回村养鸭-生产复盘.md 4.2，
# 决策 docs/decisions/0008-candidate-image-qa.md）。
#
# 《回村养鸭》第 1 集首帧：女主被画成男人、类品牌皮带扣、真实招牌、繁体错字、横图，全靠人在拼图里挑。
# 现在每张新出的候选（首尾帧、定妆图、造型 / 场景 / 场景变体 / 道具参考图）都过一次轻量的多模态质检：
#
# - 材料：这张候选 + 出图时发出去的参考图（按参考图说明的顺序，图 N 就是第 N 张附件，候选放最后）
#   + 出图提示词（含参考图说明）+ 镜头要求（运镜、动作、画幅）。审核模型与 review.run 同一条路
#   （ReviewRunner#ask_model：宿主 willdeep/ai/complete、同一套审核模型路由）。技能 skills/creative/image-qa.md。
# - 结论写在候选上：candidate.qa {status pass|warn|block, score 0–100, issues[{category, level, detail}], summary,
#   basis, model, rubric, checkedAt}。basis 是出图材料的指纹（提示词、参照图、镜头要求、尺寸、清单版本）；
#   分镜提示词或参考包一变，存着的结论读出时标 stale，不再参与推荐。
# - 推荐：每镜（首 / 尾帧分开）、每个角色、每个资产，在结论新鲜、且不是 block 的候选里按（状态，分数）取最好的，
#   同分取靠前的。读出时算（annotate），不落库。全被拦截时没有推荐。
# - 画幅不符（aspectMismatch，0.35.0-rc2）的候选不问模型，直接记 block（aspect）。
# - 质检失败（宿主拒收、模型没给出合法结论）一律当「不知道」：重试一次，仍失败就不记结论、不当通过。
#
# 花钱的地方都占进程级的质检名额（ShotPipelines 的 qa 限额，video.settings qaConcurrency），与成片质检共用。
class ImageQA
  RUBRIC_VERSION = "image-qa-v1"
  FENCE = "candidate_image_qa"
  SKILL_ID = "image-qa"
  STATUSES = %w[pass warn block].freeze
  CATEGORIES = %w[identity wardrobe text brand composition aspect other].freeze
  LEVELS = %w[must advice].freeze
  STATUS_RANK = { "pass" => 0, "warn" => 1, "block" => 2 }.freeze
  UNKNOWN_RANK = 3
  DEFAULT_SCORES = { "pass" => 90, "warn" => 60, "block" => 20 }.freeze
  TARGETS = ImageGeneration::TARGETS
  FRAME_TARGETS = %w[start end].freeze
  ASSET_TARGETS = ImageGeneration::ASSET_TARGETS
  QA_ATTEMPTS = 2
  MAX_ISSUES = 12
  DETAIL_LIMIT = 300
  SUMMARY_LIMIT = 300
  RAW_EXCERPT = 400
  MAX_CANDIDATES = 8
  REASONS_LIMIT = 6
  STOP_CODES = %w[host_review_unsupported].freeze
  # 读出时挂在镜头 / 角色 / 资产上的推荐字段（只读、不落库）。
  RECOMMENDED_FIELDS = { "start" => "recommendedStartID", "end" => "recommendedEndID" }.freeze
  RECOMMENDATION_FIELDS = { "start" => "startRecommendation", "end" => "endRecommendation" }.freeze
  TARGET_LABELS = {
    "zh-Hans" => { "start" => "首帧", "end" => "尾帧", "character" => "定妆图", "appearance" => "造型参考图", "scene" => "场景参考图",
                   "sceneVariant" => "场景变体参考图", "prop" => "道具参考图" },
    "en" => { "start" => "start frame", "end" => "end frame", "character" => "identity image", "appearance" => "appearance reference",
              "scene" => "scene reference", "sceneVariant" => "scene variant reference", "prop" => "prop reference" }
  }.freeze

  def initialize(dramas:, images:, reviews:, skills:, host:, settings:, limits:, lessons: nil, logger: ->(line) { warn(line) })
    @dramas = dramas
    @images = images
    @reviews = reviews
    @skills = skills
    @host = host
    @settings = settings
    @limits = limits
    @lessons = lessons
    @logger = logger
  end

  # ---- 开关 ----

  def available?
    @host.supports?(HostBridge::AI_COMPLETE)
  end

  # 生效的选项：参数优先，没给就取设置（video.settings imageAutoQA / imageAutoSelect / imageRetryOnBlock）。
  def options(arguments = {})
    settings = @settings.call
    retries = begin
      Integer(arguments.key?("retryOnBlock") ? arguments["retryOnBlock"] : settings["imageRetryOnBlock"])
    rescue ArgumentError, TypeError
      VideoStore::DEFAULT_SETTINGS["imageRetryOnBlock"]
    end
    {
      "autoQA" => arguments.key?("autoQA") ? arguments["autoQA"] != false : settings["imageAutoQA"] != false,
      "autoSelect" => arguments.key?("autoSelect") ? arguments["autoSelect"] != false : settings["imageAutoSelect"] != false,
      "retryOnBlock" => retries.clamp(0, VideoStore::IMAGE_RETRY_ON_BLOCK_LIMIT)
    }
  end

  # 新出的候选要不要自动质检：开着、且宿主能问模型。
  def auto?(arguments = {})
    options(arguments)["autoQA"] && available?
  end

  def self.ids_for(target, arguments)
    case target.to_s
    when "character" then { "characterID" => arguments["characterID"].to_s }
    when "start", "end" then { "episodeID" => arguments["episodeID"].to_s, "shotID" => arguments["shotID"].to_s }
    else { "assetID" => arguments["assetID"].to_s }
    end
  end

  # ---- 推荐（纯函数，页面与进度同一口径） ----

  def self.rank(qa)
    qa.is_a?(Hash) ? STATUS_RANK.fetch(qa["status"].to_s, UNKNOWN_RANK) : UNKNOWN_RANK
  end

  # 可以自动选用：通过，或只剩「建议」的提醒。
  def self.eligible?(qa)
    return false unless qa.is_a?(Hash)
    return true if qa["status"] == "pass"

    qa["status"] == "warn" && Array(qa["issues"]).all? { |issue| issue.is_a?(Hash) && issue["level"] == "advice" }
  end

  # pairs：[[候选 ID, 结论]]，按候选顺序。结论缺失或 block 的不推荐。返回 {candidateID, status, score, eligible} 或 nil。
  def self.best(pairs)
    scored = Array(pairs).each_with_index.map do |(id, qa), index|
      next nil unless qa.is_a?(Hash) && STATUSES.include?(qa["status"].to_s) && qa["status"] != "block"

      [id, qa, index]
    end.compact
    winner = scored.min_by { |_id, qa, index| [rank(qa), -qa["score"].to_i, index] }
    return nil unless winner

    id, qa, = winner
    { "candidateID" => id, "status" => qa["status"], "score" => qa["score"].to_i, "eligible" => eligible?(qa) }
  end

  # 候选列表 + 现在的指纹 → 推荐。指纹为 nil（材料算不出）时没有新鲜结论，也就没有推荐。
  def self.recommend(candidates, basis)
    return nil if basis.nil?

    best(Array(candidates).map { |candidate| [candidate["id"], fresh_qa(candidate, basis)] })
  end

  def self.fresh_qa(candidate, basis)
    qa = candidate.is_a?(Hash) ? candidate["qa"] : nil
    qa.is_a?(Hash) && basis && qa["basis"] == basis ? qa : nil
  end

  # ---- 材料 ----

  # 出图材料与指纹。返回 {ok, plan, basis, context} 或失败。cast_ids：出图时指定的 castIDs（记在结论上）。
  def material(drama, target, ids, cast_ids: nil)
    arguments = ids.merge("target" => target)
    arguments["castIDs"] = cast_ids if cast_ids.is_a?(Array)
    plan = @images.qa_plan(drama, arguments, target)
    return plan unless plan["ok"]

    context = context_for(drama, target, ids)
    return context unless context["ok"]

    references = Array(plan["references"]).map { |path| File.basename(path.to_s) }
    basis = ReviewMaterial.fingerprint([RUBRIC_VERSION, target, plan["prompt"], references.join("|"), context["requirements"], context["size"]].join("\n"))
    { "ok" => true, "plan" => plan, "basis" => basis, "context" => context }
  end

  # ---- 质检 ----

  # 逐张质检这几张候选并把结论写回候选。candidate_ids 为 nil 时取这个目标的全部候选。
  # force 为假时，结论新鲜的不再问模型（reused）。runtime：批量的 ShotPipelines（占它的质检名额、能被取消）；
  # 不给时直接占进程级质检名额。返回 {ok, basis, verdicts {候选 ID => 结论}, failed [], reused [], unknown [], material}；
  # 批量被取消 / 停下时 {ok: false, halted: true, error}。
  def check_candidates(drama_id, target, ids, candidate_ids = nil, runtime: nil, cast_ids: nil, force: false)
    loaded = @dramas.get("id" => drama_id)
    return loaded unless loaded["ok"]

    drama = loaded["drama"]
    built = material(drama, target, ids, cast_ids: cast_ids)
    return built unless built["ok"]

    candidates = candidates_of(drama, target, ids)
    wanted = candidate_ids ? candidates.select { |entry| candidate_ids.include?(entry["id"]) } : candidates
    verdicts = {}
    failed = []
    reused = []
    wanted.each do |candidate|
      fresh = ImageQA.fresh_qa(candidate, built["basis"])
      if fresh && !force
        verdicts[candidate["id"]] = fresh
        reused << candidate["id"]
        next
      end
      verdict = with_slot(runtime) { judge_with_retry(candidate, built, cast_ids) }
      if verdict == :halted
        return { "ok" => false, "halted" => true, "error" => runtime.halt_error || { "code" => "canceled", "message" => "The job was canceled." },
                 "verdicts" => verdicts }
      end
      unless verdict["ok"]
        failed << { "candidateID" => candidate["id"], "error" => compact_error(verdict["error"]) }
        break if STOP_CODES.include?(verdict.dig("error", "code"))

        next
      end
      stored = @dramas.record_candidate_qa(ids.merge("dramaID" => drama_id, "target" => target, "candidateID" => candidate["id"], "qa" => verdict["qa"]))
      unless stored["ok"]
        failed << { "candidateID" => candidate["id"], "error" => compact_error(stored["error"]) }
        next
      end
      verdicts[candidate["id"]] = verdict["qa"]
    end
    { "ok" => true, "basis" => built["basis"], "verdicts" => verdicts, "failed" => failed, "reused" => reused,
      "unknown" => Array(candidate_ids) - candidates.map { |entry| entry["id"] }, "material" => built,
      "order" => candidates.map { |entry| entry["id"] } }
  end

  # image.qa：手动（重新）质检一个目标的候选。返回逐张结论与推荐。
  def run_tool(arguments, runtime: nil)
    target = arguments["target"].to_s
    return failure("invalid_target", "target must be one of #{TARGETS.join(', ')}.") unless TARGETS.include?(target)
    unless available?
      return failure("host_review_unsupported", "Candidate image QA needs the WillDeep host's model bridge (willdeep/ai/complete). Elsewhere, look at the candidates with media.read and pick one with drama.select_image / drama.select_character_image / drama.select_asset_media.")
    end

    wanted = arguments["candidateIDs"]
    if wanted && !(wanted.is_a?(Array) && wanted.length.between?(1, MAX_CANDIDATES))
      return failure("invalid_arguments", "candidateIDs must list 1 to #{MAX_CANDIDATES} candidates.")
    end

    ids = ImageQA.ids_for(target, arguments)
    cast_ids = arguments["castIDs"].is_a?(Array) ? arguments["castIDs"].map(&:to_s) : nil
    checked = check_candidates(arguments["dramaID"], target, ids, wanted && wanted.map(&:to_s), runtime: runtime, cast_ids: cast_ids,
                                                                                               force: arguments["force"] == true)
    return checked unless checked["ok"]
    unless checked["unknown"].empty?
      return failure("candidate_not_found", "These candidates are not on that target: #{checked['unknown'].join(', ')}.")
    end

    view_result(target, checked)
  end

  # image.generate 之后：新出的这几张逐张质检，返回挂在出图结果上的 qa 摘要。
  def after_generate(arguments, result, runtime: nil)
    target = arguments["target"].to_s
    ids = ImageQA.ids_for(target, arguments)
    new_ids = Array(result["generated"]).map { |entry| entry["candidateID"] }.compact
    return nil if new_ids.empty?

    cast_ids = arguments["castIDs"].is_a?(Array) ? arguments["castIDs"].map(&:to_s) : nil
    checked = check_candidates(arguments["dramaID"], target, ids, new_ids, runtime: runtime, cast_ids: cast_ids)
    return { "state" => "failed", "error" => compact_error(checked["error"]) } unless checked["ok"]

    view_result(target, checked).reject { |key, _| key == "ok" }.merge("state" => "completed")
  end

  def view_result(target, checked)
    verdicts = checked["verdicts"]
    loaded_order = checked["order"]
    candidates = loaded_order.select { |id| verdicts.key?(id) }.map do |id|
      qa = verdicts[id]
      { "candidateID" => id, "status" => qa["status"], "score" => qa["score"], "summary" => qa["summary"], "issues" => qa["issues"],
        "reused" => checked["reused"].include?(id) }
    end
    recommendation = recommendation_for(checked)
    result = { "ok" => true, "target" => target, "basis" => checked["basis"], "checked" => candidates, "failed" => checked["failed"],
               "recommendedCandidateID" => recommendation && recommendation["candidateID"], "recommendation" => recommendation }
    result["blockedCount"] = candidates.count { |entry| entry["status"] == "block" }
    result
  end

  # 一个目标现在的推荐（没有任何结论时为 nil，不编译材料）。
  def current_recommendation(drama_id, target, ids)
    loaded = @dramas.get("id" => drama_id)
    return nil unless loaded["ok"]

    candidates = candidates_of(loaded["drama"], target, ids)
    return nil if candidates.none? { |candidate| candidate["qa"].is_a?(Hash) }

    built = material(loaded["drama"], target, ids)
    built["ok"] ? ImageQA.recommend(candidates, built["basis"]) : nil
  end

  # 整个目标（不只本次质检的几张）现在的推荐：重读一遍，按新鲜结论算。
  def recommendation_for(checked)
    loaded = @dramas.get("id" => checked["material"]["context"]["dramaID"])
    return nil unless loaded["ok"]

    context = checked["material"]["context"]
    candidates = candidates_of(loaded["drama"], context["target"], context["ids"])
    ImageQA.recommend(candidates, checked["basis"])
  end

  # ---- 读出时的推荐与过期标记 ----

  # 给读出来的剧（drama.get / drama.list / drama.list_shots / 进度）挂推荐、给候选结论标 stale。只读、不落库。
  # 只为有结论的对象编译材料。出错不影响读出（记一行日志）。
  def annotate(drama)
    return drama unless drama.is_a?(Hash)

    Array(drama["characters"]).each do |character|
      annotate_node(drama, character, character["candidates"], "character", { "characterID" => character["id"] }, "recommendedCandidateID")
    end
    Array(drama["assets"]).each do |asset|
      next unless ASSET_TARGETS.include?(asset["kind"])

      annotate_node(drama, asset, asset["candidates"], asset["kind"], { "assetID" => asset["id"] }, "recommendedCandidateID")
    end
    Array(drama["episodes"]).each do |episode|
      Array(episode["shots"]).each do |shot|
        FRAME_TARGETS.each do |target|
          annotate_node(drama, shot, shot[target == "start" ? "startCandidates" : "endCandidates"], target,
                        { "episodeID" => episode["id"], "shotID" => shot["id"] }, RECOMMENDED_FIELDS[target], RECOMMENDATION_FIELDS[target])
        end
      end
    end
    drama
  end

  # ---- 按集出首尾帧时每镜的质检、重抽与自动选定（episode.generate_frames 调用） ----

  # own_ids：这一镜本次产出的候选。generate.call(补救句, 第几次) 重抽一轮并返回 image.generate 的结果（或 :halted）。
  # update.call(changes) 更新逐项进度。返回这一项最后的字段（state / stage / imageQA / selection…）。
  def run_frame_shot(drama_id:, episode_id:, shot_id:, target:, own_ids:, options:, runtime:, since:, generate:, update:)
    ids = { "episodeID" => episode_id, "shotID" => shot_id }
    checked = check_candidates(drama_id, target, ids, own_ids, runtime: runtime)
    return halted_changes(checked["error"]) if checked["halted"]
    return qa_unavailable(checked["error"]) unless checked["ok"]

    verdicts = checked["verdicts"].dup
    failed = checked["failed"].dup
    material = checked["material"]
    own = own_ids.dup
    retries = []
    attempt = 0
    while attempt < options["retryOnBlock"] && failed.empty? && all_block?(own, verdicts)
      attempt += 1
      issues = own.flat_map { |id| Array(verdicts[id]["issues"]) }.select { |issue| issue["level"] == "must" }
      sentences = ImageRemediation.sentences(issues, material)
      update.call("stage" => "retake", "attempt" => attempt)
      result = generate.call(sentences.map { |entry| entry["text"] }, attempt)
      return halted_changes(runtime.halt_error) if result == :halted
      unless result["ok"]
        retries << { "attempt" => attempt, "sentences" => sentences, "error" => compact_error(result["error"]) }
        break
      end
      new_ids = Array(result["generated"]).map { |entry| entry["candidateID"] }.compact
      update.call("stage" => "qa", "attempt" => attempt)
      again = check_candidates(drama_id, target, ids, new_ids, runtime: runtime)
      return halted_changes(again["error"]) if again["halted"]
      return qa_unavailable(again["error"]) unless again["ok"]

      verdicts.merge!(again["verdicts"])
      failed.concat(again["failed"])
      own.concat(new_ids)
      retries << { "attempt" => attempt, "sentences" => sentences, "candidateIDs" => new_ids }
      learn(drama_id, ids, target, sentences, new_ids, again["verdicts"], attempt)
    end
    update.call("stage" => "selecting")
    decide(drama_id, ids, target, own, verdicts, failed, retries, options, since)
  end

  # ---- episode.accept_recommended_frames ----

  def accept_recommended(arguments)
    target = (arguments["target"] || "start").to_s
    return failure("invalid_target", "target must be start or end.") unless FRAME_TARGETS.include?(target)

    loaded = @dramas.get("id" => arguments["dramaID"])
    return loaded unless loaded["ok"]

    drama = annotate(loaded["drama"])
    episode = Array(drama["episodes"]).find { |entry| entry["id"] == arguments["episodeID"].to_s }
    return failure("episode_not_found", "Episode was not found.") unless episode

    shots = Array(episode["shots"]).sort_by { |shot| shot["order"].to_i }
    orders = arguments["shotOrders"].is_a?(Array) && !arguments["shotOrders"].empty? ? arguments["shotOrders"].map(&:to_i).uniq : nil
    unknown = orders ? orders - shots.map { |shot| shot["order"].to_i } : []
    shots = shots.select { |shot| orders.include?(shot["order"].to_i) } if orders
    only_passing = arguments["onlyPassing"] != false
    replace = arguments["replaceSelected"] == true
    keys = DramaService::FRAME_SELECTION_KEYS.fetch(target)
    items = shots.map do |shot|
      item = { "shotID" => shot["id"], "order" => shot["order"] }
      recommendation = shot[RECOMMENDATION_FIELDS[target]]
      current = shot[keys[:id]].to_s
      selected_exists = Array(shot[target == "start" ? "startCandidates" : "endCandidates"]).any? { |entry| entry["id"] == current }
      reason = if recommendation.nil? then "no_recommendation"
               elsif selected_exists && current == recommendation["candidateID"] then "already_selected"
               elsif selected_exists && !replace then "has_selection"
               elsif only_passing && !recommendation["eligible"] then "not_passing"
               end
      next item.merge("state" => "skipped", "reason" => reason, "recommendation" => recommendation).reject { |_key, value| value.nil? } if reason

      selected = @dramas.select_image({ "dramaID" => drama["id"], "episodeID" => episode["id"], "shotID" => shot["id"], "kind" => target,
                                        "candidateID" => recommendation["candidateID"] }, by: "recommendation")
      next item.merge("state" => "failed", "error" => compact_error(selected["error"])) unless selected["ok"]

      item.merge("state" => "selected", "candidateID" => recommendation["candidateID"], "status" => recommendation["status"], "score" => recommendation["score"],
                 "previousSelectedID" => current.empty? ? nil : current)
    end
    result = { "ok" => true, "target" => target, "selected" => items.count { |item| item["state"] == "selected" }, "items" => items }
    result["unknownOrders"] = unknown unless unknown.empty?
    result
  end

  private

  def annotate_node(drama, node, candidates, target, ids, field, detail_field = nil)
    candidates = Array(candidates)
    checked = candidates.select { |candidate| candidate.is_a?(Hash) && candidate["qa"].is_a?(Hash) }
    if checked.empty?
      node[field] = nil
      node[detail_field] = nil if detail_field
      return
    end
    cast_ids = checked.map { |candidate| candidate["qa"]["castIDs"] }.compact.first
    basis = begin
      built = material(drama, target, ids, cast_ids: cast_ids)
      built["ok"] ? built["basis"] : nil
    rescue StandardError => error
      @logger.call("video-studio: image QA basis for #{target} failed (#{error.class}: #{error.message})")
      nil
    end
    checked.each { |candidate| candidate["qa"] = candidate["qa"].merge("stale" => basis.nil? || candidate["qa"]["basis"] != basis) }
    recommendation = ImageQA.recommend(candidates, basis)
    node[field] = recommendation && recommendation["candidateID"]
    node[detail_field] = recommendation if detail_field
  end

  # 镜头要求、尺寸、标签：质检材料的一部分，也进指纹（运镜改了，构图结论就过期）。
  def context_for(drama, target, ids)
    locale = @reviews.locale
    labels = TARGET_LABELS.fetch(locale)
    frame = @dramas.frame(drama)
    size = ImageGeneration::ASPECT_TARGETS.include?(target) ? frame["image"] : ImageGeneration::IMAGE_SIZE
    base = { "ok" => true, "dramaID" => drama["id"], "target" => target, "ids" => ids, "size" => size, "aspect" => frame["aspect"], "locale" => locale }
    case target
    when "start", "end"
      episode = Array(drama["episodes"]).find { |entry| entry["id"] == ids["episodeID"] }
      shot = episode && Array(episode["shots"]).find { |entry| entry["id"] == ids["shotID"] }
      return failure("shot_not_found", "Shot was not found.") unless shot

      action = target == "start" ? shot["actionStart"] : shot["actionEnd"]
      requirements = [["运镜", shot["cameraIntent"]], ["本镜内容", shot["summary"]], [target == "start" ? "开场动作" : "结束动作", action]]
                     .reject { |_label, value| value.to_s.strip.empty? }.map { |label, value| "#{label}：#{value.to_s.strip}" }.join("\n")
      label = locale == "en" ? "Episode #{episode['order']} shot #{shot['order']} #{labels[target]}" : "第 #{episode['order']} 集第 #{shot['order']} 镜#{labels[target]}"
      base.merge("label" => label, "requirements" => requirements,
                 "shot" => shot.slice("cameraIntent", "summary", "actionStart", "actionEnd", "title"))
    when "character"
      character = Array(drama["characters"]).find { |entry| entry["id"] == ids["characterID"] }
      return failure("character_not_found", "Character was not found.") unless character

      base.merge("label" => "「#{character['name']}」#{labels[target]}", "requirements" => ImageGeneration::IDENTITY_FRAMING)
    else
      asset = Array(drama["assets"]).find { |entry| entry["id"] == ids["assetID"] }
      return failure("asset_not_found", "Asset was not found.") unless asset

      base.merge("label" => "「#{asset['name']}」#{labels[target]}", "requirements" => ImageGeneration::FRAMING[target].to_s)
    end
  end

  def candidates_of(drama, target, ids)
    case target
    when "character"
      character = Array(drama["characters"]).find { |entry| entry["id"] == ids["characterID"] }
      Array(character && character["candidates"])
    when "start", "end"
      episode = Array(drama["episodes"]).find { |entry| entry["id"] == ids["episodeID"] }
      shot = episode && Array(episode["shots"]).find { |entry| entry["id"] == ids["shotID"] }
      Array(shot && shot[target == "start" ? "startCandidates" : "endCandidates"])
    else
      asset = Array(drama["assets"]).find { |entry| entry["id"] == ids["assetID"] }
      Array(asset && asset["candidates"])
    end
  end

  def with_slot(runtime)
    return runtime.with_qa { yield } if runtime

    @limits.qa.acquire
    begin
      yield
    ensure
      @limits.qa.release
    end
  end

  def judge_with_retry(candidate, built, cast_ids)
    verdict = nil
    QA_ATTEMPTS.times do
      verdict = judge(candidate, built, cast_ids)
      break if verdict["ok"] || STOP_CODES.include?(verdict.dig("error", "code"))
    end
    verdict
  end

  # 一张候选的结论。画幅不符的不问模型。
  def judge(candidate, built, cast_ids)
    stamp = { "basis" => built["basis"], "rubric" => RUBRIC_VERSION, "checkedAt" => Time.now.utc.iso8601 }
    stamp["castIDs"] = cast_ids if cast_ids
    if candidate["aspectMismatch"]
      detail = "#{candidate['actualSize'] || '?'} ≠ #{candidate['requestedSize'] || built['context']['size']}"
      return { "ok" => true, "qa" => { "status" => "block", "score" => 0, "summary" => detail, "source" => "local",
                                       "issues" => [{ "category" => "aspect", "level" => "must", "detail" => detail }] }.merge(stamp) }
    end
    path = candidate["filePath"].to_s
    return failure("candidate_file_missing", "The candidate image file is missing.") unless File.file?(path)

    references = Array(built["plan"]["references"]).map(&:to_s).select { |entry| File.file?(entry) }
    answer = @reviews.ask_model(system: system_text(built["context"]["locale"]), content: content_text(built, references.length),
                                image_paths: references + [path])
    return answer unless answer["ok"]

    parsed = ImageQA.parse(answer["text"])
    return failure("review_invalid", "The review model did not return a valid #{FENCE} block.").merge("raw" => answer["text"].to_s[0, RAW_EXCERPT]) unless parsed

    { "ok" => true, "qa" => parsed.merge("model" => answer["model"].to_s[0, 200], "references" => references.length).merge(stamp) }
  end

  def system_text(locale)
    entry = @skills.get(SKILL_ID)
    body = entry ? entry["body"].to_s.sub(/\A---\n.*?\n---\n/m, "").strip : ""
    contract = if locale == "en"
                 "Answer in English. End with exactly one ```#{FENCE} block holding {status, score, summary, issues[{category, level, detail}]}."
               else
                 "用中文写 summary 与 detail。最后只输出一个 ```#{FENCE} 代码块，内容是 {status, score, summary, issues[{category, level, detail}]}。"
               end
    [body, contract].reject(&:empty?).join("\n\n")
  end

  def content_text(built, reference_count)
    context = built["context"]
    plan = built["plan"]
    en = context["locale"] == "en"
    attached = if reference_count.positive?
                 en ? "Attached: #{reference_count} reference image(s) in legend order (picture 1..#{reference_count}), then the candidate as the last image." : "随附 #{reference_count} 张参考图（按参考图说明的顺序，图1～图#{reference_count}），最后一张是待检查的候选图。"
               else
                 en ? "Attached: the candidate image only." : "只随附了待检查的候选图。"
               end
    sections = [
      en ? "Check this candidate: #{context['label']}" : "请检查这张候选：#{context['label']}",
      attached,
      "#{en ? 'Required canvas' : '要求的画幅'}：#{context['aspect']}（#{context['size']}）",
      context["requirements"].to_s.empty? ? "" : "【#{en ? 'Shot requirements' : '镜头要求'}】\n#{context['requirements']}",
      "【#{en ? 'Image prompt as sent (starts with the reference legend)' : '出图提示词（开头是参考图说明）'}】\n#{plan['prompt']}"
    ]
    sections.reject(&:empty?).join("\n\n")
  end

  # 认围栏，再在全文里找第一个配平的对象；规范化失败返回 nil。
  def self.parse(raw)
    named = raw.to_s.match(/```#{FENCE}(?![A-Za-z0-9_])\s*([\s\S]*?)```/i)
    [named && named[1], raw.to_s].compact.each do |candidate|
      json, truncated = ReviewMaterial.first_balanced_object(candidate)
      next if json.nil? || truncated

      value = begin
        JSON.parse(json)
      rescue JSON::ParserError
        next
      end
      normalized = normalize(value)
      return normalized if normalized
    end
    nil
  end

  # 规范化：未知类别记 other；level 缺省时 block 记 must、其余记 advice；pass 却列了问题按 warn；
  # block 却没列问题补一条 other；分数夹到 0～100，没给按状态取默认。
  def self.normalize(value)
    return nil unless value.is_a?(Hash) && STATUSES.include?(value["status"].to_s)

    status = value["status"].to_s
    issues = Array(value["issues"]).select { |issue| issue.is_a?(Hash) }.first(MAX_ISSUES).map do |issue|
      category = CATEGORIES.include?(issue["category"].to_s) ? issue["category"].to_s : "other"
      level = if LEVELS.include?(issue["level"].to_s) then issue["level"].to_s
              else status == "block" ? "must" : "advice"
              end
      detail = (issue["detail"] || issue["reason"]).to_s.strip[0, DETAIL_LIMIT]
      { "category" => category, "level" => level, "detail" => detail }
    end
    summary = value["summary"].to_s.strip[0, SUMMARY_LIMIT]
    status = "warn" if status == "pass" && !issues.empty?
    issues << { "category" => "other", "level" => "must", "detail" => summary } if status == "block" && issues.none? { |issue| issue["level"] == "must" }
    score = begin
      Integer(value["score"]).clamp(0, 100)
    rescue ArgumentError, TypeError, FloatDomainError
      (Float(value["score"]).round.clamp(0, 100) rescue DEFAULT_SCORES[status])
    end
    { "status" => status, "score" => score, "summary" => summary, "issues" => issues }
  end

  def all_block?(ids, verdicts)
    !ids.empty? && ids.all? { |id| verdicts[id].is_a?(Hash) && verdicts[id]["status"] == "block" }
  end

  # 选定：本次产出里最好的那张（不是 block）。只剩建议的才自动选；批量开始之后有人另选了（newer_selection）、
  # 当前选定的结论比它好（worse_qa）时不换。
  def decide(drama_id, ids, target, own, verdicts, failed, retries, options, since)
    best = ImageQA.best(own.map { |id| [id, verdicts[id]] })
    summary = own.map do |id|
      qa = verdicts[id]
      entry = { "candidateID" => id, "status" => qa ? qa["status"] : "unknown" }
      entry.merge!("score" => qa["score"], "categories" => Array(qa["issues"]).map { |issue| issue["category"] }.uniq) if qa
      entry
    end
    fields = { "state" => "completed", "imageQA" => { "candidates" => summary, "recommendedCandidateID" => best && best["candidateID"],
                                                      "retries" => retries.length, "failed" => failed } }
    fields["imageQA"]["remediation"] = retries.map { |entry| entry.slice("attempt", "sentences", "candidateIDs", "error") } unless retries.empty?
    if best.nil?
      if !failed.empty?
        return fields.merge("stage" => "needs_human", "selectionReason" => "qa_failed",
                            "nextStep" => "Candidate QA did not return a verdict for #{failed.map { |entry| entry['candidateID'] }.join(', ')} (#{failed.first.dig('error', 'code')}); quality is unknown, not passed. Run image.qa for this shot, or look at the candidates and pick one with drama.select_image.")
      end

      reasons = own.flat_map { |id| Array(verdicts[id] && verdicts[id]["issues"]) }.select { |issue| issue["level"] == "must" }
                   .map { |issue| "#{issue['category']}: #{issue['detail']}" }.uniq.first(REASONS_LIMIT)
      return fields.merge("stage" => "needs_human", "selectionReason" => "all_block", "reasons" => reasons,
                          "nextStep" => "Every candidate of this shot was blocked by image QA#{retries.empty? ? '' : " after #{retries.length} remediation round(s)"}. Revise the frame prompt or the reference package (identity images, appearance), then run image.generate again; or pick a candidate yourself with drama.select_image.")
    end
    return fields.merge("stage" => "done", "selectionReason" => "auto_select_off") unless options["autoSelect"]
    unless best["eligible"]
      return fields.merge("stage" => "needs_human", "selectionReason" => "needs_review",
                          "nextStep" => "The best candidate #{best['candidateID']} has must-fix notes (warn). Look at it, then pick it with drama.select_image or regenerate.")
    end

    fields.merge("stage" => "done").merge(adopt(drama_id, ids, target, best, own, since))
  end

  def adopt(drama_id, ids, target, best, own, since)
    keys = DramaService::FRAME_SELECTION_KEYS.fetch(target)
    current_status = current_selection_status(drama_id, ids, target)
    gate = lambda do |shot|
      current = shot[keys[:id]].to_s
      # 批量开始之后有人（不是批量自己）选过，哪怕选的是本次产出的另一张，也以那次选择为准。
      at = VideoSelection.parse(shot[keys[:at]])
      next "newer_selection" if !current.empty? && since && at && at >= since && shot[keys[:by]].to_s != "batch"
      next nil if current.empty? || own.include?(current)
      next "worse_qa" if STATUS_RANK.fetch(best["status"], UNKNOWN_RANK) > STATUS_RANK.fetch(current_status.to_s, UNKNOWN_RANK)

      nil
    end
    selected = @dramas.select_image(ids.merge("dramaID" => drama_id, "kind" => target, "candidateID" => best["candidateID"]),
                                    by: "batch", gate: gate, skip_if_same: true)
    if selected["ok"]
      return { "selectedCandidateID" => best["candidateID"], "selectionReason" => selected["changed"] == true ? "selected" : "already_selected" }
    end
    if selected.dig("error", "code") == "selection_kept"
      return { "selectedCandidateID" => selected["selectedCandidateID"], "selectionReason" => selected["reason"], "selectionKept" => selected["reason"],
               "bestCandidateID" => best["candidateID"] }
    end

    { "selectionReason" => "select_failed", "selectError" => compact_error(selected["error"]) }
  end

  # 当前选定那张的新鲜结论状态（锁外先读好，锁内只比较）。
  def current_selection_status(drama_id, ids, target)
    loaded = @dramas.get("id" => drama_id)
    return nil unless loaded["ok"]

    drama = loaded["drama"]
    candidates = candidates_of(drama, target, ids)
    episode = Array(drama["episodes"]).find { |entry| entry["id"] == ids["episodeID"] }
    shot = episode && Array(episode["shots"]).find { |entry| entry["id"] == ids["shotID"] }
    current = shot && candidates.find { |entry| entry["id"] == shot[DramaService::FRAME_SELECTION_KEYS.fetch(target)[:id]] }
    return nil unless current && current["qa"].is_a?(Hash)

    built = material(drama, target, ids)
    qa = built["ok"] ? ImageQA.fresh_qa(current, built["basis"]) : nil
    qa && qa["status"]
  end

  # 经验库（kind image）：每类补救句记一条——这一轮新出的候选里有没有一张不再有这一类的 must 问题、且不是 block。
  def learn(drama_id, ids, target, sentences, new_ids, verdicts, attempt)
    return if @lessons.nil? || new_ids.empty?

    best = new_ids.map { |id| verdicts[id] }.compact.min_by { |qa| [ImageQA.rank(qa), -qa["score"].to_i] }
    entries = sentences.map do |sentence|
      resolved = new_ids.any? do |id|
        qa = verdicts[id]
        qa && qa["status"] != "block" && Array(qa["issues"]).none? { |issue| issue["category"] == sentence["category"] && issue["level"] == "must" }
      end
      { "kind" => "image", "category" => sentence["category"], "remediationID" => sentence["id"], "lessonID" => sentence["id"], "text" => sentence["text"],
        "action" => "image_retry", "target" => target, "outcome" => resolved ? "resolved" : "unresolved", "fromStatus" => "block",
        "toStatus" => best ? best["status"] : "unknown", "attempt" => attempt, "dramaID" => drama_id }
        .merge(ids).merge("candidateIDs" => new_ids, "jobID" => "image:#{new_ids.first}")
    end
    @lessons.record(entries)
  end

  def halted_changes(error)
    return { "state" => "canceled", "reason" => "canceled", "stage" => nil } if error.is_a?(Hash) && error["code"] == "canceled"

    { "state" => "skipped", "reason" => "stopped_#{error.is_a?(Hash) ? error['reason'] : 'unknown'}", "stage" => nil }
  end

  def qa_unavailable(error)
    { "state" => "completed", "stage" => "needs_human", "selectionReason" => "qa_failed", "imageQA" => { "error" => compact_error(error) },
      "nextStep" => "Candidate QA could not run (#{error.is_a?(Hash) ? error['code'] : 'unknown'}). Look at the candidates and pick one with drama.select_image, or run image.qa later." }
  end

  def compact_error(error)
    return { "code" => "unknown", "message" => "Unknown error." } unless error.is_a?(Hash)

    { "code" => error["code"], "message" => error["message"].to_s[0, SUMMARY_LIMIT] }
  end

  def failure(code, message)
    { "ok" => false, "error" => { "code" => code, "message" => message } }
  end
end
