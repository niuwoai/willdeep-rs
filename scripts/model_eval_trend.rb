#!/usr/bin/env ruby
# frozen_string_literal: true

# 把模型行为评测的历史成绩渲染成趋势，必要时报警，并写回 docs/MODEL_EVAL.md。
#
#   ruby scripts/model_eval_trend.rb            # 打印趋势
#   ruby scripts/model_eval_trend.rb --alarm    # 通过率或人话率比基线掉 10 个点以上就退出 1
#   ruby scripts/model_eval_trend.rb --inject   # 写回 docs/MODEL_EVAL.md 的 marker 区块
#
# 基线取「至少七天前的最近一轮」；还没跑满一周就拿上一轮凑合，只跑过一轮
# 则没得比、不报警——「没得比」和「没变化」是两件事。
# 它不联网、不花钱：只读 bench/model-eval/history.jsonl。

require 'json'
require 'optparse'
require 'time'

require_relative 'range_trend'

module ModelEvalTrend
  MARKER_BEGIN = '<!-- model-eval:begin -->'
  MARKER_END = '<!-- model-eval:end -->'
  INJECT_TARGET = 'docs/MODEL_EVAL.md'
  ALARM_DROP_POINTS = 10.0
  BASELINE_DAYS = 7
  ALARMED = [['verifier 通过率', 'pass_rate'], ['人话率', 'narration_ratio']].freeze
  WATCHED = ALARMED + [['误报完成率', 'false_completion_rate'], ['思维链占比', 'reasoning_ratio']]

  module_function

  def by_model(history)
    history.group_by { |row| row['model'].to_s }.transform_values { |rows| rows.sort_by { |row| row['ran_at'].to_s } }
  end

  def baseline(rows, latest)
    earlier = rows.reject { |row| row.equal?(latest) || row['ran_at'].to_s >= latest['ran_at'].to_s }
    return nil if earlier.empty?

    cutoff = Time.parse(latest['ran_at']) - BASELINE_DAYS * 86_400
    earlier.reverse.find { |row| Time.parse(row['ran_at']) <= cutoff } || earlier.last
  end

  def alarms(history)
    by_model(history).flat_map do |model, rows|
      latest = rows.last
      base = baseline(rows, latest)
      next [] unless base

      ALARMED.filter_map do |label, key|
        current = latest[key]
        reference = base[key]
        next if current.nil? || reference.nil?

        drop = reference - current
        next if drop <= ALARM_DROP_POINTS

        { model: model, label: label, key: key, latest: current, baseline: reference,
          baseline_ran_at: base['ran_at'], drop: drop.round(1) }
      end
    end
  end

  def render(history, limit: 10)
    return render_empty if history.empty?

    lines = []
    by_model(history).each do |model, rows|
      shown = rows.last(limit)
      latest = shown.last
      base = baseline(rows, latest)
      lines << "### #{model}"
      lines << ''
      lines << "最近一轮 **#{latest['ran_at']}** · 代码 `#{latest['commit'] || '未知'}`#{latest['dirty'] ? '（工作区不干净）' : ''}" \
               " · 执行 #{latest['executed']}/#{latest['tasks']} · 通过 #{latest['passed']} · 作弊 #{latest['cheated']}" \
               " · 出错 #{latest['errors']}"
      lines << ''
      lines << "| 指标 | 最近一轮 | 对比基线#{base ? "（#{base['ran_at'][0, 10]}）" : ''} | 趋势 |"
      lines << '|---|---|---|---|'
      WATCHED.each do |label, key|
        lines << format('| %s | %s | %s | `%s` |', label, RangeTrend.percent(latest[key]),
                        RangeTrend.delta(latest[key], base && base[key]), RangeTrend.sparkline(shown.map { |row| row[key] }))
      end
      lines << ''
      lines << "<details><summary>历史 #{shown.size} 轮</summary>"
      lines << ''
      lines << '| 时间 | 代码 | 执行 | 通过率 | 作弊 | 人话率 | 静默工具轮 | 工具失败 | 秒/任务 |'
      lines << '|---|---|---:|---|---:|---|---:|---:|---:|'
      shown.reverse_each { |row| lines << history_row(row) }
      lines << ''
      lines << '</details>'
      lines << ''
    end
    found = alarms(history)
    if found.any?
      lines << '⚠️ **报警**：'
      found.each do |alarm|
        lines << "- `#{alarm[:model]}` #{alarm[:label]} #{RangeTrend.percent(alarm[:latest])}，" \
                 "比 #{alarm[:baseline_ran_at][0, 10]} 的 #{RangeTrend.percent(alarm[:baseline])} 掉了 #{alarm[:drop]} 个点"
      end
      lines << ''
    end
    lines.join("\n").strip
  end

  def history_row(row)
    per_task = row['executed'].to_i.zero? ? '-' : (row['seconds'].to_f / row['executed']).round
    format('| %s | `%s` | %d | %s | %d | %s | %d | %d | %s |',
           row['ran_at'], row['commit'] || '?', row['executed'].to_i, RangeTrend.percent(row['pass_rate']),
           row['cheated'].to_i, RangeTrend.percent(row['narration_ratio']), row['silent_tool_turns'].to_i,
           row['tool_failures'].to_i, per_task)
  end

  def render_empty
    <<~TEXT.strip
      任务集还没有跑过（`bench/model-eval/history.jsonl` 为空）。

      ```bash
      ruby scripts/model_eval.rb --model glm-5
      ```

      它会真的调用 Provider、真的花钱，跑完自动归档并在这里长出趋势。
    TEXT
  end

  # 注入是纯文本替换，且必须幂等：同一份内容注入两次，文件应当一模一样。
  def inject(text, block)
    from = text.index(MARKER_BEGIN)
    to = text.index(MARKER_END)
    raise "文件里找不到 #{MARKER_BEGIN} / #{MARKER_END} 这对标记" if from.nil? || to.nil?
    raise "#{MARKER_END} 出现在 #{MARKER_BEGIN} 前面" if to < from

    "#{text[0, from]}#{MARKER_BEGIN}\n#{block}\n#{MARKER_END}#{text[(to + MARKER_END.length)..] || ''}"
  end
end

if $PROGRAM_NAME == __FILE__
  options = { history: File.join(REPO_ROOT, 'bench', 'model-eval'), limit: 10, inject: false, alarm: false }
  OptionParser.new do |parser|
    parser.banner = 'Usage: ruby scripts/model_eval_trend.rb [options]'
    parser.on('--history DIR', '归档目录，默认 bench/model-eval') { |v| options[:history] = v }
    parser.on('--limit N', Integer, '每个模型最多显示多少轮，默认 10') { |v| options[:limit] = v }
    parser.on('--inject', "把结果写回 #{ModelEvalTrend::INJECT_TARGET}") { options[:inject] = true }
    parser.on('--alarm', "通过率或人话率比基线掉超过 #{ModelEvalTrend::ALARM_DROP_POINTS.to_i} 个点时退出 1") { options[:alarm] = true }
  end.parse!

  history = RangeTrend.load_history(File.join(options[:history], 'history.jsonl'))
  block = ModelEvalTrend.render(history, limit: options[:limit])
  puts block

  if options[:inject]
    path = File.join(REPO_ROOT, ModelEvalTrend::INJECT_TARGET)
    original = File.read(path, encoding: 'UTF-8')
    updated = ModelEvalTrend.inject(original, block)
    if original == updated
      puts "未变化: #{ModelEvalTrend::INJECT_TARGET}"
    else
      File.write(path, updated)
      puts "已更新: #{ModelEvalTrend::INJECT_TARGET}"
    end
  end

  if options[:alarm]
    found = ModelEvalTrend.alarms(history)
    found.each do |alarm|
      warn "报警：#{alarm[:model]} #{alarm[:label]} #{alarm[:latest]}% ← 基线 #{alarm[:baseline]}%（#{alarm[:baseline_ran_at]}），掉 #{alarm[:drop]} 点"
    end
    exit 1 if found.any?
  end
end
