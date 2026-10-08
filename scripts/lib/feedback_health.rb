# frozen_string_literal: true

require 'json'
require 'time'

module FeedbackHealth
  SCHEMA = 'willdeep.feedback.v1'
  TERMINAL = %w[suggestion_sent_verbatim suggestion_sent_edited suggestion_sent_rewritten
                suggestion_ignored_typed suggestion_dismissed suggestion_superseded].freeze
  module_function

  def read(dir)
    rows = []
    problems = Hash.new(0)
    ids = {}
    Dir.glob(File.join(dir, '*.jsonl')).sort.each do |path|
      File.foreach(path) do |line|
        row = JSON.parse(line)
        unless row.is_a?(Hash) && row['schema'] == SCHEMA && row['signal'].is_a?(String)
          problems['unsupported_or_invalid'] += 1
          next
        end
        Time.iso8601(row.fetch('ts'))
        if row['id'] && ids[row['id']]
          problems['duplicates'] += 1
          next
        end
        ids[row['id']] = true if row['id']
        rows << row
      rescue JSON::ParserError, ArgumentError, KeyError, TypeError
        problems['malformed'] += 1
      end
    end
    [rows, problems]
  end

  def counts(rows, key)
    rows.group_by { |row| safe_label(row[key]) }.transform_values(&:size)
  end

  def safe_label(value)
    value.is_a?(String) && value.match?(/\A[a-z_]{1,64}\z/) ? value : 'unknown'
  end

  def ratio(hits, total)
    total.zero? ? nil : (hits * 100.0 / total).round(1)
  end

  def summarize(rows, problems = {})
    followups = rows.select { |row| row['signal'] == 'user_followup' }
    verified = rows.select { |row| row['signal'] == 'run_verified' }
    starts = rows.select { |row| row['signal'] == 'run_started' && row['run_id'] }
    run_ids = starts.map { |row| row['run_id'] }.uniq
    finished_ids = verified.map { |row| row['run_id'] }.compact.uniq & run_ids
    resolved = rows.select { |row| run_ids.include?(row['run_id']) && %w[tool_failed tool_succeeded].include?(row['signal']) }
    resolved_failures = resolved.count { |row| row['signal'] == 'tool_failed' }
    suggestions = rows.select { |row| row['signal'].start_with?('suggestion_') }
    shown = suggestions.select { |row| row['signal'] == 'suggestion_shown' }.map { |row| row['suggestion_id'] }.compact.uniq
    terminal = suggestions.select { |row| TERMINAL.include?(row['signal']) }.map { |row| row['suggestion_id'] }.compact.uniq
    tool_failures = rows.select { |row| row['signal'] == 'tool_failed' }
    observations = tool_failures.reject { |row| %w[approval_denied hook_denied].include?(row['error_class']) }
      .group_by { |row| [safe_label(row['tool']), safe_label(row['error_class'])] }
      .map { |(tool, error), list| { 'tool' => tool, 'error_class' => error, 'count' => list.size, 'status' => 'investigate_only' } }
      .sort_by { |item| -item['count'] }
    linked = followups.count { |row| row['prev_turn_id'] }
    {
      'schema' => 'willdeep.feedback-health.v1', 'generated_at' => Time.now.utc.iso8601,
      'rows' => rows.size, 'sessions' => rows.map { |row| row['session_id'] }.compact.uniq.size,
      'first' => rows.map { |row| row['ts'] }.min, 'last' => rows.map { |row| row['ts'] }.max,
      'read_problems' => problems, 'clients' => counts(rows, 'client'), 'signals' => counts(rows, 'signal'),
      'followups' => { 'total' => followups.size, 'linked' => linked, 'linked_percent' => ratio(linked, followups.size),
                       'hints' => counts(followups, 'followup_hint') },
      'verification' => { 'observations' => verified.size, 'states' => counts(verified, 'verification'),
                          'started_runs' => run_ids.empty? ? nil : run_ids.size,
                          'attributed_runs_percent' => ratio(starts.count { |row| row['provider'] && row['model'] }, starts.size),
                          'coverage_percent' => ratio(finished_ids.size, run_ids.size) },
      'resolved_tools' => { 'total' => run_ids.empty? ? nil : resolved.size,
                            'failures' => run_ids.empty? ? nil : resolved_failures,
                            'failure_percent' => ratio(resolved_failures, resolved.size) },
      'suggestions' => { 'shown_unique' => shown.size, 'settled_unique' => (shown & terminal).size,
                         'unsettled_unique' => (shown - terminal).size, 'generation_attempts' => nil },
      'unavailable' => (run_ids.empty? ? %w[total_runs resolved_tool_calls] : []) +
        %w[in_flight_tool_calls suggestion_generation_latency sink_drops],
      'investigations' => observations,
      'notes' => ['缺少分母的指标保持 null；未知结局不算成功。', 'investigate_only 只供复现和补测试，不降低候选晋升门槛。']
    }
  end

  def markdown(report)
    display = ->(value) { value.nil? ? '未知' : value.to_s }
    hints = report['followups']['hints'].map { |hint, count| "#{hint}=#{count}" }.join('，')
    investigations = report['investigations'].map { |item| "| #{item['tool']} | #{item['error_class']} | #{item['count']} | 待调查 |" }
    ["# RSI 反馈健康报告", '', "事件：#{report['rows']}；会话：#{report['sessions']}。",
     "后续输入：#{report['followups']['total']}；已关联上一轮：#{report['followups']['linked']}。#{hints}",
     "建议展示：#{report['suggestions']['shown_unique']}；尚无终结事件：#{report['suggestions']['unsettled_unique']}。",
     "新记录的运行数：#{display.call(report['verification']['started_runs'])}；收尾验证记录覆盖率（%）：#{display.call(report['verification']['coverage_percent'])}；起始模型归属覆盖率（%）：#{display.call(report['verification']['attributed_runs_percent'])}。",
     "新记录的已结算工具调用：#{display.call(report['resolved_tools']['total'])}；失败比例（%）：#{display.call(report['resolved_tools']['failure_percent'])}。",
     '', '## 待调查问题', '', '| 工具 | 错误类别 | 次数 | 状态 |', '| --- | --- | ---: | --- |',
     *investigations, '', '## 尚缺观测', '', *report['unavailable'].map { |item| "- #{item}" }, '', *report['notes'], ''].join("\n")
  end
end
