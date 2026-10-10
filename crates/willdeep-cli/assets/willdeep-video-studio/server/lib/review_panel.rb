# frozen_string_literal: true

require "json"
require_relative "host_bridge"
require_relative "review_material"
require_relative "review_record"
require_relative "drama_canon"
require_relative "creative_skill_store"

# 专家席审稿（0.42.0-rc1，docs/decisions/0010-expert-panel-review.md）。
#
# `review.run scope=drama|episode aspect=panel`：把一部剧的主框架或一集剧本交给一张专家席——
# 技能 skills/creative/panel.md 里「## 专家席」下的每一位专家各问一次（并行、互相看不到对方），
# 再把全部意见交给「## 主持人」归纳成一份结论。结论的形状与内容审核完全一样，经
# DramaService#record_review 存在 reviews.panel 上，进度、页面、已知悉与指纹过期都复用。
#
# 为什么不开多轮：复盘里「每轮复审都冒新 warn」的教训在六位专家、几十集上会放大；这里要的是
# 每一集过一遍六双眼睛、结论落成可执行的改法。多轮讨论留给宿主圆桌（决策第二步）。
module ReviewPanel
  SKILL_ID = "panel"
  ASPECT = "panel"
  SCOPES = %w[drama episode].freeze
  MIN_EXPERTS = 2
  MAX_EXPERTS = 12
  EXPERTS_HEADINGS = %w[专家席 专家 experts panel].freeze
  CHAIR_HEADINGS = %w[主持人 chair moderator].freeze
  GUARD_TAG = /\s*\[guard\]\s*\z/i.freeze
  NAME_ROLE = /\A(.+?)\s*[·|｜:：]\s*(.+)\z/.freeze
  RAW_EXCERPT = 400
  # 宿主圆桌（0.43.0-rc1，WillDeep 1.412.0-rc1 起的 willdeep/roundtable/run）：一场十几次模型调用，
  # 宿主那头封顶 30 分钟；插件这边多等一点，别在宿主还在跑的时候先放弃。
  HOST_ROUNDTABLE_TIMEOUT = 35 * 60
  ROUNDS_RANGE = (1..3).freeze
  # 发言记录里每位专家留多少字的摘要（正文在宿主的圆桌会话里）。
  TRANSCRIPT_SUMMARY = 300

  class Invalid < StandardError; end

  module_function

  # 技能正文 → { "shared", "experts" => [{ "name", "role", "brief", "guard" }], "chair" }。
  # 结构不对就抛 Invalid（少于两位专家、没有主持人节），调用方据此报 panel_skill_invalid、不花钱。
  def parse(body)
    text = strip_frontmatter(body.to_s)
    sections = split_sections(text, 2)
    experts_section = sections.find { |heading, _| heading && EXPERTS_HEADINGS.include?(heading.strip.downcase) }
    chair_section = sections.find { |heading, _| heading && CHAIR_HEADINGS.include?(heading.strip.downcase) }
    raise Invalid, "the panel skill has no 「## 专家席」 section" unless experts_section
    raise Invalid, "the panel skill has no 「## 主持人」 section" unless chair_section

    # 专家席与主持人之外的一切（开头的说明、「所有专家共用的规则」这类小节）都是共用规则，连标题一起保留。
    shared = sections.reject { |section| section.equal?(experts_section) || section.equal?(chair_section) }
                     .map { |heading, content| heading ? "## #{heading}\n\n#{content.strip}" : content.strip }
                     .reject(&:empty?).join("\n\n")

    experts = split_sections(experts_section[1], 3).reject { |heading, _| heading.nil? }.map do |heading, brief|
      title = heading.strip
      guard = title.match?(GUARD_TAG)
      title = title.sub(GUARD_TAG, "").strip
      name, role = title.match(NAME_ROLE)&.captures || [title, ""]
      { "name" => name.strip, "role" => role.to_s.strip, "brief" => brief.strip, "guard" => guard }
    end
    experts = experts.reject { |expert| expert["name"].empty? }.first(MAX_EXPERTS)
    raise Invalid, "the panel skill lists #{experts.length} expert(s); at least #{MIN_EXPERTS} are needed" if experts.length < MIN_EXPERTS

    chair = chair_section[1].strip
    raise Invalid, "the chair section is empty" if chair.empty?

    { "shared" => shared, "experts" => experts, "chair" => chair }
  end

  def strip_frontmatter(text)
    lines = text.split("\n", -1)
    return text unless lines.first&.strip == "---"

    closing = lines.drop(1).index { |line| line.strip == "---" }
    return text unless closing

    lines.drop(closing + 2).join("\n")
  end

  # 按 `level` 个井号的标题切段：[[heading | nil, content], ...]，第一段标题为 nil。
  # 只认行首的标题，不进代码块判断——技能里的示例代码块用的是 ```，不是 #。
  def split_sections(text, level)
    marker = /\A#{'#' * level}(?!#)\s*(.+?)\s*\z/
    sections = [[nil, []]]
    in_fence = false
    text.to_s.split("\n", -1).each do |line|
      in_fence = !in_fence if line.start_with?("```")
      match = in_fence ? nil : line.match(marker)
      if match
        sections << [match[1], []]
      else
        sections.last[1] << line
      end
    end
    sections.map { |heading, lines| [heading, lines.join("\n")] }.reject { |heading, content| heading.nil? && content.strip.empty? }
  end

  # 专家席审稿的执行者。依赖与 ReviewRunner 相同：剧存档、技能、宿主桥、设置；limits 是进程级
  # 并发名额（ShotPipelines::Limits，专家发言各占一个 qa 名额），不给就不限。
  class Runner
    def initialize(drama_service:, video_store:, skills:, host:, limits: nil, route:)
      @dramas = drama_service
      @store = video_store
      @skills = skills
      @host = host
      @limits = limits
      @route = route
    end

    # 材料、每位专家与主持人的 system、一人分饰整席的 system；不调模型。
    def material(arguments)
      built, locale, error = prepare(arguments)
      return error if error

      panel = load_panel
      return panel if panel["ok"] == false

      experts = panel["experts"].map do |expert|
        { "name" => expert["name"], "role" => expert["role"], "guard" => expert["guard"], "system" => expert_system(panel, expert, locale) }
      end
      {
        "ok" => true, "scope" => arguments["scope"].to_s, "aspect" => ASPECT, "label" => built["label"], "text" => built["text"],
        "context" => built["context"].to_s, "basis" => built["basis"], "imagePaths" => [], "videoPaths" => [],
        "system" => solo_system(panel, locale), "userMessage" => user_content(built, locale, ReviewMaterial.translate(locale, "reviewRequest", "label" => built["label"])),
        "panel" => { "experts" => experts, "chair" => { "system" => chair_system(panel, locale) } },
        "fence" => ReviewMaterial::FENCE, "locale" => locale,
        # 宿主能开圆桌（WillDeep 1.412.0-rc1 起）时 review.run 会走它：在圆桌页里看得见每位专家的发言。
        "hostRoundtable" => @host.supports?(HostBridge::ROUNDTABLE_RUN),
        "writeBack" => write_back(arguments, built)
      }
    end

    def run(arguments)
      unless @host.supports?(HostBridge::AI_COMPLETE) || @host.supports?(HostBridge::ROUNDTABLE_RUN)
        return failure("host_review_unsupported", "This host cannot run models for plugin tools. Inside WillDeep update to 1.380.0 or later; elsewhere call review.get_material scope=#{arguments['scope']} aspect=panel, play the panel yourself and store the verdict with drama.record_review.")
      end

      built, locale, error = prepare(arguments)
      return error if error

      panel = load_panel
      return panel if panel["ok"] == false

      settings = @store.settings
      # 宿主圆桌（0.43.0-rc1）：WillDeep 1.412.0-rc1 起能带着插件的专家席开一场真正的圆桌，用户在圆桌页里
      # 看得见每位专家怎么说。设置 panelRoundtable 关掉就退回插件自己并行问专家的路（便宜、快、不可见）。
      if @host.supports?(HostBridge::ROUNDTABLE_RUN) && settings["panelRoundtable"] != false
        return run_via_host(arguments, built, locale, settings, panel)
      end
      unless @host.supports?(HostBridge::AI_COMPLETE)
        return failure("host_review_unsupported", "This host only offers the roundtable, and panelRoundtable is off. Turn it on in video.settings or update WillDeep.")
      end

      opinions = speak(panel, built, locale, settings)
      spoke = opinions.select { |opinion| opinion["state"] == "spoke" }
      needed = [MIN_EXPERTS, panel["experts"].length].min
      if spoke.length < needed || spoke.length * 2 < panel["experts"].length
        failed = opinions.reject { |opinion| opinion["state"] == "spoke" }.map { |opinion| "#{opinion['name']}: #{opinion.dig('error', 'message')}" }
        return failure("panel_failed", "Only #{spoke.length} of #{panel['experts'].length} experts answered; the panel verdict was not produced. #{failed.join('; ')}")
               .merge("label" => built["label"], "panel" => transcript(opinions))
      end

      moderated = moderate(panel, built, opinions, locale, settings)
      return moderated.merge("label" => built["label"], "panel" => transcript(opinions)) unless moderated["ok"]

      verdict = moderated["verdict"]
      stored = @dramas.record_review(
        "dramaID" => arguments["dramaID"], "scope" => arguments["scope"].to_s, "aspect" => ASPECT, "episodeID" => arguments["episodeID"],
        "review" => verdict, "basis" => built["basis"], "model" => moderated["model"], "mediaImages" => 0, "mediaVideos" => 0,
        "panel" => transcript(opinions)
      )
      return stored unless stored["ok"]

      review = stored["review"]
      {
        "ok" => true, "label" => built["label"], "status" => review["status"], "summary" => review["summary"], "issues" => review["issues"],
        "mustFix" => ReviewRecord.must_fix_count(review), "model" => review["model"], "mediaImages" => 0, "mediaVideos" => 0,
        "basis" => review["basis"], "panel" => review["panel"], "modelCalls" => opinions.length + moderated["calls"].to_i
      }
    end

    private

    # 宿主圆桌：插件把专家席（人设不带输出契约——发言的结构由宿主管）、主持人规则、材料与结论契约
    # 交给宿主，宿主跑完整场圆桌后交回终稿、按契约出的结论与发言记录；结论照旧经 ReviewMaterial.parse
    # 解析、drama.record_review 落库，形状与插件自己跑出来的一模一样，另记 via 与圆桌会话。
    def run_via_host(arguments, built, locale, settings, panel)
      rounds = begin
        Integer(settings["panelRounds"])
      rescue ArgumentError, TypeError
        1
      end
      rounds = rounds.clamp(ROUNDS_RANGE.begin, ROUNDS_RANGE.end)
      routing = @route.call({}, settings)
      request = {
        "title" => built["label"],
        "topic" => user_content(built, locale, ReviewMaterial.translate(locale, "reviewRequest", "label" => built["label"])),
        "experts" => panel["experts"].each_with_index.map do |expert, index|
          { "id" => "seat-#{index + 1}", "name" => expert["name"], "expertise" => expert["role"], "persona" => expert_persona(panel, expert, locale) }
        end,
        "chairPersona" => [panel["shared"], "## #{chair_heading(locale)}\n\n#{panel['chair']}"].map(&:to_s).reject(&:empty?).join("\n\n---\n\n"),
        "verdictInstruction" => ReviewMaterial.translate(locale, "panelChairContract", "fence" => ReviewMaterial::FENCE),
        "rounds" => rounds
      }
      request["provider"] = routing["provider"] if routing["provider"]
      request["model"] = routing["model"] if routing["model"]

      response = begin
        HostBridge.with_timeout(HOST_ROUNDTABLE_TIMEOUT) { @host.request(HostBridge::ROUNDTABLE_RUN, request) }
      rescue HostBridge::RequestFailed => error
        return failure("review_model_failed", error.message).merge("label" => built["label"], "via" => "host_roundtable")
      rescue HostBridge::Unsupported
        return failure("host_review_unsupported", "The host does not support willdeep/roundtable/run.")
      end
      return failure("review_model_failed", "The host returned no roundtable result.").merge("label" => built["label"]) unless response.is_a?(Hash)

      transcript = host_transcript(panel, response["transcript"])
      verdict = ReviewMaterial.parse(response["verdict"].to_s)
      unless verdict
        return failure("review_invalid", "The roundtable chair did not return a valid #{ReviewMaterial::FENCE} block.")
               .merge("label" => built["label"], "raw" => response["verdict"].to_s[0, RAW_EXCERPT], "panel" => transcript,
                      "roundtable" => roundtable_summary(response))
      end

      stored = @dramas.record_review(
        "dramaID" => arguments["dramaID"], "scope" => arguments["scope"].to_s, "aspect" => ASPECT, "episodeID" => arguments["episodeID"],
        "review" => verdict, "basis" => built["basis"], "model" => response["model"].to_s, "mediaImages" => 0, "mediaVideos" => 0,
        "panel" => transcript, "via" => "host_roundtable", "roundtable" => roundtable_summary(response)
      )
      return stored unless stored["ok"]

      review = stored["review"]
      {
        "ok" => true, "label" => built["label"], "status" => review["status"], "summary" => review["summary"], "issues" => review["issues"],
        "mustFix" => ReviewRecord.must_fix_count(review), "model" => review["model"], "mediaImages" => 0, "mediaVideos" => 0,
        "basis" => review["basis"], "panel" => review["panel"], "via" => "host_roundtable",
        "roundtable" => roundtable_summary(response).merge("document" => response["document"].to_s)
      }
    end

    # 宿主交回的发言记录 → 落库的发言摘要：每位专家取最后一次成功发言（失败的只在没有成功发言时记）。
    def host_transcript(panel, entries)
      names = panel["experts"].map { |expert| expert["name"] }
      roles = panel["experts"].each_with_object({}) { |expert, map| map[expert["name"]] = expert["role"] }
      latest = {}
      Array(entries).each do |entry|
        next unless entry.is_a?(Hash)

        name = entry["name"].to_s
        next if name.empty?

        failed = entry["isError"] == true
        next if failed && latest[name] && latest[name]["state"] == "spoke"

        record = { "expert" => name, "role" => roles[name].to_s, "state" => failed ? "failed" : "spoke" }
        if failed
          record["error"] = entry["content"].to_s[0, TRANSCRIPT_SUMMARY]
        else
          record["summary"] = entry["content"].to_s[0, TRANSCRIPT_SUMMARY]
          record["stance"] = entry["stance"].to_s unless entry["stance"].to_s.empty?
        end
        latest[name] = record
      end
      names.map { |name| latest[name] || { "expert" => name, "role" => roles[name].to_s, "state" => "failed", "error" => "did not speak" } }
    end

    def roundtable_summary(response)
      { "reportID" => response["reportID"].to_s, "sessionID" => response["sessionID"].to_s, "rounds" => response["rounds"].to_i }
    end

    # 交给宿主的专家人设：共用规则 + 身份与简介（+ 合规红线）。不带输出契约——发言怎么结构化是宿主圆桌的事。
    def expert_persona(panel, expert, locale)
      parts = [panel["shared"], "## #{identity_heading(locale)}：#{expert['name']}（#{expert['role']}）\n\n#{expert['brief']}"]
      parts << guard_body if expert["guard"]
      parts.map(&:to_s).reject(&:empty?).join("\n\n---\n\n")
    end

    def prepare(arguments)
      scope = arguments["scope"].to_s
      return [nil, nil, failure("invalid_scope", "The expert panel reviews drama or episode.")] unless SCOPES.include?(scope)

      loaded = @dramas.get("id" => arguments["dramaID"])
      return [nil, nil, loaded] unless loaded["ok"]

      locale = @store.settings["uiLocale"] == "en" ? "en" : "zh-Hans"
      target = { "scope" => scope, "aspect" => ASPECT, "episodeID" => arguments["episodeID"].to_s }
      built = ReviewMaterial.build(loaded["drama"], target, locale: locale, media: { "images" => false, "videos" => false })
      return [nil, nil, failure("nothing_to_review", "The target does not exist or has no adopted content to review yet.")] unless built

      [built, locale, nil]
    end

    def load_panel
      entry = @skills.get(SKILL_ID)
      return failure("panel_skill_invalid", "The panel skill (#{SKILL_ID}) is missing.") unless entry

      ReviewPanel.parse(entry["body"]).merge("ok" => true)
    rescue ReviewPanel::Invalid => error
      failure("panel_skill_invalid", "#{error.message}. Fix skills/creative/#{SKILL_ID}.md (or reset it in the skill settings).")
    rescue CreativeSkillStore::Unavailable => error
      failure("panel_skill_invalid", error.message)
    end

    # 每位专家各问一次，并行。线程各自继承后台任务设下的宿主请求超时（Thread 局部变量不自动继承），
    # 并各占一个进程级质检名额——专家发言与成片质检、候选图质检花的是同一种资源（宿主模型调用）。
    def speak(panel, built, locale, settings)
      timeout = Thread.current[HostBridge::TIMEOUT_KEY]
      threads = panel["experts"].map do |expert|
        Thread.new do
          Thread.current[HostBridge::TIMEOUT_KEY] = timeout
          Thread.current.report_on_exception = false if Thread.current.respond_to?(:report_on_exception=)
          with_slot { ask_expert(panel, expert, built, locale, settings) }
        end
      end
      threads.map(&:value)
    end

    def with_slot
      return yield unless @limits && @limits.respond_to?(:qa) && @limits.qa

      @limits.qa.acquire
      begin
        yield
      ensure
        @limits.qa.release
      end
    end

    def ask_expert(panel, expert, built, locale, settings)
      base = { "name" => expert["name"], "role" => expert["role"] }
      request = ReviewMaterial.translate(locale, "panelRequest", "expert" => expert["name"], "role" => expert["role"], "label" => built["label"])
      answer = ask(expert_system(panel, expert, locale), user_content(built, locale, request), settings)
      return base.merge("state" => "failed", "error" => answer["error"]) unless answer["ok"]

      verdict = ReviewMaterial.parse(answer["text"])
      unless verdict
        return base.merge("state" => "failed", "error" => { "code" => "review_invalid", "message" => "No valid #{ReviewMaterial::FENCE} block.", "raw" => answer["text"].to_s[0, RAW_EXCERPT] })
      end

      base.merge("state" => "spoke", "status" => verdict["status"], "summary" => verdict["summary"].to_s, "issues" => verdict["issues"], "model" => answer["model"])
    rescue StandardError => error
      base.merge("state" => "failed", "error" => { "code" => "internal_error", "message" => "#{error.class}: #{error.message}"[0, RAW_EXCERPT] })
    end

    # 主持人归纳。结论读不出来重试一次：六位专家的发言已经花了钱，不该因为主持人一次格式错误全丢。
    def moderate(panel, built, opinions, locale, settings)
      content = chair_content(built, opinions, locale)
      system = chair_system(panel, locale)
      calls = 0
      last = nil
      2.times do
        calls += 1
        answer = ask(system, content, settings)
        return failure("review_model_failed", answer.dig("error", "message").to_s).merge("calls" => calls) unless answer["ok"]

        verdict = ReviewMaterial.parse(answer["text"])
        return { "ok" => true, "verdict" => verdict, "model" => answer["model"], "calls" => calls } if verdict

        last = answer["text"].to_s
      end
      failure("review_invalid", "The chair did not return a valid #{ReviewMaterial::FENCE} block.").merge("raw" => last.to_s[0, RAW_EXCERPT], "calls" => calls)
    end

    def ask(system, content, settings)
      request = @route.call({ "system" => system, "messages" => [{ "role" => "user", "content" => content }] }, settings)
      response = @host.request(HostBridge::AI_COMPLETE, request)
      return failure("review_model_failed", "The host returned no answer.") unless response.is_a?(Hash)

      { "ok" => true, "text" => response["text"].to_s, "model" => response["model"].to_s }
    rescue HostBridge::RequestFailed => error
      failure("review_model_failed", error.message)
    rescue HostBridge::Unsupported
      failure("host_review_unsupported", "The host does not support willdeep/ai/complete.")
    end

    # ---- 提示词 ----

    def expert_system(panel, expert, locale)
      parts = [panel["shared"], "## #{identity_heading(locale)}：#{expert['name']}（#{expert['role']}）\n\n#{expert['brief']}"]
      parts << guard_body if expert["guard"]
      parts << ReviewMaterial.translate(locale, "panelExpertContract", "fence" => ReviewMaterial::FENCE)
      parts.map(&:to_s).reject(&:empty?).join("\n\n---\n\n")
    end

    def chair_system(panel, locale)
      [panel["shared"], "## #{chair_heading(locale)}\n\n#{panel['chair']}", ReviewMaterial.translate(locale, "panelChairContract", "fence" => ReviewMaterial::FENCE)]
        .map(&:to_s).reject(&:empty?).join("\n\n---\n\n")
    end

    # review.get_material 给任何 MCP 客户端的那份：一人分饰整席，只输出主持人的结论。
    def solo_system(panel, locale)
      experts = panel["experts"].map do |expert|
        guard = expert["guard"] ? "\n\n#{guard_body}" : ""
        "### #{expert['name']} · #{expert['role']}\n\n#{expert['brief']}#{guard}"
      end
      [panel["shared"], ReviewMaterial.translate(locale, "panelSoloInstruction"), "## #{experts_heading(locale)}\n\n#{experts.join("\n\n")}",
       "## #{chair_heading(locale)}\n\n#{panel['chair']}", ReviewMaterial.translate(locale, "panelChairContract", "fence" => ReviewMaterial::FENCE)]
        .map(&:to_s).reject(&:empty?).join("\n\n---\n\n")
    end

    def guard_body
      entry = @skills.get(CreativeSkillStore::GUARD_SKILL)
      entry ? entry["body"].to_s : ""
    end

    def user_content(built, locale, request_line)
      [request_line, built["text"], built["context"].to_s.empty? ? nil : "#{ReviewMaterial.translate(locale, 'panelContextHeader')}\n#{built['context']}"]
        .compact.map(&:to_s).reject(&:empty?).join("\n\n")
    end

    def chair_content(built, opinions, locale)
      blocks = opinions.map do |opinion|
        header = ReviewMaterial.translate(locale, "panelOpinionHeader", "expert" => opinion["name"], "role" => opinion["role"])
        if opinion["state"] == "spoke"
          body = [ReviewMaterial.translate(locale, "panelOpinionStance", "status" => opinion["status"]), opinion["summary"],
                  JSON.pretty_generate(opinion["issues"])].map(&:to_s).reject(&:empty?).join("\n")
        else
          body = ReviewMaterial.translate(locale, "panelOpinionFailed", "error" => opinion.dig("error", "message").to_s)
        end
        "#{header}\n#{body}"
      end
      request = ReviewMaterial.translate(locale, "panelChairRequest", "label" => built["label"])
      [user_content(built, locale, request), blocks.join("\n\n")].join("\n\n")
    end

    def identity_heading(locale)
      locale == "en" ? "Your seat" : "你的身份"
    end

    def chair_heading(locale)
      locale == "en" ? "Chair" : "主持人"
    end

    def experts_heading(locale)
      locale == "en" ? "The panel" : "专家席"
    end

    # ---- 落库形状 ----

    # 存在结论上的发言记录：谁说了什么立场、几条意见；正文不存（已并进主持人的 issues）。
    def transcript(opinions)
      opinions.map do |opinion|
        entry = { "expert" => opinion["name"], "role" => opinion["role"], "state" => opinion["state"] }
        if opinion["state"] == "spoke"
          entry["status"] = opinion["status"]
          entry["summary"] = opinion["summary"].to_s
          entry["issueCount"] = Array(opinion["issues"]).length
          entry["mustFix"] = ReviewRecord.must_fix_count(opinion)
          entry["model"] = opinion["model"] if opinion["model"]
        else
          entry["error"] = { "code" => opinion.dig("error", "code"), "message" => opinion.dig("error", "message").to_s[0, 300] }
        end
        entry
      end
    end

    def write_back(arguments, built)
      ids = { "dramaID" => arguments["dramaID"].to_s, "scope" => arguments["scope"].to_s, "aspect" => ASPECT }
      ids["episodeID"] = arguments["episodeID"].to_s unless arguments["episodeID"].to_s.empty?
      { "tool" => "drama.record_review", "arguments" => ids.merge("basis" => built["basis"], "mediaImages" => 0, "mediaVideos" => 0) }
    end

    def failure(code, message)
      { "ok" => false, "error" => { "code" => code, "message" => message } }
    end
  end
end
