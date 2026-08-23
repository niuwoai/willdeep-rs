#!/usr/bin/env ruby
# frozen_string_literal: true

# 把靶场的历史成绩渲染成趋势，并写回文档。
#
# 为什么要有它：单次靶场回答的是「小模型行不行」，趋势回答的是**「我这次改动
# 让它变好了还是变坏了」**——后者才是回归。`SKILL_WORKERS.md` 里那张
# 2026-08-16 的表是手抄的，抄完就冻在那儿，代码改了七天没人知道它还准不准。
# 所以这里既渲染也注入：数字由脚本写进 marker 区块，人别再用手抄。
#
#   ruby scripts/range_trend.rb                # 打印趋势
#   ruby scripts/range_trend.rb --inject       # 顺便写回 README 与 SKILL_WORKERS.md
#   ruby scripts/range_trend.rb --limit 20
#
# 它不联网、不花钱、不调 Provider：只读 bench/skill-worker-range/history.jsonl。

require 'json'
require 'optparse'

REPO_ROOT = File.expand_path('..', __dir__)

module RangeTrend
  MARKER_BEGIN = '<!-- range:begin -->'
  MARKER_END = '<!-- range:end -->'

  # 注入目标。每个文件都必须已经有一对 marker——脚本不猜该插在哪儿，
  # 猜错一次就是把生成内容塞进正文中间，而且下一次还会再塞一遍。
  INJECT_TARGETS = ['README.md', 'docs/SKILL_WORKERS.md'].freeze

  SPARK = %w[▁ ▂ ▃ ▄ ▅ ▆ ▇ █].freeze
  # 全平的序列画中间那格。取下中位而不是上中位，纯粹是为了有个定义，
  # 免得「平」这件事在不同长度的调色板上画出不同的高度。
  SPARK_MIDDLE = SPARK[(SPARK.size - 1) / 2]

  module_function

  def load_history(path)
    return [] unless File.exist?(path)

    File.readlines(path, encoding: 'UTF-8').filter_map do |line|
      line = line.strip
      next if line.empty?

      begin
        JSON.parse(line)
      rescue JSON::ParserError
        # 坏行跳过而不是整份放弃：历史是 append-only 的，一次写坏
        # 不该让之前所有轮次的成绩一起失效。
        warn "跳过无法解析的历史行：#{line[0, 60]}"
        nil
      end
    end
  end

  # 百分比曲线。null（分母为 0）画成 `·`，不画成谷底——
  # 「没验证」被画成 0% 就是让图替指标撒谎。
  def sparkline(values)
    return '' if values.empty?

    present = values.compact
    return values.map { '·' }.join if present.empty?

    low = present.min
    high = present.max
    values.map do |value|
      next '·' if value.nil?
      next SPARK_MIDDLE if high == low

      index = ((value - low) / (high - low) * (SPARK.size - 1)).round
      SPARK[index]
    end.join
  end

  def percent(value)
    value.nil? ? '-' : format('%.0f%%', value)
  end

  # 与上一轮的差值。首轮没有上一轮，打 `—` 而不是 `+0`：
  # 「没得比」和「没变化」是两件事。
  def delta(current, previous)
    return '—' if current.nil? || previous.nil?

    diff = current - previous
    return '±0' if diff.abs < 0.05

    format('%+.0f', diff)
  end

  def per_case(total, cases)
    cases.to_i.zero? ? '-' : (total.to_f / cases).round
  end

  def render(history, limit: 12)
    return render_empty if history.empty?

    rows = history.last(limit)
    latest = rows.last
    previous = rows.size >= 2 ? rows[-2] : nil

    lines = []
    lines << "最近一轮：**#{latest['ran_at']}** · 模型 `#{latest['model']}` · "\
             "代码 `#{latest['commit'] || '未知'}`#{latest['dirty'] ? '（工作区不干净）' : ''}"
    lines << ''
    lines << '| 指标 | 最近一轮 | 对比上轮 | 趋势 |'
    lines << '|---|---|---|---|'
    metric_rows(rows, latest, previous).each { |row| lines << row }
    lines << ''
    lines << "样本 #{latest['cases']}（可验证 #{latest['verified_cases']} · 只读 #{latest['report_cases']}）"\
             " · 平均 #{per_case(latest['tokens'], latest['cases'])} token/样本"\
             " · #{per_case(latest['seconds'], latest['cases'])} 秒/样本"\
             " · 作弊 #{latest['cheated']}"
    unless latest['mis_seeded'].to_a.empty?
      lines << ''
      lines << "⚠️ 派工前 verifier 未变红的无效样本：#{latest['mis_seeded'].join(', ')}"
    end
    lines << ''
    lines << "<details><summary>历史 #{rows.size} 轮</summary>"
    lines << ''
    lines << '| 时间 | 代码 | 模型 | 样本 | Verified Success | 作弊 | 引用准确率 | 答对率 | 平均尝试 |'
    lines << '|---|---|---|---:|---|---:|---|---|---|'
    rows.reverse_each { |row| lines << history_row(row) }
    lines << ''
    lines << '</details>'
    lines.join("\n")
  end

  def metric_rows(rows, latest, previous)
    [
      ['**Worker Verified Success**', 'worker_verified_success'],
      ['只读工种引用准确率', 'citation_accuracy'],
      ['只读工种答对率', 'report_answer_rate']
    ].map do |label, key|
      format(
        '| %s | %s | %s | `%s` |',
        label,
        percent(latest[key]),
        delta(latest[key], previous && previous[key]),
        sparkline(rows.map { |row| row[key] })
      )
    end
  end

  def history_row(row)
    attempts = row['cases'].to_i.zero? ? '-' : format('%.2f', row['attempts'].to_f / row['cases'])
    format(
      '| %s | `%s` | `%s` | %d | %s | %d | %s | %s | %s |',
      row['ran_at'], row['commit'] || '?', row['model'], row['cases'],
      percent(row['worker_verified_success']), row['cheated'].to_i,
      percent(row['citation_accuracy']), percent(row['report_answer_rate']), attempts
    )
  end

  def render_empty
    <<~TEXT.strip
      靶场还没有跑过（`bench/skill-worker-range/history.jsonl` 为空）。

      ```bash
      ruby scripts/skill_worker_range.rb
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

    head = text[0, from]
    tail = text[(to + MARKER_END.length)..] || ''
    "#{head}#{MARKER_BEGIN}\n#{block}\n#{MARKER_END}#{tail}"
  end
end

if __FILE__ == $PROGRAM_NAME
  options = {
    history: File.join(REPO_ROOT, 'bench', 'skill-worker-range'),
    limit: 12,
    inject: false
  }

  OptionParser.new do |parser|
    parser.banner = 'Usage: ruby scripts/range_trend.rb [options]'
    parser.on('--history DIR', '成绩归档目录，默认 bench/skill-worker-range') { |v| options[:history] = v }
    parser.on('--limit N', Integer, '最多显示多少轮，默认 12') { |v| options[:limit] = v }
    parser.on('--inject', "把结果写回 #{RangeTrend::INJECT_TARGETS.join('、')}") { options[:inject] = true }
  end.parse!

  history_path = File.join(options[:history], 'history.jsonl')
  history = RangeTrend.load_history(history_path)
  block = RangeTrend.render(history, limit: options[:limit])

  puts block

  if options[:inject]
    RangeTrend::INJECT_TARGETS.each do |relative|
      path = File.join(REPO_ROOT, relative)
      original = File.read(path, encoding: 'UTF-8')
      updated = RangeTrend.inject(original, block)
      if original == updated
        puts "未变化: #{relative}"
      else
        File.write(path, updated)
        puts "已更新: #{relative}"
      end
    end
  end
end
