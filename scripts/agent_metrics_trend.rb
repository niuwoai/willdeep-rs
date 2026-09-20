#!/usr/bin/env ruby
# frozen_string_literal: true

# 把线上派工指标的历史快照渲染成趋势，写回文档，没达标就报警。
#
#   ruby scripts/agent_metrics_trend.rb            # 打印趋势
#   ruby scripts/agent_metrics_trend.rb --inject   # 写回 README.md 与 docs/AGENT_METRICS.md 的 marker 区块
#   ruby scripts/agent_metrics_trend.rb --alarm    # 最新快照的窗口指标有没达标的就退出 1
#
# 靶场（range_trend）比的是「这次改动让它变好还是变坏」，所以对基线报警；
# 这里比的是「线上派工离设计目标还有多远」，所以对绝对目标报警——目标值与
# CLI 打印在每个比率旁边的是同一组常量。
# 它不联网、不花钱：只读 bench/agent-metrics/history.jsonl。

require 'json'
require 'optparse'

require_relative 'range_trend'
require_relative 'lib/agent_metrics_report'

module AgentMetricsTrend
  MARKER_BEGIN = '<!-- agent-metrics:begin -->'
  MARKER_END = '<!-- agent-metrics:end -->'
  INJECT_TARGETS = ['README.md', 'docs/AGENT_METRICS.md'].freeze

  # 趋势表里的行：标题、字段名。粗体的两条是 ADR 第 6 项点名要对外发布的。
  ROWS = [
    ['**Deep Share**', 'deep_share'],
    ['Skill Coverage', 'skill_coverage'],
    ['**Worker Verified Success**', 'worker_verified_success'],
    ['Escalation Rate', 'escalation_rate'],
    ['只读工种引用准确率', 'citation_accuracy']
  ].freeze

  module_function

  def render(history, limit: 12)
    return render_empty if history.empty?

    rows = history.last(limit)
    latest = rows.last
    previous = rows.size >= 2 ? rows[-2] : nil
    recent = latest['recent']
    total = latest['total']

    lines = []
    lines << "最近快照：**#{latest['ran_at']}** · 窗口 #{latest['window']} · 代码 `#{latest['commit'] || '未知'}`" \
             "#{latest['dirty'] ? '（工作区不干净）' : ''} · 版本 `#{latest['version'] || '未知'}`"
    lines << ''
    lines << "| 指标 | 近 #{latest['window']} | 对比上次 | 目标 | 趋势 |"
    lines << '|---|---|---|---|---|'
    ROWS.each do |label, key|
      lines << format('| %s | %s | %s | %s | `%s` |', label, RangeTrend.percent(recent[key]),
                      RangeTrend.delta(recent[key], previous && previous['recent'][key]),
                      AgentMetricsReport.target_text(key), RangeTrend.sparkline(rows.map { |row| row['recent'][key] }))
    end
    lines << ''
    lines << "近 #{latest['window']}：子 Agent 运行 #{recent['children']}（窄工种 #{recent['workers']} · 标准 #{recent['standard']}" \
             " · deep #{recent['deep']}）· 有 verifier #{recent['verified_runs']} · 未验证 #{recent['unverified_runs']}" \
             " · 平均尝试 #{attempts_text(recent)}"
    lines << "累计：子 Agent 运行 #{total['children']} · Deep Share #{RangeTrend.percent(total['deep_share'])}" \
             " · Worker Verified Success #{RangeTrend.percent(total['worker_verified_success'])}" \
             "（#{total['passed']}/#{total['verified_runs']}）· 未验证 #{total['unverified_runs']}"
    missed = AgentMetricsReport.misses(recent)
    if missed.any?
      lines << ''
      lines << "⚠️ 未达标：#{missed.map { |miss| "#{miss[:label]} #{RangeTrend.percent(miss[:value])}（目标 #{AgentMetricsReport.target_text(miss[:key])}）" }.join('；')}"
    end
    lines << ''
    lines << "<details><summary>历史 #{rows.size} 次快照</summary>"
    lines << ''
    lines << '| 时间 | 代码 | 窗口 | 子运行 | Deep Share | Skill Coverage | Verified Success | Escalation | 引用准确率 | 平均尝试 |'
    lines << '|---|---|---|---:|---|---|---|---|---|---|'
    rows.reverse_each { |row| lines << history_row(row) }
    lines << ''
    lines << '</details>'
    lines.join("\n")
  end

  def attempts_text(metrics)
    value = metrics['attempts_per_verified_run']
    value.nil? ? '-' : format('%.2f', value)
  end

  def history_row(row)
    recent = row['recent']
    format('| %s | `%s` | %s | %d | %s | %s | %s | %s | %s | %s |',
           row['ran_at'], row['commit'] || '?', row['window'], recent['children'].to_i,
           RangeTrend.percent(recent['deep_share']), RangeTrend.percent(recent['skill_coverage']),
           RangeTrend.percent(recent['worker_verified_success']), RangeTrend.percent(recent['escalation_rate']),
           RangeTrend.percent(recent['citation_accuracy']), attempts_text(recent))
  end

  def render_empty
    <<~TEXT.strip
      还没有拍过快照（`bench/agent-metrics/history.jsonl` 为空）。

      ```bash
      ruby scripts/agent_metrics_publish.rb
      ```

      它只读本机 Runtime 的 agent 记录，不花钱，拍完自动归档并在这里长出趋势。
    TEXT
  end

  # 最新一次快照的窗口指标里没达标的。只有一次快照也报——这里比的是目标，不是上一次。
  def alarms(history)
    return [] if history.empty?

    AgentMetricsReport.misses(history.last['recent'])
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
  options = { history: File.join(REPO_ROOT, 'bench', 'agent-metrics'), limit: 12, inject: false, alarm: false }
  OptionParser.new do |parser|
    parser.banner = 'Usage: ruby scripts/agent_metrics_trend.rb [options]'
    parser.on('--history DIR', '归档目录，默认 bench/agent-metrics') { |v| options[:history] = v }
    parser.on('--limit N', Integer, '最多显示多少次快照，默认 12') { |v| options[:limit] = v }
    parser.on('--inject', "把结果写回 #{AgentMetricsTrend::INJECT_TARGETS.join('、')}") { options[:inject] = true }
    parser.on('--alarm', '最新快照的窗口指标没达标时退出 1') { options[:alarm] = true }
  end.parse!

  history = RangeTrend.load_history(File.join(options[:history], 'history.jsonl'))
  block = AgentMetricsTrend.render(history, limit: options[:limit])
  puts block

  if options[:inject]
    AgentMetricsTrend::INJECT_TARGETS.each do |relative|
      path = File.join(REPO_ROOT, relative)
      original = File.read(path, encoding: 'UTF-8')
      updated = AgentMetricsTrend.inject(original, block)
      if original == updated
        puts "未变化: #{relative}"
      else
        File.write(path, updated)
        puts "已更新: #{relative}"
      end
    end
  end

  if options[:alarm]
    found = AgentMetricsTrend.alarms(history)
    found.each do |miss|
      warn "报警：#{miss[:label]} #{miss[:value]}%，目标 #{AgentMetricsReport.target_text(miss[:key])}"
    end
    exit 1 if found.any?
  end
end
