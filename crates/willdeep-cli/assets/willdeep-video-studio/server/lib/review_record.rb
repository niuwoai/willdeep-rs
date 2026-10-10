# frozen_string_literal: true

require "time"
require_relative "creative_schema"

# 内容审核结论的落库形状。短剧对象（策划、角色、分集、分镜）和视频任务存的是
# 同一种结构，校验与裁剪只写这一份，免得两边各自漂移。
module ReviewRecord
  module_function

  # 返回可以直接存进对象的 Hash；结论不符合 creative-v1 的 review 定义时返回 nil。
  #
  # basis 是页面对「审的是哪一份内容」算的指纹，服务端只存不解读。media 记这次
  # 真正送进模型的图片 / 视频个数：页面据此说清「看过画面」还是「只审了提示词」，
  # 这两种结论的分量不一样。
  # 问题的分量（0.35.0-rc1）。severity 说「多严重」，level 说「要不要改」：
  # - must：不改大概率过不了平台审核；
  # - advice：风格、口味或可选的优化，不改也能上线。
  # 审核模型每次复审都会冒出几条新的小意见，只看 warn 永远收不了尾；分出 advice，
  # 只剩 advice 的 warn 就算做完了。没写或写错时按 severity 推：block 一律 must
  # （block 的定义就是不改无法上线），warn 默认 advice。
  LEVELS = %w[must advice].freeze
  ACKNOWLEDGERS = %w[user agent].freeze
  NOTE_LIMIT = 500
  # 专家席审稿（0.42.0-rc1）：结论上另记每位专家的发言记录；每条问题可带提出人。
  PANEL_STATES = %w[spoke failed].freeze
  PANEL_STATUSES = %w[pass warn block].freeze
  MAX_PANEL = 12
  RAISED_BY_LIMIT = 80

  def level_for(issue)
    return "must" if issue["severity"] == "block"

    LEVELS.include?(issue["level"]) ? issue["level"] : "advice"
  end

  def must_fix_count(record)
    Array(record && record["issues"]).count { |issue| issue.is_a?(Hash) && level_for(issue) == "must" }
  end

  # 「已知悉」只对当初确认的那一份结论有效：结论被复审替换、basis 变了就自动作废。
  # current_basis 是按现在的内容算出的指纹；给了就再要求内容也没变。
  def acknowledged?(record, current_basis = nil)
    return false unless record.is_a?(Hash) && record["status"] == "warn"

    acknowledgement = record["acknowledgement"]
    return false unless acknowledgement.is_a?(Hash) && acknowledgement["basis"] == record["basis"]

    current_basis.nil? || current_basis == record["basis"]
  end

  def acknowledgement(by:, note:, basis:)
    { "acknowledgedBy" => by, "note" => clipped(note, NOTE_LIMIT), "at" => Time.now.utc.iso8601, "basis" => basis.to_s }
  end

  # 拒绝确认时抛出，带对外错误码。剧对象与视频任务两边共用。
  class Refused < StandardError
    attr_reader :code

    def initialize(code, message)
      super(message)
      @code = code
    end
  end

  # 在存好的结论上记（或撤销）一次「已知悉」。原地修改 record 并返回它。
  # arguments：basis（调用方读到的那份结论的指纹，可省）、note、revoke。
  def apply_acknowledgement(record, arguments, by)
    raise Refused.new("review_not_found", "There is no stored review for that aspect yet. Run the review first.") unless record.is_a?(Hash)

    expected = arguments["basis"].to_s
    unless expected.empty? || expected == record["basis"].to_s
      raise Refused.new("review_changed", "The stored review is not the one you read (basis differs). Read it again before acknowledging.")
    end
    if arguments["revoke"] == true
      record.delete("acknowledgement")
      return record
    end
    unless record["status"] == "warn"
      raise Refused.new("review_not_acknowledgeable", "Only warn verdicts can be acknowledged. A block verdict has to be fixed; a pass needs nothing.")
    end

    record["acknowledgement"] = acknowledgement(by: by, note: arguments["note"], basis: record["basis"])
    record
  end

  # via / roundtable（0.43.0-rc1）：专家席结论是插件自己并行问出来的（nil）还是宿主圆桌开出来的
  # （"host_roundtable"，附圆桌会话 {reportID, sessionID, rounds}，页面据此说明「在圆桌页里能看到过程」）。
  VIA_LIMIT = 40
  def normalize(review, basis:, model:, media: {}, panel: nil, via: nil, roundtable: nil)
    return nil unless CreativeSchema.valid?("review", review)

    stored = {
      "status" => review["status"],
      "summary" => clipped(review["summary"], 600),
      "issues" => review["issues"].map do |issue|
        entry = {
          "severity" => issue["severity"], "level" => level_for(issue), "category" => issue["category"],
          "location" => clipped(issue["location"], 200), "excerpt" => clipped(issue["excerpt"], 600),
          "reason" => clipped(issue["reason"], 1_000), "suggestion" => clipped(issue["suggestion"], 1_000)
        }
        raised_by = clipped(issue["raisedBy"], RAISED_BY_LIMIT)
        entry["raisedBy"] = raised_by unless raised_by.empty?
        entry
      end,
      "basis" => clipped(basis, 200),
      "model" => clipped(model, 200),
      "mediaImages" => count(media["images"]),
      "mediaVideos" => count(media["videos"]),
      "reviewedAt" => Time.now.utc.iso8601
    }
    # 模型说 pass 却列了 block 级问题时按问题算：宁可多拦一次。
    stored["status"] = "block" if stored["issues"].any? { |issue| issue["severity"] == "block" }
    stored["status"] = "warn" if stored["status"] == "pass" && !stored["issues"].empty?
    transcript = normalize_panel(panel)
    stored["panel"] = transcript if transcript
    source = clipped(via, VIA_LIMIT)
    stored["via"] = source unless source.empty?
    if roundtable.is_a?(Hash) && !roundtable["reportID"].to_s.empty?
      stored["roundtable"] = { "reportID" => clipped(roundtable["reportID"], 80), "sessionID" => clipped(roundtable["sessionID"], 80),
                               "rounds" => count(roundtable["rounds"]) }
    end
    stored
  end

  # 专家席的发言记录：谁、什么职责、发言了没有、立场、一句话、几条意见。正文不存。
  def normalize_panel(panel)
    return nil unless panel.is_a?(Array)

    entries = panel.select { |item| item.is_a?(Hash) && !item["expert"].to_s.strip.empty? }.first(MAX_PANEL).map do |item|
      entry = { "expert" => clipped(item["expert"], 80), "role" => clipped(item["role"], 120),
                "state" => PANEL_STATES.include?(item["state"]) ? item["state"] : "spoke" }
      if entry["state"] == "spoke"
        entry["status"] = item["status"] if PANEL_STATUSES.include?(item["status"])
        entry["summary"] = clipped(item["summary"], 300)
        entry["issueCount"] = count(item["issueCount"])
        entry["mustFix"] = count(item["mustFix"])
        # 宿主圆桌的发言记立场（support / oppose / neutral / undecided），没有逐条意见。
        stance = clipped(item["stance"], 20)
        entry["stance"] = stance unless stance.empty?
      else
        error = item["error"].is_a?(Hash) ? item["error"]["message"] : item["error"]
        entry["error"] = clipped(error, 300)
      end
      entry
    end
    entries.empty? ? nil : entries
  end

  def clipped(value, limit)
    value.to_s.strip[0, limit]
  end

  def count(value)
    number = Integer(value)
    number.between?(0, 100) ? number : 0
  rescue ArgumentError, TypeError
    0
  end
end
