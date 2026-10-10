# frozen_string_literal: true

require_relative "qa_remediation"
require_relative "qa_lesson_store"

# 经验库（0.38.0-rc1，docs/decisions/0004-qa-lessons.md）：把「质检发现什么问题、加哪句话重做、
# 重做后好没好」攒成数据，再用回生成里。
#
# 经验有三个来源：补救表里的内置补救句（ID 形如 remediation/<类别>/<句子 ID>）、内置预防句
# （prevention/<句子 ID>，即每条单镜提示词默认带的一镜到底句），以及 qa.save_lesson 加的手动经验
# （lesson_<uuid>）。手动经验分三种用法：
#   - remediation：某类问题自动重做时可选的补救句；
#   - prevention：每条成片提示词默认带上的预防句；
#   - note：只给人和 Agent 看的规则（例如出图侧「多参考图必须写参考图说明」），不进提示词。
#
# 用回生成：
#   - prevention_directives：内置预防句 + 手动预防句 + 「补救成功率 ≥ 阈值且尝试次数 ≥ N」的补救句，
#     由参考包编译写进每条单镜提示词（分镜写明切镜时跳过一镜到底类）。
#   - remediation_for：自动重做选哪句补救——验证过的按解决率排，没攒够次数的按表里的顺序；
#     这一镜已经试过没用的那句排到最后。
class QALessons
  KINDS = %w[video image].freeze
  USES = %w[prevention remediation note].freeze
  GENERAL = "general"
  TEXT_LIMIT = 500
  NOTE_LIMIT = 500
  UNPROVEN_PRIOR = 0.5

  Refused = Class.new(StandardError) do
    attr_reader :code

    def initialize(code, message)
      @code = code
      super(message)
    end
  end

  def initialize(store:)
    @store = store
  end

  attr_reader :store

  # ---- 读 ----

  # qa.lessons：全部经验（带统计与开关）、按类别 + 补救句汇总的统计、当前生效的预防句、阈值与补救表。
  def summary(arguments = {})
    data = @store.snapshot
    lessons = all_lessons(data)
    category = arguments["category"].to_s
    kind = arguments["kind"].to_s
    lessons = lessons.select { |lesson| lesson["category"] == category } unless category.empty?
    lessons = lessons.select { |lesson| lesson["kind"] == kind } unless kind.empty?
    {
      "ok" => true,
      "lessons" => lessons,
      "aggregate" => aggregate(data["attempts"]),
      "prevention" => prevention_from(all_lessons(data)),
      "thresholds" => { "minAttempts" => QARemediation::PREVENTION_MIN_ATTEMPTS, "minResolvedRate" => QARemediation::PREVENTION_MIN_RATE },
      "categories" => QARemediation::CATEGORIES.map do |name|
        { "category" => name, "regenerate" => QARemediation.regenerate?(name), "maxRetries" => QARemediation.max_retries(name) }
      end,
      "attemptCount" => data["attempts"].length,
      "storePath" => @store.path
    }
  rescue QALessonStore::Unavailable => error
    failure("lesson_store_unavailable", error.message)
  end

  # 成片提示词默认带的预防句。存档读不出来时退回内置默认，不让出片因此失败。
  def prevention_directives
    prevention_from(all_lessons(@store.snapshot))
  rescue QALessonStore::Unavailable
    QARemediation.builtin_prevention.map { |entry| entry.merge("source" => "builtin") }
  end

  # 某类问题这次重做用哪句补救。tried：这一镜已经试过、没解决的经验 ID。
  def remediation_for(category, tried: [])
    candidates = remediation_candidates(category)
    return nil if candidates.empty?

    ranked = candidates.each_with_index.sort_by do |lesson, index|
      stats = lesson["stats"] || {}
      score = proven?(stats) ? stats["resolvedRate"].to_f : UNPROVEN_PRIOR
      [tried.include?(lesson["id"]) ? 1 : 0, -score, index]
    end
    best = ranked.first.first
    { "id" => best["remediationID"] || best["id"], "lessonID" => best["id"], "category" => category.to_s, "text" => best["text"], "source" => best["source"] }
  end

  # ---- 写 ----

  def record(entries)
    @store.record_attempts(entries)
  rescue QALessonStore::Unavailable => error
    warn "video-studio: qa lesson attempts not recorded (#{error.message})"
    []
  end

  # qa.save_lesson：新建 / 修改手动经验，或开关一条内置经验。
  def save(arguments)
    id = arguments["lessonID"].to_s
    if builtin_id?(id)
      raise Refused.new("lesson_not_found", "Unknown built-in lesson #{id}.") unless builtin_lessons.any? { |lesson| lesson["id"] == id }
      raise Refused.new("builtin_lesson_readonly", "Built-in lessons can only be enabled or disabled; add a manual lesson to change the wording.") if (arguments.keys & %w[text category kind applyAs]).any?
      raise Refused.new("invalid_arguments", "Pass enabled: true or false for a built-in lesson.") unless [true, false].include?(arguments["enabled"])

      @store.set_override(id, arguments["enabled"])
      return { "ok" => true, "lesson" => all_lessons(@store.snapshot).find { |lesson| lesson["id"] == id } }
    end

    saved = @store.upsert_lesson(id.empty? ? nil : id) do |existing|
      raise Refused.new("lesson_not_found", "Lesson #{id} was not found.") if !id.empty? && existing.nil?

      manual_lesson(existing || {}, arguments)
    end
    { "ok" => true, "lesson" => all_lessons(@store.snapshot).find { |lesson| lesson["id"] == saved["id"] } }
  rescue Refused => error
    failure(error.code, error.message)
  rescue QALessonStore::Unavailable => error
    failure("lesson_store_unavailable", error.message)
  end

  private

  def manual_lesson(existing, arguments)
    lesson = existing.dup
    lesson["source"] = "manual"
    if arguments.key?("text")
      text = arguments["text"].to_s.gsub(/\s*\n\s*/, " ").strip
      raise Refused.new("invalid_lesson", "text must be 1 to #{TEXT_LIMIT} characters.") if text.empty? || text.length > TEXT_LIMIT

      lesson["text"] = text
    end
    raise Refused.new("invalid_lesson", "text is required for a new lesson.") if lesson["text"].to_s.empty?

    lesson["kind"] = arguments["kind"].to_s if arguments.key?("kind")
    lesson["kind"] ||= "video"
    raise Refused.new("invalid_lesson", "kind must be video or image.") unless KINDS.include?(lesson["kind"])

    lesson["category"] = arguments["category"].to_s if arguments.key?("category")
    lesson["category"] = GENERAL if lesson["category"].to_s.empty?
    unless lesson["category"] == GENERAL || QARemediation::CATEGORIES.include?(lesson["category"])
      raise Refused.new("invalid_lesson", "category must be one of #{([GENERAL] + QARemediation::CATEGORIES).join(', ')}.")
    end

    lesson["applyAs"] = arguments["applyAs"].to_s if arguments.key?("applyAs")
    lesson["applyAs"] ||= lesson["kind"] == "video" && QARemediation.regenerate?(lesson["category"]) ? "remediation" : "note"
    raise Refused.new("invalid_lesson", "applyAs must be prevention, remediation or note.") unless USES.include?(lesson["applyAs"])
    if lesson["applyAs"] == "remediation" && !(lesson["kind"] == "video" && QARemediation::CATEGORIES.include?(lesson["category"]))
      raise Refused.new("invalid_lesson", "A remediation lesson needs kind video and a QA category (#{QARemediation::CATEGORIES.join(', ')}).")
    end
    if lesson["applyAs"] != "note" && QARemediation.negative?(lesson["text"])
      raise Refused.new("negative_wording", "Prompt lessons must describe the wanted picture in positive words: models draw what a prompt mentions, so negations and words such as cut, blood, wound or text are refused. Rephrase it, or save it with applyAs: note.")
    end

    lesson["enabled"] = arguments["enabled"] == true if arguments.key?("enabled")
    lesson["enabled"] = true if lesson["enabled"].nil?
    if arguments.key?("note")
      lesson["note"] = arguments["note"].to_s.strip[0, NOTE_LIMIT]
      lesson.delete("note") if lesson["note"].empty?
    end
    by = arguments["createdBy"].to_s
    lesson["createdBy"] = %w[user agent].include?(by) ? by : (lesson["createdBy"] || "agent")
    lesson
  end

  def builtin_id?(id)
    id.start_with?("remediation/", "prevention/")
  end

  def builtin_lessons
    prevention = QARemediation.builtin_prevention.map do |entry|
      { "id" => "prevention/#{entry['id']}", "remediationID" => entry["id"], "source" => "builtin", "kind" => "video", "applyAs" => "prevention",
        "category" => entry["category"], "text" => entry["text"] }
    end
    remediation = QARemediation::CATEGORIES.flat_map do |category|
      QARemediation.remediations(category).map do |entry|
        { "id" => "remediation/#{category}/#{entry['id']}", "remediationID" => entry["id"], "source" => "builtin", "kind" => "video",
          "applyAs" => QARemediation.regenerate?(category) ? "remediation" : "note", "category" => category, "text" => entry["text"],
          "regenerate" => QARemediation.regenerate?(category) }
      end
    end
    prevention + remediation
  end

  # 内置 + 手动，带开关与统计。
  def all_lessons(data)
    stats = stats_by_lesson(data["attempts"])
    builtin = builtin_lessons.map do |lesson|
      override = data["overrides"][lesson["id"]]
      lesson.merge("enabled" => override.is_a?(Hash) ? override["enabled"] != false : true)
    end
    (builtin + data["lessons"].map { |lesson| lesson.merge("source" => "manual") }).map do |lesson|
      entry = lesson.merge("stats" => stats[lesson["id"]] || empty_stats)
      entry["proven"] = proven?(entry["stats"])
      entry
    end
  end

  def remediation_candidates(category)
    all_lessons(@store.snapshot).select do |lesson|
      lesson["enabled"] && lesson["applyAs"] == "remediation" && lesson["kind"] == "video" && lesson["category"] == category.to_s
    end
  rescue QALessonStore::Unavailable
    QARemediation.remediations(category).map do |entry|
      { "id" => "remediation/#{category}/#{entry['id']}", "remediationID" => entry["id"], "category" => category.to_s, "text" => entry["text"], "source" => "builtin" }
    end
  end

  def prevention_from(lessons)
    picked = lessons.select do |lesson|
      next false unless lesson["enabled"] && lesson["kind"] == "video"

      lesson["applyAs"] == "prevention" || (lesson["applyAs"] == "remediation" && effective?(lesson["stats"]))
    end
    entries = picked.map do |lesson|
      { "id" => lesson["id"], "category" => lesson["category"] == GENERAL ? nil : lesson["category"], "text" => lesson["text"],
        "source" => lesson["applyAs"] == "prevention" ? lesson["source"] : "proven-remediation" }.reject { |_key, value| value.nil? }
    end
    QARemediation.dedupe(entries)
  end

  def proven?(stats)
    stats["attempts"].to_i >= QARemediation::PREVENTION_MIN_ATTEMPTS
  end

  def effective?(stats)
    proven?(stats) && stats["resolvedRate"].to_f >= QARemediation::PREVENTION_MIN_RATE
  end

  def empty_stats
    { "attempts" => 0, "resolved" => 0, "resolvedRate" => nil }
  end

  def stats_by_lesson(attempts)
    grouped = {}
    attempts.each do |attempt|
      id = attempt["lessonID"].to_s
      next if id.empty?

      entry = grouped[id] ||= { "attempts" => 0, "resolved" => 0 }
      entry["attempts"] += 1
      entry["resolved"] += 1 if attempt["outcome"] == "resolved"
      entry["lastAt"] = attempt["at"]
    end
    grouped.each_value { |entry| entry["resolvedRate"] = (entry["resolved"].to_f / entry["attempts"]).round(3) }
    grouped
  end

  # 按（类别，补救句）汇总，另按生成模式分开看（ref2va 带配音音轨时跑偏更多）。
  def aggregate(attempts)
    grouped = {}
    attempts.each do |attempt|
      # kind（0.40.0-rc1）：候选图质检的重抽记 image，成片补救与裁剪是 video（旧记录没有这个键）。
      kind = attempt["kind"].to_s.empty? ? "video" : attempt["kind"].to_s
      key = [attempt["category"].to_s, attempt["remediationID"].to_s, kind]
      entry = grouped[key] ||= { "category" => key[0], "remediationID" => key[1], "kind" => kind, "lessonID" => attempt["lessonID"], "text" => attempt["text"],
                                 "attempts" => 0, "resolved" => 0, "byMode" => {} }
      entry["attempts"] += 1
      entry["resolved"] += 1 if attempt["outcome"] == "resolved"
      mode = attempt.dig("traits", "mode").to_s
      mode = "unknown" if mode.empty?
      bucket = entry["byMode"][mode] ||= { "attempts" => 0, "resolved" => 0 }
      bucket["attempts"] += 1
      bucket["resolved"] += 1 if attempt["outcome"] == "resolved"
      entry["lastAt"] = attempt["at"]
    end
    grouped.values.map { |entry| entry.merge("resolvedRate" => (entry["resolved"].to_f / entry["attempts"]).round(3)) }
           .sort_by { |entry| [entry["category"], -entry["attempts"]] }
  end

  def failure(code, message)
    { "ok" => false, "error" => { "code" => code, "message" => message } }
  end
end
