# frozen_string_literal: true

require_relative "host_bridge"
require_relative "image_backend"
require_relative "reference_package"
require_relative "frame_framing"
require_relative "media_ref"
require_relative "reference_legend"
require_relative "image_dimensions"
require_relative "identity_text"
require_relative "qa_remediation"

# `image.generate`：让聊天里的 Agent 替用户抽定妆图、首尾帧候选和资产参考图。
#
# 与页面上的「生成候选」是同一套规矩：
# - 提示词只取**已定稿**的内容（角色 visualPrompt、分镜 startPrompt/endPrompt、
#   资产 prompt）。分镜有未采用草稿时拒绝——候选图不该对应一份随时会丢的文字。
# - 首尾帧的参照图由参考包编译（ReferencePackage.compile purpose=image）给出：
#   出场角色的身份图、绑定的造型图、场景图、道具图，按同一套顺序与上限裁剪。
#   没有参考包的旧镜头按名字在本镜文本里出现过的已锁定角色带脸。castIDs 指定时
#   只带这些角色。
# - 多张之间的差异靠提示词尾巴 `(variation N)`；模型交错排、相邻请求至少隔 1 秒。
# - 尺寸（0.30.0-rc3）：首尾帧与场景图按剧的有效画幅（DramaService#frame）出图，
#   与成片同一个比例；所选模型出不了这一档时返回 image_model_aspect_unsupported，
#   不静默换模型。角色定妆、造型、道具仍是 1024x1536。
# - 取景（0.33.0-rc2）：定妆、造型、道具的提示词末尾拼一行固定的棚拍取景
#   （IDENTITY_FRAMING / PROP_FRAMING），参照图背景干净，不把杂物带进首尾帧。
# - 失败归类（0.33.0-rc2）：逐张 failed[] 带 retryable；额度用完是 quota_exhausted，
#   同一次调用剩下的张不再请求，一张都没出时整次报 image_quota_exhausted。
#
# 出图后端（设计稿 7.2）：宿主代管或环境变量直连，由 ImageBackend.select 决定；
# 两者都没有时返回 host_image_unsupported（错误码沿用，信息里说明两种配置）。
#
# 一次最多 4 张：工具调用是同步的，张数不封顶一次调用能卡十几分钟。
# dryRun: true 只返回将要发的提示词与参照图，不花钱；requestID 相同的重复调用
# 返回上一次记下的候选。
class ImageGeneration
  COUNTS = [1, 2, 4].freeze
  DEFAULT_COUNT = 2
  DEFAULT_MODELS = %w[nano-banana-2].freeze
  MAX_IMAGES_PER_CALL = 4
  # 角色定妆、造型、道具的出图尺寸：竖 2:3，与剧的画幅无关（它们是参照图，不进成片）。
  IMAGE_SIZE = "1024x1536"
  MIN_INTERVAL_SECONDS = 1.0
  # 补救句（extraDirectives）最多几句、每句多长（0.40.0-rc1）。
  EXTRA_DIRECTIVES_LIMIT = 6
  EXTRA_DIRECTIVE_MAX = 300
  TARGETS = %w[character start end appearance scene sceneVariant prop].freeze
  ASSET_TARGETS = %w[appearance scene sceneVariant prop].freeze
  # 按剧的有效画幅出图的目标：首尾帧直接进成片，场景图是合成首尾帧的参照，
  # 两者都必须和视频同一个比例。
  ASPECT_TARGETS = %w[start end scene sceneVariant].freeze

  # 参照图的统一取景（0.33.0-rc2）。定妆图、造型图、道具图之后要当参照图喂给首尾帧
  # 和视频模型，背景和景别随机的话，参照图里的杂物、光线会被带进每一帧。这里固定
  # 成棚拍：全身正面站姿 / 单个主体居中，浅灰无缝背景，柔和均匀的光。
  # 只写要什么、不写不要什么：出图和视频模型会把提到的东西画出来（写了「不要道具」
  # 就可能多出道具）。不拼剧的 visualStyle：那句写的是场景的光线色调，不是棚拍。
  IDENTITY_FRAMING = "Full-body standing front view, the baseline expression as described; " \
                     "the whole figure in frame from head to feet, plain light-grey seamless studio background, soft even lighting."
  PROP_FRAMING = "The subject described above, alone and centred, the whole subject in frame with space around it, " \
                 "plain light-grey seamless studio background, soft even lighting."
  FRAMING = { "character" => IDENTITY_FRAMING, "appearance" => IDENTITY_FRAMING, "prop" => PROP_FRAMING }.freeze

  # 额度用完时整次调用的错误码。逐张的 failed[].code 是 ImageBackend::QUOTA_EXHAUSTED。
  QUOTA_ERROR_CODE = "image_quota_exhausted"
  # 碰到这些逐张错误码就不再出剩下的张：后端没了，或额度用完，接着请求只是白白失败。
  STOP_CODES = ["host_image_unsupported", ImageBackend::QUOTA_EXHAUSTED].freeze

  # backend_selector：返回当前可用的出图后端（或 nil）。每次调用时再选：宿主可能在
  # 升级后重连并改变宣告。
  def initialize(drama_service:, host:, backend_selector: nil, sleeper: ->(seconds) { sleep(seconds) })
    @dramas = drama_service
    @host = host
    @backend_selector = backend_selector || -> { ImageBackend.select(host: host, media_root: drama_service.media_root) }
    @sleeper = sleeper
  end

  def generate(arguments)
    target = arguments["target"].to_s
    return failure("invalid_target", "target must be one of #{TARGETS.join(', ')}.") unless TARGETS.include?(target)

    models = Array(arguments["models"] || DEFAULT_MODELS).map(&:to_s).uniq
    unsupported = models - DramaService::IMAGE_MODELS
    return failure("invalid_image_model", "Unsupported image model: #{unsupported.join(', ')}.") unless unsupported.empty?
    return failure("invalid_image_model", "Pick at least one image model.") if models.empty?

    count = arguments.key?("countPerModel") ? arguments["countPerModel"].to_i : DEFAULT_COUNT
    return failure("invalid_count", "countPerModel must be one of #{COUNTS.join(', ')}.") unless COUNTS.include?(count)

    total = models.length * count
    if total > MAX_IMAGES_PER_CALL
      return failure("too_many_images", "One call generates at most #{MAX_IMAGES_PER_CALL} images (#{models.length} models x #{count} = #{total}). Split it into several calls.")
    end

    # 后端不可用时在读剧之前就报出来：dryRun 不需要后端，照常往下走。
    backend = arguments["dryRun"] == true ? nil : @backend_selector.call
    if backend.nil? && arguments["dryRun"] != true
      return failure("host_image_unsupported", "No image backend is available. Inside WillDeep, update the host to 1.378.0 or later; elsewhere set VIDEO_STUDIO_IMAGE_API_BASE and VIDEO_STUDIO_IMAGE_API_KEY.")
    end

    loaded = @dramas.get("id" => arguments["dramaID"])
    return loaded unless loaded["ok"]
    drama = loaded["drama"]

    size = IMAGE_SIZE
    if ASPECT_TARGETS.include?(target)
      frame = @dramas.frame(drama)
      size = frame["image"]
      # 模型出不了这一档就拒绝，不静默换模型：换了模型用户会以为出的是自己选的那家。
      rejected = models - frame["imageModels"]
      unless rejected.empty?
        return failure("image_model_aspect_unsupported",
                       "#{rejected.join(', ')} cannot draw #{frame['aspect']} (#{size}) frames or scene images for this drama; " \
                       "use #{frame['imageModels'].join(' or ')}. gpt-image models only support 1024x1024, 1536x1024 and 1024x1536.")
      end
    end

    request_id = arguments["requestID"].to_s.strip[0, 200]
    unless request_id.empty?
      already = candidates_of(drama, arguments, target).select { |entry| entry["requestID"] == request_id }
      unless already.empty?
        return { "ok" => true, "deduplicated" => true, "target" => target, "requested" => already.length,
                 "generated" => already.map { |entry| candidate_view(entry) }, "failed" => [], "references" => [] }
      end
    end

    extra, refused = extra_directives(arguments["extraDirectives"])
    return refused if refused

    plan = qa_plan(drama, arguments, target)
    return plan unless plan["ok"]

    # 补救句（0.40.0-rc1，候选图质检重抽时由批量传入）接在正文之后、取景行之前；质检的指纹不含它。
    plan = plan.merge("prompt" => insert_directives(plan["prompt"], extra, target)) unless extra.empty?

    if arguments["dryRun"] == true
      result = { "ok" => true, "dryRun" => true, "target" => target, "prompt" => plan["prompt"], "models" => models, "countPerModel" => count,
                 "requested" => total, "references" => plan["references"], "referenceSummary" => plan["cast"], "referenceLegend" => plan["legend"],
                 "warnings" => plan["warnings"], "dropped" => plan["dropped"], "size" => size }
      result["extraDirectives"] = extra unless extra.empty?
      return result
    end

    return failure("too_many_references", plan["warnings"].map { |entry| entry["message"] }.join(" ")) if plan["overLimit"]

    run(plan.merge("size" => size), backend, models, count, arguments, target, request_id)
  end

  # 后台任务提交前的检查（0.36.0-rc1）：现在有没有可用的出图后端。
  def available?
    !@backend_selector.call.nil?
  end

  # 出图计划（不花钱）：提示词（含参考图说明与取景行）、参照图路径、参考图说明、参照摘要，另有 entries（说明里
  # 每张图的结构化信息，候选图质检拼补救句用）。image.generate 与候选图质检（lib/image_qa.rb，0.40.0-rc1）
  # 共用这一份：质检的指纹按它算，提示词或参考包一变，存着的质检结论就过期。
  def qa_plan(drama, arguments, target)
    plan = case target
           when "character" then character_plan(drama, arguments)
           when "start", "end" then frame_plan(drama, arguments, target)
           else asset_plan(drama, arguments, target)
           end
    return plan unless plan["ok"]

    # 取景行拼在最后（在 `(variation N)` 之前），dryRun 返回的就是要发出去的整段。
    framing = FRAMING[target]
    framing ? plan.merge("prompt" => "#{plan['prompt']}\n\n#{framing}", "framing" => framing) : plan
  end

  private

  def character_plan(drama, arguments)
    character = drama["characters"].find { |entry| entry["id"] == arguments["characterID"].to_s }
    return failure("character_not_found", "Character was not found.") unless character

    prompt = character["visualPrompt"].to_s.strip
    if prompt.empty?
      return failure("prompt_missing", "This character has no adopted visualPrompt yet. Save the character with commit: true first.")
    end
    plan(prompt, [], [], [], []).merge("identity" => prompt)
  end

  def frame_plan(drama, arguments, kind)
    episode = drama["episodes"].find { |entry| entry["id"] == arguments["episodeID"].to_s }
    return failure("episode_not_found", "Episode was not found.") unless episode

    shot = episode["shots"].find { |entry| entry["id"] == arguments["shotID"].to_s }
    return failure("shot_not_found", "Shot was not found.") unless shot
    if shot["draft"].is_a?(Hash) && !shot["draft"].empty?
      return failure("shot_has_draft", "This shot has an unadopted draft. Adopt or discard it before generating frames.")
    end

    prompt = shot[kind == "start" ? "startPrompt" : "endPrompt"].to_s.strip
    return failure("prompt_missing", "This shot has no #{kind} frame prompt yet.") if prompt.empty?

    cast_override = nil
    if arguments["castIDs"].is_a?(Array)
      wanted = arguments["castIDs"].map(&:to_s)
      locked = drama["characters"].select { |entry| MediaRef.selected(entry, @dramas.media_root) }.map { |entry| entry["id"] }
      unknown = wanted - locked
      return failure("cast_not_locked", "These characters have no selected identity image: #{unknown.join(', ')}.") unless unknown.empty?
      cast_override = wanted
    end

    compiled = ReferencePackage.compile(drama, episode, shot, purpose: "image", capabilities: {}, cast_override: cast_override, media_root: @dramas.media_root)
    return compiled unless compiled["ok"]

    entries = compiled["slots"].map { |slot| frame_reference(slot, compiled) }
    faces = entries.select { |entry| entry["semanticType"] == "identity" }.map { |entry| entry["characterID"] }
    unreferenced = Array(compiled["subjects"]).reject { |subject| faces.include?(subject["characterID"]) }
                                              .map { |subject| { "name" => subject["name"], "identity" => subject["identity"], "wardrobe" => subject["wardrobe"] } }
    legend = ReferenceLegend.build(entries, unreferenced: unreferenced)
    # 取景句（0.41.0-rc1，lib/frame_framing.rb）：运镜里的景别与机位，放在描述前面。
    framing = FrameFraming.line(shot["cameraIntent"], kind)
    framed = framing.empty? ? prompt : "#{framing}\n#{prompt}"
    plan(ReferenceLegend.prepend(legend["legend"], framed), legend["references"], legend["summary"], compiled["warnings"], compiled["dropped"],
         compiled["overLimit"], legend["legend"]).merge("entries" => entries, "unreferenced" => unreferenced, "body" => prompt)
  end

  # 参考包槽位 -> 参考图说明的一项。身份图带角色的 visualPrompt（摘年龄性别），
  # 造型图带它属于谁，场景用参考包里展开后的场景名（含变体）。
  def frame_reference(slot, compiled)
    subjects = Array(compiled["subjects"])
    entry = { "semanticType" => slot["semanticType"], "assetID" => slot["assetID"], "name" => slot["assetName"],
              "fileName" => slot["fileName"], "filePath" => slot["filePath"] }
    case slot["semanticType"]
    when "identity"
      subject = subjects.find { |item| item["characterID"] == slot["assetID"] }
      entry["characterID"] = slot["assetID"]
      entry["identity"] = subject ? subject["identity"] : ""
      entry["wardrobe"] = subject ? subject["wardrobe"].to_s : ""
    when "appearance"
      subject = subjects.find { |item| item["appearanceID"].to_s == slot["assetID"] }
      entry["characterName"] = subject ? subject["name"] : ""
    when "scene"
      name = compiled["scene"].is_a?(Hash) ? compiled["scene"]["name"].to_s : ""
      entry["name"] = name unless name.empty?
      entry["scenePrompt"] = compiled["scene"].is_a?(Hash) ? compiled["scene"]["prompt"].to_s : ""
    end
    entry
  end

  # 资产参考图：造型图带角色的身份图作参照并把身份提示词拼在前面；场景变体带
  # 场景本身的选定图；场景、道具只按描述出图。
  def asset_plan(drama, arguments, kind)
    asset = Array(drama["assets"]).find { |entry| entry["id"] == arguments["assetID"].to_s }
    return failure("asset_not_found", "Asset was not found.") unless asset
    return failure("invalid_target", "Asset #{asset['id']} is a #{asset['kind']}, not #{kind}.") unless asset["kind"] == kind
    if asset["draft"].is_a?(Hash) && !asset["draft"].empty?
      return failure("asset_has_draft", "This asset has an unadopted draft. Adopt or discard it before generating images.")
    end

    prompt = asset["prompt"].to_s.strip
    return failure("prompt_missing", "This asset has no adopted prompt yet.") if prompt.empty?

    entries = []
    warnings = []
    case kind
    when "appearance"
      character = drama["characters"].find { |entry| entry["id"] == asset["characterID"].to_s }
      return failure("character_not_found", "The character of this appearance was not found.") unless character
      identity = MediaRef.selected(character, @dramas.media_root)
      # 造型图只借角色的长相，日常穿着不拼进来，否则和造型打架（lib/identity_text.rb，0.38.0-rc1）。
      looks = IdentityText.for_cast(character["visualPrompt"], prompt, asset["category"])
      if identity
        entries << { "semanticType" => "identity", "characterID" => character["id"], "assetID" => character["id"], "name" => character["name"],
                     "fileName" => identity["fileName"], "filePath" => identity["filePath"], "identity" => looks }
      else
        warnings << { "code" => "identity_missing", "message" => "「#{character['name']}」还没有选定形象图，造型图不带脸的参照。", "characterID" => character["id"] }
      end
      base = looks.strip
      prompt = base.empty? ? prompt : "#{base}\n\n#{prompt}"
    when "sceneVariant"
      scene = Array(drama["assets"]).find { |entry| entry["id"] == asset["sceneID"].to_s }
      image = scene && MediaRef.selected(scene, @dramas.media_root)
      if image
        entries << { "semanticType" => "scene", "assetID" => scene["id"], "name" => scene["name"], "fileName" => image["fileName"], "filePath" => image["filePath"] }
        prompt = "#{scene['prompt'].to_s.strip}\n\n#{prompt}" unless scene["prompt"].to_s.strip.empty?
      end
      prompt = "#{prompt}\n\nLighting: #{asset['lighting']}" unless asset["lighting"].to_s.strip.empty?
    end
    legend = ReferenceLegend.build(entries)
    plan(ReferenceLegend.prepend(legend["legend"], prompt), legend["references"], legend["summary"], warnings, [], false, legend["legend"])
      .merge("entries" => entries, "body" => asset["prompt"].to_s.strip)
  end

  # legend：拼在提示词最前面的参考图说明（已含在 prompt 里），dryRun 单独回给调用方看。
  def plan(prompt, references, cast, warnings, dropped, over_limit = false, legend = "")
    { "ok" => true, "prompt" => prompt, "references" => references, "cast" => cast, "warnings" => warnings, "dropped" => dropped,
      "overLimit" => over_limit, "legend" => legend }
  end

  def run(plan, backend, models, count, arguments, target, request_id)
    jobs = (0...count).flat_map { |index| models.map { |model| [model, index] } }
    generated = []
    failed = []
    warnings = Array(plan["warnings"]).dup
    stopped = nil
    jobs.each_with_index do |(model, index), position|
      @sleeper.call(MIN_INTERVAL_SECONDS) if position.positive?
      prompt = index.zero? ? plan["prompt"] : "#{plan['prompt']}\n\n(variation #{index + 1})"
      begin
        image = backend.generate(prompt: prompt, model: model, size: plan["size"], reference_paths: plan["references"])
      rescue ImageBackend::Error => error
        failed << failure_entry(model, error)
        if STOP_CODES.include?(error.code)
          stopped = { "code" => error.code, "skipped" => jobs.length - position - 1 }
          break
        end
        next
      end
      mismatch = ASPECT_TARGETS.include?(target) ? aspect_mismatch(image, plan["size"]) : nil
      recorded = record(arguments, target, plan["prompt"], image, request_id, mismatch)
      if recorded["ok"]
        generated << recorded["candidate"]
        if mismatch
          warnings << { "code" => "aspect_mismatch", "candidateID" => recorded["candidate"]["candidateID"], "model" => model,
                        "requestedSize" => plan["size"], "actualSize" => mismatch["actualSize"],
                        "message" => "#{model} returned a #{mismatch['actualSize']} image for a #{plan['size']} request; the candidate is kept " \
                                     "and flagged aspectMismatch. Regenerate this frame before using it." }
        end
      else
        # 图已经出了、钱已经花了，只是记不进剧本；把文件路径交回去，至少人能找到。
        failed << { "model" => model, "code" => recorded.dig("error", "code"), "message" => recorded.dig("error", "message"),
                    "retryable" => false, "filePath" => image.is_a?(Hash) ? image["filePath"] : nil }
      end
    end

    summary = {
      "target" => target, "backend" => backend.name, "size" => plan["size"], "requested" => jobs.length, "generated" => generated, "failed" => failed,
      "references" => plan["cast"], "warnings" => warnings,
      # 画幅不符的候选单独计数：已计费、已记下，但不该直接进成片，调用方据此重抽。
      "aspectMismatchCount" => generated.count { |entry| entry["aspectMismatch"] }
    }
    # stopped：中途停下没出的张数，调用方据此知道剩下的不是失败而是没发。
    summary["stopped"] = stopped if stopped
    return { "ok" => true }.merge(summary) unless generated.empty?

    { "ok" => false, "error" => top_level_error(failed) }.merge(summary)
  end

  def failure_entry(model, error)
    entry = { "model" => model, "code" => error.code, "message" => error.message, "retryable" => error.retryable }
    entry["httpStatus"] = error.http_status if error.http_status
    entry["upstreamQuota"] = error.upstream_quota == true if error.code == ImageBackend::QUOTA_EXHAUSTED
    entry
  end

  # 一张都没出时整次调用的错误。额度用完单独给 image_quota_exhausted：调用方要知道
  # 重试没用，得先有人补额度；其余仍是 image_generation_failed，retryable 看逐张。
  def top_level_error(failed)
    quota = failed.find { |entry| entry["code"] == ImageBackend::QUOTA_EXHAUSTED }
    if quota
      who = if quota["upstreamQuota"]
              "the image platform's upstream quota is exhausted (upstream_quota_exhausted); it comes back only after the platform operator tops up its upstream account"
            else
              "the image account's quota or balance is exhausted; top up the account before generating again"
            end
      return { "code" => QUOTA_ERROR_CODE, "message" => "Image generation stopped: #{who}. Retrying now will fail the same way. Upstream said: #{quota['message']}",
               "retryable" => false, "upstreamQuota" => quota["upstreamQuota"] }
    end

    { "code" => "image_generation_failed", "message" => failed.first ? failed.first["message"] : "No image was generated.",
      "retryable" => failed.any? { |entry| entry["retryable"] } }
  end

  # 读回图片宽高，方向与请求的尺寸相反（竖屏剧回了横图等）时返回
  # { "actualSize" => "WxH" }；一致或读不出时返回 nil（读不出不当问题报）。
  def aspect_mismatch(image, size)
    return nil unless image.is_a?(Hash)

    requested = ImageDimensions.parse_size(size)
    actual = ImageDimensions.read(image["filePath"].to_s)
    return nil unless requested && actual
    return nil if ImageDimensions.orientation(*requested) == ImageDimensions.orientation(*actual)

    { "actualSize" => "#{actual[0]}x#{actual[1]}", "requestedSize" => size }
  end

  def record(arguments, target, prompt, image, request_id, mismatch = nil)
    return failure("invalid_host_response", "Host returned no image.") unless image.is_a?(Hash)

    common = { "dramaID" => arguments["dramaID"], "prompt" => prompt, "mediaURL" => image["mediaURL"],
               "filePath" => image["filePath"], "model" => image["model"], "requestID" => request_id }
    common = common.merge("aspectMismatch" => true, "actualSize" => mismatch["actualSize"], "requestedSize" => mismatch["requestedSize"]) if mismatch
    result = case target
             when "character" then @dramas.record_character_image(common.merge("characterID" => arguments["characterID"]))
             when "start", "end" then @dramas.record_image(common.merge("episodeID" => arguments["episodeID"], "shotID" => arguments["shotID"], "kind" => target))
             else @dramas.record_asset_media(common.merge("assetID" => arguments["assetID"]))
             end
    return result unless result["ok"]

    latest = candidates_of(result["drama"], arguments, target).last
    { "ok" => true, "candidate" => candidate_view(latest) }
  end

  def candidate_view(entry)
    view = { "candidateID" => entry["id"], "model" => entry["model"], "mediaURL" => entry["mediaURL"], "filePath" => entry["filePath"], "fileName" => entry["fileName"] }
    view.merge!("aspectMismatch" => true, "actualSize" => entry["actualSize"], "requestedSize" => entry["requestedSize"]) if entry["aspectMismatch"]
    view["qa"] = entry["qa"] if entry["qa"].is_a?(Hash)
    view
  end

  # extraDirectives：字符串或 {text} 的数组，最多 EXTRA_DIRECTIVES_LIMIT 句，每句只写正面描述
  # （否定词与 text / cut 一类词按 QARemediation.negative? 拒收，negative_wording）。返回 [句子数组, 错误]。
  def extra_directives(value)
    return [[], nil] if value.nil?
    return [[], failure("invalid_arguments", "extraDirectives must be an array of sentences.")] unless value.is_a?(Array)

    sentences = value.map { |entry| (entry.is_a?(Hash) ? entry["text"] : entry).to_s.gsub(/\s*\n\s*/, " ").strip }.reject(&:empty?)
    if sentences.length > EXTRA_DIRECTIVES_LIMIT || sentences.any? { |text| text.length > EXTRA_DIRECTIVE_MAX }
      return [[], failure("invalid_arguments", "extraDirectives takes at most #{EXTRA_DIRECTIVES_LIMIT} sentences of #{EXTRA_DIRECTIVE_MAX} characters.")]
    end
    negative = sentences.find { |text| QARemediation.negative?(text) }
    if negative
      return [[], failure("negative_wording", "Image prompts must describe the wanted picture positively; this sentence was refused: #{negative}")]
    end

    [sentences.uniq, nil]
  end

  # 补救句放在正文与取景行之间：取景行一直是最后一段（在 `(variation N)` 之前）。
  def insert_directives(prompt, sentences, target)
    block = sentences.join("\n")
    framing = FRAMING[target]
    return "#{prompt}\n\n#{block}" unless framing && prompt.end_with?("\n\n#{framing}")

    "#{prompt[0...(prompt.length - framing.length - 2)]}\n\n#{block}\n\n#{framing}"
  end

  def candidates_of(drama, arguments, target)
    case target
    when "character"
      character = drama["characters"].find { |entry| entry["id"] == arguments["characterID"].to_s }
      Array(character && character["candidates"])
    when "start", "end"
      episode = drama["episodes"].find { |entry| entry["id"] == arguments["episodeID"].to_s }
      shot = episode && episode["shots"].find { |entry| entry["id"] == arguments["shotID"].to_s }
      Array(shot && shot[target == "start" ? "startCandidates" : "endCandidates"])
    else
      asset = Array(drama["assets"]).find { |entry| entry["id"] == arguments["assetID"].to_s }
      Array(asset && asset["candidates"])
    end
  end

  def failure(code, message)
    { "ok" => false, "error" => { "code" => code, "message" => message } }
  end
end
